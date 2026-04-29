//! DMX Out node — sends DMX channel values over a USB serial adapter
//! (Enttec USB Pro framed, or Enttec Open DMX raw FTDI).

use crate::dmx::{DmxAction, DmxAdapter};
use crate::graph::{Connection, Graph, NodeId, PortKind, PortValue};
use eframe::egui;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &mut egui::Ui,
    port_name: &mut String,
    adapter: &mut DmxAdapter,
    frame_rate_hz: &mut u8,
    channel_count: &mut usize,
    start_at: &mut u16,
    active: &mut bool,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    serial_ports: &[String],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    dmx_output_open: bool,
    dmx_actions: &mut Vec<DmxAction>,
) {
    // ── Status row ─────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{1F4A1}").size(16.0));
        let status = if *active && dmx_output_open {
            "Sending"
        } else if *active {
            "Opening..."
        } else {
            "Stopped"
        };
        let color = if *active && dmx_output_open {
            egui::Color32::from_rgb(120, 220, 140)
        } else if *active {
            egui::Color32::from_rgb(220, 200, 80)
        } else {
            egui::Color32::from_rgb(130, 130, 130)
        };
        ui.colored_label(color, egui::RichText::new(status).strong());
    });

    // ── Port + adapter ─────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Port:");
        egui::ComboBox::from_id_salt(egui::Id::new(("dmx_out_port", node_id)))
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
    ui.horizontal(|ui| {
        ui.label("Adapter:");
        egui::ComboBox::from_id_salt(egui::Id::new(("dmx_out_adapter", node_id)))
            .selected_text(match adapter { DmxAdapter::UsbPro => "Enttec USB Pro", DmxAdapter::OpenDmx => "Enttec Open DMX" })
            .width(150.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(adapter, DmxAdapter::UsbPro, "Enttec USB Pro")
                    .on_hover_text("Adapter handles DMX timing — most reliable on macOS.");
                ui.selectable_value(adapter, DmxAdapter::OpenDmx, "Enttec Open DMX")
                    .on_hover_text("Raw FTDI BREAK + 250 kbaud. Watch the FTDI latency-timer setting on macOS.");
            });
    });
    ui.horizontal(|ui| {
        ui.label("Rate:");
        let mut hz = *frame_rate_hz as u32;
        if ui.add(egui::DragValue::new(&mut hz).range(10..=44).suffix(" Hz")).changed() {
            *frame_rate_hz = hz.clamp(10, 44) as u8;
        }
    });

    // ── Channel range ──────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Channels:");
        if ui.small_button("−").clicked() && *channel_count > 1 {
            pending_disconnects.push((node_id, *channel_count - 1));
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

    // ── Inline input ports ─────────────────────────────────────
    ui.separator();
    for i in 0..*channel_count {
        ui.horizontal(|ui| {
            super::inline_port_circle(
                ui, node_id, i, true, connections,
                port_positions, dragging_from, pending_disconnects, PortKind::Normalized,
            );
            let ch = (*start_at as usize) + i;
            let v = Graph::static_input_value(connections, values, node_id, i).as_float();
            let byte = (v.clamp(0.0, 1.0) * 255.0) as u8;
            ui.label(format!("Ch {}: {}", ch, byte));
        });
    }

    // ── Start / Stop ───────────────────────────────────────────
    ui.separator();
    ui.horizontal(|ui| {
        if *active {
            if ui.button("\u{23F9} Stop").clicked() {
                *active = false;
                dmx_actions.push(DmxAction::Close { node_id });
            }
        } else {
            let enable = !port_name.is_empty();
            if ui.add_enabled(enable, egui::Button::new("\u{25B6} Start")).clicked() {
                *active = true;
                dmx_actions.push(DmxAction::OpenOutput {
                    node_id,
                    port_name: port_name.clone(),
                    adapter: *adapter,
                    frame_rate_hz: *frame_rate_hz,
                });
            }
        }
    });

    // ── Push current frame to manager ──────────────────────────
    if !*active { return; }
    let start_idx = (*start_at as usize).saturating_sub(1).min(511);
    let mut data = vec![0u8; 512];
    for i in 0..*channel_count {
        let ch_idx = start_idx + i;
        if ch_idx >= 512 { break; }
        let v = Graph::static_input_value(connections, values, node_id, i).as_float();
        data[ch_idx] = (v.clamp(0.0, 1.0) * 255.0) as u8;
    }
    dmx_actions.push(DmxAction::SetUniverse { node_id, data });
}
