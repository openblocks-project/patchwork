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
        egui::ComboBox::from_id_salt(egui::Id::new(("audio_input_dev", node_id)))
            .selected_text(if display.len() > 22 { &display[..22] } else { display })
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

    // ── Auto-Level (AGC) toggle ───────────────────────────────────────
    // Automatic gain control. ON by default — built-in mics typically run
    // at -40 dBFS and need massive boost before the spectrogram encoding
    // gets usable signal. AGC tracks RMS and gently pulls the level up.
    ui.horizontal(|ui| {
        ui.checkbox(agc_enabled, "Auto Level");
        if *agc_enabled {
            ui.label(egui::RichText::new("adaptive").small()
                .color(egui::Color32::from_rgb(120, 200, 255)));
        }
    });

    // ── Gain slider ───────────────────────────────────────────────────
    // Range is 0..20× (+26 dB) so built-in mics can reach strong levels
    // without AGC. Display as dB because that's how users think about mic
    // levels. Manual gain applies AFTER AGC (if on) — acts as a trim.
    let gain_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    crate::nodes::inline_port_circle(
        ui, node_id, 0, true, connections,
        port_positions, dragging_from, pending_disconnects, PortKind::Normalized,
    );
    if gain_wired {
        let v = Graph::static_input_value(connections, values, node_id, 0).as_float();
        *gain = v.clamp(0.0, 20.0);
        let db = 20.0 * gain.max(1e-4).log10();
        ui.horizontal(|ui| {
            ui.label("Gain:");
            ui.label(format!("{:+.1} dB", db));
        });
    } else {
        ui.horizontal(|ui| {
            ui.label("Gain:");
            let db_val = 20.0 * gain.max(1e-4).log10();
            ui.add(egui::Slider::new(gain, 0.0..=20.0)
                .logarithmic(true)
                .show_value(false)
                .clamping(egui::SliderClamping::Always));
            ui.label(format!("{:+.1} dB", db_val));
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
