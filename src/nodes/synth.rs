use crate::audio::{AudioManager, Waveform};
use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    audio: &AudioManager,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let (waveform, frequency, amplitude, active, fm_depth) = match node_type {
        NodeType::Synth {
            waveform,
            frequency,
            amplitude,
            active,
            fm_depth,
        } => (waveform, frequency, amplitude, active, fm_depth),
        _ => return,
    };

    // ── Waveform selector ────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Wave:").small());
        egui::ComboBox::from_id_salt(egui::Id::new(("synth_wave", node_id)))
            .selected_text(waveform.name())
            .width(80.0)
            .show_ui(ui, |ui| {
                for w in Waveform::all() {
                    if ui.selectable_label(*waveform == *w, w.name()).clicked() {
                        *waveform = *w;
                    }
                }
            });
    });

    // Read control inputs
    let freq_in = Graph::static_input_value(connections, values, node_id, 0);
    let amp_in = Graph::static_input_value(connections, values, node_id, 1);
    let gate_in = Graph::static_input_value(connections, values, node_id, 2);

    let freq_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 0);
    let amp_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 1);
    let gate_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 2);

    // Apply control inputs (numbers set frequency directly)
    if freq_wired {
        let v = freq_in.as_float();
        if v > 0.0 {
            *frequency = v;
        }
    }
    if let PortValue::Float(a) = amp_in {
        *amplitude = a.clamp(0.0, 1.0);
    }
    if let PortValue::Float(g) = gate_in {
        *active = g > 0.5;
    }

    let display_freq = *frequency;
    let display_amp = *amplitude;
    let display_active = *active;

    // ── Waveform visualization ───────────────────────────────────────
    let viz_w = ui.available_width().min(200.0);
    let viz_h = 40.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(viz_w, viz_h), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, egui::Color32::from_rgb(15, 15, 25));
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(0.5, egui::Color32::from_rgb(40, 40, 55)),
        egui::StrokeKind::Outside,
    );
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ],
        egui::Stroke::new(0.5, egui::Color32::from_rgb(40, 40, 55)),
    );

    let cycles = (display_freq / 100.0).clamp(1.0, 16.0).round().max(1.0);
    let n = 120;
    let points: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let phase = (t * cycles) % 1.0;
            let y = waveform.sample(phase) * display_amp;
            egui::pos2(
                rect.left() + t * rect.width(),
                rect.center().y - y * viz_h * 0.4,
            )
        })
        .collect();

    let wave_col = if display_active {
        egui::Color32::from_rgb(80, 220, 140)
    } else {
        egui::Color32::from_rgb(60, 60, 70)
    };
    for w in points.windows(2) {
        painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, wave_col));
    }
    painter.text(
        egui::pos2(rect.right() - 3.0, rect.top() + 2.0),
        egui::Align2::RIGHT_TOP,
        format!("{:.0} Hz", display_freq),
        egui::FontId::new(9.0, egui::FontFamily::Proportional),
        egui::Color32::from_rgba_unmultiplied(180, 180, 200, 160),
    );

    ui.separator();

    // ── Port 0: Freq (number = direct frequency, audio = FM carrier) ─
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
        ui.label(egui::RichText::new("Freq").small());
        if freq_wired {
            ui.label(
                egui::RichText::new(format!("{:.2} Hz", display_freq))
                    .strong()
                    .monospace(),
            );
        } else {
            ui.add(
                egui::DragValue::new(frequency)
                    .speed(0.1)
                    .range(20.0..=20000.0)
                    .suffix(" Hz")
                    .max_decimals(2),
            );
        }
    });

    // ── Port 1: Amp ──────────────────────────────────────────────────
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
            PortKind::Normalized,
        );
        ui.label(egui::RichText::new("Amp").small());
        if amp_wired {
            ui.label(
                egui::RichText::new(format!("{:.2}", display_amp))
                    .strong()
                    .monospace(),
            );
        } else {
            ui.add(egui::Slider::new(amplitude, 0.0..=1.0).show_value(true));
        }
    });

    // ── Port 2: Gate ─────────────────────────────────────────────────
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(
            ui,
            node_id,
            2,
            true,
            connections,
            port_positions,
            dragging_from,
            pending_disconnects,
            PortKind::Gate,
        );
        ui.label(egui::RichText::new("Gate").small());
        if gate_wired {
            ui.label(
                egui::RichText::new(if display_active { "ON" } else { "off" })
                    .strong()
                    .monospace(),
            );
        } else {
            let mut on = *active;
            ui.checkbox(&mut on, "");
            *active = on;
        }
    });

    // ── Port 3: FM Depth (always visible) ────────────────────────────
    // Controls how much audio input modulates frequency.
    // 0 = no modulation, 100 = ±100Hz modulation, etc.
    let fm_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 3);
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(
            ui,
            node_id,
            3,
            true,
            connections,
            port_positions,
            dragging_from,
            pending_disconnects,
            PortKind::Number,
        );
        ui.label(
            egui::RichText::new("FM")
                .small()
                .color(egui::Color32::from_rgb(255, 200, 80)),
        );
        if fm_wired {
            *fm_depth = Graph::static_input_value(connections, values, node_id, 3)
                .as_float()
                .max(0.0);
            ui.label(
                egui::RichText::new(format!("{:.0} Hz", *fm_depth))
                    .small()
                    .monospace(),
            );
        } else {
            ui.add(
                egui::DragValue::new(fm_depth)
                    .speed(1.0)
                    .range(0.0..=10000.0)
                    .suffix(" Hz"),
            );
        }
    });

    // ── Output: Audio ────────────────────────────────────────────────
    ui.separator();
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

    // ── Write params to engine ───────────────────────────────────────
    audio.engine_write_param(node_id, 0, *frequency);
    audio.engine_write_param(node_id, 1, *amplitude);
    audio.engine_write_param(node_id, 2, if *active { 1.0 } else { 0.0 });
    audio.engine_write_param(node_id, 3, *fm_depth);
    audio.engine_write_param(node_id, 4, *waveform as u32 as f32);
}
