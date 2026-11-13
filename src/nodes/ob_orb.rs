use crate::graph::*;
use crate::ob::ObManager;
use eframe::egui;
use std::collections::HashMap;

const EFFECTS: &[&str] = &[
    "Manual",  // 0 — set color from laptop
    "Tilt",    // 1 — IMU: color shifts with tilt
    "Pulse",   // 2 — breathing glow
    "Rainbow", // 3 — rotating rainbow
];

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    ob_manager: &mut ObManager,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let (device_id, hub_node_id, mode, color, param1, _param2, speed, brightness) = match node_type
    {
        NodeType::ObOrb {
            device_id,
            hub_node_id,
            mode,
            color,
            param1,
            param2,
            speed,
            brightness,
        } => (
            device_id,
            hub_node_id,
            mode,
            color,
            param1,
            param2,
            speed,
            brightness,
        ),
        _ => return,
    };

    let did = *device_id;
    let hid = *hub_node_id;

    // Read IMU (drop borrow before mutable use)
    let (is_active, imu_vals) = {
        let dev = ob_manager
            .get_hub(hid)
            .and_then(|h| h.get_device("orb", did))
            .or_else(|| ob_manager.find_device("orb", did).map(|(_, d)| d));
        let active = dev.map(|d| d.is_active).unwrap_or(false);
        let v: [f32; 6] = if let Some(d) = dev {
            [
                d.values.get("ax").copied().unwrap_or(0.0),
                d.values.get("ay").copied().unwrap_or(0.0),
                d.values.get("az").copied().unwrap_or(0.0),
                d.values.get("gx").copied().unwrap_or(0.0),
                d.values.get("gy").copied().unwrap_or(0.0),
                d.values.get("gz").copied().unwrap_or(0.0),
            ]
        } else {
            [0.0; 6]
        };
        (active, v)
    };

    // ── Header ───────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("ID:").small());
        ui.add(egui::DragValue::new(device_id).range(1..=255));
        if is_active {
            ui.colored_label(egui::Color32::from_rgb(60, 200, 200), "●");
        } else {
            ui.colored_label(egui::Color32::from_rgb(80, 80, 80), "○");
        }
    });

    // ── Color + buttons ──────────────────────────────────────────
    ui.horizontal(|ui| {
        let mut c = egui::Color32::from_rgb(color[0], color[1], color[2]);
        if ui.color_edit_button_srgba(&mut c).changed() {
            let a = c.to_array();
            *color = [a[0], a[1], a[2]];
        }
        if ui.small_button("Set All").clicked() {
            let col = *color;
            let bri = *brightness;
            if let Some(hub) = if hid != 0 {
                ob_manager.get_hub_mut(hid)
            } else {
                ob_manager.find_any_hub_mut()
            } {
                hub.send_command(&format!(
                    "/orb/{}/allstrip {} {} {}",
                    did,
                    (col[0] as f32 * bri) as u8,
                    (col[1] as f32 * bri) as u8,
                    (col[2] as f32 * bri) as u8
                ));
            }
        }
        if ui.small_button("Off").clicked() {
            if let Some(hub) = if hid != 0 {
                ob_manager.get_hub_mut(hid)
            } else {
                ob_manager.find_any_hub_mut()
            } {
                hub.send_command(&format!("/orb/{}/allstrip 0 0 0", did));
            }
        }
    });

    // ── Effect ───────────────────────────────────────────────────
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(egui::Id::new(("orb_fx", node_id)))
            .selected_text(*EFFECTS.get(*mode as usize).unwrap_or(&"Manual"))
            .width(100.0)
            .show_ui(ui, |ui| {
                for (i, name) in EFFECTS.iter().enumerate() {
                    if ui.selectable_label(*mode == i as u8, *name).clicked() {
                        *mode = i as u8;
                    }
                }
            });
        ui.add(egui::Slider::new(brightness, 0.0..=1.0).show_value(false));
    });

    // ── Drive input (one port — connects to anything) ────────────
    let drive_wired = connections
        .iter()
        .any(|c| c.to_node == node_id && c.to_port == 0);
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(
            ui,
            node_id,
            0,
            true,
            connections,
            port_positions,
            dragging_from,
            pending_disconnects,
            PortKind::Number,
        );
        if drive_wired {
            let v = Graph::static_input_value(connections, values, node_id, 0).as_float();
            *param1 = v;
            ui.label(
                egui::RichText::new(format!("Drive: {:.2}", v))
                    .small()
                    .color(egui::Color32::from_rgb(80, 170, 255)),
            );
        } else {
            ui.label(egui::RichText::new("Drive").small());
            ui.add(egui::Slider::new(param1, 0.0..=1.0).show_value(false));
        }
    });

    // ── LED preview ──────────────────────────────────────────────
    let sz = 12.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(sz * 8.0 + 14.0, sz), egui::Sense::hover());
    let p = ui.painter();
    let bri = *brightness;
    let pc = egui::Color32::from_rgb(
        (color[0] as f32 * bri) as u8,
        (color[1] as f32 * bri) as u8,
        (color[2] as f32 * bri) as u8,
    );
    for i in 0..8 {
        let r = egui::Rect::from_min_size(
            egui::pos2(rect.left() + i as f32 * (sz + 2.0), rect.top()),
            egui::vec2(sz, sz),
        );
        p.rect_filled(r, 2.0, pc);
    }

    // ── IMU outputs ──────────────────────────────────────────────
    ui.separator();
    let labels = ["AX", "AY", "AZ", "GX", "GY", "GZ"];
    for (i, lbl) in labels.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} {:.2}", lbl, imu_vals[i]))
                    .small()
                    .monospace(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                crate::nodes::inline_port_circle(
                    ui,
                    node_id,
                    i,
                    false,
                    connections,
                    port_positions,
                    dragging_from,
                    pending_disconnects,
                    PortKind::Number,
                );
            });
        });
    }

    // ── Send (throttled) ─────────────────────────────────────────
    if *mode > 0 {
        let key = format!(
            "{} {:.2} {:.1} {} {} {} {:.2}",
            *mode, *param1, *speed, color[0], color[1], color[2], bri
        );
        let kid = egui::Id::new(("orb_cmd", node_id));
        let prev: Option<String> = ui.ctx().data_mut(|d| d.get_temp(kid));
        if prev.as_deref() != Some(&key) {
            ui.ctx().data_mut(|d| d.insert_temp(kid, key));
            if let Some(hub) = if hid != 0 {
                ob_manager.get_hub_mut(hid)
            } else {
                ob_manager.find_any_hub_mut()
            } {
                hub.send_command(&format!(
                    "/orb/{}/effect {} {:.2} {:.2} {:.1}",
                    did,
                    *mode - 1,
                    *param1,
                    bri,
                    *speed
                ));
                hub.send_command(&format!(
                    "/orb/{}/color {} {} {}",
                    did, color[0], color[1], color[2]
                ));
            }
        }
    }
}
