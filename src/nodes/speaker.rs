use crate::graph::*;
use crate::audio::AudioManager;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    active: &mut bool,
    volume: &mut f32,
    pan: &mut f32,
    channel_offset: &mut usize,
    device: &mut String,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    audio: &AudioManager,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let has_l = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    let has_r = connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
    let has_audio = has_l || has_r;
    let vol_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);
    let pan_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 3);

    // Read volume from port if wired
    if vol_wired {
        *volume = Graph::static_input_value(connections, values, node_id, 2).as_float().clamp(0.0, 1.0);
    }
    // Read pan from port if wired
    if pan_wired {
        *pan = Graph::static_input_value(connections, values, node_id, 3).as_float().clamp(-1.0, 1.0);
    }

    // Determine effective device and channel count
    let max_channels = if device.is_empty() {
        audio.output_channel_count
    } else {
        audio.device_channel_counts.get(device.as_str()).copied().unwrap_or(2)
    };

    // Clamp channel offset to valid range for this device
    if *channel_offset + 2 > max_channels {
        *channel_offset = if max_channels >= 2 { max_channels - 2 } else { 0 };
    }

    // ── Large speaker icon as main identity ───────────────────────────
    let icon = if *active && has_audio {
        crate::icons::SPEAKER_HIGH
    } else {
        crate::icons::SPEAKER_X
    };

    let icon_color = if *active && has_audio {
        egui::Color32::from_rgb(80, 220, 100)
    } else if *active {
        egui::Color32::from_rgb(160, 160, 160)
    } else {
        egui::Color32::from_rgb(80, 80, 90)
    };

    // ── Title ──────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(icon).size(28.0).color(icon_color));
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new("Speaker").size(14.0).strong().color(egui::Color32::from_rgb(80, 200, 80)));
            let mode = if has_l && has_r { "Stereo" } else if has_l { "Mono" } else { "No input" };
            let status = if !has_audio {
                (mode, egui::Color32::from_rgb(100, 100, 110))
            } else if *active {
                (mode, egui::Color32::from_rgb(80, 220, 100))
            } else {
                ("Muted", egui::Color32::from_rgb(200, 100, 80))
            };
            ui.label(egui::RichText::new(status.0).small().color(status.1));
        });
        let remaining = ui.available_width() - 30.0;
        if remaining > 0.0 { ui.add_space(remaining); }
        ui.toggle_value(active, if *active { "On" } else { "Off" });
    });

    ui.add_space(4.0);

    // ── Audio input ports ────────────────────────────────────────────
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Audio);
        ui.label(egui::RichText::new("L / Mono").small());
    });
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Audio);
        ui.label(egui::RichText::new("R").small().color(if has_r { egui::Color32::from_rgb(80, 200, 120) } else { egui::Color32::GRAY }));
    });

    ui.add_space(2.0);

    // ── Volume control ───────────────────────────────────────────────
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 2, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);
        ui.label(egui::RichText::new("Vol").small());
        if vol_wired {
            ui.label(egui::RichText::new(format!("{:.0}%", *volume * 100.0)).small().color(egui::Color32::from_rgb(80, 170, 255)));
        } else {
            ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false));
            ui.label(egui::RichText::new(format!("{:.0}%", *volume * 100.0)).small());
        }
    });

    // ── Pan control ──────────────────────────────────────────────────
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 3, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
        ui.label(egui::RichText::new("Pan").small());
        if pan_wired {
            let pan_label = pan_display(*pan);
            ui.label(egui::RichText::new(pan_label).small().color(egui::Color32::from_rgb(80, 170, 255)));
        } else {
            ui.add(egui::Slider::new(pan, -1.0..=1.0).show_value(false));
            ui.label(egui::RichText::new(pan_display(*pan)).small());
        }
    });

    ui.add_space(2.0);
    ui.separator();

    // ── Output device selector ──────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Device").small().strong());
    });

    let display_device = if device.is_empty() {
        shorten_device_name(&audio.output_device_name)
    } else {
        shorten_device_name(device.as_str())
    };

    let is_non_primary = !device.is_empty() && device.as_str() != audio.output_device_name;

    egui::ComboBox::from_id_salt(egui::Id::new(("speaker_dev", node_id)))
        .selected_text(egui::RichText::new(&display_device).small())
        .width(ui.available_width() - 4.0)
        .show_ui(ui, |ui| {
            // Primary device option
            let primary_label = format!("{} (Primary)", shorten_device_name(&audio.output_device_name));
            if ui.selectable_label(device.is_empty(), egui::RichText::new(&primary_label).small()).clicked() {
                device.clear();
            }
            // Other available devices
            for d in &audio.cached_output_devices {
                if d == &audio.output_device_name { continue; }
                let label = shorten_device_name(d);
                if ui.selectable_label(device.as_str() == d, egui::RichText::new(&label).small()).clicked() {
                    *device = d.clone();
                }
            }
        });

    // Show warning for non-primary device (future feature)
    if is_non_primary {
        ui.label(egui::RichText::new("⚠ Secondary device").small().color(egui::Color32::from_rgb(255, 180, 60)));
    }

    // ── Channel pair selector (filtered by device) ───────────────────
    let num_pairs = (max_channels / 2).max(1);
    if num_pairs > 1 {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Output").small());
            let ch_label = format!("Ch {}-{}", *channel_offset + 1, *channel_offset + 2);
            egui::ComboBox::from_id_salt(egui::Id::new(("speaker_ch", node_id)))
                .selected_text(egui::RichText::new(&ch_label).small())
                .width(70.0)
                .show_ui(ui, |ui| {
                    for pair in 0..num_pairs {
                        let offset = pair * 2;
                        let label = format!("Ch {}-{}", offset + 1, offset + 2);
                        ui.selectable_value(channel_offset, offset, egui::RichText::new(label).small());
                    }
                });
        });
    } else {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Output").small());
            ui.label(egui::RichText::new("Ch 1-2").small().color(egui::Color32::GRAY));
        });
    }

    ui.add_space(4.0);

    // Update engine params
    audio.engine_write_param(node_id, 0, *volume);
    audio.engine_write_param(node_id, 1, if *active { 1.0 } else { 0.0 });
    audio.engine_write_param(node_id, 2, *pan);
    audio.engine_write_param(node_id, 3, *channel_offset as f32);
}

fn pan_display(pan: f32) -> String {
    if pan < -0.01 {
        format!("L{:.0}", pan.abs() * 100.0)
    } else if pan > 0.01 {
        format!("R{:.0}", pan * 100.0)
    } else {
        "C".to_string()
    }
}

/// Shorten common macOS device names for display
fn shorten_device_name(name: &str) -> String {
    if name.is_empty() { return "Default".to_string(); }
    // Common macOS names are verbose — shorten them
    if name.contains("MacBook") && name.contains("Speaker") {
        return "MacBook Speakers".to_string();
    }
    if name.contains("MacBook") && name.contains("Microphone") {
        return "MacBook Mic".to_string();
    }
    if name.contains("External Headphones") {
        return "Headphones".to_string();
    }
    // Keep short names as-is, truncate very long ones
    if name.chars().count() > 24 {
        let head: String = name.chars().take(22).collect();
        format!("{}…", head)
    } else {
        name.to_string()
    }
}
