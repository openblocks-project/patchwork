use crate::graph::*;
use crate::ob::{self, ObManager};
use eframe::egui;
use std::collections::HashMap;

/// OB Pressure — force-sensitive resistor (FSR) in a voltage divider, plus an
/// indicator LED strip, on a dedicated native-USB MCU connected wired over USB.
///
/// Zero-config: auto-discovered (USB descriptor "OpenBlocks" or content-probe)
/// and bound by `("pressure", id)`. No port to choose; manual override under
/// "Port (manual)" for non-standard boards.
///
/// Hardware: FSR between 3V3 and the ADC junction (GPIO1), 10 kΩ pull-down to
/// GND. WS2812 strip DATA → A2. Pressing lowers FSR resistance → junction
/// voltage rises → value rises.
///
/// Protocol:
///   device → Patchwork: `/sys/ready pressure <id>`, `/pressure/<id>/val <0..1>`
///   Patchwork → device: `/pressure/<id>/color <r> <g> <b>`, `/pressure/<id>/blink`
pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _connections: &[Connection],
    ob_manager: &mut ObManager,
) {
    let (device_id, label_color, port_name, selected_port) = match node_type {
        NodeType::ObPressure { device_id, label_color, port_name, selected_port, .. } =>
            (device_id, label_color, port_name, selected_port),
        _ => return,
    };
    let did = *device_id;
    let error_id = egui::Id::new(("ob_press_error", node_id));
    let led_sent_key = egui::Id::new(("ob_press_led_sent", node_id));

    let (present, active, val) = match ob_manager.device("pressure", did) {
        Some(d) => (true, d.is_active, d.values.get("val").copied().unwrap_or(0.0)),
        None => (false, false, 0.0),
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
            egui::ComboBox::from_id_salt(egui::Id::new(("ob_press_port", node_id)))
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
            ob_manager.send_to_device("pressure", did, &format!("/pressure/{}/blink", did));
        }
        if last_sent != Some(*label_color) {
            ob_manager.send_to_device("pressure", did, &format!("/pressure/{}/color {} {} {}",
                did, label_color[0], label_color[1], label_color[2]));
            ui.ctx().data_mut(|d| d.insert_temp(led_sent_key, *label_color));
        }
    } else {
        ui.ctx().data_mut(|d| d.remove::<[u8; 3]>(led_sent_key));
    }

    // ── Pressure bar ────────────────────────────────────────────────────────
    let bar_w = ui.available_width().min(160.0);
    let bar_h = 20.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 20, 30));
    let fill_w = rect.width() * val.clamp(0.0, 1.0);
    let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, bar_h));
    // Tint the bar with the assigned LED colour so the node matches the device.
    let bar_color = egui::Color32::from_rgb(label_color[0], label_color[1], label_color[2]);
    painter.rect_filled(fill_rect, 4.0, bar_color);

    ui.label(egui::RichText::new(format!("{:.2}", val)).monospace().strong());
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
