#![allow(dead_code)]
use crate::graph::*;
use eframe::egui;
use crate::nodes::ScrollAreaExt;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    messages: &mut Vec<String>,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let _ = values;

    // Input port
    ui.horizontal(|ui| {
        super::inline_port_circle(
            ui, node_id, 0, true, connections,
            port_positions, dragging_from, pending_disconnects, PortKind::Generic,
        );
        let connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        if connected {
            ui.label(egui::RichText::new("Logging input").small().color(
                egui::Color32::from_rgb(80, 200, 80)));
        } else {
            ui.label(egui::RichText::new("Connect to log").small().color(
                ui.visuals().widgets.noninteractive.fg_stroke.color));
        }
    });

    ui.horizontal(|ui| {
        if ui.small_button("Clear").clicked() {
            messages.clear();
        }
        ui.label(egui::RichText::new(format!("{} msgs", messages.len())).small()
            .color(ui.visuals().widgets.noninteractive.fg_stroke.color));
    });

    ui.separator();

    // Scrollable message area
    egui::ScrollArea::vertical()
        .max_height(200.0)
        .stick_to_bottom(true)
        .show_pannable(ui, |ui| {
            if messages.is_empty() {
                ui.label(egui::RichText::new("No messages yet").small().italics()
                    .color(ui.visuals().widgets.noninteractive.fg_stroke.color));
            }
            for msg in messages.iter() {
                // Color-code based on content
                let color = if msg.contains("error") || msg.contains("Error") || msg.contains("ERR") {
                    egui::Color32::from_rgb(255, 100, 100)
                } else if msg.contains("warn") || msg.contains("Warning") || msg.contains("WARN") {
                    egui::Color32::from_rgb(255, 200, 100)
                } else if msg.contains("ok") || msg.contains("success") || msg.contains("OK") {
                    egui::Color32::from_rgb(100, 255, 100)
                } else {
                    ui.visuals().text_color()
                };

                ui.label(egui::RichText::new(msg).color(color).monospace().size(11.0));
            }
        });
}
