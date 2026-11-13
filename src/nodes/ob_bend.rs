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
    ob_manager: &mut ObManager,
) {
    let (device_id, hub_node_id, label_color) = match node_type {
        NodeType::ObBend {
            device_id,
            hub_node_id,
            label_color,
        } => (device_id, hub_node_id, label_color),
        _ => return,
    };

    ui.horizontal(|ui| {
        ui.label("ID:");
        ui.add(egui::DragValue::new(device_id).range(1..=255));
    });

    let did = *device_id;
    let hid = *hub_node_id;

    let (val, is_active) = {
        let device = ob_manager
            .get_hub(hid)
            .and_then(|h| h.get_device("bend", did))
            .or_else(|| ob_manager.find_device("bend", did).map(|(_, d)| d));
        if let Some(dev) = device {
            (dev.values.get("val").copied().unwrap_or(0.0), dev.is_active)
        } else {
            (0.0, false)
        }
    };

    if is_active {
        ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● Active");
    } else {
        ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Waiting...");
    }

    let bar_w = ui.available_width().min(160.0);
    let bar_h = 20.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 20, 30));
    let fill_w = rect.width() * val.clamp(0.0, 1.0);
    let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, bar_h));
    painter.rect_filled(
        fill_rect,
        4.0,
        egui::Color32::from_rgb(label_color[0], label_color[1], label_color[2]),
    );

    ui.label(
        egui::RichText::new(format!("{:.2}", val))
            .monospace()
            .strong(),
    );

    ui.separator();
    ui.horizontal(|ui| {
        let mut c = egui::Color32::from_rgb(label_color[0], label_color[1], label_color[2]);
        if ui.color_edit_button_srgba(&mut c).changed() {
            let a = c.to_array();
            *label_color = [a[0], a[1], a[2]];
        }
        if ui.small_button("Set").clicked() {
            let col = *label_color;
            if let Some(hub) = if hid != 0 {
                ob_manager.get_hub_mut(hid)
            } else {
                ob_manager.find_any_hub_mut()
            } {
                hub.send_command(&format!(
                    "/bend/{}/color {} {} {}",
                    did, col[0], col[1], col[2]
                ));
            }
        }
    });
}
