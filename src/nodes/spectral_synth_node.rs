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

const NUM_BINS: usize = 64;

#[derive(Debug, Clone)]
pub struct SpectralSynthNode {
    pub speed: f32,
    pub volume: f32,
    pub phase: f32,
    pub running: bool,
    pub magnitudes: [f32; NUM_BINS],
    params_cache: Vec<f32>,
    last_instant: Instant,
    cached_input_image: Option<std::sync::Arc<crate::graph::ImageData>>,
    last_trigger: f32,
}

impl Default for SpectralSynthNode {
    fn default() -> Self {
        Self {
            speed: 1.0,
            volume: 0.8,
            phase: 0.0,
            running: false, // starts paused — use trigger or play button
            magnitudes: [0.0; NUM_BINS],
            params_cache: vec![0.0; 67],
            last_instant: Instant::now(),
            cached_input_image: None,
            last_trigger: 0.0,
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

        let wired: Vec<bool> = (0..4).map(|p|
            ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == p)
        ).collect();

        // ── Advance scan phase ───────────────────────────────────────────
        let now = Instant::now();
        let _dt = now.duration_since(self.last_instant).as_secs_f64().min(0.25);
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

        // Trigger input — rising edge starts playback from 0
        if wired[3] {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 3);
            let trig = v.as_float();
            if trig > 0.5 && self.last_trigger <= 0.5 {
                self.phase = 0.0;
                self.running = true;
            }
            self.last_trigger = trig;
        }

        // Pixel-stepping phase advancement
        if self.running {
            let img_w = self.cached_input_image.as_ref()
                .map(|img| img.width.max(1) as f32)
                .unwrap_or(256.0);
            let step = self.speed / img_w;
            self.phase += step;
            if self.phase >= 1.0 { self.phase = self.phase.fract(); }
            if self.phase < 0.0 { self.phase = 0.0; }
        }

        // Extract magnitudes from cached image at current phase
        if let Some(ref img) = self.cached_input_image {
            let w = img.width;
            let h = img.height;
            if w > 0 && h > 0 {
                let col = (self.phase * w as f32).floor().min((w - 1) as f32) as u32;
                for bin in 0..NUM_BINS {
                    let y = ((NUM_BINS - 1 - bin) as f32 / (NUM_BINS - 1) as f32 * (h - 1) as f32) as u32;
                    let y = y.min(h - 1);
                    let idx = ((y * w + col) * 4) as usize;
                    if idx + 2 < img.pixels.len() {
                        let r = img.pixels[idx]     as f32 / 255.0;
                        let g = img.pixels[idx + 1] as f32 / 255.0;
                        let b = img.pixels[idx + 2] as f32 / 255.0;
                        self.magnitudes[bin] = (r + g + b) / 3.0;
                    } else {
                        self.magnitudes[bin] = 0.0;
                    }
                }
            }
        } else {
            self.magnitudes = [0.0; NUM_BINS];
        }

        // ── Update params cache (read by audio_params() trait method) ────
        if self.params_cache.len() < 67 { self.params_cache.resize(67, 0.0); }
        for i in 0..NUM_BINS {
            self.params_cache[i] = self.magnitudes[i];
        }
        self.params_cache[64] = self.volume;
        self.params_cache[65] = if self.running { 1.0 } else { 0.0 };
        self.params_cache[66] = 0.0;

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
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
            ui.label(RichText::new("Trigger").small().color(
                if wired[3] { egui::Color32::from_rgb(255, 150, 60) } else { dim }));
            if !wired[3] {
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
            ui.add_space(4.0);
            // Seek DragValue
            ui.label(RichText::new("Seek").small().color(dim));
            ui.add(egui::DragValue::new(&mut self.phase).speed(0.002).range(0.0..=1.0).max_decimals(3));
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
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("speed").and_then(|v| v.as_f64()) { self.speed = v as f32; }
        if let Some(v) = state.get("volume").and_then(|v| v.as_f64()) { self.volume = v as f32; }
        if let Some(v) = state.get("running").and_then(|v| v.as_bool()) { self.running = v; }
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
