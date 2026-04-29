//! Art-Net Out node — sends DMX channel values over UDP.
//!
//! One node per (destination, universe). Inputs are normalized
//! 0..1 floats; we scale to 0..255 and pack into the universe at
//! `start_at` (1-indexed). Other channels are sent as 0.

use crate::artnet::ArtNetAction;
use crate::graph::{Connection, Graph, NodeId, PortKind, PortValue};
use eframe::egui;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &mut egui::Ui,
    dest_host: &mut String,
    dest_port: &mut u16,
    universe: &mut u16,
    channel_count: &mut usize,
    start_at: &mut u16,
    sequence_enabled: &mut bool,
    active: &mut bool,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    artnet_actions: &mut Vec<ArtNetAction>,
) {
    // ── Status row ─────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{1F4A1}").size(16.0)); // 💡
        let status = if *active { "Sending" } else { "Idle" };
        let color = if *active {
            egui::Color32::from_rgb(120, 220, 140)
        } else {
            egui::Color32::from_rgb(130, 130, 130)
        };
        ui.colored_label(color, egui::RichText::new(status).strong());
    });

    // ── Destination ────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("To:");
        ui.add(egui::TextEdit::singleline(dest_host).desired_width(110.0));
        ui.label(":");
        let mut port_str = dest_port.to_string();
        if ui.add(egui::TextEdit::singleline(&mut port_str).desired_width(50.0)).changed() {
            if let Ok(p) = port_str.parse::<u16>() { *dest_port = p; }
        }
    });
    // Universe (15-bit, displayed as 0–32767)
    ui.horizontal(|ui| {
        ui.label("Univ:");
        let mut u32v = *universe as u32;
        if ui.add(egui::DragValue::new(&mut u32v).range(0..=32767)).changed() {
            *universe = u32v.min(32767) as u16;
        }
        ui.checkbox(sequence_enabled, "Seq");
    });

    // ── Channel range ──────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Channels:");
        if ui.small_button("−").clicked() && *channel_count > 1 {
            // Disconnect the trailing input port we're about to drop.
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

    // ── Inline input ports + per-channel value display ─────────
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
            if ui.button("\u{23F9} Stop").clicked() { *active = false; }
        } else {
            if ui.button("\u{25B6} Start").clicked() { *active = true; }
        }
    });

    // ── Push action when active ────────────────────────────────
    if !*active { return; }

    let dest = match format!("{}:{}", dest_host, dest_port).parse::<std::net::SocketAddr>() {
        Ok(a) => a,
        Err(_) => return, // bad host/port string — silently skip until user fixes it
    };

    // Build the universe slice. Channels outside our range stay at 0.
    let start_idx = (*start_at as usize).saturating_sub(1).min(511);
    let end_idx = (start_idx + *channel_count).min(512);
    let length = end_idx; // we send from byte 0 up to the highest channel we touched
    let mut data = vec![0u8; length.max(2)]; // Art-Net length must be even & ≥ 2; pad below if needed
    for i in 0..(end_idx - start_idx) {
        let v = Graph::static_input_value(connections, values, node_id, i).as_float();
        data[start_idx + i] = (v.clamp(0.0, 1.0) * 255.0) as u8;
    }
    if data.len() % 2 == 1 { data.push(0); }

    artnet_actions.push(ArtNetAction::Send {
        node_id,
        dest,
        universe: *universe,
        sequence_enabled: *sequence_enabled,
        data,
    });
}
