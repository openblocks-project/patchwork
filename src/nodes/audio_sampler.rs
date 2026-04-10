#![allow(dead_code)]
use crate::audio::AudioManager;
use crate::audio::buffers::SamplerBuffer;
use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;
use std::sync::Arc;

/// Export a region of the sampler buffer as a 32-bit float WAV file.
fn export_wav(buffer: &Arc<SamplerBuffer>, start: usize, end: usize, sample_rate: u32, path: &std::path::Path) -> Result<(), String> {
    let len = end.saturating_sub(start);
    if len == 0 { return Err("Nothing to export".into()); }

    // Read raw samples directly from buffer
    let data = unsafe { &*buffer.data.get() };
    let raw: Vec<f32> = (start..end.min(buffer.capacity)).map(|i| data[i]).collect();
    if raw.is_empty() { return Err("No samples to export".into()); }

    // Write WAV: 32-bit float, mono
    let num_samples = raw.len() as u32;
    let byte_rate = sample_rate * 4; // 4 bytes per sample (f32)
    let data_size = num_samples * 4;

    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    use std::io::Write;

    // RIFF header
    file.write_all(b"RIFF").map_err(|e| e.to_string())?;
    file.write_all(&(36 + data_size).to_le_bytes()).map_err(|e| e.to_string())?;
    file.write_all(b"WAVE").map_err(|e| e.to_string())?;

    // fmt chunk — format 3 = IEEE float
    file.write_all(b"fmt ").map_err(|e| e.to_string())?;
    file.write_all(&16u32.to_le_bytes()).map_err(|e| e.to_string())?;   // chunk size
    file.write_all(&3u16.to_le_bytes()).map_err(|e| e.to_string())?;    // format = IEEE float
    file.write_all(&1u16.to_le_bytes()).map_err(|e| e.to_string())?;    // channels = 1 (mono)
    file.write_all(&sample_rate.to_le_bytes()).map_err(|e| e.to_string())?;
    file.write_all(&byte_rate.to_le_bytes()).map_err(|e| e.to_string())?;
    file.write_all(&4u16.to_le_bytes()).map_err(|e| e.to_string())?;    // block align
    file.write_all(&32u16.to_le_bytes()).map_err(|e| e.to_string())?;   // bits per sample

    // data chunk
    file.write_all(b"data").map_err(|e| e.to_string())?;
    file.write_all(&data_size.to_le_bytes()).map_err(|e| e.to_string())?;
    for &s in &raw {
        file.write_all(&s.to_le_bytes()).map_err(|e| e.to_string())?;
    }

    Ok(())
}

const WAVEFORM_HEIGHT: f32 = 50.0;
const WHEEL_SIZE: f32 = 38.0;

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
    let (record_duration, trim_start, trim_end, volume, looping, reverse, play_mode, range_as_duration, speed, seek) = match node_type {
        NodeType::AudioSampler { record_duration, trim_start, trim_end, volume, looping, reverse, play_mode, range_as_duration, speed, seek } =>
            (record_duration, trim_start, trim_end, volume, looping, reverse, play_mode, range_as_duration, speed, seek),
        _ => return,
    };

    // Get or create sampler buffer
    let buffer = audio.get_or_create_sampler_buffer(node_id, *record_duration);
    let sr = buffer.sample_rate.load(std::sync::atomic::Ordering::Relaxed) as f32;
    let is_recording = buffer.recording.load(std::sync::atomic::Ordering::Relaxed);
    let is_playing = buffer.playing.load(std::sync::atomic::Ordering::Relaxed);
    let rec_len = buffer.recorded_length.load(std::sync::atomic::Ordering::Relaxed);

    // During recording, use write_pos as the "current length" for display purposes
    let current_write_pos = buffer.write_pos.load(std::sync::atomic::Ordering::Relaxed);
    let display_len = if is_recording { current_write_pos } else { rec_len };
    let display_dur = if sr > 0.0 { display_len as f32 / sr } else { 0.0 };

    // ── Range port overrides (Start + End/Dur) ───────────────────
    // When wired, these ports override the manual trim sliders so a
    // patch can drive the playback region from upstream nodes (LFO,
    // Map Range, etc.). Done BEFORE the atomic store below so the
    // override propagates to the audio thread the same frame.
    let recorded_dur_override = buffer.recorded_duration_secs();
    let start_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 4);
    let endur_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 5);
    if start_wired && recorded_dur_override > 0.0 {
        let v = Graph::static_input_value(connections, values, node_id, 4).as_float();
        *trim_start = v.clamp(0.0, recorded_dur_override);
    }
    if endur_wired && recorded_dur_override > 0.0 {
        let v = Graph::static_input_value(connections, values, node_id, 5).as_float();
        let new_end = if *range_as_duration {
            (*trim_start + v.max(0.0)).min(recorded_dur_override)
        } else {
            v.clamp(*trim_start, recorded_dur_override)
        };
        *trim_end = new_end;
    }

    // Sync UI state ↔ buffer atomics
    buffer.looping.store(*looping, std::sync::atomic::Ordering::Relaxed);
    buffer.reverse.store(*reverse, std::sync::atomic::Ordering::Relaxed);
    if sr > 0.0 && !is_recording && rec_len > 0 {
        buffer.trim_start.store((*trim_start * sr) as usize, std::sync::atomic::Ordering::Relaxed);
        let te = if *trim_end > 0.0 { (*trim_end * sr) as usize } else { rec_len };
        buffer.trim_end.store(te, std::sync::atomic::Ordering::Relaxed);
    }

    // Read trigger inputs
    let rec_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
    if rec_wired {
        let rec_val = Graph::static_input_value(connections, values, node_id, 1).as_float();
        if rec_val > 0.5 && !is_recording {
            buffer.start_recording();
        } else if rec_val <= 0.5 && is_recording {
            buffer.stop_recording();
            *trim_start = 0.0;
            *trim_end = buffer.recorded_duration_secs();
        }
    }

    // Play port behavior depends on `play_mode`:
    //   0 = Gate  — current behavior: play while high, stop on low
    //   1 = Trig  — rising edge always (re)starts; ignores low
    //   2 = Full  — rising edge starts only if not already playing (one-shot)
    let play_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);
    if play_wired {
        let play_val = Graph::static_input_value(connections, values, node_id, 2).as_float();
        // Track previous value per-node for rising-edge detection.
        // Stored in egui's ctx data store so it survives across frames
        // without leaking when the node is deleted (egui GCs eventually).
        let prev_id = egui::Id::new(("sampler_prev_play", node_id));
        let prev = ui.ctx().data_mut(|d| d.get_temp::<f32>(prev_id)).unwrap_or(0.0);
        let rising = prev <= 0.5 && play_val > 0.5;
        ui.ctx().data_mut(|d| d.insert_temp(prev_id, play_val));

        match *play_mode {
            0 => {
                // Gate
                if play_val > 0.5 && !is_playing && !is_recording && rec_len > 0 {
                    buffer.start_playback();
                } else if play_val <= 0.5 && is_playing {
                    buffer.stop_playback();
                }
            }
            1 => {
                // Trig — rising edge always restarts from beginning
                if rising && !is_recording && rec_len > 0 {
                    buffer.start_playback();
                }
            }
            _ => {
                // Full — rising edge starts only if not already playing
                if rising && !is_playing && !is_recording && rec_len > 0 {
                    buffer.start_playback();
                }
            }
        }
    }

    // Volume from port
    if connections.iter().any(|c| c.to_node == node_id && c.to_port == 3) {
        *volume = Graph::static_input_value(connections, values, node_id, 3).as_float().clamp(0.0, 1.0);
    }
    // Write volume to ParamStore
    audio.engine_write_param(node_id, 0, *volume);

    // Speed override (port 6)
    let speed_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 6);
    if speed_wired {
        *speed = Graph::static_input_value(connections, values, node_id, 6).as_float().clamp(0.1, 8.0);
    }
    audio.engine_write_param(node_id, 3, *speed);

    // Seek override (port 7) — continuously sets playhead when wired (scrubbing)
    let seek_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 7);
    if seek_wired {
        *seek = Graph::static_input_value(connections, values, node_id, 7).as_float().clamp(0.0, 1.0);
        // Write the seek value; processor will call seek_to_normalized each frame
        audio.engine_write_param(node_id, 4, *seek);
    } else {
        // No seek pending this frame — use sentinel -1.0
        // (UI slider writes directly via engine_write_param when changed)
        audio.engine_write_param(node_id, 4, -1.0);
    }

    // ── Input ports ──────────────────────────────────────────────
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Audio);
        ui.label(egui::RichText::new("In").small());
        ui.add_space(4.0);
        crate::nodes::inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Rec").small());
        ui.add_space(4.0);
        crate::nodes::inline_port_circle(ui, node_id, 2, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Play").small());
        ui.add_space(6.0);
        // Play-mode dropdown — Gate / Trigger / Full.
        let mode_label = match *play_mode {
            0 => "Gate",
            1 => "Trigger",
            _ => "Full",
        };
        egui::ComboBox::from_id_salt(("sampler_play_mode", node_id))
            .selected_text(egui::RichText::new(mode_label).small())
            .width(64.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(play_mode, 0, "Gate");
                ui.selectable_value(play_mode, 1, "Trigger");
                ui.selectable_value(play_mode, 2, "Full");
            });
    });

    // ── Waveform display ─────────────────────────────────────────
    let w = 230.0_f32.min(ui.available_width().max(100.0));
    let (wf_rect, _wf_response) = ui.allocate_exact_size(egui::vec2(w, WAVEFORM_HEIGHT), egui::Sense::click_and_drag());
    let painter = ui.painter();
    painter.rect_filled(wf_rect, 4.0, egui::Color32::from_rgb(25, 25, 30));

    if display_len > 0 {
        // Draw waveform bars — use display_len so waveform shows during recording too
        let bars = buffer.waveform_snapshot_live(60, display_len);
        let num_bars = bars.len();
        let bar_w = wf_rect.width() / num_bars as f32;
        let center_y = wf_rect.center().y;

        if is_recording {
            // During recording: show live waveform filling up
            let fill_frac = current_write_pos as f32 / buffer.capacity as f32;
            for (i, &amp) in bars.iter().enumerate() {
                let bar_frac = i as f32 / num_bars as f32;
                let half_h = amp.clamp(0.05, 1.0) * (WAVEFORM_HEIGHT * 0.4);
                let x = wf_rect.left() + i as f32 * bar_w;
                let color = if bar_frac <= fill_frac {
                    egui::Color32::from_rgb(255, 80, 80) // red - recorded region
                } else {
                    egui::Color32::from_rgb(35, 35, 40)
                };
                painter.rect_filled(
                    egui::Rect::from_center_size(
                        egui::pos2(x + bar_w * 0.5, center_y),
                        egui::vec2(bar_w * 0.6, half_h * 2.0),
                    ),
                    1.0, color,
                );
            }
            // Recording position indicator
            let wx = wf_rect.left() + fill_frac.min(1.0) * wf_rect.width();
            painter.line_segment(
                [egui::pos2(wx, wf_rect.top()), egui::pos2(wx, wf_rect.bottom())],
                egui::Stroke::new(2.5, egui::Color32::from_rgb(255, 50, 50)),
            );
        } else {
            // Trim region highlight
            let trim_start_frac = if display_dur > 0.0 { *trim_start / display_dur } else { 0.0 };
            let trim_end_frac = if display_dur > 0.0 && *trim_end > 0.0 {
                (*trim_end / display_dur).min(1.0)
            } else { 1.0 };

            let trim_left = wf_rect.left() + trim_start_frac * wf_rect.width();
            let trim_right = wf_rect.left() + trim_end_frac * wf_rect.width();
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(trim_left, wf_rect.top()),
                    egui::pos2(trim_right, wf_rect.bottom()),
                ),
                0.0,
                egui::Color32::from_rgba_premultiplied(220, 120, 80, 20),
            );

            // Playback progress
            let playback_pos = buffer.playback_position();
            let progress = if rec_len > 0 { playback_pos as f32 / rec_len as f32 } else { 0.0 };

            for (i, &amp) in bars.iter().enumerate() {
                let bar_frac = i as f32 / num_bars as f32;
                let half_h = amp.clamp(0.05, 1.0) * (WAVEFORM_HEIGHT * 0.4);
                let x = wf_rect.left() + i as f32 * bar_w;

                let in_trim = bar_frac >= trim_start_frac && bar_frac <= trim_end_frac;
                let played = is_playing && bar_frac <= progress;

                let color = if played && in_trim {
                    egui::Color32::from_rgb(220, 120, 80)
                } else if in_trim {
                    egui::Color32::from_rgb(140, 80, 55)
                } else {
                    egui::Color32::from_rgb(40, 40, 45)
                };

                painter.rect_filled(
                    egui::Rect::from_center_size(
                        egui::pos2(x + bar_w * 0.5, center_y),
                        egui::vec2(bar_w * 0.6, half_h * 2.0),
                    ),
                    1.0, color,
                );
            }

            // Trim handles
            let handle_color = egui::Color32::from_rgb(255, 200, 100);
            painter.line_segment(
                [egui::pos2(trim_left, wf_rect.top()), egui::pos2(trim_left, wf_rect.bottom())],
                egui::Stroke::new(1.5, handle_color),
            );
            painter.line_segment(
                [egui::pos2(trim_right, wf_rect.top()), egui::pos2(trim_right, wf_rect.bottom())],
                egui::Stroke::new(1.5, handle_color),
            );

            // Playhead
            if is_playing {
                let px = wf_rect.left() + progress * wf_rect.width();
                let ph_color = egui::Color32::from_rgb(255, 60, 60);
                painter.line_segment(
                    [egui::pos2(px, wf_rect.top()), egui::pos2(px, wf_rect.bottom())],
                    egui::Stroke::new(2.0, ph_color),
                );
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        egui::pos2(px - 3.0, wf_rect.top()),
                        egui::pos2(px + 3.0, wf_rect.top()),
                        egui::pos2(px, wf_rect.top() + 5.0),
                    ],
                    ph_color, egui::Stroke::NONE,
                ));
            }
        }
    } else {
        // Empty state — show connection hint
        let msg = if connections.iter().any(|c| c.to_node == node_id && c.to_port == 0) {
            "Press ⏺ to record"
        } else {
            "Connect audio source"
        };
        painter.text(
            wf_rect.center(), egui::Align2::CENTER_CENTER,
            msg,
            egui::FontId::proportional(11.0),
            egui::Color32::from_rgb(80, 80, 90),
        );
    }

    // ── Time display ─────────────────────────────────────────────
    let fmt_time = |s: f32| -> String {
        let m = (s / 60.0).floor() as i32;
        let sec = s % 60.0;
        format!("{}:{:05.2}", m, sec)
    };

    {
        ui.horizontal(|ui| {
            let current_pos = if is_recording {
                if sr > 0.0 { current_write_pos as f32 / sr } else { 0.0 }
            } else if is_playing {
                let rp = buffer.playback_position() as f32;
                if sr > 0.0 { rp / sr } else { 0.0 }
            } else { 0.0 };

            let state_label = if is_recording { "REC" } else if is_playing { "PLAY" } else { "IDLE" };
            let state_color = if is_recording {
                egui::Color32::from_rgb(255, 50, 50)
            } else if is_playing {
                egui::Color32::from_rgb(80, 200, 120)
            } else {
                egui::Color32::from_rgb(80, 80, 90)
            };

            ui.label(egui::RichText::new(state_label).small().strong().color(state_color));
            ui.label(egui::RichText::new(fmt_time(current_pos)).small().monospace().color(state_color));
            if display_dur > 0.0 {
                ui.label(egui::RichText::new("/").small().color(egui::Color32::from_rgb(80, 80, 85)));
                ui.label(egui::RichText::new(fmt_time(display_dur)).small().monospace().color(egui::Color32::from_rgb(120, 120, 130)));
            }
        });
    }

    // ── Transport: Spinning wheel + buttons ──────────────────────
    ui.horizontal(|ui| {
        // Spinning wheel (like a reel)
        let (wheel_rect, _) = ui.allocate_exact_size(egui::vec2(WHEEL_SIZE, WHEEL_SIZE), egui::Sense::hover());
        let center = wheel_rect.center();
        let radius = WHEEL_SIZE * 0.44;
        let wp = ui.painter().clone();

        // Wheel background
        let wheel_bg = if is_recording {
            egui::Color32::from_rgb(60, 20, 20)
        } else if is_playing {
            egui::Color32::from_rgb(20, 40, 25)
        } else {
            egui::Color32::from_rgb(30, 30, 35)
        };
        wp.circle_filled(center, radius, wheel_bg);
        wp.circle_stroke(center, radius, egui::Stroke::new(1.5, egui::Color32::from_rgb(55, 55, 65)));
        wp.circle_filled(center, radius * 0.18, egui::Color32::from_rgb(50, 50, 55));

        // Spinning animation
        let time = ui.ctx().input(|i| i.time) as f32;
        let spin_speed = if is_recording { 3.0 } else if is_playing { if *reverse { -4.0 } else { 4.0 } } else { 0.0 };
        let angle = time * spin_speed;

        // Reel notches (3 spokes)
        for spoke in 0..3 {
            let a = angle + spoke as f32 * std::f32::consts::TAU / 3.0;
            let inner = egui::pos2(center.x + a.cos() * radius * 0.25, center.y + a.sin() * radius * 0.25);
            let outer = egui::pos2(center.x + a.cos() * radius * 0.75, center.y + a.sin() * radius * 0.75);
            let spoke_color = if is_recording {
                egui::Color32::from_rgb(255, 80, 80)
            } else if is_playing {
                egui::Color32::from_rgb(80, 200, 120)
            } else {
                egui::Color32::from_rgb(60, 60, 70)
            };
            wp.line_segment([inner, outer], egui::Stroke::new(1.5, spoke_color));
        }

        // Groove circles
        for i in 1..3 {
            wp.circle_stroke(center, radius * (0.35 + i as f32 * 0.18),
                egui::Stroke::new(0.5, egui::Color32::from_rgb(42, 42, 48)));
        }

        // Buttons column
        ui.vertical(|ui| {
            let accent = ui.ctx().data_mut(|d| d.get_temp::<[u8; 3]>(egui::Id::new("theme_accent")))
                .unwrap_or([80, 160, 255]);

            ui.horizontal(|ui| {
                // Record button (red circle)
                let rec_btn_color = if is_recording {
                    egui::Color32::from_rgb(255, 50, 50)
                } else {
                    egui::Color32::from_rgb(180, 50, 50)
                };
                let rec_btn = egui::Button::new(
                    egui::RichText::new("⏺").size(14.0).color(if is_recording { egui::Color32::WHITE } else { rec_btn_color })
                )
                    .fill(if is_recording { rec_btn_color } else { egui::Color32::TRANSPARENT })
                    .min_size(egui::vec2(26.0, 22.0));
                if ui.add(rec_btn).clicked() {
                    if is_recording {
                        buffer.stop_recording();
                        *trim_start = 0.0;
                        *trim_end = buffer.recorded_duration_secs();
                    } else {
                        buffer.start_recording();
                    }
                }

                // Play/Pause button — enabled when we have recorded data
                let has_data = rec_len > 0;
                let play_icon = if is_playing { crate::icons::PAUSE } else { crate::icons::PLAY };
                let play_btn = if !is_playing && has_data {
                    egui::Button::new(egui::RichText::new(play_icon).size(14.0).color(egui::Color32::WHITE))
                        .fill(egui::Color32::from_rgb(accent[0], accent[1], accent[2]))
                        .min_size(egui::vec2(26.0, 22.0))
                } else {
                    egui::Button::new(egui::RichText::new(play_icon).size(14.0))
                        .min_size(egui::vec2(26.0, 22.0))
                };
                if ui.add(play_btn).clicked() {
                    if is_playing {
                        buffer.stop_playback();
                    } else if has_data && !is_recording {
                        buffer.start_playback();
                    }
                }

                // Stop button
                if ui.add(egui::Button::new(egui::RichText::new(crate::icons::STOP).size(14.0)).min_size(egui::vec2(24.0, 22.0))).clicked() {
                    if is_recording {
                        buffer.stop_recording();
                        *trim_start = 0.0;
                        *trim_end = buffer.recorded_duration_secs();
                    }
                    buffer.stop_playback();
                }
            });

            // Loop + Reverse + PlayMode + Save
            ui.horizontal(|ui| {
                let loop_color = if *looping { egui::Color32::from_rgb(80, 170, 255) } else { egui::Color32::GRAY };
                if ui.add(egui::Button::new(egui::RichText::new("↻").size(12.0).color(loop_color)).min_size(egui::vec2(22.0, 20.0))).clicked() {
                    *looping = !*looping;
                }

                let rev_color = if *reverse { egui::Color32::from_rgb(255, 180, 60) } else { egui::Color32::GRAY };
                if ui.add(egui::Button::new(egui::RichText::new("◀").size(10.0).color(rev_color)).min_size(egui::vec2(22.0, 20.0))).clicked() {
                    *reverse = !*reverse;
                }


                // Save button — export trimmed region as WAV
                if rec_len > 0 && !is_recording {
                    if ui.add(egui::Button::new(egui::RichText::new(crate::icons::FLOPPY_DISK).size(12.0)).min_size(egui::vec2(22.0, 20.0)))
                        .on_hover_text("Save as WAV").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("WAV Audio", &["wav"])
                            .set_file_name("recording.wav")
                            .save_file()
                        {
                            let sr = buffer.sample_rate.load(std::sync::atomic::Ordering::Relaxed);
                            let trim_s = (*trim_start * sr as f32) as usize;
                            let trim_e = if *trim_end > 0.0 { (*trim_end * sr as f32) as usize } else { rec_len };
                            let trim_e = trim_e.min(rec_len);
                            match export_wav(&buffer, trim_s, trim_e, sr, &path) {
                                Ok(()) => crate::system_log::log(format!("Saved WAV: {}", path.display())),
                                Err(e) => crate::system_log::error(format!("WAV export failed: {}", e)),
                            }
                        }
                    }
                }
            });

            // Volume slider
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, node_id, 3, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);
                ui.label(egui::RichText::new(crate::icons::SPEAKER_HIGH).size(10.0));
                if ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false)).changed() {
                    audio.engine_write_param(node_id, 0, *volume);
                }
                ui.label(egui::RichText::new(format!("{:.0}%", *volume * 100.0)).small().color(egui::Color32::GRAY));
            });

            // Speed slider — port 6
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, node_id, 6, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
                ui.label(egui::RichText::new("Spd").small().color(egui::Color32::GRAY));
                ui.add_enabled(
                    !speed_wired,
                    egui::Slider::new(speed, 0.1_f32..=4.0_f32).show_value(false).logarithmic(true),
                );
                ui.label(egui::RichText::new(format!("{:.2}x", *speed)).small().color(egui::Color32::GRAY));
            });

            // Seek slider — port 7
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, node_id, 7, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);
                ui.label(egui::RichText::new("Seek").small().color(egui::Color32::GRAY));
                let seek_resp = ui.add_enabled(
                    !seek_wired && !is_recording,
                    egui::Slider::new(seek, 0.0_f32..=1.0_f32).show_value(false),
                );
                if seek_resp.changed() {
                    // One-shot seek from UI slider drag
                    audio.engine_write_param(node_id, 4, *seek);
                }
                ui.label(egui::RichText::new(format!("{:.0}%", *seek * 100.0)).small().color(egui::Color32::GRAY));
            });
        });
    });

    // ── Record Duration ──────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Dur").small().color(egui::Color32::GRAY));
        if ui.add(egui::Slider::new(record_duration, 0.1..=300.0).show_value(false).logarithmic(true)).changed() {
            // Recreate buffer with new duration on next frame
            audio.sampler_buffers.remove(&node_id);
        }
        ui.label(egui::RichText::new(format!("{:.1}s", *record_duration)).small().monospace().color(egui::Color32::GRAY));
    });

    // ── Trim sliders ─────────────────────────────────────────────
    // Each slider has its corresponding input port circle on the left,
    // matching the existing pattern (e.g. volume port). When a port is
    // wired the slider becomes a read-only display since the value is
    // driven externally — the slider is still rendered (not hidden) so
    // the layout stays stable as ports get connected/disconnected.
    let recorded_dur = buffer.recorded_duration_secs();
    if recorded_dur > 0.0 && !is_recording {
        let label_color = egui::Color32::from_rgb(255, 200, 100);
        let val_color = egui::Color32::from_rgb(160, 140, 100);

        // Start row — port (4) + slider
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 4, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("Start").small().color(label_color));
            ui.add_enabled(!start_wired, egui::Slider::new(trim_start, 0.0..=recorded_dur).show_value(false));
            ui.label(egui::RichText::new(fmt_time(*trim_start)).small().monospace().color(val_color));
        });

        // End/Dur row — port (5) + slider + clickable mode chip
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 5, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
            // Clickable label that toggles range_as_duration. Stays the
            // same physical width as the Start label so the two slider
            // tracks line up.
            let chip_text = if *range_as_duration { "Dur " } else { "End " };
            if ui.add(egui::Button::new(egui::RichText::new(chip_text).small().color(label_color)).frame(false))
                .on_hover_text("Click to switch between End position and Duration")
                .clicked()
            {
                *range_as_duration = !*range_as_duration;
            }
            if *trim_end <= 0.0 || *trim_end > recorded_dur { *trim_end = recorded_dur; }
            if *range_as_duration {
                // Slider edits a derived `duration = trim_end - trim_start`.
                // Mutates trim_end via a temp on commit so the existing
                // atomic-store path stays unchanged.
                let max_dur = (recorded_dur - *trim_start).max(0.0);
                let mut dur_val = (*trim_end - *trim_start).clamp(0.0, max_dur);
                if ui.add_enabled(!endur_wired, egui::Slider::new(&mut dur_val, 0.0..=max_dur).show_value(false)).changed() {
                    *trim_end = (*trim_start + dur_val).min(recorded_dur);
                }
                ui.label(egui::RichText::new(fmt_time(dur_val)).small().monospace().color(val_color));
            } else {
                ui.add_enabled(!endur_wired, egui::Slider::new(trim_end, *trim_start..=recorded_dur).show_value(false));
                ui.label(egui::RichText::new(fmt_time(*trim_end)).small().monospace().color(val_color));
            }
        });
    }

    // ── Output ports ─────────────────────────────────────────────
    crate::nodes::audio_port_row(ui, "Audio", node_id, 0, false, port_positions, dragging_from, connections, pending_disconnects, PortKind::Audio);

    // Progress output
    let progress = if is_playing && rec_len > 0 {
        let rp = buffer.playback_position() as f32;
        (rp / rec_len as f32).clamp(0.0, 1.0)
    } else { 0.0 };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{:.0}%", progress * 100.0)).small().color(egui::Color32::GRAY));
        crate::nodes::inline_port_circle(ui, node_id, 1, false, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);
    });
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("sampler_progress", node_id)), progress));

    // Request repaint while active
    if is_playing || is_recording { ui.ctx().request_repaint(); }
}
