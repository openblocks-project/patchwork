use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    room_size: &mut f32,
    damping: &mut f32,
    mix: &mut f32,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    // Audio input
    super::audio_port_row(
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

    // Room Size
    let room_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 1);
    ui.horizontal(|ui| {
        super::inline_port_circle(
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
        ui.label(egui::RichText::new("Room").small());
        if room_wired {
            *room_size = Graph::static_input_value(connections, values, node_id, 1)
                .as_float()
                .clamp(0.0, 1.0);
            ui.label(
                egui::RichText::new(format!("{:.0}%", *room_size * 100.0))
                    .small()
                    .monospace(),
            );
        } else {
            ui.add(egui::Slider::new(room_size, 0.0..=1.0).show_value(false));
            ui.label(
                egui::RichText::new(format!("{:.0}%", *room_size * 100.0))
                    .small()
                    .monospace(),
            );
        }
    });

    // Damping
    let damp_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 2);
    ui.horizontal(|ui| {
        super::inline_port_circle(
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
        ui.label(egui::RichText::new("Damp").small());
        if damp_wired {
            *damping = Graph::static_input_value(connections, values, node_id, 2)
                .as_float()
                .clamp(0.0, 1.0);
            ui.label(
                egui::RichText::new(format!("{:.0}%", *damping * 100.0))
                    .small()
                    .monospace(),
            );
        } else {
            ui.add(egui::Slider::new(damping, 0.0..=1.0).show_value(false));
            ui.label(
                egui::RichText::new(format!("{:.0}%", *damping * 100.0))
                    .small()
                    .monospace(),
            );
        }
    });

    // Mix (wet/dry)
    let mix_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 3);
    ui.horizontal(|ui| {
        super::inline_port_circle(
            ui,
            node_id,
            3,
            true,
            connections,
            port_positions,
            dragging_from,
            pending_disconnects,
            PortKind::Normalized,
        );
        ui.label(egui::RichText::new("Mix").small());
        if mix_wired {
            *mix = Graph::static_input_value(connections, values, node_id, 3)
                .as_float()
                .clamp(0.0, 1.0);
            ui.label(
                egui::RichText::new(format!("{:.0}%", *mix * 100.0))
                    .small()
                    .monospace(),
            );
        } else {
            ui.add(egui::Slider::new(mix, 0.0..=1.0).show_value(false));
            ui.label(
                egui::RichText::new(format!("{:.0}%", *mix * 100.0))
                    .small()
                    .monospace(),
            );
        }
    });

    // Audio output
    super::audio_port_row(
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
