use crate::graph::*;
use crate::ob::{self, ObManager};
use eframe::egui;
use crate::nodes::ScrollAreaExt;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    ob_manager: &mut ObManager,
) {
    let (port_name, selected_port) = match node_type {
        NodeType::ObHub { port_name, selected_port, .. } => (port_name, selected_port),
        _ => return,
    };

    let ports = ob::available_ports();
    let is_connected = ob_manager.get_hub(node_id).is_some();
    let error_id = egui::Id::new(("ob_hub_error", node_id));

    // Port selector
    ui.horizontal(|ui| {
        ui.label("Port:");
        egui::ComboBox::from_id_salt(egui::Id::new(("ob_hub_port", node_id)))
            .selected_text(if selected_port.is_empty() { "Select..." } else { selected_port.as_str() })
            .width(120.0)
            .show_ui(ui, |ui| {
                for p in &ports {
                    if ui.selectable_label(selected_port.as_str() == p.as_str(), p).clicked() {
                        *selected_port = p.clone();
                        // Clear error when user selects a new port
                        ui.ctx().data_mut(|d| d.remove::<String>(error_id));
                    }
                }
            });
    });

    // Connect / Disconnect button
    ui.horizontal(|ui| {
        if is_connected {
            let status_color = egui::Color32::from_rgb(80, 200, 80);
            ui.colored_label(status_color, "● Connected");
            if ui.button("Disconnect").clicked() {
                ob_manager.disconnect_hub(node_id);
                port_name.clear();
                ui.ctx().data_mut(|d| d.remove::<String>(error_id));
            }
        } else {
            ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Disconnected");
            let can_connect = !selected_port.is_empty();
            if ui.add_enabled(can_connect, egui::Button::new("Connect")).clicked() {
                match ob_manager.connect_hub(node_id, selected_port) {
                    Ok(()) => {
                        *port_name = selected_port.clone();
                        ui.ctx().data_mut(|d| d.remove::<String>(error_id));
                    }
                    Err(e) => {
                        ui.ctx().data_mut(|d| d.insert_temp(error_id, e));
                    }
                }
            }
        }
    });

    // Show persistent error message
    let error_msg: Option<String> = ui.ctx().data_mut(|d| d.get_temp(error_id));
    if let Some(err) = error_msg {
        ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("⚠ {}", err));
        ui.label(egui::RichText::new("Close other apps using this port and retry").small().color(egui::Color32::GRAY));
    }

    // Show write warning if port clone failed
    if let Some(hub) = ob_manager.get_hub(node_id) {
        if let Some(ref warn) = hub.write_warning {
            ui.colored_label(egui::Color32::from_rgb(255, 180, 60), warn);
        }
    }

    // Show connected devices
    if let Some(hub) = ob_manager.get_hub(node_id) {
        let device_count = hub.devices.len();
        if device_count > 0 {
            ui.separator();
            ui.label(egui::RichText::new(format!("Devices ({})", device_count)).strong().small());
        }
        // Collect device info before iterating (to avoid borrow issues)
        let device_infos: Vec<(String, u8, bool, Vec<(String, f32)>)> = hub.devices.iter()
            .map(|((dtype, id), dev)| {
                let vals: Vec<(String, f32)> = dev.values.iter()
                    .take(4)
                    .map(|(k, v)| (k.clone(), *v))
                    .collect();
                (dtype.clone(), *id, dev.is_active, vals)
            })
            .collect();

        for (dtype, id, is_active, vals) in &device_infos {
            ui.horizontal(|ui| {
                let dot = if *is_active { "●" } else { "○" };
                let color = if *is_active {
                    egui::Color32::from_rgb(80, 200, 80)
                } else {
                    egui::Color32::from_rgb(200, 80, 80)
                };
                ui.colored_label(color, dot);
                ui.label(format!("{} #{}", dtype, id));

                // Spawn node button
                if ui.small_button("➕").on_hover_text(format!("Create {} node", dtype)).clicked() {
                    let spawn_id = egui::Id::new(("ob_spawn", node_id));
                    ui.ctx().data_mut(|d| d.insert_temp(spawn_id, (dtype.clone(), *id)));
                }

                // Show key values inline
                if !vals.is_empty() {
                    let vals_str: Vec<String> = vals.iter()
                        .map(|(k, v)| format!("{}={:.2}", k, v))
                        .collect();
                    ui.label(
                        egui::RichText::new(vals_str.join(" "))
                            .small()
                            .color(egui::Color32::GRAY),
                    );
                }
            });
        }

        // Log (collapsible)
        if !hub.log.is_empty() {
            ui.collapsing("Log", |ui| {
                egui::ScrollArea::vertical().max_height(100.0).show_pannable(ui, |ui| {
                    for line in hub.log.iter().rev().take(20) {
                        ui.label(egui::RichText::new(line).small().monospace());
                    }
                });
            });
        }
    }

    // Send command input
    if is_connected {
        ui.separator();
        let cmd_id = egui::Id::new(("ob_hub_cmd", node_id));
        let mut cmd = ui.ctx().data_mut(|d| d.get_temp::<String>(cmd_id).unwrap_or_default());
        let mut send_now = false;
        ui.horizontal(|ui| {
            ui.label("Cmd:");
            let r = ui.text_edit_singleline(&mut cmd);
            // Enter inside the text input ALSO sends, but the Send button is
            // the reliable path — Enter sometimes gets eaten by global hotkeys
            // (Add menu, etc.) when focus shifts.
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                send_now = true;
            }
            if ui.add_enabled(!cmd.is_empty(), egui::Button::new("Send")).clicked() {
                send_now = true;
            }
        });
        if send_now && !cmd.is_empty() {
            if let Some(hub) = ob_manager.get_hub_mut(node_id) {
                hub.send_command(&cmd);
            }
            cmd.clear();
        }
        ui.ctx().data_mut(|d| d.insert_temp(cmd_id, cmd));
    }
}
