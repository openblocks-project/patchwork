use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let (points, mode, speed, looping, manual_gate, sustain_index,
         phase, playing, last_phase, last_step_index) = match node_type {
        NodeType::Curve { points, mode, speed, looping, manual_gate, sustain_index,
                          phase, playing, last_phase, last_step_index, .. } =>
            (points, mode, speed, looping, manual_gate, sustain_index,
             phase, playing, last_phase, last_step_index),
        _ => return,
    };

    // Ensure at least 2 points
    if points.len() < 2 {
        *points = vec![[0.0, 0.0], [1.0, 1.0]];
    }
    // Treat legacy LFO (mode==2) as Played for the renderer; the eval
    // block will rewrite the field to mode=1 + looping=true on the next
    // frame, but we want the segmented row to show the correct state
    // immediately when an old project is opened.
    let played = *mode != 0;

    // ── Mode selector (top of node, segmented) ───────────────────────
    ui.horizontal(|ui| {
        let avail = ui.available_width();
        let half = (avail - 6.0) * 0.5;
        if ui.add_sized([half, 24.0],
            egui::SelectableLabel::new(*mode == 0, "Lookup")).clicked()
        {
            *mode = 0;
            *playing = false;
        }
        ui.add_space(6.0);
        if ui.add_sized([half, 24.0],
            egui::SelectableLabel::new(played, "Played")).clicked()
        {
            *mode = 1;
        }
    });

    ui.separator();

    // ── Input ports ───────────────────────────────────────────────────
    // Port 0: X (always shown — primary input in Lookup mode)
    let x_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    let x_input = Graph::static_input_value(connections, values, node_id, 0).as_float().clamp(0.0, 1.0);

    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);
        ui.label(egui::RichText::new("X:").small());
        if x_wired || *mode == 0 {
            ui.label(egui::RichText::new(format!("{:.2}", x_input)).small().color(
                if x_wired { egui::Color32::from_rgb(80, 170, 255) } else { egui::Color32::GRAY }
            ));
        } else {
            ui.label(egui::RichText::new("—").small().color(egui::Color32::from_rgb(70, 70, 80)));
        }
    });

    // Played-mode-only input ports
    if played {
        // Port 1: Trigger
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
            ui.label(egui::RichText::new("Trigger").small());
        });

        // Port 2: Speed
        let speed_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 2, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("Speed:").small());
            if speed_wired {
                ui.label(egui::RichText::new(format!("{:.1}x", speed)).small().color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                ui.add(egui::DragValue::new(speed).speed(0.05).range(0.1..=20.0).suffix("x"));
            }
        });

        // Port 3: Gate — checkbox-when-not-wired (mirrors synth gate UX)
        let gate_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 3);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 3, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Gate);
            ui.label(egui::RichText::new("Gate").small());
            if gate_wired {
                let g = Graph::static_input_value(connections, values, node_id, 3).as_float();
                ui.label(egui::RichText::new(if g > 0.5 { "ON" } else { "off" })
                    .small().monospace()
                    .color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                ui.checkbox(manual_gate, "");
            }
            if sustain_index.is_some() {
                ui.label(egui::RichText::new("· sustain")
                    .small()
                    .color(egui::Color32::from_rgb(255, 200, 60)));
            }
        });
    }

    ui.separator();

    // ── Presets ────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        if ui.small_button("Lin").clicked() { *points = vec![[0.0, 0.0], [1.0, 1.0]]; }
        if ui.small_button("Ease").clicked() { *points = vec![[0.0, 0.0], [0.4, 0.0], [0.6, 1.0], [1.0, 1.0]]; }
        if ui.small_button("S").clicked() { *points = vec![[0.0, 0.0], [0.3, 0.0], [0.7, 1.0], [1.0, 1.0]]; }
    });
    ui.horizontal(|ui| {
        if ui.small_button("ADSR").clicked() {
            *points = vec![[0.0, 0.0], [0.1, 1.0], [0.3, 0.7], [0.8, 0.7], [1.0, 0.0]];
        }
        if ui.small_button("Bell").clicked() {
            *points = vec![[0.0, 0.0], [0.3, 0.0], [0.5, 1.0], [0.7, 0.0], [1.0, 0.0]];
        }
        if ui.small_button("Notch").clicked() {
            *points = vec![[0.0, 1.0], [0.3, 1.0], [0.5, 0.0], [0.7, 1.0], [1.0, 1.0]];
        }
    });

    // ── Curve editor ──────────────────────────────────────────────────
    let size = 180.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    // Background
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 20, 30));
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 70)), egui::StrokeKind::Outside);

    // Grid
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let x = rect.left() + t * size;
        let y = rect.top() + t * size;
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(0.5, egui::Color32::from_rgb(35, 35, 50)));
        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(0.5, egui::Color32::from_rgb(35, 35, 50)));
    }

    // Draw curve
    let steps = 50;
    let mut prev_screen = None;
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let y = evaluate_curve(points, t);
        let sx = rect.left() + t * size;
        let sy = rect.bottom() - y.clamp(0.0, 1.0) * size;
        let screen_pt = egui::pos2(sx, sy);
        if let Some(prev) = prev_screen {
            painter.line_segment([prev, screen_pt], egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 200, 160)));
        }
        prev_screen = Some(screen_pt);
    }

    // ── Playback head (Played mode) ──────────────────────────────────
    let current_x = if *mode == 0 { x_input } else { *phase };
    let current_y = evaluate_curve(points, current_x);

    if played {
        // Vertical playback head line
        let head_x = rect.left() + current_x.clamp(0.0, 1.0) * size;
        painter.line_segment(
            [egui::pos2(head_x, rect.top()), egui::pos2(head_x, rect.bottom())],
            egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(255, 200, 80, 120)),
        );
    }

    // Position dot (orange)
    {
        let ix = rect.left() + current_x.clamp(0.0, 1.0) * size;
        let iy = rect.bottom() - current_y.clamp(0.0, 1.0) * size;
        painter.circle_filled(egui::pos2(ix, iy), 4.0, egui::Color32::from_rgb(255, 200, 80));
    }

    // Control points — drag to move, click to open options popup.
    let drag_id = egui::Id::new(("curve_drag", node_id));
    let menu_id = egui::Id::new(("curve_menu", node_id));
    let active_drag: Option<usize> = ui.ctx().data_mut(|d| d.get_temp(drag_id));
    let menu_open_for: Option<usize> = ui.ctx().data_mut(|d| d.get_temp(menu_id));
    let mut dragged_idx: Option<usize> = None;
    let mut clicked_point_idx: Option<usize> = None;
    let mut hit_any_point = false;

    for (i, pt) in points.iter().enumerate() {
        let sx = rect.left() + pt[0] * size;
        let sy = rect.bottom() - pt[1].clamp(0.0, 1.0) * size;
        let screen_pt = egui::pos2(sx, sy);
        let hit = response.hover_pos().map(|p| p.distance(screen_pt) < 14.0).unwrap_or(false);
        if hit { hit_any_point = true; }
        let is_active = active_drag == Some(i);
        let is_sustain = *sustain_index == Some(i as u8);

        if is_active {
            painter.circle_filled(screen_pt, 12.0, egui::Color32::from_rgba_premultiplied(100, 200, 160, 30));
            painter.circle_filled(screen_pt, 8.0, egui::Color32::WHITE);
            painter.circle_stroke(screen_pt, 8.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 220, 160)));
        } else if hit {
            painter.circle_filled(screen_pt, 7.5, egui::Color32::from_rgb(220, 240, 230));
            painter.circle_stroke(screen_pt, 7.5, egui::Stroke::new(1.5, egui::Color32::from_rgb(100, 200, 160)));
        } else {
            painter.circle_filled(screen_pt, 6.5, egui::Color32::from_rgb(160, 200, 180));
            painter.circle_stroke(screen_pt, 6.5, egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 100, 90)));
        }

        // Gold ring for sustain marker
        if is_sustain {
            painter.circle_stroke(screen_pt, 10.5,
                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 200, 60)));
        }

        if hit && response.drag_started() {
            dragged_idx = Some(i);
        }
        if hit && response.clicked() {
            clicked_point_idx = Some(i);
        }
    }

    if let Some(idx) = dragged_idx {
        ui.ctx().data_mut(|d| d.insert_temp(drag_id, idx));
    }

    if let Some(idx) = active_drag {
        if response.dragged() {
            if let Some(pos) = response.hover_pos() {
                let nx = ((pos.x - rect.left()) / size).clamp(0.0, 1.0);
                let ny = ((rect.bottom() - pos.y) / size).clamp(0.0, 1.0);
                if idx < points.len() {
                    if idx == 0 { points[idx] = [0.0, ny]; }
                    else if idx == points.len() - 1 { points[idx] = [1.0, ny]; }
                    else {
                        // Clamp x between neighbours so points stay sorted
                        let lo = points[idx - 1][0] + 0.001;
                        let hi = points[idx + 1][0] - 0.001;
                        points[idx] = [nx.clamp(lo, hi), ny];
                    }
                }
            }
        }
        if !response.dragged() {
            ui.ctx().data_mut(|d| d.remove::<usize>(drag_id));
        }
    }

    // ── Click on a point: open options popup. Click on empty area: insert. ──
    if let Some(i) = clicked_point_idx {
        // Toggle: clicking the same point closes the menu
        if menu_open_for == Some(i) {
            ui.ctx().data_mut(|d| d.remove::<usize>(menu_id));
        } else {
            ui.ctx().data_mut(|d| d.insert_temp(menu_id, i));
        }
    } else if response.clicked() && !hit_any_point {
        // Click on empty curve area
        if menu_open_for.is_some() {
            // First click closes any open menu
            ui.ctx().data_mut(|d| d.remove::<usize>(menu_id));
        } else if points.len() < 16 {
            if let Some(pos) = response.interact_pointer_pos() {
                let nx = ((pos.x - rect.left()) / size).clamp(0.0, 1.0);
                let ny = ((rect.bottom() - pos.y) / size).clamp(0.0, 1.0);
                // Find sorted insertion index, never at 0 or end (those are anchor points)
                let mut insert_at = 1usize;
                for i in 1..points.len() {
                    if points[i][0] > nx { insert_at = i; break; }
                    insert_at = i + 1;
                }
                if insert_at == 0 { insert_at = 1; }
                if insert_at >= points.len() { insert_at = points.len() - 1; }
                points.insert(insert_at, [nx, ny]);
                // Maintain sustain index after insertion
                if let Some(s) = *sustain_index {
                    if (s as usize) >= insert_at {
                        *sustain_index = Some(s + 1);
                    }
                }
            }
        }
    }

    // ── Popup menu for the active point ──
    let menu_open_for: Option<usize> = ui.ctx().data_mut(|d| d.get_temp(menu_id));
    if let Some(idx) = menu_open_for {
        if idx < points.len() {
            let pt_screen = egui::pos2(
                rect.left() + points[idx][0] * size,
                rect.bottom() - points[idx][1].clamp(0.0, 1.0) * size,
            );
            let area = egui::Area::new(egui::Id::new(("curve_menu_area", node_id)))
                .order(egui::Order::Foreground)
                .fixed_pos(pt_screen + egui::vec2(12.0, -8.0));
            area.show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(150.0);
                    ui.label(egui::RichText::new(format!("Point {}: ({:.2}, {:.2})", idx, points[idx][0], points[idx][1]))
                        .small().color(egui::Color32::GRAY));
                    ui.separator();
                    let is_sustain = *sustain_index == Some(idx as u8);
                    if ui.add_enabled(played, egui::Button::new(
                        if is_sustain { "✓ Sustain marker" } else { "Mark as sustain" })).clicked()
                    {
                        *sustain_index = if is_sustain { None } else { Some(idx as u8) };
                        ui.ctx().data_mut(|d| d.remove::<usize>(menu_id));
                    }
                    let can_delete = points.len() > 2 && idx != 0 && idx != points.len() - 1;
                    if ui.add_enabled(can_delete, egui::Button::new("Delete point")).clicked() {
                        points.remove(idx);
                        if let Some(s) = *sustain_index {
                            let s = s as usize;
                            if s == idx { *sustain_index = None; }
                            else if s > idx { *sustain_index = Some((s - 1) as u8); }
                        }
                        ui.ctx().data_mut(|d| d.remove::<usize>(menu_id));
                    }
                    ui.separator();
                    if ui.button("Cancel").clicked() {
                        ui.ctx().data_mut(|d| d.remove::<usize>(menu_id));
                    }
                });
            });
        } else {
            ui.ctx().data_mut(|d| d.remove::<usize>(menu_id));
        }
    }

    // ── Transport controls (Played mode) ─────────────────────────────
    if played {
        ui.separator();
        ui.horizontal(|ui| {
            if ui.small_button(if *playing { "⏸" } else { "▶" }).clicked() {
                if *playing {
                    *playing = false;
                } else {
                    *playing = true;
                    if *phase >= 1.0 { *phase = 0.0; }
                    *last_phase = -1.0;
                    *last_step_index = -1;
                }
            }
            if ui.small_button("⏹").clicked() {
                *playing = false;
                *phase = 0.0;
                *last_phase = -1.0;
                *last_step_index = -1;
            }
            if ui.small_button("↺").clicked() {
                *phase = 0.0;
                *playing = true;
                *last_phase = -1.0;
                *last_step_index = -1;
            }
            // Loop toggle (replaces the standalone Loop checkbox above)
            if ui.add(egui::SelectableLabel::new(*looping, "🔁")).clicked() {
                *looping = !*looping;
            }
            // Phase display
            ui.label(egui::RichText::new(format!("{:.0}%", current_x * 100.0)).small().color(
                if *playing { egui::Color32::from_rgb(80, 200, 120) } else { egui::Color32::GRAY }
            ));
        });
    }

    ui.separator();

    ui.separator();

    // ── Output ports (right-aligned, stable) ──────────────────────────
    crate::nodes::output_port_row(ui, "Y", &format!("{:.2}", current_y), node_id, 0, port_positions, dragging_from, connections, pending_disconnects, PortKind::Normalized);
    crate::nodes::output_port_row(ui, "Phase", &format!("{:.2}", current_x), node_id, 1, port_positions, dragging_from, connections, pending_disconnects, PortKind::Normalized);
    crate::nodes::output_port_row(ui, "End", &format!("{}", if !*playing && *phase >= 1.0 && played { 1 } else { 0 }), node_id, 2, port_positions, dragging_from, connections, pending_disconnects, PortKind::Trigger);
    // Image output (port 3) handled by standard port system in app.rs
    if played {
        let step_val = values.get(&(node_id, 4)).map(|v| v.as_float()).unwrap_or(0.0);
        let step_trig = values.get(&(node_id, 5)).map(|v| v.as_float()).unwrap_or(0.0);
        crate::nodes::output_port_row(ui, "Step Val", &format!("{:.2}", step_val), node_id, 4, port_positions, dragging_from, connections, pending_disconnects, PortKind::Normalized);
        crate::nodes::output_port_row(ui, "Step Trig", if step_trig > 0.5 { "▲" } else { "·" }, node_id, 5, port_positions, dragging_from, connections, pending_disconnects, PortKind::Trigger);
    }

    // Request repaint when animating
    if *playing {
        ui.ctx().request_repaint();
    }
}

/// Evaluate the curve at position x (0-1). Uses cubic Hermite interpolation.
pub fn evaluate_curve(points: &[[f32; 2]], x: f32) -> f32 {
    if points.is_empty() { return 0.0; }
    if points.len() == 1 { return points[0][1]; }

    let x = x.clamp(0.0, 1.0);

    for i in 0..points.len() - 1 {
        let (x0, y0) = (points[i][0], points[i][1]);
        let (x1, y1) = (points[i + 1][0], points[i + 1][1]);
        if x >= x0 && x <= x1 {
            if (x1 - x0).abs() < 1e-6 { return y0; }
            let t = (x - x0) / (x1 - x0);
            // Smooth interpolation (cubic hermite)
            let t2 = t * t;
            let t3 = t2 * t;
            let h = 2.0 * t3 - 3.0 * t2 + 1.0;
            return h * y0 + (1.0 - h) * y1;
        }
    }

    if x <= points[0][0] { points[0][1] }
    else { points[points.len() - 1][1] }
}
