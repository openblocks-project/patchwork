use eframe::egui::{self, RichText, Sense};
use std::time::Instant;
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Spectral Synth Node ──────────────────────────────────────────────────────
//
// Image → Audio via additive synthesis.
// Reads an image column by column, converts pixel brightness to 64 frequency
// bin magnitudes, and synthesizes audio from 64 sine oscillators.
// Frequencies match the spectrogram bins (log-spaced 30 Hz → Nyquist).
//
// Ports:
//   In  0: Image  (Image)     In  1: Speed (Number)    In  2: Volume (Normalized)
//   Out 0: Audio  (Audio)     Out 1: Phase (Normalized)

const NUM_BINS: usize = 256;
// Params layout:
//   [0..256]      magnitudes (sqrt-encoded, 0..1)
//   [256..512]    phases (normalized 0..1, 0.5 = 0 rad)
//   [512]         volume
//   [513]         gate
//   [514]         frame_seq (monotonic counter — DSP phase-resets when it changes)
const TOTAL_PARAMS: usize = 515;
const PARAM_PHASE_OFFSET: usize = 256;
const PARAM_VOLUME: usize = 512;
const PARAM_GATE: usize = 513;
const PARAM_FRAME_SEQ: usize = 514;

// Default playback rate in columns/sec. Matches Frame Recorder's default
// base_fps (48000 / SPECTRUM_FFT_SIZE), so speed=1.0 plays back an image
// recorded at the default rate in real time.
const DEFAULT_PLAYBACK_RATE: f32 = 93.75;

#[derive(Debug, Clone)]
pub struct SpectralSynthNode {
    pub speed: f32,
    pub volume: f32,
    pub phase: f32,
    pub running: bool,
    pub playback_rate: f32,
    pub gain_db: f32,
    pub magnitudes: [f32; NUM_BINS],
    /// Per-bin phase read from the image's alpha channel, normalized 0..1
    /// where 0.5 = 0 radians. Decoded on the DSP side.
    pub phases_in: [f32; NUM_BINS],
    params_cache: Vec<f32>,
    last_instant: Instant,
    cached_input_image: Option<std::sync::Arc<crate::graph::ImageData>>,
    last_trigger: f32,
    /// Monotonic counter incremented each time the image column the node
    /// samples from changes. DSP watches this — on change, it hard-resets
    /// each oscillator's phase to the stored image phase so voice harmonics
    /// stay phase-coherent across frames.
    frame_seq: u32,
    /// Last image column index we sampled, to detect column crossings.
    last_column: i64,
}

impl Default for SpectralSynthNode {
    fn default() -> Self {
        Self {
            speed: 1.0,
            volume: 0.8,
            phase: 0.0,
            running: false, // starts paused — use trigger or play button
            playback_rate: DEFAULT_PLAYBACK_RATE,
            // 0 dB is the correct default now that encoding is sqrt(mag) and
            // decode is v² — the round-trip is mathematically exact, no
            // compensation needed. User can bump Gain for quiet mic input.
            gain_db: 0.0,
            magnitudes: [0.0; NUM_BINS],
            phases_in: [0.5; NUM_BINS],
            params_cache: vec![0.0; TOTAL_PARAMS],
            last_instant: Instant::now(),
            cached_input_image: None,
            last_trigger: 0.0,
            frame_seq: 0,
            last_column: -1,
        }
    }
}

impl NodeBehavior for SpectralSynthNode {
    fn title(&self)      -> &str   { "Spectral Synth" }
    fn type_tag(&self)   -> &str   { "spectral_synth" }
    fn color_hint(&self) -> [u8;3] { [255, 100, 160] }
    fn min_width(&self)  -> Option<f32> { Some(230.0) }
    fn inline_ports(&self) -> bool { true }
    fn needs_cpu_image_input(&self, _port: usize) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image",   PortKind::Image),
            PortDef::new("Speed",   PortKind::Number),
            PortDef::new("Volume",  PortKind::Normalized),
            PortDef::new("Seek",    PortKind::Normalized),
            PortDef::new("Trigger", PortKind::Trigger),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Audio", PortKind::Audio),
            PortDef::new("Phase", PortKind::Normalized),
        ]
    }

    fn audio_params(&self) -> &[f32] { &self.params_cache }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Capture the input image for render_with_context to display
        if let Some(PortValue::Image(img)) = inputs.first() {
            self.cached_input_image = Some(img.clone());
        }
        // Magnitudes are extracted in render_with_context (runs every frame)
        vec![(1, PortValue::Float(self.phase))]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim  = egui::Color32::from_rgb(110, 110, 120);
        let blue = egui::Color32::from_rgb(100, 180, 240);

        let wired: Vec<bool> = (0..5).map(|p|
            ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == p)
        ).collect();

        // ── Advance scan phase ───────────────────────────────────────────
        let now = Instant::now();
        let dt = now.duration_since(self.last_instant).as_secs_f32().min(0.25);
        self.last_instant = now;

        // Read speed/volume overrides
        if wired[1] {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 1);
            if let PortValue::Float(f) = v { self.speed = f; }
        }
        if wired[2] {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 2);
            if let PortValue::Float(f) = v { self.volume = f.clamp(0.0, 1.0); }
        }

        // Seek input — overrides phase directly
        if wired[3] {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 3);
            if let PortValue::Float(f) = v { self.phase = f.clamp(0.0, 1.0); }
        }

        // Trigger input — rising edge starts playback from 0
        if wired[4] {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 4);
            let trig = v.as_float();
            if trig > 0.5 && self.last_trigger <= 0.5 {
                self.phase = 0.0;
                self.running = true;
            }
            self.last_trigger = trig;
        }

        // Wall-clock phase advancement: speed=1.0 at playback_rate=rec_rate
        // means playback takes the same wall time as recording.
        if self.running {
            let img_w = self.cached_input_image.as_ref()
                .map(|img| img.width.max(1) as f32)
                .unwrap_or(256.0);
            let step = dt * self.speed * self.playback_rate / img_w;
            self.phase += step;
            if self.phase >= 1.0 { self.phase = self.phase.fract(); }
            if self.phase < 0.0 { self.phase = self.phase.rem_euclid(1.0); }
        }

        // Extract magnitudes AND phases from cached image at current phase.
        //
        // R=G=B carries sqrt-encoded magnitude. Alpha carries phase encoded
        // as 0..1 = −π..π (0.5 = 0 rad). v² decode inverts the sqrt encoding
        // exactly; phase is passed through to the DSP which locks each
        // oscillator's phase to the image value on each new frame.
        let mut new_column: i64 = -1;
        if let Some(ref img) = self.cached_input_image {
            let w = img.width;
            let h = img.height;
            if w > 0 && h > 0 {
                let col = (self.phase * w as f32).floor().min((w - 1) as f32) as u32;
                new_column = col as i64;
                for bin in 0..NUM_BINS {
                    let y = ((NUM_BINS - 1 - bin) as f32 / (NUM_BINS - 1) as f32 * (h - 1) as f32) as u32;
                    let y = y.min(h - 1);
                    let idx = ((y * w + col) * 4) as usize;
                    if idx + 3 < img.pixels.len() {
                        let r = img.pixels[idx]     as f32 / 255.0;
                        let g = img.pixels[idx + 1] as f32 / 255.0;
                        let b = img.pixels[idx + 2] as f32 / 255.0;
                        let a = img.pixels[idx + 3] as f32 / 255.0;
                        let v = (r + g + b) / 3.0;
                        // Floor at 0.04 kills the analysis noise baseline
                        // plus frame-to-frame spillover jitter in quiet bins.
                        // Only confidently-lit bins get synthesized.
                        self.magnitudes[bin] = if v < 0.04 { 0.0 } else { v * v };
                        self.phases_in[bin] = a;
                    } else {
                        self.magnitudes[bin] = 0.0;
                        self.phases_in[bin] = 0.5;
                    }
                }
            }
        } else {
            self.magnitudes = [0.0; NUM_BINS];
            self.phases_in = [0.5; NUM_BINS];
        }
        // Bump frame_seq whenever we land on a new image column. The DSP
        // watches this to phase-reset its oscillators, which is what
        // preserves voice harmonic coherence across the column boundary.
        if new_column != self.last_column && new_column >= 0 {
            self.frame_seq = self.frame_seq.wrapping_add(1);
            self.last_column = new_column;
        }

        // ── Update params cache (read by audio_params() trait method) ────
        if self.params_cache.len() < TOTAL_PARAMS {
            self.params_cache.resize(TOTAL_PARAMS, 0.0);
        }
        // Gain applied pre-DSP-clamp: the processor clamps each magnitude to
        // [0,1], so gain > 0 dB acts as soft saturation on loud bins rather
        // than linear boost. Desirable for voice — gives a little warmth.
        let gain_linear = 10f32.powf(self.gain_db / 20.0);
        for i in 0..NUM_BINS {
            self.params_cache[i] = self.magnitudes[i] * gain_linear;
            self.params_cache[PARAM_PHASE_OFFSET + i] = self.phases_in[i];
        }
        self.params_cache[PARAM_VOLUME] = self.volume;
        // Gate is ON whenever we have an image — running only controls playhead movement.
        // This way, changing visuals produce sound even when playhead is paused.
        let has_signal = self.cached_input_image.is_some() && self.magnitudes.iter().any(|&m| m > 0.001);
        self.params_cache[PARAM_GATE] = if has_signal { 1.0 } else { 0.0 };
        self.params_cache[PARAM_FRAME_SEQ] = self.frame_seq as f32;

        // ── Input ports ──────────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            ui.label(RichText::new(if wired[0] { "Image ✓" } else { "Image" }).small()
                .color(if wired[0] { blue } else { dim }));
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("Speed").small().color(dim));
            if !wired[1] {
                ui.add(egui::DragValue::new(&mut self.speed).speed(0.1).range(0.1..=50.0).max_decimals(1));
            } else {
                ui.label(RichText::new(format!("{:.1}", self.speed)).small().monospace().color(blue));
            }
        });

        // Rate = image columns per second at speed=1.0. Match Frame Recorder's
        // base_fps to get record-duration == playback-duration.
        ui.horizontal(|ui| {
            ui.add_space(14.0); // align with other port rows (no port circle here)
            ui.label(RichText::new("Rate").small().color(dim));
            ui.add(
                egui::DragValue::new(&mut self.playback_rate)
                    .speed(0.25)
                    .range(1.0..=240.0)
                    .max_decimals(2),
            );
            ui.label(RichText::new("col/s").small().color(dim));
        });

        // Gain (dB) — compensates for the dB-encoded grayscale's squashing
        // of quiet signals. Raise for mic / voice sources, drop for loud
        // oscillators if the output clips.
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(RichText::new("Gain").small().color(dim));
            ui.add(
                egui::DragValue::new(&mut self.gain_db)
                    .speed(0.5)
                    .range(-24.0..=40.0)
                    .max_decimals(1),
            );
            ui.label(RichText::new("dB").small().color(dim));
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 2, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized);
            ui.label(RichText::new("Volume").small().color(dim));
            if !wired[2] {
                ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false));
                ui.label(RichText::new(format!("{:.0}%", self.volume * 100.0)).small().monospace());
            } else {
                ui.label(RichText::new(format!("{:.0}%", self.volume * 100.0)).small().monospace().color(blue));
            }
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 3, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized);
            ui.label(RichText::new("Seek").small().color(dim));
            if !wired[3] {
                ui.add(egui::Slider::new(&mut self.phase, 0.0..=1.0).show_value(false));
                ui.label(RichText::new(format!("{:.1}%", self.phase * 100.0)).small().monospace());
            } else {
                ui.label(RichText::new(format!("{:.1}%", self.phase * 100.0)).small().monospace().color(blue));
            }
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 4, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
            ui.label(RichText::new("Trigger").small().color(
                if wired[4] { egui::Color32::from_rgb(255, 150, 60) } else { dim }));
            if !wired[4] {
                if ui.small_button("▶ Play").clicked() {
                    self.phase = 0.0;
                    self.running = true;
                }
            }
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Controls ─────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            if ui.small_button(if self.running { "⏸" } else { "▶" }).clicked() { self.running = !self.running; }
            if ui.small_button("↺").clicked() { self.phase = 0.0; }
        });

        ui.add_space(2.0);

        // ── Image preview with playhead ──────────────────────────────────
        let preview_w = ui.available_width().max(120.0).min(240.0);
        let preview_h;

        if let Some(ref img) = self.cached_input_image {
            let aspect = img.height as f32 / img.width.max(1) as f32;
            preview_h = (preview_w * aspect).clamp(40.0, 160.0);

            // Render image as egui texture
            let tex_id = egui::Id::new(("spectral_img_tex", node_id));
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [img.width as usize, img.height as usize], &img.pixels);
            let texture = ui.ctx().data_mut(|d| d.get_temp::<egui::TextureHandle>(tex_id));
            let tex = if let Some(mut existing) = texture {
                existing.set(color_image, egui::TextureOptions::LINEAR);
                existing
            } else {
                ui.ctx().load_texture(format!("spectral_{}", node_id), color_image, egui::TextureOptions::LINEAR)
            };
            ui.ctx().data_mut(|d| d.insert_temp(tex_id, tex.clone()));

            let (rect, response) = ui.allocate_exact_size(egui::vec2(preview_w, preview_h), Sense::click_and_drag());
            let painter = ui.painter_at(rect);

            // Draw image
            painter.image(tex.id(), rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE);

            // Playhead line
            let ph_x = rect.min.x + self.phase.clamp(0.0, 1.0) * rect.width();
            painter.line_segment(
                [egui::pos2(ph_x, rect.min.y), egui::pos2(ph_x, rect.max.y)],
                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 255, 100)));
            // Playhead triangle at top
            let tri_sz = 5.0;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(ph_x - tri_sz, rect.min.y),
                    egui::pos2(ph_x + tri_sz, rect.min.y),
                    egui::pos2(ph_x, rect.min.y + tri_sz * 1.5),
                ],
                egui::Color32::from_rgb(255, 255, 100),
                egui::Stroke::NONE,
            ));

            // Click/drag to seek
            if response.clicked() || response.dragged() {
                if let Some(pos) = response.interact_pointer_pos() {
                    self.phase = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                }
            }
        } else {
            preview_h = 60.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(preview_w, preview_h), Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 3.0, egui::Color32::from_rgb(18, 18, 22));
            painter.text(rect.center(), egui::Align2::CENTER_CENTER,
                "Connect an image", egui::FontId::proportional(12.0), dim);
        }

        ui.add_space(2.0);

        // ── Mini spectrum bar chart (current magnitudes) ─────────────────
        let bar_total_w = ui.available_width().max(100.0).min(240.0);
        let bar_h = 28.0;
        let (bar_rect, _) = ui.allocate_exact_size(egui::vec2(bar_total_w, bar_h), Sense::hover());
        let bar_painter = ui.painter_at(bar_rect);
        bar_painter.rect_filled(bar_rect, 2.0, egui::Color32::from_rgb(14, 14, 18));

        let bw = bar_rect.width() / NUM_BINS as f32;
        for (i, &v) in self.magnitudes.iter().enumerate() {
            let h = bar_rect.height() * v.clamp(0.0, 1.0);
            if h > 0.5 {
                let x0 = bar_rect.min.x + i as f32 * bw;
                let bar = egui::Rect::from_min_size(
                    egui::pos2(x0, bar_rect.max.y - h),
                    egui::vec2((bw - 0.5).max(1.0), h),
                );
                let t = i as f32 / NUM_BINS as f32;
                let r = (255.0 * (1.0 - t * 0.5)) as u8;
                let g = (100.0 + 120.0 * t) as u8;
                let bv = (150.0 + 105.0 * t) as u8;
                bar_painter.rect_filled(bar, 0.0, egui::Color32::from_rgb(r, g, bv));
            }
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports ─────────────────────────────────────────────────
        crate::nodes::output_port_row(ui, "Audio", "", node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Audio);
        crate::nodes::output_port_row(ui, "Phase", &format!("{:.2}", self.phase), node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Normalized);

        if self.running {
            ui.ctx().request_repaint();
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "speed": self.speed,
            "volume": self.volume,
            "running": self.running,
            "playback_rate": self.playback_rate,
            "gain_db": self.gain_db,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("speed").and_then(|v| v.as_f64()) { self.speed = v as f32; }
        if let Some(v) = state.get("volume").and_then(|v| v.as_f64()) { self.volume = v as f32; }
        if let Some(v) = state.get("running").and_then(|v| v.as_bool()) { self.running = v; }
        if let Some(v) = state.get("playback_rate").and_then(|v| v.as_f64()) {
            self.playback_rate = (v as f32).max(0.01);
        }
        if let Some(v) = state.get("gain_db").and_then(|v| v.as_f64()) {
            self.gain_db = (v as f32).clamp(-24.0, 40.0);
        }
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("spectral_synth", |state| {
        let mut n = SpectralSynthNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
