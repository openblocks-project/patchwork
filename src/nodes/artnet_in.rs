//! Art-Net In node — listens for ArtDmx packets and surfaces channel
//! values as normalized 0..1 outputs.
//!
//! The listener thread is owned by `crate::artnet::ArtNetManager`; this
//! file is just the per-node UI + the action queue interface.

use crate::artnet::ArtNetAction;
use crate::graph::{NodeId, PortKind};
use eframe::egui;
use std::collections::HashMap;

#[allow(clippy::too_many_arguments)]
pub fn render(
    ui: &mut egui::Ui,
    listen_port: &mut u16,
    universe: &mut u16,
    universe_filter: &mut bool,
    channel_count: &mut usize,
    start_at: &mut u16,
    last_values: &[u8],
    listening: &mut bool,
    node_id: NodeId,
    connections: &[crate::graph::Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    artnet_actions: &mut Vec<ArtNetAction>,
) {
    // ── Status row ─────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{1F4E1}").size(16.0)); // 📡
        let status = if *listening { "Listening" } else { "Stopped" };
        let color = if *listening {
            egui::Color32::from_rgb(120, 220, 140)
        } else {
            egui::Color32::from_rgb(130, 130, 130)
        };
        ui.colored_label(color, egui::RichText::new(status).strong());
    });

    // ── Listen port ────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Port:");
        let mut p = *listen_port as u32;
        if ui.add(egui::DragValue::new(&mut p).range(1024..=65535)).changed() {
            *listen_port = p.clamp(1024, 65535) as u16;
        }
    });
    // Universe filter (drop packets for other universes)
    ui.horizontal(|ui| {
        ui.checkbox(universe_filter, "Filter universe");
        let mut u32v = *universe as u32;
        if ui.add_enabled(*universe_filter, egui::DragValue::new(&mut u32v).range(0..=32767)).changed() {
            *universe = u32v.min(32767) as u16;
        }
    });

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
                artnet_actions.push(ArtNetAction::StopListening { node_id });
            }
        } else {
            if ui.button("\u{25B6} Start").clicked() {
                *listening = true;
                artnet_actions.push(ArtNetAction::StartListening {
                    node_id,
                    port: *listen_port,
                });
            }
        }
    });
}
