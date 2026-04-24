use crate::audio::AudioManager;
use crate::audio::clap_host::ClapInstance;
use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
    let (plugin_path, plugin_name, param_names, param_ranges, param_flags, param_values, param_labels, is_instrument) = match node_type {
        NodeType::ClapPlugin { plugin_path, plugin_name, param_names, param_ranges, param_flags, param_values, param_labels, is_instrument } =>
            (plugin_path, plugin_name, param_names, param_ranges, param_flags, param_values, param_labels, is_instrument),
        _ => return,
    };

    // ── Input ports (adapt to plugin type) ─────────────────────────
    if *is_instrument {
        // Instrument: Note, Velocity, Gate
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("Note").small());
        });
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);
            ui.label(egui::RichText::new("Vel").small());
        });
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 2, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Gate);
            ui.label(egui::RichText::new("Gate").small());
        });
    } else {
        // Effect: Audio In
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Audio);
            ui.label(egui::RichText::new("Audio In").small());
        });
    }

    // ── Empty state: file picker ─────────────────────────────────
    if plugin_path.is_empty() {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(200.0), 40.0), egui::Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, 6.0, egui::Color32::from_rgb(35, 35, 40));
        painter.text(rect.center(), egui::Align2::CENTER_CENTER,
            "Load .clap plugin",
            egui::FontId::proportional(12.0), egui::Color32::from_rgb(160, 80, 255));
        if resp.clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("CLAP Plugin", &["clap"])
                .pick_file()
            {
                *plugin_path = path.to_string_lossy().to_string();
            }
        }
        // Output port even when empty
        crate::nodes::audio_port_row(ui, "Audio", node_id, 0, false, port_positions, dragging_from, connections, pending_disconnects, PortKind::Audio);
        return;
    }

    // ── Load plugin if not yet in engine ──────────────────────────
    if !audio.has_processor(node_id) && !plugin_path.is_empty() {
        match ClapInstance::load(plugin_path, audio.engine_sample_rate as f64, 2048) {
            Ok(instance) => {
                // Cache plugin info for UI
                *plugin_name = instance.info.name.clone();
                *param_names = instance.info.params.iter().map(|p| p.name.clone()).collect();
                *param_ranges = instance.info.params.iter().map(|p| [p.min, p.max, p.default]).collect();
                *param_flags = instance.info.params.iter().map(|p| p.flags).collect();
                *param_labels = instance.info.params.iter().map(|p| p.value_labels.clone()).collect();
                *is_instrument = instance.info.plugin_type == crate::audio::clap_host::ClapPluginType::Instrument;
                // Initialize param values to defaults (normalized 0..1)
                *param_values = instance.info.params.iter().map(|p| {
                    if (p.max - p.min).abs() > f64::EPSILON {
                        ((p.default - p.min) / (p.max - p.min)) as f32
                    } else { 0.0 }
                }).collect();

                let param_count = instance.info.params.len();

                crate::system_log::log(format!("Plugin '{}' loaded with {} params", plugin_name, param_count));

                // Create GUI handle BEFORE moving instance to audio thread
                // Store in AudioManager (not egui temp) so we can close all GUIs on DSP stop
                if let Some(gui_handle) = instance.create_gui_handle() {
                    crate::system_log::log("  Plugin supports GUI");
                    let gui = Arc::new(Mutex::new(gui_handle));
                    audio.clap_gui_handles.insert(node_id, gui);
                }

                let processor = crate::audio::processors::clap::ClapProcessor::new(instance);
                // Use add_processor to properly register with engine
                let shared = audio.add_processor(node_id, Box::new(processor), param_count);
                // Write default values to the shared params
                for (i, &v) in param_values.iter().enumerate() {
                    if i < shared.len() { shared[i].store(v); }
                }
            }
            Err(e) => {
                crate::system_log::error(format!("Failed to load CLAP: {}", e));
                *plugin_path = String::new(); // reset so user can try again
            }
        }
    }

    // ── Plugin name + buttons ──────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(plugin_name.as_str()).strong().color(egui::Color32::from_rgb(160, 80, 255)));

        // Open UI button (if plugin supports GUI)
        if let Some(gui) = audio.clap_gui_handles.get(&node_id) {
            if let Ok(mut handle) = gui.try_lock() {
                let btn_label = if handle.is_open { "Close UI" } else { "Open UI" };
                let btn_color = if handle.is_open { egui::Color32::from_rgb(80, 200, 120) } else { egui::Color32::from_rgb(160, 80, 255) };
                if ui.add(egui::Button::new(egui::RichText::new(btn_label).small().color(btn_color))).clicked() {
                    if handle.is_open { handle.close(); } else { handle.open(); }
                }
            }
        }

        if ui.small_button("↻").on_hover_text("Reload plugin").clicked() {
            // Close GUI before reloading
            if let Some(gui) = audio.clap_gui_handles.remove(&node_id) {
                if let Ok(mut h) = gui.try_lock() { h.close(); }
            }
            audio.remove_processor(node_id);
        }
    });

    // ── Parameters (scrollable) ──────────────────────────────────
    if !param_names.is_empty() {
        ui.label(egui::RichText::new(format!("{} params", param_names.len())).small().color(egui::Color32::GRAY));

        let scroll_height = if param_names.len() > 8 { 180.0 } else { param_names.len() as f32 * 22.0 };
        egui::ScrollArea::vertical().max_height(scroll_height).show(ui, |ui| {
            for (i, name) in param_names.iter().enumerate() {
                // For instruments: ports 0-2 = Note/Vel/Gate, params start at 3
                // For effects: port 0 = Audio, params start at 1
                let param_port_offset = if *is_instrument { 3 } else { 1 };
                let port_idx = i + param_port_offset;
                let param_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == port_idx);

                // Read from wired input if connected
                if param_wired {
                    let v = Graph::static_input_value(connections, values, node_id, port_idx).as_float();
                    if i < param_values.len() { param_values[i] = v.clamp(0.0, 1.0); }
                } else {
                    // Read back from engine atomics (plugin GUI may have changed the value)
                    if let Some(shared) = audio.node_params.get(&node_id) {
                        if i < shared.len() {
                            let engine_val = shared[i].load();
                            if (engine_val - param_values[i]).abs() > 0.001 {
                                param_values[i] = engine_val;
                            }
                        }
                    }
                }

                ui.horizontal(|ui| {
                    crate::nodes::inline_port_circle(ui, node_id, port_idx, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);

                    // Truncate long names (UTF-8-safe)
                    let display_name: String = if name.chars().count() > 14 {
                        name.chars().take(14).collect()
                    } else {
                        name.clone()
                    };
                    ui.label(egui::RichText::new(display_name).small());

                    if i < param_values.len() {
                        let labels = param_labels.get(i).cloned().unwrap_or_default();
                        let range = param_ranges.get(i).copied().unwrap_or([0.0, 1.0, 0.5]);

                        if !param_wired {
                            if !labels.is_empty() {
                                // Enum/stepped with labels → ComboBox dropdown
                                let current_idx = (param_values[i] * (labels.len().max(1) - 1) as f32).round() as usize;
                                let current_idx = current_idx.min(labels.len().saturating_sub(1));
                                let current_label = labels.get(current_idx).cloned().unwrap_or_default();
                                let max_w = labels.iter().map(|l| l.len()).max().unwrap_or(5).min(14) as f32 * 7.0 + 20.0;
                                egui::ComboBox::from_id_salt(egui::Id::new(("clap_param", node_id, i)))
                                    .selected_text(&current_label)
                                    .width(max_w)
                                    .show_ui(ui, |ui| {
                                        for (idx, label) in labels.iter().enumerate() {
                                            if ui.selectable_label(idx == current_idx, label).clicked() {
                                                param_values[i] = idx as f32 / (labels.len().max(2) - 1) as f32;
                                            }
                                        }
                                    });
                            } else {
                                // Continuous: normalized drag
                                let actual = range[0] + param_values[i] as f64 * (range[1] - range[0]);
                                ui.add(egui::DragValue::new(&mut param_values[i])
                                    .speed(0.005)
                                    .range(0.0..=1.0)
                                    .custom_formatter(move |v, _| {
                                        let a = range[0] + v * (range[1] - range[0]);
                                        if (range[1] - range[0]).abs() > 100.0 {
                                            format!("{:.0}", a)
                                        } else {
                                            format!("{:.2}", a)
                                        }
                                    }));
                                let _ = actual;
                            }
                        } else {
                            // Wired: show current value
                            if !labels.is_empty() {
                                let idx = (param_values[i] * (labels.len().max(1) - 1) as f32).round() as usize;
                                let label = labels.get(idx.min(labels.len().saturating_sub(1))).cloned().unwrap_or_default();
                                ui.label(egui::RichText::new(label).small().color(egui::Color32::from_rgb(80, 170, 255)));
                            } else {
                                ui.label(egui::RichText::new(format!("{:.2}", param_values[i]))
                                    .small().monospace().color(egui::Color32::from_rgb(80, 170, 255)));
                            }
                        }
                    }
                });
            }
        });
    }

    // ── Write params to engine ────────────────────────────────────
    for (i, &val) in param_values.iter().enumerate() {
        audio.engine_write_param(node_id, i, val);
    }

    // For instruments: write virtual note/vel/gate params from input ports
    if *is_instrument {
        let note_val = if connections.iter().any(|c| c.to_node == node_id && c.to_port == 0) {
            Graph::static_input_value(connections, values, node_id, 0).as_float()
        } else { 60.0 };
        let vel_val = if connections.iter().any(|c| c.to_node == node_id && c.to_port == 1) {
            Graph::static_input_value(connections, values, node_id, 1).as_float()
        } else { 0.8 };
        let gate_val = if connections.iter().any(|c| c.to_node == node_id && c.to_port == 2) {
            Graph::static_input_value(connections, values, node_id, 2).as_float()
        } else { 0.0 };

        let real_param_count = param_values.len();
        audio.engine_write_param(node_id, real_param_count, note_val);      // virtual param: Note
        audio.engine_write_param(node_id, real_param_count + 1, vel_val);   // virtual param: Velocity
        audio.engine_write_param(node_id, real_param_count + 2, gate_val);  // virtual param: Gate
    }

    // ── Audio output port ────────────────────────────────────────
    crate::nodes::audio_port_row(ui, "Audio", node_id, 0, false, port_positions, dragging_from, connections, pending_disconnects, PortKind::Audio);
}
