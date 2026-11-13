use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    time_ms: &mut f32,
    feedback: &mut f32,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    // Audio input port (port 0)
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

    // Read params from ports if connected
    if connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 1)
    {
        *time_ms = Graph::static_input_value(connections, values, node_id, 1).as_float();
    }
    if connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 2)
    {
        *feedback = Graph::static_input_value(connections, values, node_id, 2).as_float();
    }

    // Time parameter with port
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
        ui.label("Time:");
        ui.add(
            egui::DragValue::new(time_ms)
                .speed(1.0)
                .range(1.0..=2000.0)
                .suffix(" ms"),
        );
    });

    // Feedback parameter with port
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
            PortKind::Normalized,
        );
        ui.label("FB:");
        ui.add(egui::DragValue::new(feedback).speed(0.01).range(0.0..=0.95));
    });

    // Audio output port (port 0)
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
