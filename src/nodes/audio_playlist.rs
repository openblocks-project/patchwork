use crate::audio::AudioManager;
use crate::graph::*;
use eframe::egui;
use crate::nodes::ScrollAreaExt;
use std::collections::HashMap;

const WAVEFORM_HEIGHT: f32 = 36.0;
const LIST_ROW_H: f32 = 22.0;
const LIST_MAX_H: f32 = 160.0;

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
    let (tracks, current_index, volume, loop_playlist, duration_secs) = match node_type {
        NodeType::AudioPlaylist { tracks, current_index, volume, loop_playlist, duration_secs } =>
            (tracks, current_index, volume, loop_playlist, duration_secs),
        _ => return,
    };

    let is_playing = audio.file_playing.get(&node_id).copied().unwrap_or(false);
    let is_paused  = audio.is_file_paused(node_id);
    let has_tracks = !tracks.is_empty();

    // ── Duration probe for current track ─────────────────────────────
    let current_path: String = tracks.get(*current_index).cloned().unwrap_or_default();
    if !current_path.is_empty() && *duration_secs <= 0.0 {
        let cached = audio.get_file_duration(node_id);
        if cached > 0.0 {
            *duration_secs = cached;
        } else {
            let probe_id = egui::Id::new(("pl_dur_probe", node_id));
            let probe_result: Option<f64> = ui.ctx().data_mut(|d| d.get_temp(probe_id));
            match probe_result {
                Some(dur) if dur > 0.0 => {
                    *duration_secs = dur;
                    audio.file_durations.insert(node_id, dur);
                }
                Some(_) => {}
                None => {
                    let path = current_path.clone();
                    let ctx  = ui.ctx().clone();
                    let pid  = probe_id;
                    std::thread::spawn(move || {
                        let dur = crate::audio::probe_file_duration(&path).unwrap_or(0.0);
                        ctx.data_mut(|d| d.insert_temp(pid, dur));
                        ctx.request_repaint();
                    });
                    ui.ctx().data_mut(|d| d.insert_temp(probe_id, -1.0f64));
                }
            }
        }
    }

    let duration = *duration_secs;

    // ── Auto-advance when track finishes ─────────────────────────────
    if is_playing && !is_paused && audio.is_file_finished(node_id) {
        let next = *current_index + 1;
        if next < tracks.len() {
            *current_index = next;
            *duration_secs = 0.0;
            audio.file_durations.remove(&node_id);
            let path = tracks[next].clone();
            audio.stop_file(node_id);
            let _ = audio.play_file(node_id, &path);
        } else if *loop_playlist && !tracks.is_empty() {
            *current_index = 0;
            *duration_secs = 0.0;
            audio.file_durations.remove(&node_id);
            let path = tracks[0].clone();
            audio.stop_file(node_id);
            let _ = audio.play_file(node_id, &path);
        } else {
            audio.stop_file(node_id);
        }
    }

    // ── Input port handling ───────────────────────────────────────────
    // Port 0: Play trigger
    let play_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    if play_wired {
        let v = Graph::static_input_value(connections, values, node_id, 0).as_float();
        if v > 0.5 && !is_playing && has_tracks {
            let _ = audio.play_file(node_id, &tracks[*current_index]);
        } else if v <= 0.5 && is_playing {
            audio.pause_file(node_id);
        }
    }

    // Port 1: Stop trigger (rising edge)
    let stop_last_id = egui::Id::new(("pl_stop_last", node_id));
    let stop_last: f32 = ui.ctx().data_mut(|d| d.get_temp(stop_last_id).unwrap_or(0.0f32));
    let stop_val = Graph::static_input_value(connections, values, node_id, 1).as_float();
    if connections.iter().any(|c| c.to_node == node_id && c.to_port == 1) {
        if stop_val > 0.5 && stop_last <= 0.5 {
            audio.stop_file(node_id);
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(stop_last_id, stop_val));

    // Port 2: Next trigger (rising edge)
    let next_last_id = egui::Id::new(("pl_next_last", node_id));
    let next_last: f32 = ui.ctx().data_mut(|d| d.get_temp(next_last_id).unwrap_or(0.0f32));
    let next_val = Graph::static_input_value(connections, values, node_id, 2).as_float();
    if connections.iter().any(|c| c.to_node == node_id && c.to_port == 2) {
        if next_val > 0.5 && next_last <= 0.5 && has_tracks {
            let n = (*current_index + 1) % tracks.len();
            *current_index = n;
            *duration_secs = 0.0;
            audio.file_durations.remove(&node_id);
            let path = tracks[n].clone();
            audio.stop_file(node_id);
            let _ = audio.play_file(node_id, &path);
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(next_last_id, next_val));

    // Port 3: Prev trigger (rising edge)
    let prev_last_id = egui::Id::new(("pl_prev_last", node_id));
    let prev_last: f32 = ui.ctx().data_mut(|d| d.get_temp(prev_last_id).unwrap_or(0.0f32));
    let prev_val = Graph::static_input_value(connections, values, node_id, 3).as_float();
    if connections.iter().any(|c| c.to_node == node_id && c.to_port == 3) {
        if prev_val > 0.5 && prev_last <= 0.5 && has_tracks {
            let p = if *current_index == 0 { tracks.len() - 1 } else { *current_index - 1 };
            *current_index = p;
            *duration_secs = 0.0;
            audio.file_durations.remove(&node_id);
            let path = tracks[p].clone();
            audio.stop_file(node_id);
            let _ = audio.play_file(node_id, &path);
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(prev_last_id, prev_val));

    // Port 4: Index (number) — jump to specific index
    let idx_last_id = egui::Id::new(("pl_idx_last", node_id));
    let idx_last: f32 = ui.ctx().data_mut(|d| d.get_temp(idx_last_id).unwrap_or(-1.0f32));
    if connections.iter().any(|c| c.to_node == node_id && c.to_port == 4) {
        let idx_val = Graph::static_input_value(connections, values, node_id, 4).as_float();
        let idx = idx_val.floor() as usize;
        if idx < tracks.len() && (idx_val - idx_last).abs() > 0.1 {
            *current_index = idx;
            *duration_secs = 0.0;
            audio.file_durations.remove(&node_id);
            let path = tracks[idx].clone();
            audio.stop_file(node_id);
            let _ = audio.play_file(node_id, &path);
        }
        ui.ctx().data_mut(|d| d.insert_temp(idx_last_id, idx_val));
    }

    // Port 5: Volume
    if connections.iter().any(|c| c.to_node == node_id && c.to_port == 5) {
        *volume = Graph::static_input_value(connections, values, node_id, 5).as_float().clamp(0.0, 1.0);
        audio.set_file_volume(node_id, *volume);
    }

    // ── Re-read state after port handling ────────────────────────────
    let is_playing = audio.file_playing.get(&node_id).copied().unwrap_or(false);
    let is_paused  = audio.is_file_paused(node_id);

    let playback_pos = if is_playing || is_paused {
        audio.get_playback_position(node_id)
    } else { 0.0 };

    ui.set_max_width(240.0);

    // ── Input ports row ───────────────────────────────────────────────
    let dim   = egui::Color32::from_rgb(100, 100, 110);
    let accent_bytes = ui.ctx().data_mut(|d| d.get_temp::<[u8; 3]>(egui::Id::new("theme_accent"))).unwrap_or([140, 80, 200]);
    let accent = egui::Color32::from_rgb(accent_bytes[0], accent_bytes[1], accent_bytes[2]);

    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Play").small().color(dim));
        ui.add_space(4.0);
        crate::nodes::inline_port_circle(ui, node_id, 1, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Stop").small().color(dim));
    });
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 2, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Next").small().color(dim));
        ui.add_space(4.0);
        crate::nodes::inline_port_circle(ui, node_id, 3, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Prev").small().color(dim));
    });
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, 4, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
        ui.label(egui::RichText::new("Index").small().color(dim));
        ui.add_space(4.0);
        crate::nodes::inline_port_circle(ui, node_id, 5, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);
        ui.label(egui::RichText::new("Vol").small().color(dim));
    });

    ui.separator();

    // ── Track list ────────────────────────────────────────────────────
    let mut action: Option<PlaylistAction> = None;

    egui::ScrollArea::vertical()
        .id_salt(egui::Id::new(("pl_scroll", node_id)))
        .max_height(LIST_MAX_H)
        .auto_shrink([false, false])
        .show_pannable(ui, |ui| {
            // 3 buttons × 18px + 2px gap each = ~58px reserved on right
            const BTN_AREA: f32 = 58.0;

            let n = tracks.len();
            for i in 0..n {
                let is_current = i == *current_index;
                let short = short_name(&tracks[i]);

                ui.horizontal(|ui| {
                    ui.set_height(LIST_ROW_H);

                    // ── Indicator ─────────────────────────────────
                    let (ind_icon, ind_col) = if is_current && is_playing {
                        ("▶", egui::Color32::from_rgb(80, 220, 120))
                    } else if is_current {
                        ("›", accent)
                    } else {
                        (" ", dim)
                    };
                    ui.label(egui::RichText::new(ind_icon).small().color(ind_col));

                    // ── Index number ───────────────────────────────
                    ui.label(egui::RichText::new(format!("{:2}.", i + 1))
                        .small().monospace().color(dim));

                    // ── Track name (truncated, fills remaining space) ──
                    let name_col = if is_current {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::from_rgb(170, 170, 180)
                    };
                    let name_w = (ui.available_width() - BTN_AREA).max(10.0);
                    let resp = ui.add_sized(
                        [name_w, LIST_ROW_H],
                        egui::Label::new(egui::RichText::new(&short).small().color(name_col))
                            .sense(egui::Sense::click())
                            .truncate(),
                    );
                    if resp.clicked() { action = Some(PlaylistAction::JumpTo(i)); }
                    resp.on_hover_text(&tracks[i]);

                    // ── Buttons: ↑ ↓ × right-aligned ──────────────
                    // Always render all three slots so columns stay aligned
                    if i > 0 {
                        if ui.add(egui::Button::new(
                            egui::RichText::new("↑").small()
                        ).min_size(egui::vec2(18.0, LIST_ROW_H))).clicked() {
                            action = Some(PlaylistAction::MoveUp(i));
                        }
                    } else {
                        ui.add_space(20.0);
                    }

                    if i + 1 < n {
                        if ui.add(egui::Button::new(
                            egui::RichText::new("↓").small()
                        ).min_size(egui::vec2(18.0, LIST_ROW_H))).clicked() {
                            action = Some(PlaylistAction::MoveDown(i));
                        }
                    } else {
                        ui.add_space(20.0);
                    }

                    if ui.add(egui::Button::new(
                        egui::RichText::new("×").small().color(egui::Color32::from_rgb(200, 80, 80))
                    ).min_size(egui::vec2(18.0, LIST_ROW_H))).clicked() {
                        action = Some(PlaylistAction::Remove(i));
                    }
                });
            }

            if tracks.is_empty() {
                ui.label(egui::RichText::new("No tracks — click + to add").small().color(dim));
            }
        });

    // ── Add tracks button ─────────────────────────────────────────────
    ui.horizontal(|ui| {
        if ui.button("+ Add tracks").clicked() {
            if let Some(paths) = rfd::FileDialog::new()
                .add_filter("Audio", &["mp3", "wav", "ogg", "flac", "aac", "m4a"])
                .pick_files()
            {
                action = Some(PlaylistAction::Add(
                    paths.iter().map(|p| p.to_string_lossy().to_string()).collect()
                ));
            }
        }
        // Loop toggle
        let lc = if *loop_playlist { accent } else { dim };
        if ui.add(egui::Button::new(egui::RichText::new("↻ Loop").small().color(lc))).clicked() {
            *loop_playlist = !*loop_playlist;
        }
    });

    // ── Apply deferred action ─────────────────────────────────────────
    // (We collected the action above while borrowing tracks immutably in the closure,
    //  now apply it with a mutable borrow of node_type)
    if let Some(act) = action {
        let (tracks, current_index, duration_secs) = match node_type {
            NodeType::AudioPlaylist { tracks, current_index, duration_secs, .. } =>
                (tracks, current_index, duration_secs),
            _ => unreachable!(),
        };
        match act {
            PlaylistAction::JumpTo(i) => {
                *current_index = i;
                *duration_secs = 0.0;
                audio.file_durations.remove(&node_id);
                let path = tracks[i].clone();
                audio.stop_file(node_id);
                let _ = audio.play_file(node_id, &path);
            }
            PlaylistAction::Remove(i) => {
                tracks.remove(i);
                if *current_index >= tracks.len() && !tracks.is_empty() {
                    *current_index = tracks.len() - 1;
                }
                if tracks.is_empty() {
                    audio.stop_file(node_id);
                }
            }
            PlaylistAction::MoveUp(i) => {
                tracks.swap(i, i - 1);
                if *current_index == i { *current_index = i - 1; }
                else if *current_index == i - 1 { *current_index = i; }
            }
            PlaylistAction::MoveDown(i) => {
                tracks.swap(i, i + 1);
                if *current_index == i { *current_index = i + 1; }
                else if *current_index == i + 1 { *current_index = i; }
            }
            PlaylistAction::Add(paths) => {
                tracks.extend(paths);
            }
        }
    }

    // Re-borrow after action applied
    let (tracks, current_index, volume, duration_secs) = match node_type {
        NodeType::AudioPlaylist { tracks, current_index, volume, duration_secs, .. } =>
            (tracks, current_index, volume, duration_secs),
        _ => return,
    };
    let duration = *duration_secs;
    let has_tracks = !tracks.is_empty();
    let current_path: String = tracks.get(*current_index).cloned().unwrap_or_default();
    let is_playing = audio.file_playing.get(&node_id).copied().unwrap_or(false);
    let is_paused  = audio.is_file_paused(node_id);

    ui.separator();

    // ── Waveform for current track ────────────────────────────────────
    if has_tracks {
        let w = 220.0_f32.min(ui.available_width().max(80.0));
        let (wf_rect, wf_resp) = ui.allocate_exact_size(egui::vec2(w, WAVEFORM_HEIGHT), egui::Sense::click_and_drag());
        let painter = ui.painter();
        painter.rect_filled(wf_rect, 4.0, egui::Color32::from_rgb(22, 22, 28));

        // Seek on click/drag
        if (wf_resp.clicked() || wf_resp.dragged()) && duration > 0.0 {
            if let Some(pos) = wf_resp.interact_pointer_pos() {
                let t = ((pos.x - wf_rect.left()) / wf_rect.width()).clamp(0.0, 1.0);
                if is_playing || is_paused {
                    let _ = audio.seek_file(node_id, &current_path, t as f64 * duration);
                }
            }
        }

        // Waveform bars (hash-seeded, same as AudioPlayer)
        {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            current_path.hash(&mut h);
            let seed = h.finish();
            let num_bars = 48u32;
            let bar_w = wf_rect.width() / num_bars as f32;
            let center_y = wf_rect.center().y;
            let progress = if duration > 0.0 { (playback_pos as f32 / duration as f32).clamp(0.0, 1.0) } else { 0.0 };

            for i in 0..num_bars {
                let t = i as f64 / num_bars as f64;
                let v = ((seed as f64 * 0.0001 + t * 13.7).sin() * 0.5 + 0.5) as f32
                    * ((seed as f64 * 0.0003 + t * 7.3).sin() * 0.3 + 0.5) as f32;
                let half_h = v.clamp(0.05, 0.9) * (WAVEFORM_HEIGHT * 0.4);
                let x = wf_rect.left() + i as f32 * bar_w;
                let bar_progress = i as f32 / num_bars as f32;
                let col = if bar_progress <= progress {
                    egui::Color32::from_rgb(accent_bytes[0], accent_bytes[1], accent_bytes[2])
                } else {
                    egui::Color32::from_rgb(45, 45, 55)
                };
                painter.rect_filled(
                    egui::Rect::from_center_size(egui::pos2(x + bar_w * 0.5, center_y), egui::vec2(bar_w * 0.55, half_h * 2.0)),
                    1.0, col);
            }

            // Playhead
            let px = wf_rect.left() + (if duration > 0.0 { (playback_pos as f32 / duration as f32).clamp(0.0, 1.0) } else { 0.0 }) * wf_rect.width();
            painter.line_segment(
                [egui::pos2(px, wf_rect.top()), egui::pos2(px, wf_rect.bottom())],
                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 60, 60)));
        }

        // Time display
        ui.horizontal(|ui| {
            let fmt = |s: f32| { let m = (s / 60.0) as i32; format!("{}:{:05.2}", m, s % 60.0) };
            ui.label(egui::RichText::new(fmt(playback_pos as f32)).small().monospace()
                .color(egui::Color32::from_rgb(255, 60, 60)));
            ui.label(egui::RichText::new("/").small().color(dim));
            ui.label(egui::RichText::new(fmt(duration as f32)).small().monospace().color(dim));
        });

        // Current track name
        let name = short_name(&current_path);
        ui.label(egui::RichText::new(format!("{}  {}", *current_index + 1, name)).small()
            .color(egui::Color32::from_rgb(160, 160, 170)));
    }

    // ── Transport controls ────────────────────────────────────────────
    ui.horizontal(|ui| {
        // Prev
        if ui.add(egui::Button::new(egui::RichText::new("⏮").size(13.0)).min_size(egui::vec2(26.0, 22.0))).clicked() && has_tracks {
            let p = if *current_index == 0 { tracks.len() - 1 } else { *current_index - 1 };
            *current_index = p;
            *duration_secs = 0.0;
            audio.file_durations.remove(&node_id);
            let path = tracks[p].clone();
            audio.stop_file(node_id);
            let _ = audio.play_file(node_id, &path);
        }

        // Play/Pause
        let play_icon = if is_playing { "⏸" } else { "▶" };
        let play_btn = if !is_playing {
            egui::Button::new(egui::RichText::new(play_icon).size(13.0).color(egui::Color32::WHITE))
                .fill(accent).min_size(egui::vec2(28.0, 22.0))
        } else {
            egui::Button::new(egui::RichText::new(play_icon).size(13.0)).min_size(egui::vec2(28.0, 22.0))
        };
        if ui.add(play_btn).clicked() && has_tracks {
            if is_playing {
                audio.pause_file(node_id);
            } else {
                let path = tracks[*current_index].clone();
                let _ = audio.play_file(node_id, &path);
                let dur = audio.get_file_duration(node_id);
                if dur > 0.0 { *duration_secs = dur; }
            }
        }

        // Stop
        if ui.add(egui::Button::new(egui::RichText::new("⏹").size(13.0)).min_size(egui::vec2(26.0, 22.0))).clicked() {
            audio.stop_file(node_id);
        }

        // Next
        if ui.add(egui::Button::new(egui::RichText::new("⏭").size(13.0)).min_size(egui::vec2(26.0, 22.0))).clicked() && has_tracks {
            let n = (*current_index + 1) % tracks.len();
            *current_index = n;
            *duration_secs = 0.0;
            audio.file_durations.remove(&node_id);
            let path = tracks[n].clone();
            audio.stop_file(node_id);
            let _ = audio.play_file(node_id, &path);
        }
    });

    // ── Volume ────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("🔊").size(10.0));
        if ui.add(egui::Slider::new(volume, 0.0..=1.0).show_value(false)).changed() {
            audio.set_file_volume(node_id, *volume);
        }
        ui.label(egui::RichText::new(format!("{:.0}%", *volume * 100.0)).small().color(dim));
    });

    ui.separator();

    // ── Output ports ──────────────────────────────────────────────────
    crate::nodes::audio_port_row(ui, "Audio", node_id, 0, false, port_positions, dragging_from, connections, pending_disconnects, PortKind::Audio);

    let progress = if duration > 0.0 { (playback_pos / duration).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let track_name = tracks.get(*current_index).map(|p| short_name(p)).unwrap_or_default();

    crate::nodes::output_port_row(ui, "Progress", &format!("{:.1}%", progress * 100.0),
        node_id, 1, port_positions, dragging_from, connections, pending_disconnects, PortKind::Normalized);
    crate::nodes::output_port_row(ui, "Index", &current_index.to_string(),
        node_id, 2, port_positions, dragging_from, connections, pending_disconnects, PortKind::Number);
    crate::nodes::output_port_row(ui, "Name", &track_name,
        node_id, 3, port_positions, dragging_from, connections, pending_disconnects, PortKind::Text);

    // Store values for graph eval
    ui.ctx().data_mut(|d| {
        d.insert_temp(egui::Id::new(("pl_progress", node_id)), progress);
        d.insert_temp(egui::Id::new(("pl_index",    node_id)), *current_index as f32);
        d.insert_temp(egui::Id::new(("pl_name",     node_id)),
            tracks.get(*current_index).map(|p| short_name(p)).unwrap_or_default());
    });

    if is_playing { ui.ctx().request_repaint(); }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn short_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

enum PlaylistAction {
    JumpTo(usize),
    Remove(usize),
    MoveUp(usize),
    MoveDown(usize),
    Add(Vec<String>),
}
