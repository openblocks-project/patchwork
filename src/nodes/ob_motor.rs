// OB Motor — WIP. Wireless stepper (TMC2208 + NEMA 17 over an OB Hub).
// Status DAT carries {moving, cur_deg, tgt_deg}; commands sent are
// /motor/<id>/motor <dir> <speed> <deg> and /motor/<id>/spin <dir> <speed> <rot>.
// No /stop — firmware busy-rejects mid-move commands.

use crate::graph::*;
use crate::ob::ObManager;
use eframe::egui;
use std::collections::HashMap;

const MODE_MOVE: u8 = 0;
const MODE_SPIN: u8 = 1;

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
    let (device_id, hub_node_id, mode, dir, speed, amount) = match node_type {
        NodeType::ObMotor { device_id, hub_node_id, mode, dir, speed, amount } =>
            (device_id, hub_node_id, mode, dir, speed, amount),
        _ => return,
    };

    let did = *device_id;
    let hid = *hub_node_id;

    // Live state from device (drop borrow before any mutable hub use)
    let (is_active, moving, cur_deg, tgt_deg) = {
        let dev = ob_manager.get_hub(hid)
            .and_then(|h| h.get_device("motor", did))
            .or_else(|| ob_manager.find_device("motor", did).map(|(_, d)| d));
        if let Some(d) = dev {
            (
                d.is_active,
                d.values.get("moving").copied().unwrap_or(0.0) > 0.5,
                d.values.get("position").copied().unwrap_or(0.0),
                d.values.get("target").copied().unwrap_or(0.0),
            )
        } else {
            (false, false, 0.0, 0.0)
        }
    };

    // ── WIP banner ────────────────────────────────────────────────
    ui.colored_label(egui::Color32::from_rgb(220, 170, 60),
        egui::RichText::new("⚠ WIP — wireless stepper").small());

    // ── Header: ID + status ───────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("ID:").small());
        ui.add(egui::DragValue::new(device_id).range(1..=255));
        if !is_active {
            ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Waiting");
        } else if moving {
            ui.colored_label(egui::Color32::from_rgb(255, 180, 60), "● Moving");
        } else {
            ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● Idle");
        }
    });

    // ── Mode + direction ──────────────────────────────────────────
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(egui::Id::new(("motor_mode", node_id)))
            .selected_text(if *mode == MODE_SPIN { "Spin (rot)" } else { "Move (deg)" })
            .width(100.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(*mode == MODE_MOVE, "Move (deg)").clicked() { *mode = MODE_MOVE; }
                if ui.selectable_label(*mode == MODE_SPIN, "Spin (rot)").clicked() { *mode = MODE_SPIN; }
            });
        egui::ComboBox::from_id_salt(egui::Id::new(("motor_dir", node_id)))
            .selected_text(if *dir == 1 { "CW" } else { "CCW" })
            .width(60.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(*dir == 1, "CW").clicked()  { *dir = 1; }
                if ui.selectable_label(*dir == 0, "CCW").clicked() { *dir = 0; }
            });
    });

    // ── Speed slider ──────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Speed").small());
        // Range tuned for 12V / 2A NEMA 17 with no acceleration ramp.
        ui.add(egui::Slider::new(speed, 100.0..=600.0).integer().suffix(" sps"));
    });

    // ── Amount (degrees or rotations) ─────────────────────────────
    ui.horizontal(|ui| {
        let label = if *mode == MODE_SPIN { "Rotations" } else { "Degrees" };
        ui.label(egui::RichText::new(label).small());
        let speed_factor = if *mode == MODE_SPIN { 0.05 } else { 1.0 };
        ui.add(egui::DragValue::new(amount).speed(speed_factor).range(0.0..=10000.0));
    });

    // ── Trigger input (port 0) ────────────────────────────────────
    let trig_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    let mut send_now = false;
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 0, true, connections,
            port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Trigger").small());
        if !trig_wired {
            // Manual fire button when nothing is wired.
            if ui.add_enabled(!moving, egui::Button::new("Send")).clicked() {
                send_now = true;
            }
        } else {
            // Rising-edge detection on wired Trigger.
            let v = Graph::static_input_value(connections, values, node_id, 0).as_float();
            let key = egui::Id::new(("motor_prev_trig", node_id));
            let prev: f32 = ui.ctx().data_mut(|d| d.get_temp(key)).unwrap_or(0.0);
            if v > 0.5 && prev <= 0.5 && !moving {
                send_now = true;
            }
            ui.ctx().data_mut(|d| d.insert_temp(key, v));
            if moving {
                ui.colored_label(egui::Color32::from_rgb(180, 180, 180),
                    egui::RichText::new("(busy)").small());
            }
        }
    });

    if send_now {
        let action = if *mode == MODE_SPIN { "spin" } else { "motor" };
        let cmd = format!("/motor/{}/{} {} {} {}", did, action, *dir, *speed as i32, *amount);
        if let Some(hub) = if hid != 0 { ob_manager.get_hub_mut(hid) } else { ob_manager.find_any_hub_mut() } {
            hub.send_command(&cmd);
        }
    }

    // ── Position readout ──────────────────────────────────────────
    ui.separator();
    ui.label(
        egui::RichText::new(format!("pos {:>8.2}°  →  tgt {:>8.2}°", cur_deg, tgt_deg))
            .small().monospace()
    );
}
