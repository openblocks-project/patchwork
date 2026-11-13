#![allow(dead_code)]
use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

/// Returns the new zoom value if changed by user interaction.
pub fn render(
    ui: &mut egui::Ui,
    zoom_value: &mut f32,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    current_zoom: f32,
) -> Option<f32> {
    // If input port is connected, use that value
    let connected = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 0);
    if connected {
        let v = Graph::static_input_value(connections, values, node_id, 0);
        let input_zoom = v.as_float().clamp(0.1, 5.0);
        *zoom_value = input_zoom;
    } else {
        *zoom_value = current_zoom;
    }

    let mut new_zoom = None;

    ui.horizontal(|ui| {
        ui.label(format!("{:.0}%", *zoom_value * 100.0));
        let mut z = *zoom_value;
        let resp = ui.add(
            egui::Slider::new(&mut z, 0.1..=5.0)
                .logarithmic(true)
                .show_value(false),
        );
        if resp.changed() {
            *zoom_value = z;
            new_zoom = Some(z);
        }
    });

    ui.horizontal(|ui| {
        if ui.small_button("50%").clicked() {
            *zoom_value = 0.5;
            new_zoom = Some(0.5);
        }
        if ui.small_button("100%").clicked() {
            *zoom_value = 1.0;
            new_zoom = Some(1.0);
        }
        if ui.small_button("200%").clicked() {
            *zoom_value = 2.0;
            new_zoom = Some(2.0);
        }
    });

    new_zoom
}
