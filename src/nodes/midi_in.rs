use crate::midi::MidiAction;
use crate::graph::NodeId;
use eframe::egui;
use crate::nodes::ScrollAreaExt;

const MAX_LOG_LINES: usize = 200;

/// MIDI In node with live message logger.
/// Outputs: Channel, Note, Velocity as floats.
pub fn render(
    ui: &mut egui::Ui,
    port_name: &mut String,
    channel: &mut u8,
    note: &mut u8,
    velocity: &mut u8,
    log: &mut Vec<String>,
    node_id: NodeId,
    available_ports: &[String],
    is_connected: bool,
    midi_actions: &mut Vec<MidiAction>,
) {
    // ── Device selector ─────────────────────────────────────────────
    ui.horizontal(|ui| {
        let (status, color) = if is_connected {
            ("\u{25cf}", egui::Color32::from_rgb(80, 220, 80))
        } else {
            ("\u{25cb}", egui::Color32::GRAY)
        };
        ui.label(egui::RichText::new(status).color(color));

        egui::ComboBox::from_id_salt(("midi_in_port", node_id))
            .selected_text(if port_name.is_empty() { "Select device..." } else { port_name.as_str() })
            .width(140.0)
            .show_ui(ui, |ui| {
                for p in available_ports {
                    if ui.selectable_value(port_name, p.clone(), p.as_str()).clicked() {
                        midi_actions.push(MidiAction::ConnectInput { node_id, port_name: p.clone() });
                    }
                }
            });

        // Refresh — re-scan MIDI ports (e.g. after enabling IAC Driver
        // in Audio MIDI Setup) without restarting Patchwork.
        if ui.small_button("↻").on_hover_text("Refresh MIDI port list").clicked() {
            midi_actions.push(MidiAction::RescanPorts);
        }
    });

    ui.horizontal(|ui| {
        if !port_name.is_empty() && !is_connected {
            if ui.button("Connect").clicked() {
                midi_actions.push(MidiAction::ConnectInput { node_id, port_name: port_name.clone() });
            }
        }
        if is_connected && ui.button("Disconnect").clicked() {
            midi_actions.push(MidiAction::DisconnectInput { node_id });
        }
        if !log.is_empty() && ui.button("Clear log").clicked() {
            log.clear();
        }
    });

    ui.separator();

    // ── Last received values ────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(format!("Ch: {}", channel));
        ui.label(format!("Note: {}", note));
        ui.label(format!("Vel: {}", velocity));
    });

    // ── Live log ────────────────────────────────────────────────────
    if !log.is_empty() {
        ui.separator();
        ui.label(
            egui::RichText::new(format!("Log ({} msgs)", log.len()))
                .small()
                .color(egui::Color32::GRAY),
        );
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .stick_to_bottom(true)
            .show_pannable(ui, |ui| {
                for line in log.iter() {
                    ui.label(egui::RichText::new(line.as_str()).monospace().small());
                }
            });
    } else if is_connected {
        ui.label(
            egui::RichText::new("Listening...")
                .small()
                .color(egui::Color32::GRAY),
        );
    }

    // Trim log
    while log.len() > MAX_LOG_LINES {
        log.remove(0);
    }
}

/// Format a raw MIDI message into a human-readable log line.
pub fn format_midi_message(msg: &[u8]) -> String {
    if msg.is_empty() { return "empty".into(); }
    let status = msg[0];
    let kind = status & 0xF0;
    let ch = status & 0x0F;
    match kind {
        0x80 => format!("NoteOff  ch:{:2} note:{:3} vel:{:3}", ch, msg.get(1).unwrap_or(&0), msg.get(2).unwrap_or(&0)),
        0x90 => {
            let vel = msg.get(2).unwrap_or(&0);
            if *vel == 0 {
                format!("NoteOff  ch:{:2} note:{:3}", ch, msg.get(1).unwrap_or(&0))
            } else {
                format!("NoteOn   ch:{:2} note:{:3} vel:{:3}", ch, msg.get(1).unwrap_or(&0), vel)
            }
        }
        0xA0 => format!("Aftertouch ch:{:2} note:{:3} val:{:3}", ch, msg.get(1).unwrap_or(&0), msg.get(2).unwrap_or(&0)),
        0xB0 => format!("CC       ch:{:2} cc:{:3}  val:{:3}", ch, msg.get(1).unwrap_or(&0), msg.get(2).unwrap_or(&0)),
        0xC0 => format!("ProgChg  ch:{:2} prog:{:3}", ch, msg.get(1).unwrap_or(&0)),
        0xD0 => format!("ChanPres ch:{:2} val:{:3}", ch, msg.get(1).unwrap_or(&0)),
        0xE0 => format!("PitchBnd ch:{:2} {:3} {:3}", ch, msg.get(1).unwrap_or(&0), msg.get(2).unwrap_or(&0)),
        0xF0..=0xFF => format!("System   {:02X} [{}b]", status, msg.len()),
        _ => format!("Raw      {:02X?}", msg),
    }
}
