use crate::audio::AudioManager;
use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    audio: &mut AudioManager,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let (selected_device, gain, active, agc_enabled) = match node_type {
        NodeType::AudioInput { selected_device, gain, active, agc_enabled } =>
            (selected_device, gain, active, agc_enabled),
        _ => return,
    };

    // ── Mic icon + status ─────────────────────────────────────────────
    let status_text = if *active { "Listening" } else { "Stopped" };
    let status_color = if *active {
        egui::Color32::from_rgb(255, 80, 80)
    } else {
        egui::Color32::from_rgb(130, 130, 130)
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("\u{1F3A4}").size(18.0)); // 🎤
        ui.colored_label(status_color, egui::RichText::new(status_text).strong());
        if *active {
            ui.colored_label(egui::Color32::from_rgb(255, 60, 60), "●");
        }
    });

    // ── Device selector ───────────────────────────────────────────────
    let devices = &audio.cached_input_devices;
    ui.horizontal(|ui| {
        ui.label("Device:");
        let display = if selected_device.is_empty() {
            "Default"
        } else {
            selected_device.as_str()
        };
        let truncated: String = if display.chars().count() > 22 {
            display.chars().take(22).collect()
        } else {
            display.to_string()
        };
        egui::ComboBox::from_id_salt(egui::Id::new(("audio_input_dev", node_id)))
            .selected_text(truncated)
            .width(140.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(selected_device.is_empty(), "Default").clicked() {
                    *selected_device = String::new();
                }
                for dev in devices {
                    if ui.selectable_label(*selected_device == *dev, dev).clicked() {
                        *selected_device = dev.clone();
                    }
                }
            });
    });

    // Auto-start input if active but stream doesn't exist
    // (happens after project load or DSP restart)
    if *active && !audio.input_buffers.contains_key(&node_id) {
        let dev = if selected_device.is_empty() { None } else { Some(selected_device.as_str()) };
        if let Err(e) = audio.start_input(node_id, dev) {
            crate::system_log::warn(format!("Mic auto-start failed: {}", e));
            *active = false;
        }
    }

    // ── Start / Stop ──────────────────────────────────────────────────
    ui.horizontal(|ui| {
        if *active {
            if ui.button("\u{23F9} Stop").clicked() {
                *active = false;
                audio.stop_input(node_id);
            }
        } else {
            if ui.button("\u{25B6} Start").clicked() {
                let dev = if selected_device.is_empty() { None } else { Some(selected_device.as_str()) };
                match audio.start_input(node_id, dev) {
                    Ok(()) => *active = true,
                    Err(e) => crate::system_log::error(format!("Mic start failed: {}", e)),
                }
            }
        }
    });

    // AGC toggle — opt-in. Default off so mic → effects / sampler paths
    // aren't silently shaped by a compressor the user didn't ask for.
    ui.horizontal(|ui| {
        ui.checkbox(agc_enabled, egui::RichText::new("Auto Level (experimental)").small());
    });

    // ── Gain slider ───────────────────────────────────────────────────
    // Familiar 0..2× (0–200%) range. This matches the pre-branch UX —
    // unity gain sits at the slider's midpoint, so users don't have to
    // hunt for "normal" on a log-dB scale.
    let gain_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    crate::nodes::inline_port_circle(
        ui, node_id, 0, true, connections,
        port_positions, dragging_from, pending_disconnects, PortKind::Normalized,
    );
    if gain_wired {
        let v = Graph::static_input_value(connections, values, node_id, 0).as_float();
        *gain = v.clamp(0.0, 2.0);
        ui.horizontal(|ui| {
            ui.label("Gain:");
            ui.label(format!("{:.0}%", *gain * 100.0));
        });
    } else {
        ui.horizontal(|ui| {
            ui.label("Gain:");
            ui.add(egui::Slider::new(gain, 0.0..=2.0).show_value(true));
        });
    }

    // ── Minimal input level indicator ─────────────────────────────────
    // A thin bar that fills proportional to the recent peak amplitude of
    // the mic signal. The peak is read from the live input ring buffer
    // (~21 ms window) — no explicit decay needed; the bar naturally falls
    // back as the window slides past a transient.
    {
        let (level, bar_color) = if *active {
            let raw = audio.input_buffers.get(&node_id)
                .map(|buf| buf.peek_level(1024))
                .unwrap_or(0.0);
            // Post-gain so the meter reflects what downstream nodes roughly
            // receive. Note: meter can't see the AGC multiplier (which is DSP
            // state); with AGC on, actual downstream level is typically
            // higher than this meter shows. sqrt gives a perceptual curve.
            let after_gain = (raw * *gain).clamp(0.0, 1.0).sqrt();
            (after_gain, egui::Color32::from_rgb(255, 100, 100))
        } else {
            (0.0, egui::Color32::from_rgb(70, 70, 80))
        };
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().max(60.0), 4.0),
            egui::Sense::hover(),
        );
        let p = ui.painter();
        p.rect_filled(rect, 1.5, egui::Color32::from_rgb(22, 22, 28));
        if level > 0.005 {
            let fill_w = rect.width() * level;
            let fill = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
            p.rect_filled(fill, 1.5, bar_color);
        }
        if *active {
            // Keep the UI refreshing while the mic is live so the meter
            // animates in real time.
            ui.ctx().request_repaint();
        }
    }

    // ── Output port: Audio ────────────────────────────────────────────
    ui.separator();
    crate::nodes::audio_port_row(ui, "Audio", node_id, 0, false, port_positions, dragging_from, connections, pending_disconnects, PortKind::Audio);

    // Write gain + AGC toggle to engine (lock-free atomics)
    if *active {
        audio.engine_write_param(node_id, 0, *gain);
        audio.engine_write_param(node_id, 1, if *agc_enabled { 1.0 } else { 0.0 });
    }
}
