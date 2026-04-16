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
        NodeType::ObDistance { device_id, hub_node_id, label_color } => (device_id, hub_node_id, label_color),
        _ => return,
    };

    ui.horizontal(|ui| {
        ui.label("ID:");
        ui.add(egui::DragValue::new(device_id).range(1..=255));
    });

    let did = *device_id;
    let hid = *hub_node_id;

    // Lookup order matches app/mod.rs ObDistance injection:
    //   1. Bound hub → 2. Any serial hub → 3. HID auto-discovery
    //
    // `status` is a firmware-reported diagnostic (HID path only):
    //   0 = valid reading, 1 = out of range / no target, 2 = sensor error
    // Hub path doesn't carry this, so unwrap defaults to 0.
    #[derive(PartialEq)]
    enum Source { Hub, Hid, None }
    let (val, mm, status, is_active, source) = {
        let from_hub = ob_manager.get_hub(hid)
            .and_then(|h| h.get_device("distance", did))
            .or_else(|| ob_manager.find_device("distance", did).map(|(_, d)| d))
            .map(|dev| (
                dev.values.get("val").copied().unwrap_or(0.0),
                dev.values.get("mm").copied().unwrap_or(0.0),
                dev.values.get("status").copied().unwrap_or(0.0),
                dev.is_active,
                Source::Hub,
            ));
        from_hub
            .or_else(|| crate::hid::global().find_device("distance", did)
                .map(|dev| (
                    dev.values.get("val").copied().unwrap_or(0.0),
                    dev.values.get("mm").copied().unwrap_or(0.0),
                    dev.values.get("status").copied().unwrap_or(0.0),
                    dev.is_active,
                    Source::Hid,
                )))
            .unwrap_or((0.0, 0.0, 0.0, false, Source::None))
    };

    if is_active {
        // Pick the status-aware label so the user sees WHY mm is 0 —
        // no target in range is very different from sensor init failure.
        let status_i = status.round() as i32;
        let (dot_color, label) = match (source == Source::Hid, status_i) {
            (true, 0)  => (egui::Color32::from_rgb(80, 200, 80),  "● Active (HID)"),
            (true, 1)  => (egui::Color32::from_rgb(220, 180, 60), "● No target (HID)"),
            (true, 2)  => (egui::Color32::from_rgb(220, 80, 80),  "⚠ Sensor error (HID)"),
            (true, _)  => (egui::Color32::from_rgb(180, 180, 180), "● Active (HID)"),
            (false, _) => (egui::Color32::from_rgb(80, 200, 80),  "● Active"),
        };
        ui.colored_label(dot_color, label);
    } else {
        ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Waiting...");
    }

    // Horizontal bar visualization of the normalized value
    let bar_w = ui.available_width().min(160.0);
    let bar_h = 20.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 20, 30));
    let fill_w = rect.width() * val.clamp(0.0, 1.0);
    let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, bar_h));
    // Neutral accent while LED control is disabled.
    let bar_color = egui::Color32::from_rgb(140, 180, 230);
    painter.rect_filled(fill_rect, 4.0, bar_color);

    // Readouts: normalized value + raw mm in subtle secondary color
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{:.2}", val)).monospace().strong());
        ui.label(egui::RichText::new(format!("({:.0} mm)", mm))
            .small()
            .color(egui::Color32::from_rgb(150, 150, 160)));
    });
}
