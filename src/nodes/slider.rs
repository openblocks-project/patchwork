use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

const SLIDER_HEIGHT: f32 = 160.0;
const KNOB_RADIUS: f32 = 12.0;
const RAIL_WIDTH: f32 = 4.0;
const TRACK_HIT_WIDTH: f32 = 30.0; // clickable area around the rail

pub fn render(
    ui: &mut egui::Ui,
    value: &mut f32,
    min: &mut f32,
    max: &mut f32,
    step: &mut f32,
    slider_color: &mut [u8; 3],
    label: &mut String,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let in_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    if in_wired {
        let raw = Graph::static_input_value(connections, values, node_id, 0).as_float();
        *value = raw.clamp(*min, *max);
    }

    let knob_color = egui::Color32::from_rgb(slider_color[0], slider_color[1], slider_color[2]);

    // ── Input port (top center) ─────────────────────────
    ui.vertical_centered(|ui| {
        super::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
    });

    ui.add_space(4.0);

    // ── Vertical slider track ───────────────────────────
    // Allocate full area with hover only (so window can still drag for movement)
    let w = ui.available_width();
    let (rect, body_response) = ui.allocate_exact_size(
        egui::vec2(w.max(40.0), SLIDER_HEIGHT),
        egui::Sense::click(), // click only — drag is handled by window (for movement)
    );
    let painter = ui.painter();
    let cx = rect.center().x;

    // Overlay a draggable track strip in the center (this consumes drag, preventing window move)
    let track_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(TRACK_HIT_WIDTH, SLIDER_HEIGHT),
    );
    let track_response = ui.interact(track_rect, egui::Id::new(("slider_track", node_id)), egui::Sense::click_and_drag());

    // Rail
    let rail_rect = egui::Rect::from_center_size(
        egui::pos2(cx, rect.center().y),
        egui::vec2(RAIL_WIDTH, SLIDER_HEIGHT - KNOB_RADIUS * 2.0),
    );
    painter.rect_filled(rail_rect, 2.0, egui::Color32::from_rgb(40, 40, 50));

    // Knob position
    let range = (*max - *min).max(0.0001);
    let t = ((*value - *min) / range).clamp(0.0, 1.0);
    let knob_y = rect.bottom() - KNOB_RADIUS - t * (SLIDER_HEIGHT - KNOB_RADIUS * 2.0);

    // Filled portion
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(rail_rect.left(), knob_y), rail_rect.max),
        2.0, knob_color.linear_multiply(0.4),
    );

    // Knob
    painter.circle_filled(egui::pos2(cx, knob_y), KNOB_RADIUS, knob_color);

    // Track drag → adjust value
    if (track_response.dragged() || track_response.clicked()) && !in_wired {
        if let Some(pos) = track_response.interact_pointer_pos() {
            let rel = (rect.bottom() - KNOB_RADIUS - pos.y) / (SLIDER_HEIGHT - KNOB_RADIUS * 2.0);
            let new_val = *min + rel.clamp(0.0, 1.0) * range;
            *value = if *step > 0.0 {
                (*min + (((new_val - *min) / *step).round() * *step)).clamp(*min, *max)
            } else {
                new_val.clamp(*min, *max)
            };
        }
    }

    // Click on body (not on track, not dragged) → open popup
    let popup_id = egui::Id::new(("slider_popup", node_id));
    let popup_time_id = egui::Id::new(("slider_popup_time", node_id));
    let now = ui.ctx().input(|i| i.time);
    if body_response.clicked() && !track_response.clicked() {
        let is_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
        if !is_open {
            ui.ctx().data_mut(|d| {
                d.insert_temp(popup_id, true);
                d.insert_temp(popup_time_id, now);
            });
        }
    }

    ui.add_space(2.0);

    // ── Value display ───────────────────────────────────
    // No hard range constraint — if user types a value beyond, expand the range
    ui.vertical_centered(|ui| {
        if ui.add(egui::DragValue::new(value).speed(*step)).changed() && !in_wired {
            if *value > *max { *max = *value; }
            if *value < *min { *min = *value; }
        }
    });

    // Label — show default "Slider #N" if empty
    ui.vertical_centered(|ui| {
        let display_label = if label.is_empty() {
            format!("Slider #{}", node_id)
        } else {
            label.clone()
        };
        ui.label(egui::RichText::new(display_label).small().color(egui::Color32::GRAY));
    });

    ui.add_space(2.0);

    // ── Output port (bottom center) ─────────────────────
    ui.vertical_centered(|ui| {
        super::inline_port_circle(ui, node_id, 0, false, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
    });

    // Selection feedback (the orange/accent ring) is drawn centrally by
    // PatchworkApp for every node — see `app/mod.rs` "Draw selection
    // highlight". The popup-open state is signalled by the popup contents
    // appearing alongside the node; a second ring here would just double
    // up with the global selection ring.

    // ── Options popup (below the node) ──────────────────
    let popup_is_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
    if popup_is_open {
        let popup_pos = egui::pos2(rect.right() + 8.0, rect.top());
        let opened_time = ui.ctx().data_mut(|d| d.get_temp::<f64>(popup_time_id).unwrap_or(0.0));

        let area_resp = egui::Area::new(egui::Id::new(("slider_opts", node_id)))
            .fixed_pos(popup_pos)
            .order(egui::Order::Tooltip)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).corner_radius(12.0).inner_margin(12.0).show(ui, |ui| {
                    ui.set_min_width(200.0);

                    // Row 1: Pin | Color | Label | Delete
                    ui.horizontal(|ui| {
                        if ui.small_button(crate::icons::PUSH_PIN).on_hover_text("Pin").clicked() {
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("slider_pin_action", node_id)), true));
                            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
                        }
                        let mut color = egui::Color32::from_rgb(slider_color[0], slider_color[1], slider_color[2]);
                        if ui.color_edit_button_srgba(&mut color).changed() {
                            let a = color.to_array();
                            *slider_color = [a[0], a[1], a[2]];
                        }
                        ui.add(egui::TextEdit::singleline(label).hint_text(format!("Slider #{}", node_id)).desired_width(90.0));
                        if ui.small_button(egui::RichText::new(crate::icons::TRASH).color(egui::Color32::from_rgb(200, 80, 80))).clicked() {
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("slider_delete_action", node_id)), true));
                            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
                        }
                    });

                    ui.add_space(4.0);

                    // Value — expand range if user types beyond
                    ui.horizontal(|ui| {
                        ui.label("Value");
                        if ui.add(egui::DragValue::new(value).speed(*step)).changed() && !in_wired {
                            if *value > *max { *max = *value; }
                            if *value < *min { *min = *value; }
                        }
                    });

                    // Range
                    ui.horizontal(|ui| {
                        ui.label("Range");
                        ui.add(egui::DragValue::new(min).speed(0.1));
                        ui.label("—");
                        ui.add(egui::DragValue::new(max).speed(0.1));
                    });

                    // Step
                    ui.horizontal(|ui| {
                        ui.label("Step");
                        ui.add(egui::DragValue::new(step).speed(0.001).range(0.0001..=100.0));
                    });

                    // Node ID (bottom, subtle)
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(format!("ID: {}", node_id)).small().color(egui::Color32::from_rgb(80, 80, 80)));
                });
            });

        // Close logic: use actual popup rect, skip the frame it was opened
        let popup_rect = area_resp.response.rect;
        let esc = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));

        // Only check for click-outside after 0.2s (so opening click doesn't immediately close)
        let click_outside = if now - opened_time > 0.2 {
            ui.ctx().input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
                && ui.ctx().pointer_latest_pos().map(|p| {
                    !popup_rect.contains(p) && !rect.contains(p)
                }).unwrap_or(false)
        } else {
            false
        };

        if esc || click_outside {
            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
        }
    }
}
