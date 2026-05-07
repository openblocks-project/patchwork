use crate::graph::*;
use crate::serial::SerialAction;
use eframe::egui;
use crate::nodes::ScrollAreaExt;
use std::collections::HashMap;

const MAX_LOG: usize = 500;
const BAUD_RATES: &[u32] = &[9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600];

/// Serial node: connect to a serial port, read lines, send data.
/// Input: Send (text or float converted to string)
/// Output: Last Line (text), all received lines in a scrolling log.
pub fn render(
    ui: &mut egui::Ui,
    port_name: &mut String,
    baud_rate: &mut u32,
    log: &mut Vec<String>,
    last_line: &mut String,
    send_buf: &mut String,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    available_ports: &[String],
    is_connected: bool,
    serial_actions: &mut Vec<SerialAction>,
) {
    // ── Port & baud selector ────────────────────────────────────────
    ui.horizontal(|ui| {
        let (status, color) = if is_connected {
            ("\u{25cf}", egui::Color32::from_rgb(80, 220, 80))
        } else {
            ("\u{25cb}", egui::Color32::GRAY)
        };
        ui.label(egui::RichText::new(status).color(color));

        egui::ComboBox::from_id_salt(("serial_port", node_id))
            .selected_text(if port_name.is_empty() { "Port..." } else { port_name.as_str() })
            .width(120.0)
            .show_ui(ui, |ui| {
                for p in available_ports {
                    ui.selectable_value(port_name, p.clone(), p.as_str());
                }
            });

        egui::ComboBox::from_id_salt(("serial_baud", node_id))
            .selected_text(format!("{}", baud_rate))
            .width(70.0)
            .show_ui(ui, |ui| {
                for &rate in BAUD_RATES {
                    ui.selectable_value(baud_rate, rate, format!("{}", rate));
                }
            });
    });

    ui.horizontal(|ui| {
        if !port_name.is_empty() && !is_connected {
            if ui.button("Connect").clicked() {
                serial_actions.push(SerialAction::Connect {
                    node_id,
                    port_name: port_name.clone(),
                    baud_rate: *baud_rate,
                });
            }
        }
        if is_connected && ui.button("Disconnect").clicked() {
            serial_actions.push(SerialAction::Disconnect { node_id });
        }
        if !log.is_empty() && ui.button("Clear").clicked() {
            log.clear();
        }
    });

    ui.separator();

    // ── Send area ───────────────────────────────────────────────────
    // Check if input port has data to send
    let input_val = Graph::static_input_value(connections, values, node_id, 0);
    match &input_val {
        PortValue::Float(v) => {
            if is_connected {
                serial_actions.push(SerialAction::Send {
                    node_id,
                    data: format!("{}", v),
                });
            }
        }
        PortValue::Text(t) if !t.is_empty() => {
            if is_connected {
                serial_actions.push(SerialAction::Send {
                    node_id,
                    data: t.clone(),
                });
            }
        }
        _ => {}
    }

    ui.horizontal(|ui| {
        ui.label("Send:");
        let re = ui.add(
            egui::TextEdit::singleline(send_buf)
                .desired_width(120.0)
                .hint_text("type & enter"),
        );
        if (re.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) || ui.button("TX").clicked() {
            if is_connected && !send_buf.is_empty() {
                serial_actions.push(SerialAction::Send {
                    node_id,
                    data: send_buf.clone(),
                });
                log.push(format!("> {}", send_buf));
                send_buf.clear();
            }
            re.request_focus();
        }
    });

    ui.separator();

    // ── Log ─────────────────────────────────────────────────────────
    if !log.is_empty() {
        ui.label(
            egui::RichText::new(format!("{} lines", log.len()))
                .small()
                .color(egui::Color32::GRAY),
        );
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .stick_to_bottom(true)
            .show_pannable(ui, |ui| {
                for line in log.iter() {
                    let color = if line.starts_with('>') {
                        egui::Color32::from_rgb(100, 180, 255)
                    } else {
                        egui::Color32::from_rgb(200, 200, 200)
                    };
                    ui.label(egui::RichText::new(line.as_str()).monospace().small().color(color));
                }
            });
    } else if is_connected {
        ui.label(egui::RichText::new("Listening...").small().color(egui::Color32::GRAY));
    }

    // Update last_line for output port
    if let Some(l) = log.iter().rev().find(|l| !l.starts_with('>')) {
        *last_line = l.clone();
    }

    while log.len() > MAX_LOG { log.remove(0); }
}
