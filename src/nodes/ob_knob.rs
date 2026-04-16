use crate::graph::*;
use crate::ob::ObManager;
use eframe::egui;
use std::collections::HashMap;

/// OB Knob — single-axis rotary knob sensor. Mirrors `ob_bend::render` almost
/// exactly; the only protocol-level difference is that the knob stores its
/// reading under `dev.values["value"]` (see `ob.rs` apply_data arm for
/// `("knob", "value")`), not `"val"`.
pub fn render(
    ui: &mut egui::Ui,
    _node_id: NodeId,
    node_type: &mut NodeType,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _connections: &[Connection],
    ob_manager: &mut ObManager,
) {
    // `label_color` is still stored on the node (don't break saved projects)
    // but the color picker/Set button UI is hidden for now. Will return when
    // OB hardware LEDs get re-enabled.
    let (device_id, hub_node_id, _label_color) = match node_type {
        NodeType::ObKnob { device_id, hub_node_id, label_color } => (device_id, hub_node_id, label_color),
        _ => return,
    };

    ui.horizontal(|ui| {
        ui.label("ID:");
        ui.add(egui::DragValue::new(device_id).range(1..=255));
    });

    let did = *device_id;
    let hid = *hub_node_id;

    // Lookup order matches app/mod.rs ObKnob injection:
    //   1. Bound hub → 2. Any serial hub → 3. HID auto-discovery
    //
    // HID manager is a process-wide singleton — never construct one here.
    #[derive(PartialEq)]
    enum Source { Hub, Hid, None }
    let (val, is_active, source) = {
        let from_hub = ob_manager.get_hub(hid)
            .and_then(|h| h.get_device("knob", did))
            .or_else(|| ob_manager.find_device("knob", did).map(|(_, d)| d))
            .map(|dev| (dev.values.get("value").copied().unwrap_or(0.0), dev.is_active, Source::Hub));
        from_hub
            .or_else(|| crate::hid::global().find_device("knob", did)
                .map(|dev| (dev.values.get("value").copied().unwrap_or(0.0), dev.is_active, Source::Hid)))
            .unwrap_or((0.0, false, Source::None))
    };

    if is_active {
        let label = if source == Source::Hid { "● Active (HID)" } else { "● Active" };
        ui.colored_label(egui::Color32::from_rgb(80, 200, 80), label);
    } else {
        ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Waiting...");
    }

    // Circular dial: more natural for a rotary knob than a linear bar.
    // Value is treated as a normalized 0..1 rotation (270° sweep, starting
    // at 7 o'clock and ending at 5 o'clock — the common pot visualization).
    let size = 80.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    let center = rect.center();
    let radius = size * 0.42;
    // Dial backing
    painter.circle_filled(center, radius, egui::Color32::from_rgb(20, 20, 30));
    painter.circle_stroke(center, radius, egui::Stroke::new(1.5, egui::Color32::from_rgb(55, 55, 70)));
    // Sweep arc: approximate with a line strip
    let v = val.clamp(0.0, 1.0);
    let start_deg: f32 = 135.0;         // 7 o'clock
    let sweep_deg: f32 = 270.0 * v;     // full sweep at v=1 lands at 5 o'clock
    let seg_count = ((sweep_deg.abs() / 6.0).ceil() as usize).max(1);
    let mut pts: Vec<egui::Pos2> = Vec::with_capacity(seg_count + 1);
    for i in 0..=seg_count {
        let t = i as f32 / seg_count as f32;
        let deg = start_deg + sweep_deg * t;
        let r = deg.to_radians();
        pts.push(egui::pos2(
            center.x + radius * 0.82 * r.cos(),
            center.y + radius * 0.82 * r.sin(),
        ));
    }
    // Neutral accent for dial arc + indicator. When LED control returns
    // we'll switch back to label_color so the node visually matches the
    // physical device.
    let accent = egui::Color32::from_rgb(140, 180, 230);
    if pts.len() >= 2 {
        painter.add(egui::Shape::line(pts, egui::Stroke::new(3.0, accent)));
    }
    // Indicator line from center to current angle
    let end_r = (start_deg + 270.0 * v).to_radians();
    let tip = egui::pos2(
        center.x + radius * 0.72 * end_r.cos(),
        center.y + radius * 0.72 * end_r.sin(),
    );
    painter.line_segment([center, tip], egui::Stroke::new(2.0, accent));

    ui.label(egui::RichText::new(format!("{:.2}", val)).monospace().strong());
}
