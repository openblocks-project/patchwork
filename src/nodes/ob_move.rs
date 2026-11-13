use crate::graph::*;
use crate::ob::ObManager;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    _node_id: NodeId,
    node_type: &mut NodeType,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _connections: &[Connection],
    ob_manager: &ObManager,
) {
    let (device_id, hub_node_id) = match node_type {
        NodeType::ObMove {
            device_id,
            hub_node_id,
        } => (device_id, hub_node_id),
        _ => return,
    };

    ui.horizontal(|ui| {
        ui.label("ID:");
        ui.add(egui::DragValue::new(device_id).range(1..=255));
    });

    let did = *device_id;
    let hid = *hub_node_id;

    let (vals, is_active) = {
        let device = ob_manager
            .get_hub(hid)
            .and_then(|h| h.get_device("move", did))
            .or_else(|| ob_manager.find_device("move", did).map(|(_, d)| d));
        if let Some(dev) = device {
            (
                [
                    dev.values.get("ax").copied().unwrap_or(0.0),
                    dev.values.get("ay").copied().unwrap_or(0.0),
                    dev.values.get("az").copied().unwrap_or(0.0),
                    dev.values.get("gx").copied().unwrap_or(0.0),
                    dev.values.get("gy").copied().unwrap_or(0.0),
                    dev.values.get("gz").copied().unwrap_or(0.0),
                ],
                dev.is_active,
            )
        } else {
            ([0.0; 6], false)
        }
    };

    if is_active {
        ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● Active");
    } else {
        ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Waiting...");
    }

    let labels = ["AX", "AY", "AZ", "GX", "GY", "GZ"];
    for (i, lbl) in labels.iter().enumerate() {
        ui.label(
            egui::RichText::new(format!("{} {:.2}", lbl, vals[i]))
                .small()
                .monospace(),
        );
    }
}
