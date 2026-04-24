#![allow(dead_code)]
use eframe::egui;
use crate::graph::{NodeId, PortValue, Connection, Graph, PortKind};
use std::collections::HashMap;

const STAIRCASE_W: f32 = 150.0;
const STAIRCASE_H: f32 = 70.0;
const HISTORY_MAX: usize = 40;

pub fn render(
    ui: &mut egui::Ui,
    held_float: &mut f32,
    held_text: &mut String,
    is_text: &mut bool,
    last_trigger: &mut f32,
    history: &mut Vec<f32>,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let val_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    let trig_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);

    // ── Input ports ───────────────────────────────────────────────────
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Generic);
        ui.label(egui::RichText::new("Value:").small());
        if val_wired {
            let v = Graph::static_input_value(connections, values, node_id, 0);
            let s = match &v {
                PortValue::Float(f) => format!("{:.3}", f),
                PortValue::Text(t) => {
                    if t.chars().count() > 16 {
                        let head: String = t.chars().take(16).collect();
                        format!("\"{}...\"", head)
                    } else { format!("\"{}\"", t) }
                }
                _ => "—".into(),
            };
            ui.label(egui::RichText::new(s).small().color(egui::Color32::from_rgb(80, 170, 255)));
        } else {
            ui.label(egui::RichText::new("—").small().color(egui::Color32::GRAY));
        }
    });

    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Trigger:").small());
        if trig_wired {
            let t = Graph::static_input_value(connections, values, node_id, 1).as_float();
            let col = if t > 0.5 { egui::Color32::from_rgb(255, 200, 60) } else { egui::Color32::from_rgb(80, 170, 255) };
            ui.label(egui::RichText::new(format!("{:.1}", t)).small().color(col));
        } else {
            ui.label(egui::RichText::new("—").small().color(egui::Color32::GRAY));
        }
    });

    ui.separator();

    // Manual sample button + Trigger output on the right
    let trigger_val = Graph::static_input_value(connections, values, node_id, 1).as_float();
    let rising_edge = trigger_val > 0.5 && *last_trigger <= 0.5;
    let mut do_sample = rising_edge;

    ui.horizontal(|ui| {
        if ui.button("📸 Sample Now").clicked() {
            do_sample = true;
        }
        // Trigger output — flush right via layout
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, false, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
            ui.label(egui::RichText::new(format!("{}", if rising_edge { 1 } else { 0 })).small().monospace());
        });
    });
    *last_trigger = trigger_val;

    // Perform sampling
    if do_sample && val_wired {
        let v = Graph::static_input_value(connections, values, node_id, 0);
        match &v {
            PortValue::Float(f) => {
                *held_float = *f;
                *is_text = false;
                history.push(*f);
                while history.len() > HISTORY_MAX { history.remove(0); }
            }
            PortValue::Text(t) => {
                *held_text = t.clone();
                *is_text = true;
                let hash_val = (t.len() as f32) + t.bytes().take(4).map(|b| b as f32).sum::<f32>() * 0.001;
                history.push(hash_val);
                while history.len() > HISTORY_MAX { history.remove(0); }
            }
            _ => {}
        }
    }

    // Flash indicator for rising edge
    let flash_col = egui::Color32::from_rgba_unmultiplied(255, 220, 60, if rising_edge { 255 } else { 0 });

    // Held value display
    ui.horizontal(|ui| {
        // Sample indicator dot
        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        if rising_edge {
            ui.painter().circle_filled(dot_rect.center(), 5.0, flash_col);
        } else {
            ui.painter().circle_filled(dot_rect.center(), 3.0, egui::Color32::from_rgb(60, 60, 70));
        }

        ui.label(egui::RichText::new("Held:").small().strong());
        if *is_text {
            let display = if held_text.chars().count() > 20 {
                let head: String = held_text.chars().take(20).collect();
                format!("\"{}...\"", head)
            } else {
                format!("\"{}\"", held_text)
            };
            ui.label(egui::RichText::new(display).small().color(egui::Color32::from_rgb(80, 220, 80)));
        } else {
            ui.label(egui::RichText::new(format!("{:.4}", held_float)).strong().color(egui::Color32::from_rgb(255, 220, 80)));
        }
    });

    ui.separator();

    // Staircase visualization with clear button
    if !history.is_empty() {
        // Chart header with clear button
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{} samples", history.len())).small().color(egui::Color32::from_rgb(100, 100, 120)));
            let remaining = ui.available_width() - 40.0;
            if remaining > 0.0 { ui.add_space(remaining); }
            if ui.small_button("✕ Clear").clicked() {
                history.clear();
            }
        });

        let (rect, _) = ui.allocate_exact_size(egui::vec2(STAIRCASE_W, STAIRCASE_H), egui::Sense::hover());
        let painter = ui.painter();

        // Background
        painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 22, 28));
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 60)), egui::StrokeKind::Outside);

        // Auto-fit range
        let min_v = history.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_v = history.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let range = (max_v - min_v).max(0.01);
        let margin = range * 0.1;
        let lo = min_v - margin;
        let hi = max_v + margin;

        let pad = 4.0;
        let inner_w = rect.width() - pad * 2.0;
        let inner_h = rect.height() - pad * 2.0;
        let n = history.len();

        // Draw staircase
        let step_w = inner_w / n.max(1) as f32;
        let mut points: Vec<egui::Pos2> = Vec::new();

        for (i, val) in history.iter().enumerate() {
            let x = rect.left() + pad + i as f32 * step_w;
            let y = rect.bottom() - pad - ((val - lo) / (hi - lo)) * inner_h;
            let y = y.clamp(rect.top() + pad, rect.bottom() - pad);
            points.push(egui::pos2(x, y));
            points.push(egui::pos2(x + step_w, y));
        }

        if points.len() >= 2 {
            // Fill under the staircase
            let mut fill_points = points.clone();
            fill_points.push(egui::pos2(rect.right() - pad, rect.bottom() - pad));
            fill_points.push(egui::pos2(rect.left() + pad, rect.bottom() - pad));
            painter.add(egui::Shape::convex_polygon(
                fill_points,
                egui::Color32::from_rgba_unmultiplied(80, 200, 120, 25),
                egui::Stroke::NONE,
            ));

            // Staircase line
            for w in points.windows(2) {
                painter.line_segment(
                    [w[0], w[1]],
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 200, 120)),
                );
            }

            // Vertical step lines
            for i in 0..n.saturating_sub(1) {
                let x = rect.left() + pad + (i + 1) as f32 * step_w;
                let y1 = rect.bottom() - pad - ((history[i] - lo) / (hi - lo)) * inner_h;
                let y2 = rect.bottom() - pad - ((history[i + 1] - lo) / (hi - lo)) * inner_h;
                painter.line_segment(
                    [egui::pos2(x, y1.clamp(rect.top() + pad, rect.bottom() - pad)),
                     egui::pos2(x, y2.clamp(rect.top() + pad, rect.bottom() - pad))],
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(80, 200, 120, 80)),
                );
            }
        }

        // Current held value line
        let current_y = rect.bottom() - pad - ((*held_float - lo) / (hi - lo)) * inner_h;
        let cy = current_y.clamp(rect.top() + pad, rect.bottom() - pad);
        painter.line_segment(
            [egui::pos2(rect.left() + pad, cy), egui::pos2(rect.right() - pad, cy)],
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 220, 60, 120)),
        );
    } else {
        ui.colored_label(egui::Color32::GRAY, "No samples yet");
    }

    ui.separator();

    // ── Output port: Out ────────────────────────────────────────────
    let out_val = if *is_text {
        let short = if held_text.chars().count() > 10 {
            held_text.chars().take(10).collect::<String>()
        } else {
            held_text.clone()
        };
        format!("\"{}\"", short)
    } else {
        format!("{:.3}", held_float)
    };
    crate::nodes::output_port_row(ui, "Out", &out_val, node_id, 0, port_positions, dragging_from, connections, pending_disconnects, PortKind::Generic);
}
