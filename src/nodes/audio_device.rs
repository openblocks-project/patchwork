use crate::audio::AudioManager;
use crate::graph::*;
use eframe::egui;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    audio: &mut AudioManager,
) {
    let (selected_output, selected_input, master_volume, enabled) = match node_type {
        NodeType::AudioDevice {
            selected_output,
            selected_input,
            master_volume,
            enabled,
        } => (selected_output, selected_input, master_volume, enabled),
        _ => return,
    };

    // Fixed minimum width to prevent jumping
    ui.set_min_width(200.0);

    // ── DSP Enable/Disable — central switch ──────────────────────────
    {
        let (label, color) = if *enabled {
            ("⏻ DSP On", egui::Color32::from_rgb(80, 220, 80))
        } else {
            ("⏻ DSP Off", egui::Color32::from_rgb(180, 80, 80))
        };
        let btn = egui::Button::new(egui::RichText::new(label).strong().size(14.0).color(color))
            .min_size(egui::vec2(ui.available_width(), 28.0));
        if ui.add(btn).clicked() {
            *enabled = !*enabled;
            if *enabled {
                let dev = if selected_output.is_empty() {
                    None
                } else {
                    Some(selected_output.as_str())
                };
                if let Err(e) = audio.start_output(dev) {
                    crate::system_log::error(format!("Audio start failed: {}", e));
                    *enabled = false;
                }
            } else {
                audio.stop_output();
            }
        }
    }

    ui.separator();

    // ── Output device selector (always visible) ──────────────────────
    ui.label(egui::RichText::new("Output").small().strong());
    let mut device_changed = false;
    egui::ComboBox::from_id_salt(egui::Id::new(("audio_out", node_id)))
        .selected_text(if selected_output.is_empty() {
            "Default"
        } else {
            selected_output.as_str()
        })
        .width(170.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(selected_output.is_empty(), "Default")
                .clicked()
            {
                if !selected_output.is_empty() {
                    device_changed = true;
                }
                selected_output.clear();
            }
            for d in &audio.cached_output_devices {
                if ui.selectable_label(selected_output == d, d).clicked() {
                    if selected_output != d {
                        device_changed = true;
                    }
                    *selected_output = d.clone();
                }
            }
        });

    // Auto-restart stream when device selection changes
    if device_changed && *enabled && audio.is_running() {
        audio.stop_output();
        let dev = if selected_output.is_empty() {
            None
        } else {
            Some(selected_output.as_str())
        };
        if let Err(e) = audio.start_output(dev) {
            crate::system_log::error(format!("Device switch failed: {}", e));
        }
    }

    ui.separator();

    // ── Master volume (always visible) ───────────────────────────────
    ui.horizontal(|ui| {
        ui.label("Master:");
        ui.add(egui::Slider::new(master_volume, 0.0..=1.0).show_value(false));
        // Write directly to shared atomic — engine reads it every callback, no command needed
        audio.master_volume.store(*master_volume);
        ui.label(
            egui::RichText::new(format!("{:.0}%", *master_volume * 100.0))
                .small()
                .monospace(),
        );
    });

    ui.separator();

    // ── Input device (always visible) ────────────────────────────────
    ui.label(egui::RichText::new("Input").small().strong());
    egui::ComboBox::from_id_salt(egui::Id::new(("audio_in", node_id)))
        .selected_text(if selected_input.is_empty() {
            "None"
        } else {
            selected_input.as_str()
        })
        .width(170.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(selected_input.is_empty(), "None")
                .clicked()
            {
                selected_input.clear();
            }
            for d in &audio.cached_input_devices {
                if ui.selectable_label(selected_input == d, d).clicked() {
                    *selected_input = d.clone();
                }
            }
        });

    ui.separator();

    // ── Performance metrics ──────────────────────────────────────────
    {
        let sr = audio.engine_sample_rate as u32;
        let sr_text = if sr >= 1000 {
            format!("{}.{}kHz", sr / 1000, (sr % 1000) / 100)
        } else {
            format!("{}Hz", sr)
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("SR:").small());
            ui.label(
                egui::RichText::new(sr_text)
                    .small()
                    .color(egui::Color32::from_rgb(120, 180, 255)),
            );
        });
    }

    // Dropout counter — read from AtomicU32 (outside the mutex, always works)
    let dropouts = audio
        .dropout_count
        .load(std::sync::atomic::Ordering::Relaxed);
    if dropouts > 0 {
        ui.label(
            egui::RichText::new(format!("⚠ {} dropouts", dropouts))
                .small()
                .color(egui::Color32::from_rgb(255, 120, 60)),
        );
    }

    // Request repaint to keep metrics updating
    if *enabled {
        ui.ctx().request_repaint();
    }
}
