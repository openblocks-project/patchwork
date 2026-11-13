use crate::graph::{NodeId, PortKind, PortValue};
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    interval: &mut f32,
    elapsed: &mut f32,
    running: &mut bool,
    pulse_width: &mut f32,
    ref_time: &mut f64,
    paused_elapsed: &mut f64,
    time_initialized: &mut bool,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[crate::graph::Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    // Read wired interval input (port 0)
    let interval_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 0);
    if interval_wired {
        let pv = crate::graph::Graph::static_input_value(connections, values, node_id, 0);
        if let PortValue::Float(v) = pv {
            if v > 0.0 {
                *interval = v;
            }
        }
    }

    // Read wired BPM input (port 1)
    let bpm_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 1);
    if bpm_wired {
        let pv = crate::graph::Graph::static_input_value(connections, values, node_id, 1);
        if let PortValue::Float(v) = pv {
            if v > 0.0 {
                *interval = 60.0 / v;
            }
        }
    }

    // ── Input ports ───────────────────────────────────────────────────
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(
            ui,
            node_id,
            0,
            true,
            connections,
            port_positions,
            dragging_from,
            pending_disconnects,
            PortKind::Number,
        );
        ui.label(egui::RichText::new("Interval:").small());
        if interval_wired {
            ui.label(
                egui::RichText::new(format!("{:.2}s", interval))
                    .small()
                    .color(egui::Color32::from_rgb(80, 170, 255)),
            );
        }
    });

    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(
            ui,
            node_id,
            1,
            true,
            connections,
            port_positions,
            dragging_from,
            pending_disconnects,
            PortKind::Number,
        );
        ui.label(egui::RichText::new("BPM:").small());
        if bpm_wired {
            let bpm_in = 60.0 / interval.max(0.01);
            ui.label(
                egui::RichText::new(format!("{:.1}", bpm_in))
                    .small()
                    .color(egui::Color32::from_rgb(80, 170, 255)),
            );
        }
    });

    ui.separator();

    // Controls — pause/resume with wall-clock bookkeeping
    let was_running = *running;
    ui.horizontal(|ui| {
        if ui.button(if *running { "⏸" } else { "▶" }).clicked() {
            *running = !*running;
            if *running && !was_running {
                // Resuming: set ref_time to now, keep paused_elapsed
                *paused_elapsed = *elapsed as f64;
                *ref_time = 0.0; // will be re-initialized in evaluate
                *time_initialized = false;
            } else if !*running && was_running {
                // Pausing: snapshot elapsed into paused_elapsed
                *paused_elapsed = *elapsed as f64;
            }
        }
        if ui.button("Reset").clicked() {
            *elapsed = 0.0;
            *paused_elapsed = 0.0;
            *ref_time = 0.0;
            *time_initialized = false;
        }
    });

    // Interval slider (only when not wired)
    if !interval_wired && !bpm_wired {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Interval").small());
            ui.add(
                egui::Slider::new(interval, 0.001..=30.0)
                    .step_by(0.001)
                    .suffix("s")
                    .logarithmic(true)
                    .custom_formatter(|v, _| format!("{:.3}", v))
                    .clamping(egui::SliderClamping::Never),
            );
        });
    }

    // Pulse width
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Pulse").small());
        ui.add(
            egui::Slider::new(pulse_width, 0.001..=1.0)
                .step_by(0.001)
                .suffix("s")
                .logarithmic(true)
                .custom_formatter(|v, _| format!("{:.3}", v))
                .clamping(egui::SliderClamping::Never),
        );
    });

    // BPM display
    let bpm = 60.0 / interval.max(0.01);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{:.1} BPM", bpm)).strong());
        ui.label(
            egui::RichText::new(format!("({:.2}s)", interval))
                .small()
                .color(egui::Color32::GRAY),
        );
    });

    // Phase
    let phase = (*elapsed % interval.max(0.01)) / interval.max(0.01);

    // Spinner visual
    let spinner_size = 60.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(spinner_size, spinner_size), egui::Sense::hover());
    let center = rect.center();
    let radius = spinner_size * 0.4;
    let painter = ui.painter();

    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(2.0, egui::Color32::from_rgb(60, 60, 70)),
    );

    let segments = 40;
    let filled = (phase * segments as f32) as usize;
    for i in 0..segments {
        let a1 = std::f32::consts::TAU * (i as f32 / segments as f32) - std::f32::consts::FRAC_PI_2;
        let a2 = std::f32::consts::TAU * ((i + 1) as f32 / segments as f32)
            - std::f32::consts::FRAC_PI_2;
        let r_inner = radius - 6.0;
        let r_outer = radius;
        let p1 = center + egui::vec2(a1.cos() * r_inner, a1.sin() * r_inner);
        let p2 = center + egui::vec2(a1.cos() * r_outer, a1.sin() * r_outer);
        let p3 = center + egui::vec2(a2.cos() * r_outer, a2.sin() * r_outer);
        let p4 = center + egui::vec2(a2.cos() * r_inner, a2.sin() * r_inner);
        let col = if i < filled {
            if *running {
                egui::Color32::from_rgb(80, 200, 120)
            } else {
                egui::Color32::from_rgb(120, 120, 60)
            }
        } else {
            egui::Color32::from_rgb(40, 40, 50)
        };
        painter.add(egui::Shape::convex_polygon(
            vec![p1, p2, p3, p4],
            col,
            egui::Stroke::NONE,
        ));
    }

    let is_pulse = phase < (*pulse_width / interval.max(0.01));
    let dot_col = if is_pulse && *running {
        egui::Color32::from_rgb(255, 220, 60)
    } else {
        egui::Color32::from_rgb(80, 80, 90)
    };
    painter.circle_filled(center, 6.0, dot_col);

    let angle = std::f32::consts::TAU * phase - std::f32::consts::FRAC_PI_2;
    let tip = center + egui::vec2(angle.cos() * (radius - 2.0), angle.sin() * (radius - 2.0));
    painter.line_segment([center, tip], egui::Stroke::new(2.0, egui::Color32::WHITE));

    // Status — always visible
    ui.horizontal(|ui| {
        if *running {
            ui.colored_label(egui::Color32::from_rgb(80, 200, 120), "⏱ Running");
        } else {
            ui.colored_label(egui::Color32::GRAY, "⏸ Paused");
        }
    });

    ui.separator();

    // ── Output ports ──────────────────────────────────────────────────
    let trig_label = if is_pulse && *running {
        "PULSE"
    } else {
        "Trigger"
    };
    crate::nodes::output_port_row(
        ui,
        trig_label,
        &format!("{}", if is_pulse && *running { 1 } else { 0 }),
        node_id,
        0,
        port_positions,
        dragging_from,
        connections,
        pending_disconnects,
        PortKind::Trigger,
    );
    crate::nodes::output_port_row(
        ui,
        "Phase",
        &format!("{:.2}", phase),
        node_id,
        1,
        port_positions,
        dragging_from,
        connections,
        pending_disconnects,
        PortKind::Normalized,
    );
    crate::nodes::output_port_row(
        ui,
        "BPM",
        &format!("{:.0}", bpm),
        node_id,
        2,
        port_positions,
        dragging_from,
        connections,
        pending_disconnects,
        PortKind::Number,
    );

    if *running {
        ui.ctx().request_repaint();
    }
}
