//! DMX In node — receives DMX channel values from a USB serial adapter.
//! Requires Enttec USB Pro (Open DMX is one-way).

use crate::dmx::{DmxAction, DmxAdapter};
use crate::graph::{NodeId, PortKind};
use eframe::egui;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &mut egui::Ui,
    port_name: &mut String,
    channel_count: &mut usize,
    start_at: &mut u16,
    last_values: &[u8],
    listening: &mut bool,
    node_id: NodeId,
    connections: &[crate::graph::Connection],
    serial_ports: &[String],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    dmx_actions: &mut Vec<DmxAction>,
) {
    // ── Status row ─────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{1F4E1}").size(16.0));
        let status = if *listening { "Listening" } else { "Stopped" };
        let color = if *listening {
            egui::Color32::from_rgb(120, 220, 140)
        } else {
            egui::Color32::from_rgb(130, 130, 130)
        };
        ui.colored_label(color, egui::RichText::new(status).strong());
    });

    // ── Port (USB Pro only — Open DMX is output-only) ──────────
    ui.horizontal(|ui| {
        ui.label("Port:");
        egui::ComboBox::from_id_salt(egui::Id::new(("dmx_in_port", node_id)))
            .selected_text(if port_name.is_empty() { "<select>".to_string() } else { port_name.clone() })
            .width(150.0)
            .show_ui(ui, |ui| {
                for p in serial_ports {
                    if ui.selectable_label(p == port_name, p).clicked() {
                        *port_name = p.clone();
                    }
                }
            });
    });
    ui.label(
        egui::RichText::new("Requires Enttec USB Pro").small()
            .color(egui::Color32::from_rgb(150, 150, 160)),
    );

    // ── Channel range ──────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Channels:");
        if ui.small_button("−").clicked() && *channel_count > 1 {
            *channel_count -= 1;
        }
        ui.label(egui::RichText::new(format!("{}", channel_count)).strong());
        if ui.small_button("+").clicked() && *channel_count < 64 {
            *channel_count += 1;
        }
        ui.label("from");
        let mut s = *start_at as u32;
        if ui.add(egui::DragValue::new(&mut s).range(1..=512)).changed() {
            *start_at = s.clamp(1, 512) as u16;
        }
    });

    // ── Output ports + current values ──────────────────────────
    ui.separator();
    for i in 0..*channel_count {
        ui.horizontal(|ui| {
            super::inline_port_circle(
                ui, node_id, i, false, connections,
                port_positions, dragging_from, pending_disconnects, PortKind::Normalized,
            );
            let ch = (*start_at as usize) + i;
            let byte = last_values.get(i).copied().unwrap_or(0);
            ui.label(format!("Ch {}: {}", ch, byte));
        });
    }

    // ── Listen toggle ──────────────────────────────────────────
    ui.separator();
    ui.horizontal(|ui| {
        if *listening {
            if ui.button("\u{23F9} Stop").clicked() {
                *listening = false;
                dmx_actions.push(DmxAction::StopListening { node_id });
            }
        } else {
            let enable = !port_name.is_empty();
            if ui.add_enabled(enable, egui::Button::new("\u{25B6} Start")).clicked() {
                *listening = true;
                dmx_actions.push(DmxAction::StartListening {
                    node_id,
                    port_name: port_name.clone(),
                    adapter: DmxAdapter::UsbPro,
                });
            }
        }
    });
}
