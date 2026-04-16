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
        NodeType::ObEncoder { device_id, hub_node_id, label_color } => (device_id, hub_node_id, label_color),
        _ => return,
    };

    ui.horizontal(|ui| {
        ui.label("ID:");
        ui.add(egui::DragValue::new(device_id).range(1..=255));
    });

    let did = *device_id;
    let hid = *hub_node_id;

    // Lookup order matches app/mod.rs ObEncoder injection:
    //   1. Bound hub → 2. Any serial hub → 3. HID auto-discovery (OB Wheel)
    #[derive(PartialEq)]
    enum Source { Hub, Hid, None }
    let (turn, click, position, is_active, source) = {
        let from_hub = ob_manager.get_hub(hid)
            .and_then(|h| h.get_device("encoder", did))
            .or_else(|| ob_manager.find_device("encoder", did).map(|(_, d)| d))
            .map(|dev| (
                dev.values.get("turn").copied().unwrap_or(0.0),
                dev.values.get("click").copied().unwrap_or(0.0),
                dev.values.get("position").copied().unwrap_or(0.0),
                dev.is_active,
                Source::Hub,
            ));
        from_hub
            .or_else(|| crate::hid::global().find_device("encoder", did)
                .map(|dev| (
                    dev.values.get("turn").copied().unwrap_or(0.0),
                    dev.values.get("click").copied().unwrap_or(0.0),
                    dev.values.get("position").copied().unwrap_or(0.0),
                    dev.is_active,
                    Source::Hid,
                )))
            .unwrap_or((0.0, 0.0, 0.0, false, Source::None))
    };

    if is_active {
        let label = if source == Source::Hid { "● Active (HID)" } else { "● Active" };
        ui.colored_label(egui::Color32::from_rgb(80, 200, 80), label);
    } else {
        ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Waiting...");
    }

    // Encoder visualization
    let viz_size = 60.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(viz_size, viz_size), egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    let radius = viz_size * 0.4;

    painter.circle_filled(center, radius, egui::Color32::from_rgb(20, 20, 30));
    painter.circle_stroke(center, radius, egui::Stroke::new(1.5, egui::Color32::from_rgb(60, 60, 80)));

    let angle = -position * 0.3; // negated so CW rotation matches visual CW
    let indicator_end = egui::pos2(
        center.x + (angle as f32).cos() * radius * 0.8,
        center.y - (angle as f32).sin() * radius * 0.8,
    );
    // Neutral accent while LED control is disabled; restored from label_color
    // when that feature returns.
    let ind_color = if is_active {
        egui::Color32::from_rgb(140, 180, 230)
    } else {
        egui::Color32::from_rgb(100, 100, 100)
    };
    painter.line_segment([center, indicator_end], egui::Stroke::new(2.0, ind_color));
    painter.circle_filled(indicator_end, 3.0, ind_color);

    if click > 0.5 {
        painter.circle_filled(center, 5.0, egui::Color32::from_rgb(255, 100, 100));
    } else {
        painter.circle_filled(center, 3.0, egui::Color32::from_rgb(60, 60, 60));
    }

    // Values
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("Pos:{:.0}", position)).monospace().small());
        if turn.abs() > 0.5 {
            let dir = if turn > 0.0 { "↺" } else { "↻" };
            ui.colored_label(egui::Color32::from_rgb(200, 200, 80), dir);
        }
        if click > 0.5 {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), "●");
        }
    });

}
