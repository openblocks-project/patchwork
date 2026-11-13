use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

/// Frequency labels for the EQ curve (log-spaced)
const FREQ_LABELS: &[(f32, &str)] = &[
    (0.07, "50"),
    (0.18, "100"),
    (0.29, "250"),
    (0.42, "500"),
    (0.53, "1k"),
    (0.64, "2k"),
    (0.75, "5k"),
    (0.87, "10k"),
];

/// EQ presets: (name, points)
fn eq_preset(name: &str) -> Vec<[f32; 2]> {
    match name {
        "Flat" => vec![[0.0, 0.5], [0.25, 0.5], [0.5, 0.5], [0.75, 0.5], [1.0, 0.5]],
        "Bass+" => vec![
            [0.0, 0.75],
            [0.15, 0.7],
            [0.3, 0.55],
            [0.5, 0.5],
            [0.75, 0.5],
            [1.0, 0.5],
        ],
        "Treble+" => vec![
            [0.0, 0.5],
            [0.25, 0.5],
            [0.5, 0.5],
            [0.7, 0.55],
            [0.85, 0.7],
            [1.0, 0.75],
        ],
        "Mid Scoop" => vec![
            [0.0, 0.6],
            [0.2, 0.55],
            [0.4, 0.35],
            [0.6, 0.35],
            [0.8, 0.55],
            [1.0, 0.6],
        ],
        "Presence" => vec![
            [0.0, 0.5],
            [0.3, 0.5],
            [0.5, 0.6],
            [0.65, 0.65],
            [0.8, 0.55],
            [1.0, 0.5],
        ],
        "Warmth" => vec![
            [0.0, 0.65],
            [0.2, 0.6],
            [0.4, 0.52],
            [0.6, 0.5],
            [0.8, 0.45],
            [1.0, 0.4],
        ],
        _ => vec![[0.0, 0.5], [1.0, 0.5]],
    }
}

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    points: &mut Vec<[f32; 2]>,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    connections: &[Connection],
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    // Audio input port
    crate::nodes::audio_port_row(
        ui,
        "Audio",
        node_id,
        0,
        true,
        port_positions,
        dragging_from,
        connections,
        pending_disconnects,
        PortKind::Audio,
    );

    // ── Presets ──────────────────────────────────────────────────
    ui.horizontal(|ui| {
        for name in &[
            "Flat",
            "Bass+",
            "Treble+",
            "Mid Scoop",
            "Presence",
            "Warmth",
        ] {
            if ui.small_button(*name).clicked() {
                *points = eq_preset(name);
            }
        }
    });

    // ── Curve editor ─────────────────────────────────────────────
    let curve_w = 240.0_f32.min(ui.available_width().max(120.0));
    let curve_h = 140.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(curve_w, curve_h), egui::Sense::click_and_drag());
    let painter = ui.painter();

    // Background
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(18, 18, 22));

    // Grid lines (frequency bands)
    for &(x_norm, _label) in FREQ_LABELS {
        let x = rect.left() + x_norm * rect.width();
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(0.5, egui::Color32::from_rgb(35, 35, 42)),
        );
    }

    // Horizontal dB lines: -24, -12, 0, +12, +24
    for db_norm in &[0.0_f32, 0.25, 0.5, 0.75, 1.0] {
        let y = rect.bottom() - db_norm * rect.height();
        let color = if (*db_norm - 0.5).abs() < 0.01 {
            egui::Color32::from_rgb(60, 60, 70) // 0dB line brighter
        } else {
            egui::Color32::from_rgb(30, 30, 38)
        };
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(0.5, color),
        );
    }

    // ── Draw the curve ──────────────────────────────────────────
    if points.len() >= 2 {
        // Sort points by x
        points.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));

        // Draw filled curve area (subtle fill below/above 0dB line)
        let num_segments = 80;
        let mut curve_points_screen: Vec<egui::Pos2> = Vec::with_capacity(num_segments + 2);
        for i in 0..=num_segments {
            let t = i as f32 / num_segments as f32;
            let y = evaluate_curve(points, t);
            let sx = rect.left() + t * rect.width();
            let sy = rect.bottom() - y * rect.height();
            curve_points_screen.push(egui::pos2(sx, sy));
        }

        // Draw curve line
        for i in 1..curve_points_screen.len() {
            painter.line_segment(
                [curve_points_screen[i - 1], curve_points_screen[i]],
                egui::Stroke::new(2.0, egui::Color32::from_rgb(180, 140, 255)),
            );
        }

        // Subtle fill between curve and 0dB center
        let center_y = rect.bottom() - 0.5 * rect.height();
        for i in 1..curve_points_screen.len() {
            let p0 = curve_points_screen[i - 1];
            let p1 = curve_points_screen[i];
            let boost = p0.y < center_y || p1.y < center_y;
            let fill_color = if boost {
                egui::Color32::from_rgba_premultiplied(140, 100, 255, 15)
            } else {
                egui::Color32::from_rgba_premultiplied(255, 80, 80, 12)
            };
            // Small quad from curve to center line
            painter.rect_filled(
                egui::Rect::from_two_pos(
                    egui::pos2(p0.x, p0.y.min(center_y)),
                    egui::pos2(p1.x, p0.y.max(center_y)),
                ),
                0.0,
                fill_color,
            );
        }
    }

    // ── Draw control points ─────────────────────────────────────
    let drag_id = egui::Id::new(("eq_drag", node_id));
    let dragging_idx: Option<usize> = ui.ctx().data_mut(|d| d.get_temp(drag_id));

    for (i, point) in points.iter().enumerate() {
        let px = rect.left() + point[0] * rect.width();
        let py = rect.bottom() - point[1] * rect.height();
        let pt = egui::pos2(px, py);
        let is_dragging = dragging_idx == Some(i);
        let is_hovered = response
            .hover_pos()
            .map(|p| p.distance(pt) < 14.0)
            .unwrap_or(false);

        if is_dragging {
            painter.circle_filled(
                pt,
                12.0,
                egui::Color32::from_rgba_premultiplied(160, 120, 255, 30),
            );
            painter.circle_filled(pt, 8.0, egui::Color32::WHITE);
            painter.circle_stroke(
                pt,
                8.0,
                egui::Stroke::new(2.0, egui::Color32::from_rgb(180, 140, 255)),
            );
        } else if is_hovered {
            painter.circle_filled(pt, 7.5, egui::Color32::from_rgb(230, 200, 255));
            painter.circle_stroke(
                pt,
                7.5,
                egui::Stroke::new(1.5, egui::Color32::from_rgb(180, 140, 255)),
            );
        } else {
            painter.circle_filled(pt, 6.5, egui::Color32::from_rgb(200, 160, 255));
            painter.circle_stroke(
                pt,
                6.5,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(100, 80, 160)),
            );
        }
    }

    // ── Point interaction ───────────────────────────────────────
    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            // Find closest point to click
            let mut closest = None;
            let mut min_dist = f32::MAX;
            for (i, point) in points.iter().enumerate() {
                let px = rect.left() + point[0] * rect.width();
                let py = rect.bottom() - point[1] * rect.height();
                let dist = ((pos.x - px).powi(2) + (pos.y - py).powi(2)).sqrt();
                if dist < min_dist {
                    min_dist = dist;
                    closest = Some(i);
                }
            }
            if min_dist < 20.0 {
                if let Some(idx) = closest {
                    ui.ctx().data_mut(|d| d.insert_temp(drag_id, idx));
                }
            } else {
                // Add new point at click position
                let nx = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                let ny = ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0);
                points.push([nx, ny]);
                let new_idx = points.len() - 1;
                ui.ctx().data_mut(|d| d.insert_temp(drag_id, new_idx));
            }
        }
    }

    if response.dragged() {
        if let Some(idx) = dragging_idx {
            if let Some(pos) = response.interact_pointer_pos() {
                if idx < points.len() {
                    points[idx][0] = ((pos.x - rect.left()) / rect.width()).clamp(0.01, 0.99);
                    points[idx][1] = ((rect.bottom() - pos.y) / rect.height()).clamp(0.0, 1.0);
                }
            }
        }
    }

    if response.drag_stopped() {
        ui.ctx().data_mut(|d| d.remove_temp::<usize>(drag_id));
    }

    // Double-click to remove a point (keep at least 2)
    if response.double_clicked() && points.len() > 2 {
        if let Some(pos) = response.interact_pointer_pos() {
            let mut closest_idx = None;
            let mut min_dist = f32::MAX;
            for (i, point) in points.iter().enumerate() {
                let px = rect.left() + point[0] * rect.width();
                let py = rect.bottom() - point[1] * rect.height();
                let dist = ((pos.x - px).powi(2) + (pos.y - py).powi(2)).sqrt();
                if dist < min_dist && dist < 15.0 {
                    min_dist = dist;
                    closest_idx = Some(i);
                }
            }
            if let Some(idx) = closest_idx {
                points.remove(idx);
            }
        }
    }

    // ── Frequency labels ────────────────────────────────────────
    for &(x_norm, label) in FREQ_LABELS {
        let x = rect.left() + x_norm * rect.width();
        painter.text(
            egui::pos2(x, rect.bottom() + 2.0),
            egui::Align2::CENTER_TOP,
            label,
            egui::FontId::proportional(8.0),
            egui::Color32::from_rgb(90, 90, 100),
        );
    }

    // ── dB labels (left side) ───────────────────────────────────
    for &(db, label) in &[
        (-24.0, "-24"),
        (-12.0, "-12"),
        (0.0, "0"),
        (12.0, "+12"),
        (24.0, "+24"),
    ] {
        let y_norm = (db + 24.0) / 48.0; // -24→0, 0→0.5, +24→1
        let y = rect.bottom() - y_norm * rect.height();
        painter.text(
            egui::pos2(rect.left() - 2.0, y),
            egui::Align2::RIGHT_CENTER,
            label,
            egui::FontId::proportional(8.0),
            egui::Color32::from_rgb(80, 80, 90),
        );
    }

    ui.add_space(10.0); // Space for frequency labels

    // Audio output port
    crate::nodes::audio_port_row(
        ui,
        "Audio",
        node_id,
        0,
        false,
        port_positions,
        dragging_from,
        connections,
        pending_disconnects,
        PortKind::Audio,
    );
}

/// Cubic Hermite interpolation of sorted curve points
fn evaluate_curve(points: &[[f32; 2]], x: f32) -> f32 {
    if points.is_empty() {
        return 0.5;
    }
    if points.len() == 1 {
        return points[0][1];
    }
    if x <= points[0][0] {
        return points[0][1];
    }
    if let Some(last) = points.last() {
        if x >= last[0] {
            return last[1];
        }
    }

    for i in 0..points.len() - 1 {
        let x0 = points[i][0];
        let x1 = points[i + 1][0];
        if x >= x0 && x <= x1 {
            let t = if (x1 - x0).abs() < 1e-6 {
                0.0
            } else {
                (x - x0) / (x1 - x0)
            };
            let y0 = points[i][1];
            let y1 = points[i + 1][1];
            let t2 = t * t;
            let t3 = t2 * t;
            let h = 2.0 * t3 - 3.0 * t2 + 1.0;
            return y0 * h + y1 * (1.0 - h);
        }
    }
    0.5
}
