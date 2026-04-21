use eframe::egui::{self, RichText, Sense};
use std::sync::Arc;
use crate::audio::analysis::SPECTRUM_BINS;
use crate::graph::{ImageData, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Music Visualizer Node ────────────────────────────────────────────────────
//
// Audio-reactive visualizations: spectrogram, bars, waveform, rings.
// Reads FFT bins from the audio engine (same pipeline as SpectrumAnalyzer)
// and renders them as output images + Bass/Mid/Treble scalar outputs.

const HISTORY_W: usize = 256;
// One image row per spectrum bin so the visualizer → recorder → spectral-synth
// chain stays lossless vertically.
const IMG_H: usize = SPECTRUM_BINS;

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
enum VizMode { Spectrogram, Bars, Waveform, Rings }

#[derive(Debug, Clone)]
pub struct MusicVisualizerNode {
    mode: VizMode,
    history: Vec<[f32; SPECTRUM_BINS]>,
    /// Phase history parallel to `history`, stored normalized 0..1
    /// where 0.5 = 0 radians. Initialized to 0.5 (neutral).
    phase_history: Vec<[f32; SPECTRUM_BINS]>,
    history_pos: usize,
    bass: f32,
    mid: f32,
    treble: f32,
    cached_image: Option<Arc<ImageData>>,
}

impl Default for MusicVisualizerNode {
    fn default() -> Self {
        Self {
            mode: VizMode::Spectrogram,
            history: vec![[0.0; SPECTRUM_BINS]; HISTORY_W],
            phase_history: vec![[0.5; SPECTRUM_BINS]; HISTORY_W],
            history_pos: 0,
            bass: 0.0, mid: 0.0, treble: 0.0,
            cached_image: None,
        }
    }
}

// ── Color helpers ────────────────────────────────────────────────────────────

fn heat_color(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    if t < 0.2 {
        let f = t / 0.2;
        (0, (40.0 * f) as u8, (20.0 + 100.0 * f) as u8)
    } else if t < 0.4 {
        let f = (t - 0.2) / 0.2;
        (0, (40.0 + 120.0 * f) as u8, (120.0 + 80.0 * f) as u8)
    } else if t < 0.6 {
        let f = (t - 0.4) / 0.2;
        ((80.0 * f) as u8, (160.0 + 60.0 * f) as u8, (200.0 - 80.0 * f) as u8)
    } else if t < 0.8 {
        let f = (t - 0.6) / 0.2;
        ((80.0 + 175.0 * f) as u8, (220.0 + f * 0.0) as u8, (120.0 - 60.0 * f) as u8)
    } else {
        let f = (t - 0.8) / 0.2;
        (255, (220.0 + 35.0 * f) as u8, (60.0 + 195.0 * f) as u8)
    }
}

fn bar_color(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let r = (255.0 * (1.0 - t * 0.6)) as u8;
    let g = (120.0 + 80.0 * (1.0 - (t - 0.5).abs() * 2.0).max(0.0)) as u8;
    let b = (80.0 + 175.0 * t) as u8;
    (r, g, b)
}

fn band_energies(bins: &[f32]) -> (f32, f32, f32) {
    let n = bins.len();
    if n == 0 { return (0.0, 0.0, 0.0); }
    // Splits tuned for the linear 0–SPECTRUM_FREQ_MAX (8 kHz) bin layout in
    // analysis.rs: bass ≤ 250 Hz (1/32 of range), mid ≤ 2 kHz (1/4 of range).
    // Uses fractions of n so we stay correct if SPECTRUM_BINS ever changes.
    let bass_end = (n / 32).max(1);
    let mid_end = (n / 4).max(bass_end + 1);
    // Peak-in-band rather than mean-across-band. Voice/music concentrates
    // energy into a handful of bins per band; averaging the whole band
    // drowns loud peaks in the many silent-floor bins next to them.
    // Each bin is already time-smoothed in analysis.rs, so `max` is stable.
    let peak = |slice: &[f32]| -> f32 {
        slice.iter().copied().fold(0.0f32, f32::max)
    };
    let bass = peak(&bins[..bass_end]);
    let mid = peak(&bins[bass_end..mid_end]);
    let treble = peak(&bins[mid_end..]);
    (bass, mid, treble)
}

// ── Image renderers ──────────────────────────────────────────────────────────

fn render_spectrogram(
    history: &[[f32; SPECTRUM_BINS]],
    phase_history: &[[f32; SPECTRUM_BINS]],
    pos: usize,
    w: usize,
    h: usize,
) -> Vec<u8> {
    // R = G = B = magnitude * 255 (grayscale, visually unchanged).
    // Alpha = phase * 255. Phase is stored normalized 0..1 where 0.5 = 0 rad.
    // Alpha appears as full opacity under standard egui rendering, so the
    // spectrogram looks identical to users. Spectral Synth reads alpha back
    // and uses it for phase-coherent resynthesis — this is what makes voice
    // come out vocal rather than saw-like.
    let mut pixels = vec![0u8; w * h * 4];
    for x in 0..w {
        let col_idx = (pos + x) % w;
        let mag_col = &history[col_idx.min(history.len() - 1)];
        let phase_col = &phase_history[col_idx.min(phase_history.len() - 1)];
        for y in 0..h {
            let bin_idx = ((h - 1 - y) * SPECTRUM_BINS) / h; // flip: low freq at bottom
            let bin_idx = bin_idx.min(SPECTRUM_BINS - 1);
            let v = mag_col[bin_idx].clamp(0.0, 1.0);
            let p = phase_col[bin_idx].clamp(0.0, 1.0);
            let gray = (v * 255.0) as u8;
            let a = (p * 255.0) as u8;
            let i = (y * w + x) * 4;
            pixels[i] = gray; pixels[i+1] = gray; pixels[i+2] = gray; pixels[i+3] = a;
        }
    }
    pixels
}

fn render_bars(bins: &[f32], w: usize, h: usize) -> Vec<u8> {
    let mut pixels = vec![0u8; w * h * 4];
    let n = bins.len().max(1);
    for x in 0..w {
        let bi = (x * n) / w;
        let v = bins[bi.min(n - 1)].clamp(0.0, 1.0);
        let bar_h = (v * h as f32) as usize;
        let (r, g, b) = bar_color(bi as f32 / n as f32);
        // Mirror: bars grow from center
        let mid = h / 2;
        let half_bar = bar_h / 2;
        for y in 0..h {
            let dist_from_mid = if y >= mid { y - mid } else { mid - y };
            let lit = dist_from_mid <= half_bar;
            let i = (y * w + x) * 4;
            if lit {
                pixels[i] = r; pixels[i+1] = g; pixels[i+2] = b; pixels[i+3] = 255;
            } else {
                pixels[i] = 12; pixels[i+1] = 12; pixels[i+2] = 16; pixels[i+3] = 255;
            }
        }
    }
    pixels
}

fn render_waveform(bins: &[f32], w: usize, h: usize) -> Vec<u8> {
    let mut pixels = vec![12u8; w * h * 4]; // dark bg
    // Set alpha
    for i in (3..pixels.len()).step_by(4) { pixels[i] = 255; }

    let n = bins.len().max(1);
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let max_r = cx.min(cy) * 0.85;

    // Draw circular waveform
    let steps = 360;
    for s in 0..steps {
        let angle = (s as f32 / steps as f32) * std::f32::consts::TAU;
        let bin_idx = (s * n / steps).min(n - 1);
        let v = bins[bin_idx].clamp(0.0, 1.0);
        let r = max_r * (0.3 + 0.7 * v);

        let px = (cx + r * angle.cos()) as i32;
        let py = (cy + r * angle.sin()) as i32;
        let (cr, cg, cb) = bar_color(bin_idx as f32 / n as f32);

        // Draw thick dot
        for dy in -2..=2i32 {
            for dx in -2..=2i32 {
                let x = (px + dx).clamp(0, w as i32 - 1) as usize;
                let y = (py + dy).clamp(0, h as i32 - 1) as usize;
                let i = (y * w + x) * 4;
                pixels[i] = cr; pixels[i+1] = cg; pixels[i+2] = cb;
            }
        }
    }

    // Center dot
    for dy in -3..=3i32 {
        for dx in -3..=3i32 {
            let x = (cx as i32 + dx).clamp(0, w as i32 - 1) as usize;
            let y = (cy as i32 + dy).clamp(0, h as i32 - 1) as usize;
            let i = (y * w + x) * 4;
            pixels[i] = 255; pixels[i+1] = 255; pixels[i+2] = 255;
        }
    }
    pixels
}

fn render_rings(bins: &[f32], w: usize, h: usize) -> Vec<u8> {
    let mut pixels = vec![0u8; w * h * 4];
    let n = bins.len().max(1);
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let max_r = cx.min(cy);

    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let norm_dist = (dist / max_r).clamp(0.0, 1.0);

            // Map distance to bin (outer = bass/low, inner = treble/high)
            let bin_idx = ((1.0 - norm_dist) * (n - 1) as f32) as usize;
            let bin_idx = bin_idx.min(n - 1);
            let v = bins[bin_idx].clamp(0.0, 1.0);

            // Ring pattern: brightness pulses with magnitude
            let ring_phase = (norm_dist * 8.0).fract();
            let ring = (1.0 - (ring_phase - 0.5).abs() * 4.0).clamp(0.0, 1.0);
            let brightness = ring * v * 1.5;

            let (cr, cg, cb) = heat_color(v);
            let i = (y * w + x) * 4;
            pixels[i]   = (cr as f32 * brightness).min(255.0) as u8;
            pixels[i+1] = (cg as f32 * brightness).min(255.0) as u8;
            pixels[i+2] = (cb as f32 * brightness).min(255.0) as u8;
            pixels[i+3] = 255;
        }
    }
    pixels
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for MusicVisualizerNode {
    fn title(&self)      -> &str   { "Music Visualizer" }
    fn type_tag(&self)   -> &str   { "music_visualizer" }
    fn color_hint(&self) -> [u8;3] { [255, 120, 200] }
    fn min_width(&self)  -> Option<f32> { Some(240.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Audio", PortKind::Audio)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Image", PortKind::Image)]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        if let Some(ref img) = self.cached_image {
            vec![(0, PortValue::Image(img.clone()))]
        } else {
            vec![]
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim = egui::Color32::from_rgb(110, 110, 120);

        // ── Audio input port ─────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Audio);
            let source: String = ui.ctx().data_mut(|d|
                d.get_temp(egui::Id::new(("spectrum_source", node_id))).unwrap_or_default());
            if source.is_empty() {
                ui.label(RichText::new("Connect audio").small().color(dim));
            } else {
                ui.label(RichText::new(format!("← {}", source)).small()
                    .color(egui::Color32::from_rgb(80, 200, 120)));
            }
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Mode selector ────────────────────────────────────────────────
        ui.horizontal(|ui| {
            for (m, label) in [(VizMode::Spectrogram, "Spectrogram"), (VizMode::Bars, "Bars"),
                               (VizMode::Waveform, "Waveform"), (VizMode::Rings, "Rings")] {
                if ui.selectable_label(self.mode == m, RichText::new(label).small()).clicked() {
                    self.mode = m;
                }
            }
        });

        ui.add_space(2.0);

        // ── Read FFT bins + phases from egui temp ────────────────────────
        let bins: Vec<f32> = ui.ctx().data_mut(|d|
            d.get_temp(egui::Id::new(("spectrum_bins", node_id)))
                .unwrap_or_else(|| vec![0.0f32; SPECTRUM_BINS]));
        let phases: Vec<f32> = ui.ctx().data_mut(|d|
            d.get_temp(egui::Id::new(("spectrum_phases", node_id)))
                .unwrap_or_else(|| vec![0.5f32; SPECTRUM_BINS]));

        // Update history ring buffers (magnitude and phase in lockstep)
        let mut mag_frame = [0.0f32; SPECTRUM_BINS];
        let mut phase_frame = [0.5f32; SPECTRUM_BINS];
        for (i, &v) in bins.iter().take(SPECTRUM_BINS).enumerate() { mag_frame[i] = v; }
        for (i, &p) in phases.iter().take(SPECTRUM_BINS).enumerate() { phase_frame[i] = p; }
        if self.history.len() < HISTORY_W {
            self.history.resize(HISTORY_W, [0.0; SPECTRUM_BINS]);
        }
        if self.phase_history.len() < HISTORY_W {
            self.phase_history.resize(HISTORY_W, [0.5; SPECTRUM_BINS]);
        }
        self.history[self.history_pos % HISTORY_W] = mag_frame;
        self.phase_history[self.history_pos % HISTORY_W] = phase_frame;
        self.history_pos = (self.history_pos + 1) % HISTORY_W;

        // Band energies
        let (bass, mid, treble) = band_energies(&bins);
        self.bass = bass;
        self.mid = mid;
        self.treble = treble;

        // ── Render visualization image ───────────────────────────────────
        let img_w = HISTORY_W;
        let img_h = IMG_H;
        let pixels = match self.mode {
            VizMode::Spectrogram => render_spectrogram(&self.history, &self.phase_history, self.history_pos, img_w, img_h),
            VizMode::Bars        => render_bars(&bins, img_w, img_h),
            VizMode::Waveform    => render_waveform(&bins, img_w, img_h),
            VizMode::Rings       => render_rings(&bins, img_w, img_h),
        };
        let img = Arc::new(ImageData::new(img_w as u32, img_h as u32, pixels));
        self.cached_image = Some(img.clone());

        // ── Display in node ──────────────────────────────────────────────
        let preview_w = ui.available_width().max(120.0).min(260.0);
        let aspect = img_h as f32 / img_w as f32;
        let preview_h = (preview_w * aspect).min(140.0);

        let tex_id = egui::Id::new(("viz_tex", node_id));
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [img_w, img_h], &img.pixels);

        let texture = ui.ctx().data_mut(|d| d.get_temp::<egui::TextureHandle>(tex_id));
        let tex = if let Some(mut existing) = texture {
            existing.set(color_image, egui::TextureOptions::LINEAR);
            existing
        } else {
            ui.ctx().load_texture(format!("viz_{}", node_id), color_image, egui::TextureOptions::LINEAR)
        };
        ui.ctx().data_mut(|d| d.insert_temp(tex_id, tex.clone()));

        let (rect, _) = ui.allocate_exact_size(egui::vec2(preview_w, preview_h), Sense::hover());
        ui.painter().image(tex.id(), rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE);

        ui.add_space(2.0);

        // ── Band meters ──────────────────────────────────────────────────
        let bar_h = 6.0;
        let meter = |ui: &mut egui::Ui, label: &str, value: f32, color: egui::Color32| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).small().color(dim));
                let bw = ui.available_width() - 40.0;
                let bw = bw.max(30.0).min(100.0);
                let (r, _) = ui.allocate_exact_size(egui::vec2(bw, bar_h), Sense::hover());
                let p = ui.painter();
                p.rect_filled(r, 2.0, egui::Color32::from_rgb(25, 25, 30));
                let fw = r.width() * value.clamp(0.0, 1.0);
                if fw > 0.5 {
                    p.rect_filled(egui::Rect::from_min_size(r.min, egui::vec2(fw, r.height())), 2.0, color);
                }
                ui.label(RichText::new(format!("{:.0}%", value * 100.0)).small().monospace());
            });
        };
        meter(ui, "Bass",   self.bass,   egui::Color32::from_rgb(255, 80, 80));
        meter(ui, "Mid",    self.mid,    egui::Color32::from_rgb(80, 200, 255));
        meter(ui, "Treble", self.treble, egui::Color32::from_rgb(200, 120, 255));

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output port ──────────────────────────────────────────────────
        crate::nodes::output_port_row(ui, "Image", if self.cached_image.is_some() { "✓" } else { "—" }, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Image);

        if self.bass > 0.001 || self.mid > 0.001 || self.treble > 0.001 {
            ui.ctx().request_repaint();
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({ "mode": match self.mode {
            VizMode::Spectrogram => "spectrogram",
            VizMode::Bars => "bars",
            VizMode::Waveform => "waveform",
            VizMode::Rings => "rings",
        }})
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(m) = state.get("mode").and_then(|v| v.as_str()) {
            self.mode = match m {
                "bars" => VizMode::Bars,
                "waveform" => VizMode::Waveform,
                "rings" => VizMode::Rings,
                _ => VizMode::Spectrogram,
            };
        }
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("music_visualizer", |state| {
        let mut n = MusicVisualizerNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
