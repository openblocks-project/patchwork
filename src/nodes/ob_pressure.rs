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
    // `label_color` kept on the node for future LED support; UI hidden for now.
    let (device_id, hub_node_id, _label_color) = match node_type {
        NodeType::ObPressure { device_id, hub_node_id, label_color } => (device_id, hub_node_id, label_color),
        _ => return,
    };

    ui.horizontal(|ui| {
        ui.label("ID:");
        ui.add(egui::DragValue::new(device_id).range(1..=255));
    });

    let did = *device_id;
    let hid = *hub_node_id;

    // Lookup order matches app/mod.rs ObPressure injection:
    //   1. Bound hub → 2. Any serial hub → 3. HID auto-discovery
    #[derive(PartialEq)]
    enum Source { Hub, Hid, None }
    let (val, is_active, source) = {
        let from_hub = ob_manager.get_hub(hid)
            .and_then(|h| h.get_device("pressure", did))
            .or_else(|| ob_manager.find_device("pressure", did).map(|(_, d)| d))
            .map(|dev| (dev.values.get("val").copied().unwrap_or(0.0), dev.is_active, Source::Hub));
        from_hub
            .or_else(|| crate::hid::global().find_device("pressure", did)
                .map(|dev| (dev.values.get("val").copied().unwrap_or(0.0), dev.is_active, Source::Hid)))
            .unwrap_or((0.0, false, Source::None))
    };

    if is_active {
        let label = if source == Source::Hid { "● Active (HID)" } else { "● Active" };
        ui.colored_label(egui::Color32::from_rgb(80, 200, 80), label);
    } else {
        ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Waiting...");
    }

    // Pressure bar
    let bar_w = ui.available_width().min(160.0);
    let bar_h = 20.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 20, 30));
    let fill_w = rect.width() * val.clamp(0.0, 1.0);
    let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, bar_h));
    // Neutral accent while LED control is disabled.
    painter.rect_filled(fill_rect, 4.0, egui::Color32::from_rgb(140, 180, 230));

    ui.label(egui::RichText::new(format!("{:.2}", val)).monospace().strong());
}
