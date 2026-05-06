use crate::graph::*;
use crate::midi::MidiAction;
use eframe::egui;
use std::collections::HashMap;

/// MIDI Out node: select a device, choose Note/CC mode, send MIDI.
/// Inputs: Channel, Note/CC#, Velocity/Value (all float, clamped to MIDI range).
/// Input ports have inline DragValue — editable when unconnected, display-only when wired.
pub fn render(
    ui: &mut egui::Ui,
    port_name: &mut String,
    mode: &mut MidiMode,
    channel: &mut u8,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    available_ports: &[String],
    is_connected: bool,
    midi_actions: &mut Vec<MidiAction>,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    // Manual values for Note/CC# and Velocity/Value when not connected
    manual_d1: &mut u8,
    manual_d2: &mut u8,
) {
    // ── Device selector (top) ───────────────────────────────────────
    ui.horizontal(|ui| {
        let status = if is_connected { "●" } else { "○" };
        let status_color = if is_connected {
            egui::Color32::from_rgb(80, 220, 80)
        } else {
            egui::Color32::GRAY
        };
        ui.label(egui::RichText::new(status).color(status_color));

        egui::ComboBox::from_id_salt(("midi_out_port", node_id))
            .selected_text(if port_name.is_empty() { "Select device..." } else { port_name.as_str() })
            .width(120.0)
            .show_ui(ui, |ui| {
                for p in available_ports {
                    if ui.selectable_value(port_name, p.clone(), p.as_str()).clicked() {
                        midi_actions.push(MidiAction::ConnectOutput { node_id, port_name: p.clone() });
                    }
                }
            });

        if !port_name.is_empty() && !is_connected {
            if ui.small_button("▶").on_hover_text("Connect").clicked() {
                midi_actions.push(MidiAction::ConnectOutput { node_id, port_name: port_name.clone() });
            }
        }
        if is_connected {
            if ui.small_button("■").on_hover_text("Disconnect").clicked() {
                midi_actions.push(MidiAction::DisconnectOutput { node_id });
            }
        }

        // Refresh — re-scan MIDI ports (e.g. after enabling IAC Driver
        // in Audio MIDI Setup) without restarting Patchwork.
        if ui.small_button("↻").on_hover_text("Refresh MIDI port list").clicked() {
            midi_actions.push(MidiAction::RescanPorts);
        }
    });

    // ── Mode toggle ─────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Mode:");
        if ui.selectable_label(*mode == MidiMode::Note, "Note").clicked() { *mode = MidiMode::Note; }
        if ui.selectable_label(*mode == MidiMode::CC, "CC").clicked() { *mode = MidiMode::CC; }
    });

    ui.separator();

    // ── Inline input ports with DragValue ────────────────────────────
    let is_note = *mode == MidiMode::Note;
    let input_labels: [&str; 3] = ["Channel", if is_note { "Note" } else { "CC#" }, if is_note { "Velocity" } else { "Value" }];
    let input_ranges: [(i32, i32); 3] = [(0, 15), (0, 127), (0, 127)];
    let mut manual_vals: [u8; 3] = [*channel, *manual_d1, *manual_d2];

    // ── Ports ────────────────────────────────────────────────────────
    for i in 0..3 {
        let label = input_labels[i];
        let (range_min, range_max) = input_ranges[i];
        let is_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == i);

        ui.horizontal(|ui| {
            super::inline_port_circle(ui, node_id, i, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);

            // Label
            ui.label(egui::RichText::new(label).small());

            if is_wired {
                let v = Graph::static_input_value(connections, values, node_id, i).as_float() as u8;
                ui.label(egui::RichText::new(format!("{}", v)).strong().monospace());
                ui.label(egui::RichText::new("⟵").small().color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                let mut val = manual_vals[i] as i32;
                ui.add(egui::DragValue::new(&mut val).range(range_min..=range_max).speed(0.5));
                manual_vals[i] = val.clamp(range_min, range_max) as u8;
            }
        });
    }

    // Write back manual values
    *channel = manual_vals[0];
    *manual_d1 = manual_vals[1];
    *manual_d2 = manual_vals[2];

    // ── Build and send MIDI message ─────────────────────────────────
    // Resolve actual values: prefer connected input, fall back to manual
    let ch = if connections.iter().any(|c| c.to_node == node_id && c.to_port == 0) {
        Graph::static_input_value(connections, values, node_id, 0).as_float().clamp(0.0, 15.0) as u8
    } else {
        *channel
    };
    let d1 = if connections.iter().any(|c| c.to_node == node_id && c.to_port == 1) {
        Graph::static_input_value(connections, values, node_id, 1).as_float().clamp(0.0, 127.0) as u8
    } else {
        *manual_d1
    };
    let d2 = if connections.iter().any(|c| c.to_node == node_id && c.to_port == 2) {
        Graph::static_input_value(connections, values, node_id, 2).as_float().clamp(0.0, 127.0) as u8
    } else {
        *manual_d2
    };

    if is_connected {
        let status_byte = match mode {
            MidiMode::Note => 0x90 | (ch & 0x0F),
            MidiMode::CC => 0xB0 | (ch & 0x0F),
        };
        let msg = [status_byte, d1 & 0x7F, d2 & 0x7F];
        midi_actions.push(MidiAction::Send { node_id, message: msg });
    }
}
