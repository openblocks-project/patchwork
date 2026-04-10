use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    semitones: &mut f32,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    // Audio input
    crate::nodes::audio_port_row(ui, "Audio", node_id, 0, true, port_positions, dragging_from, connections, pending_disconnects, PortKind::Audio);

    // Semitones — port + slider + drag value
    let wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
    if wired {
        *semitones = Graph::static_input_value(connections, values, node_id, 1)
            .as_float().clamp(-12.0, 12.0);
    }
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
        ui.label("Semitones:");
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::Slider::new(semitones, -12.0..=12.0)
                .step_by(0.01)
                .show_value(false),
        );
        ui.add(
            egui::DragValue::new(semitones)
                .speed(0.01)
                .range(-12.0..=12.0)
                .fixed_decimals(2),
        );
    });

    // Audio output
    crate::nodes::audio_port_row(ui, "Audio", node_id, 0, false, port_positions, dragging_from, connections, pending_disconnects, PortKind::Audio);
}
