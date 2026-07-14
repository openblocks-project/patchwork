use crate::graph::*;
use crate::ob::{self, ObManager};
use eframe::egui;
use std::collections::HashMap;

/// OB Wheel — rotary encoder (CLK/DT + push switch) plus an indicator LED
/// strip, on a dedicated native-USB MCU connected wired over USB serial.
///
/// Zero-config: auto-discovered by USB descriptor string (`manufacturer =
/// "OpenBlocks"`) and bound by `("encoder", id)`. No port to choose; a manual
/// override is tucked under "Port (manual)" for non-standard boards.
///
/// Hardware: CLK→GPIO1 (A1), DT→GPIO2 (A2), SW→SDA, WS2812 strip DATA→A6.
///
/// Protocol:
///   device → Patchwork: `/sys/ready encoder <id>`,
///                       `/encoder/<id>/turn <±1>`, `/encoder/<id>/click <0|1>`
///   Patchwork → device: `/encoder/<id>/color <r> <g> <b>`, `/encoder/<id>/blink`
pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _connections: &[Connection],
    ob_manager: &mut ObManager,
) {
    let (device_id, label_color, port_name, selected_port) = match node_type {
        NodeType::ObEncoder { device_id, label_color, port_name, selected_port, .. } =>
            (device_id, label_color, port_name, selected_port),
        _ => return,
    };
    let did = *device_id;
    let error_id = egui::Id::new(("ob_enc_error", node_id));
    let led_sent_key = egui::Id::new(("ob_enc_led_sent", node_id));

    // Snapshot device state up front (owned) so we can take &mut ob_manager
    // later for LED commands without a borrow conflict.
    let (present, active, turn, click, position) = match ob_manager.device("encoder", did) {
        Some(d) => (
            true, d.is_active,
            d.values.get("turn").copied().unwrap_or(0.0),
            d.values.get("click").copied().unwrap_or(0.0),
            d.values.get("position").copied().unwrap_or(0.0),
        ),
        None => (false, false, 0.0, 0.0, 0.0),
    };

    // ── Status ──────────────────────────────────────────────────────────────
    if present && active {
        ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● Connected");
    } else if present {
        ui.colored_label(egui::Color32::from_rgb(200, 180, 80), "○ Connecting…");
    } else {
        ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Searching for device…");
    }

    // ── Manual port override (collapsed; only for non-OpenBlocks boards) ─────
    ui.collapsing("Port (manual)", |ui| {
        let ports = ob::available_ports();
        let manual_connected = ob_manager.get_hub(node_id).is_some();
        ui.horizontal(|ui| {
            ui.label("Port:");
            egui::ComboBox::from_id_salt(egui::Id::new(("ob_enc_port", node_id)))
                .selected_text(if selected_port.is_empty() { "Auto" } else { selected_port.as_str() })
                .width(120.0)
                .show_ui(ui, |ui| {
                    for p in &ports {
                        if ui.selectable_label(selected_port.as_str() == p.as_str(), p).clicked() {
                            *selected_port = p.clone();
                            ui.ctx().data_mut(|d| d.remove::<String>(error_id));
                        }
                    }
                });
        });
        ui.horizontal(|ui| {
            if manual_connected {
                if ui.button("Disconnect").clicked() {
                    ob_manager.disconnect_hub(node_id);
                    port_name.clear();
                    ui.ctx().data_mut(|d| d.remove::<String>(error_id));
                }
            } else {
                let can_connect = !selected_port.is_empty();
                if ui.add_enabled(can_connect, egui::Button::new("Connect")).clicked() {
                    match ob_manager.connect_hub(node_id, selected_port) {
                        Ok(()) => {
                            *port_name = selected_port.clone();
                            ui.ctx().data_mut(|d| d.remove::<String>(error_id));
                        }
                        Err(e) => { ui.ctx().data_mut(|d| d.insert_temp(error_id, e)); }
                    }
                }
            }
        });
        let error_msg: Option<String> = ui.ctx().data_mut(|d| d.get_temp(error_id));
        if let Some(err) = error_msg {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("⚠ {}", err));
        }
        ui.label(egui::RichText::new("OpenBlocks devices auto-connect. Use this only for non-standard boards.")
            .small().color(egui::Color32::GRAY));
    });

    // ── Device ID + LED colour ──────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("ID:");
        ui.add(egui::DragValue::new(device_id).range(1..=255));
        ui.label("LED:");
        let mut c = egui::Color32::from_rgb(label_color[0], label_color[1], label_color[2]);
        if ui.color_edit_button_srgba(&mut c).changed() {
            *label_color = [c.r(), c.g(), c.b()];
        }
    });

    // ── Reconcile LED → device (assign + blink on appear, push on change) ───
    if present {
        let last_sent: Option<[u8; 3]> = ui.ctx().data_mut(|d| d.get_temp(led_sent_key));
        if last_sent.is_none() {
            if *label_color == [255, 255, 255] {
                *label_color = auto_led_color(node_id);
            }
            ob_manager.send_to_device("encoder", did, &format!("/encoder/{}/blink", did));
        }
        if last_sent != Some(*label_color) {
            ob_manager.send_to_device("encoder", did, &format!("/encoder/{}/color {} {} {}",
                did, label_color[0], label_color[1], label_color[2]));
            ui.ctx().data_mut(|d| d.insert_temp(led_sent_key, *label_color));
        }
    } else {
        ui.ctx().data_mut(|d| d.remove::<[u8; 3]>(led_sent_key));
    }

    // ── Encoder visualization ───────────────────────────────────────────────
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
    // Tint the indicator with the assigned LED colour so the node matches the device.
    let accent = egui::Color32::from_rgb(label_color[0], label_color[1], label_color[2]);
    let ind_color = if active { accent } else { egui::Color32::from_rgb(100, 100, 100) };
    painter.line_segment([center, indicator_end], egui::Stroke::new(2.0, ind_color));
    painter.circle_filled(indicator_end, 3.0, ind_color);

    if click > 0.5 {
        painter.circle_filled(center, 5.0, egui::Color32::from_rgb(255, 100, 100));
    } else {
        painter.circle_filled(center, 3.0, egui::Color32::from_rgb(60, 60, 60));
    }

    // ── Values ──────────────────────────────────────────────────────────────
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

/// Distinct, stable LED colour per node via golden-ratio hue rotation.
fn auto_led_color(node_id: NodeId) -> [u8; 3] {
    let hue = ((node_id as f32) * 0.618_034).fract();
    hsv_to_rgb(hue, 0.65, 1.0)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}
