use crate::audio::AudioManager;
use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    _audio: &AudioManager,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    connections: &[Connection],
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let _ = values;

    // Read analysis stored by app/mod.rs (which does the source tracing)
    let (amp, peak, bass, mid, treble, source_name) = ui.ctx().data_mut(|d| {
        let vals: [f32; 5] = d.get_temp(egui::Id::new(("audio_analysis", node_id))).unwrap_or([0.0; 5]);
        let name: String = d.get_temp(egui::Id::new(("audio_analysis_source", node_id))).unwrap_or_default();
        (vals[0], vals[1], vals[2], vals[3], vals[4], name)
    });

    // Audio input port + connection status (single inline row)
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(
            ui, node_id, 0, true, connections,
            port_positions, dragging_from, pending_disconnects, PortKind::Audio,
        );
        if source_name.is_empty() {
            ui.label(egui::RichText::new("Connect audio input").small().color(egui::Color32::from_rgb(100, 100, 110)));
        } else {
            ui.label(egui::RichText::new(format!("← {}", source_name)).small().color(egui::Color32::from_rgb(80, 200, 120)));
        }
    });

    // Audio pass-through output (port 0) — placed before closure to avoid borrow conflicts
    crate::nodes::audio_port_row(ui, "Audio", node_id, 0, false, port_positions, dragging_from, connections, pending_disconnects, PortKind::Audio);

    // ── Level meters (output ports 1-5) ──────────────────────────────
    let bar_w = 110.0;
    let bar_h = 8.0;

    let mut meter = |ui: &mut egui::Ui, label: &str, value: f32, color: egui::Color32, port: usize| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Port circle flush at right edge
            crate::nodes::inline_port_circle(
                ui, node_id, port, false, connections,
                port_positions, dragging_from, pending_disconnects, PortKind::Normalized,
            );
            // Value label
            ui.label(egui::RichText::new(format!("{:.0}%", value * 100.0)).small().monospace());
            // Bar fills remaining space
            let available = ui.available_width().max(20.0).min(bar_w);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(available, bar_h), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(25, 25, 30));
            let fill_w = rect.width() * value.clamp(0.0, 1.0);
            if fill_w > 0.5 {
                // Bar fills left-to-right even in RTL layout
                let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
                painter.rect_filled(fill_rect, 2.0, color);
            }
            // Label at the left
            ui.label(egui::RichText::new(label).small().color(egui::Color32::from_rgb(160, 160, 170)));
        });
    };

    meter(ui, "Amp", amp, egui::Color32::from_rgb(80, 200, 120), 1);
    meter(ui, "Peak", peak, egui::Color32::from_rgb(255, 200, 60), 2);
    meter(ui, "Bass", bass, egui::Color32::from_rgb(255, 80, 80), 3);
    meter(ui, "Mid", mid, egui::Color32::from_rgb(80, 160, 255), 4);
    meter(ui, "Treble", treble, egui::Color32::from_rgb(200, 120, 255), 5);

    if amp > 0.001 {
        ui.ctx().request_repaint();
    }
}
