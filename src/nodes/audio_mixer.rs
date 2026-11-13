use crate::audio::AudioManager;
use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    channel_count: &mut usize,
    gains: &mut Vec<f32>,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    audio: &AudioManager,
) {
    // Ensure gains vec matches channel_count
    while gains.len() < *channel_count {
        gains.push(0.8);
    }
    gains.truncate(*channel_count);

    // Read gain control inputs (odd ports: 1, 3, 5, ...)
    for ch in 0..*channel_count {
        let gain_port = ch * 2 + 1;
        if connections
            .iter()
            .any(|c| c.to_node == node_id && c.to_port == gain_port)
        {
            let v = Graph::static_input_value(connections, values, node_id, gain_port).as_float();
            gains[ch] = v.clamp(0.0, 1.0);
        }
    }

    // ── Channel strips (vertical sliders side by side) ────────────────
    let strip_w = 36.0;
    let slider_h = 80.0;

    ui.horizontal(|ui| {
        for ch in 0..*channel_count {
            let audio_port = ch * 2;
            let gain_port = ch * 2 + 1;
            let audio_wired = connections
                .iter()
                .any(|c| c.to_node == node_id && c.to_port == audio_port);
            let gain_wired = connections
                .iter()
                .any(|c| c.to_node == node_id && c.to_port == gain_port);

            ui.vertical(|ui| {
                ui.set_width(strip_w);

                // Channel label
                ui.label(
                    egui::RichText::new(format!("Ch{}", ch + 1))
                        .small()
                        .strong(),
                );

                // Audio input port
                crate::nodes::inline_port_circle(
                    ui,
                    node_id,
                    audio_port,
                    true,
                    connections,
                    port_positions,
                    dragging_from,
                    pending_disconnects,
                    PortKind::Audio,
                );

                // Gain control port (small, below audio)
                crate::nodes::inline_port_circle(
                    ui,
                    node_id,
                    gain_port,
                    true,
                    connections,
                    port_positions,
                    dragging_from,
                    pending_disconnects,
                    PortKind::Normalized,
                );

                // Vertical fader
                if !gain_wired {
                    let mut g = gains[ch];
                    let resp = ui.add(
                        egui::Slider::new(&mut g, 0.0..=1.0)
                            .vertical()
                            .show_value(false),
                    );
                    if resp.changed() {
                        gains[ch] = g;
                    }
                } else {
                    // Show value bar when gain is wired
                    let bar_w = 12.0;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(bar_w, slider_h), egui::Sense::hover());
                    let painter = ui.painter();
                    painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(30, 30, 40));
                    let fill_h = gains[ch].clamp(0.0, 1.0) * rect.height();
                    let fill_rect = egui::Rect::from_min_max(
                        egui::pos2(rect.left(), rect.bottom() - fill_h),
                        rect.max,
                    );
                    painter.rect_filled(fill_rect, 2.0, egui::Color32::from_rgb(80, 180, 255));
                }

                // Gain value label
                ui.label(
                    egui::RichText::new(format!("{:.0}%", gains[ch] * 100.0))
                        .small()
                        .color(if audio_wired {
                            egui::Color32::from_rgb(180, 180, 200)
                        } else {
                            egui::Color32::from_rgb(80, 80, 90)
                        }),
                );
            });
        }

        // Add channel button (vertical, on the right)
        ui.vertical(|ui| {
            ui.add_space(20.0);
            if ui.small_button("+").on_hover_text("Add channel").clicked() && *channel_count < 8 {
                *channel_count += 1;
                gains.push(0.8);
            }
            if ui
                .small_button("−")
                .on_hover_text("Remove channel")
                .clicked()
                && *channel_count > 2
            {
                // Disconnect the removed channel's ports before shrinking
                let removed_ch = *channel_count - 1;
                pending_disconnects.push((node_id, removed_ch * 2)); // audio port
                pending_disconnects.push((node_id, removed_ch * 2 + 1)); // gain port
                *channel_count -= 1;
                gains.truncate(*channel_count);
            }
        });
    });

    ui.separator();

    // ── Output port (right-aligned) ───────────────────────────────────
    crate::nodes::audio_port_row(
        ui,
        "Mix Out",
        node_id,
        0,
        false,
        port_positions,
        dragging_from,
        connections,
        pending_disconnects,
        PortKind::Audio,
    );
    // Mixer source registration is handled by build_audio_chains() in app/mod.rs,
    // which correctly walks backward through effect nodes to find the actual Synth sources.
    // Calling set_mixer() here would incorrectly register effect nodes (e.g. Delay) as
    // mixer inputs instead of the actual audio sources.
    let _ = audio; // suppress unused warning
}
