//! Spectrum Analyzer node — FFT-based bar visualizer.
//!
//! Reads pre-computed spectrum bins from `audio_manager.spectrum_results`
//! (the audio thread runs the FFT in `SpectrumProcessor`), draws them
//! as a vertical bar display, and exposes scalar reactivity ports
//! (Peak Hz, Centroid Hz, Energy) plus a rasterized Image output.
//!
//! Bin values are stored per-frame in egui temp data by `app/mod.rs`
//! before the eval pass, so this renderer just reads + draws.

use crate::audio::AudioManager;
use crate::audio::analysis::SPECTRUM_BINS;
use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _audio: &AudioManager,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    connections: &[Connection],
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let dim = egui::Color32::from_rgb(110, 110, 120);

    // ── Audio input port (left side) ─────────────────────────────────
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(
            ui, node_id, 0, true, connections,
            port_positions, dragging_from, pending_disconnects, PortKind::Audio,
        );
        let source_name: String = ui.ctx().data_mut(|d|
            d.get_temp(egui::Id::new(("spectrum_source", node_id))).unwrap_or_default());
        if source_name.is_empty() {
            ui.label(egui::RichText::new("Audio").small().color(dim));
        } else {
            ui.label(egui::RichText::new(format!("← {}", source_name)).small()
                .color(egui::Color32::from_rgb(80, 200, 120)));
        }
    });

    // Read bins stashed by app/mod.rs
    let (bins, peak_hz, centroid_hz, energy) = ui.ctx().data_mut(|d| {
        let bins: Vec<f32> = d.get_temp(egui::Id::new(("spectrum_bins", node_id)))
            .unwrap_or_else(|| vec![0.0f32; SPECTRUM_BINS]);
        let scalars: [f32; 3] = d.get_temp(egui::Id::new(("spectrum_scalars", node_id)))
            .unwrap_or([0.0; 3]);
        (bins, scalars[0], scalars[1], scalars[2])
    });

    // ── Bar visualization ────────────────────────────────────────────
    let display_w = ui.available_width().max(120.0).min(250.0);
    let display_h = 90.0f32;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(display_w, display_h), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, egui::Color32::from_rgb(18, 18, 22));

    let n = bins.len().max(1);
    let bar_w = rect.width() / n as f32;
    for (i, &v) in bins.iter().enumerate() {
        let h = rect.height() * v.clamp(0.0, 1.0);
        let x0 = rect.min.x + i as f32 * bar_w;
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(x0, rect.max.y - h),
            egui::vec2((bar_w - 1.0).max(1.0), h),
        );
        let t = i as f32 / n as f32;
        let r = (255.0 * (1.0 - t * 0.6)) as u8;
        let g = (120.0 + 80.0 * (1.0 - (t - 0.5).abs() * 2.0).max(0.0)) as u8;
        let b = (80.0 + 175.0 * t) as u8;
        painter.rect_filled(bar_rect, 0.0, egui::Color32::from_rgb(r, g, b));
    }

    ui.add_space(2.0);
    ui.separator();
    ui.add_space(2.0);

    // ── Output ports (right side) ───────────────────────────────────
    crate::nodes::output_port_row(ui, "Image", "256×96", node_id, 0,
        port_positions, dragging_from, connections, pending_disconnects, PortKind::Image);
    crate::nodes::output_port_row(ui, "Peak", &format!("{:.0} Hz", peak_hz), node_id, 1,
        port_positions, dragging_from, connections, pending_disconnects, PortKind::Number);
    crate::nodes::output_port_row(ui, "Centroid", &format!("{:.0} Hz", centroid_hz), node_id, 2,
        port_positions, dragging_from, connections, pending_disconnects, PortKind::Number);
    crate::nodes::output_port_row(ui, "Energy", &format!("{:.0}%", energy * 100.0), node_id, 3,
        port_positions, dragging_from, connections, pending_disconnects, PortKind::Normalized);

    if energy > 0.001 {
        ui.ctx().request_repaint();
    }
}

/// Rasterize a spectrum bin slice into an `ImageData` (RGBA8) suitable
/// for `PortValue::Image`. Used by `app/mod.rs` after reading the
/// shared `Spectrum` each frame.
pub fn rasterize_bins(bins: &[f32], width: u32, height: u32) -> ImageData {
    let w = width.max(1) as usize;
    let h = height.max(1) as usize;
    let mut pixels = vec![0u8; w * h * 4];
    let n = bins.len().max(1);
    for y in 0..h {
        for x in 0..w {
            let bi = (x * n) / w;
            let v = bins.get(bi).copied().unwrap_or(0.0).clamp(0.0, 1.0);
            let bar_h = (v * h as f32) as usize;
            let lit = (h - y) <= bar_h;
            let idx = (y * w + x) * 4;
            if lit {
                let t = bi as f32 / n as f32;
                let r = (255.0 * (1.0 - t * 0.6)) as u8;
                let g = (120.0 + 80.0 * (1.0 - (t - 0.5).abs() * 2.0).max(0.0)) as u8;
                let b = (80.0 + 175.0 * t) as u8;
                pixels[idx] = r;
                pixels[idx + 1] = g;
                pixels[idx + 2] = b;
                pixels[idx + 3] = 255;
            } else {
                pixels[idx] = 18;
                pixels[idx + 1] = 18;
                pixels[idx + 2] = 22;
                pixels[idx + 3] = 255;
            }
        }
    }
    ImageData::new(width, height, pixels)
}
