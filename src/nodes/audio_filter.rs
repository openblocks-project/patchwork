use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render_lpf(
    ui: &mut egui::Ui,
    cutoff: &mut f32,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
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

    if connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 1)
    {
        *cutoff = Graph::static_input_value(connections, values, node_id, 1).as_float();
    }
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
        ui.label("Cutoff:");
        ui.add(
            egui::DragValue::new(cutoff)
                .speed(1.0)
                .range(20.0..=20000.0)
                .suffix(" Hz"),
        );
    });

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

pub fn render_hpf(
    ui: &mut egui::Ui,
    cutoff: &mut f32,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
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

    if connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 1)
    {
        *cutoff = Graph::static_input_value(connections, values, node_id, 1).as_float();
    }
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
        ui.label("Cutoff:");
        ui.add(
            egui::DragValue::new(cutoff)
                .speed(1.0)
                .range(20.0..=20000.0)
                .suffix(" Hz"),
        );
    });

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
