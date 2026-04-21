//! Frame Recorder node — an "Audio Sampler for images."
//!
//! Records incoming image frames over time into a rolling buffer,
//! selectable between three modes:
//!
//! - **Scroll**: shifts a fixed-size canvas left by `stride` columns each
//!   record tick and pastes the rightmost `stride` columns of the input on
//!   the right. Primary use case: spectrogram history from Spectrum
//!   Analyzer → Frame Recorder → Spectral Synth for audio round-trip.
//!
//! - **Strip**: thumbnails each captured frame into a fixed grid
//!   (contact-sheet view). Wraps row-by-row; oldest cell overwritten.
//!
//! - **Tape**: records whole frames into a bounded `Vec<Arc<ImageData>>`.
//!   Outputs the frame at the current phase (0–1). Scrubbable via Seek port.
//!
//! Recording timing lives in `evaluate()` — not the render path — so
//! behavior is independent of monitor refresh rate. `frame_stamp` is
//! bumped whenever the output image changes so the dyn_img_cache in
//! `app/mod.rs` picks up new frames.

use crate::graph::{ImageData, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum RecorderMode {
    Scroll,
    Strip,
    Tape,
}

impl Default for RecorderMode {
    fn default() -> Self {
        RecorderMode::Scroll
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameRecorderNode {
    // ── Common parameters ──
    #[serde(default)]
    pub mode: RecorderMode,
    #[serde(default = "default_base_fps")]
    pub base_fps: f32,
    #[serde(default = "default_speed")]
    pub speed: f32,
    #[serde(default = "default_true")]
    pub looping: bool,

    // ── Scroll mode ──
    #[serde(default = "default_scroll_w")]
    pub scroll_w: u32,
    #[serde(default = "default_scroll_h")]
    pub scroll_h: u32,
    #[serde(default = "default_stride")]
    pub scroll_stride: u32,

    // ── Strip mode ──
    #[serde(default = "default_strip_cols")]
    pub strip_cols: u32,
    #[serde(default = "default_strip_rows")]
    pub strip_rows: u32,
    #[serde(default = "default_cell_w")]
    pub cell_w: u32,
    #[serde(default = "default_cell_h")]
    pub cell_h: u32,

    // ── Tape mode ──
    #[serde(default = "default_max_frames")]
    pub max_frames: usize,
    #[serde(default)]
    pub phase: f32,

    // ── Manual Record toggle (used when Rec port is unwired) ──
    #[serde(default)]
    pub manual_rec: bool,

    // ── Cache-miss key — bumped on every output change ──
    #[serde(default)]
    pub frame_stamp: u64,

    // ── Runtime state (not persisted) ──
    #[serde(skip)]
    scroll_canvas: Vec<u8>,
    #[serde(skip)]
    strip_canvas: Vec<u8>,
    #[serde(skip)]
    tape: Vec<Arc<ImageData>>,
    #[serde(skip)]
    frames_recorded: u64,
    #[serde(skip)]
    last_clear_input: f32,
    #[serde(skip)]
    last_step_instant: Option<Instant>,
    #[serde(skip)]
    cached_output: Option<Arc<ImageData>>,
    #[serde(skip)]
    cached_scroll_dims: (u32, u32),
    #[serde(skip)]
    cached_strip_dims: (u32, u32, u32, u32), // cols, rows, cell_w, cell_h
    /// Most recent input image, regardless of recording state. Used to
    /// show a live preview in the UI before/between recordings so users
    /// can see that their camera / visualizer is actually connected.
    #[serde(skip)]
    latest_input: Option<Arc<ImageData>>,
}

// 93.75 Hz = 48000 / SPECTRUM_HOP_SIZE (512). Matches the Spectrum analyzer's
// new overlap FFT rate so Scroll-mode recording catches every column.
fn default_base_fps() -> f32 { 93.75 }
fn default_speed() -> f32 { 1.0 }
fn default_true() -> bool { true }
fn default_scroll_w() -> u32 { 512 }
// 256 = SPECTRUM_BINS. One row per bin keeps the round-trip lossless vertically.
fn default_scroll_h() -> u32 { 256 }
fn default_stride() -> u32 { 1 }
fn default_strip_cols() -> u32 { 8 }
fn default_strip_rows() -> u32 { 6 }
fn default_cell_w() -> u32 { 128 }
fn default_cell_h() -> u32 { 72 }
fn default_max_frames() -> usize { 64 }

impl Default for FrameRecorderNode {
    fn default() -> Self {
        Self {
            mode: RecorderMode::Scroll,
            base_fps: 93.75,
            speed: 1.0,
            looping: true,
            scroll_w: 512,
            scroll_h: 256,
            scroll_stride: 1,
            strip_cols: 8,
            strip_rows: 6,
            cell_w: 128,
            cell_h: 72,
            max_frames: 64,
            phase: 0.0,
            manual_rec: false,
            frame_stamp: 0,
            scroll_canvas: Vec::new(),
            strip_canvas: Vec::new(),
            tape: Vec::new(),
            frames_recorded: 0,
            last_clear_input: 0.0,
            last_step_instant: None,
            cached_output: None,
            cached_scroll_dims: (0, 0),
            cached_strip_dims: (0, 0, 0, 0),
            latest_input: None,
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

impl FrameRecorderNode {
    fn ensure_scroll_canvas(&mut self) {
        let dims = (self.scroll_w.max(1), self.scroll_h.max(1));
        if dims != self.cached_scroll_dims {
            let size = (dims.0 * dims.1 * 4) as usize;
            self.scroll_canvas = vec![0u8; size];
            self.cached_scroll_dims = dims;
            self.cached_output = None;
        }
    }

    fn ensure_strip_canvas(&mut self) {
        let dims = (
            self.strip_cols.max(1),
            self.strip_rows.max(1),
            self.cell_w.max(1),
            self.cell_h.max(1),
        );
        if dims != self.cached_strip_dims {
            let total_w = dims.0 * dims.2;
            let total_h = dims.1 * dims.3;
            let size = (total_w * total_h * 4) as usize;
            self.strip_canvas = vec![0u8; size];
            self.cached_strip_dims = dims;
            self.frames_recorded = 0;
            self.cached_output = None;
        }
    }

    fn clear_all(&mut self) {
        self.scroll_canvas.fill(0);
        self.strip_canvas.fill(0);
        self.tape.clear();
        self.frames_recorded = 0;
        self.phase = 0.0;
        self.cached_output = None;
        self.frame_stamp = self.frame_stamp.wrapping_add(1);
    }

    /// Record a single tick in Scroll mode: shift canvas left by `stride`
    /// columns, paste rightmost `stride` columns of input on the right.
    /// Input rows resampled to canvas height via nearest neighbour.
    fn record_step_scroll(&mut self, img: &ImageData) {
        if img.width == 0 || img.height == 0 || img.pixels.is_empty() {
            return;
        }
        self.ensure_scroll_canvas();
        let cw = self.scroll_w as usize;
        let ch = self.scroll_h as usize;
        let stride = (self.scroll_stride.max(1) as usize).min(cw);
        let row_bytes = cw * 4;
        let stride_bytes = stride * 4;
        let keep_bytes = (cw - stride) * 4;

        // Shift each row left by `stride` pixels (copy within the row).
        for y in 0..ch {
            let row_start = y * row_bytes;
            // Move bytes [stride..cw] → [0..cw-stride]
            self.scroll_canvas.copy_within(
                row_start + stride_bytes..row_start + row_bytes,
                row_start,
            );
            // Zero the rightmost `stride` pixels (about to be filled)
            self.scroll_canvas[row_start + keep_bytes..row_start + row_bytes].fill(0);
        }

        // Sample the rightmost `stride` columns of the input, nearest-neighbour
        // resampled to canvas height, and write into rightmost canvas slots.
        let src_col_start = img.width.saturating_sub(stride as u32) as usize;
        for y in 0..ch {
            let src_y = ((y as f32 / ch.max(1) as f32) * img.height as f32) as usize;
            let src_y = src_y.min(img.height as usize - 1);
            let src_row_start = src_y * img.width as usize * 4;
            let dest_row_start = y * row_bytes + keep_bytes;
            for sx in 0..stride {
                let src_x = src_col_start + sx;
                if src_x >= img.width as usize {
                    break;
                }
                let si = src_row_start + src_x * 4;
                let di = dest_row_start + sx * 4;
                if si + 3 < img.pixels.len() && di + 3 < self.scroll_canvas.len() {
                    self.scroll_canvas[di..di + 4].copy_from_slice(&img.pixels[si..si + 4]);
                }
            }
        }

        self.frames_recorded += 1;
        self.cached_output = None;
    }

    /// Record a single tick in Strip mode: thumbnail the input to cell
    /// size and blit into the next cell (row-by-row wrap).
    fn record_step_strip(&mut self, img: &ImageData) {
        if img.width == 0 || img.height == 0 || img.pixels.is_empty() {
            return;
        }
        self.ensure_strip_canvas();
        let cols = self.strip_cols.max(1) as usize;
        let rows = self.strip_rows.max(1) as usize;
        let cell_w = self.cell_w.max(1) as usize;
        let cell_h = self.cell_h.max(1) as usize;
        let total_w = cols * cell_w;

        let cell_idx = (self.frames_recorded as usize) % (cols * rows);
        let col = cell_idx % cols;
        let row = cell_idx / cols;
        let off_x = col * cell_w;
        let off_y = row * cell_h;

        let src_w = img.width as usize;
        let src_h = img.height as usize;

        for y in 0..cell_h {
            let sy = ((y as f32 / cell_h as f32) * src_h as f32) as usize;
            let sy = sy.min(src_h - 1);
            let src_row = sy * src_w * 4;
            let dest_row = (off_y + y) * total_w * 4 + off_x * 4;
            for x in 0..cell_w {
                let sx = ((x as f32 / cell_w as f32) * src_w as f32) as usize;
                let sx = sx.min(src_w - 1);
                let si = src_row + sx * 4;
                let di = dest_row + x * 4;
                if si + 3 < img.pixels.len() && di + 3 < self.strip_canvas.len() {
                    self.strip_canvas[di..di + 4].copy_from_slice(&img.pixels[si..si + 4]);
                }
            }
        }

        self.frames_recorded += 1;
        self.cached_output = None;
    }

    /// Record a frame to Tape: clone the Arc and push. Drop oldest at capacity.
    fn record_step_tape(&mut self, img: Arc<ImageData>) {
        if img.width == 0 || img.height == 0 {
            return;
        }
        if self.tape.len() >= self.max_frames.max(1) {
            self.tape.remove(0);
        }
        self.tape.push(img);
        self.frames_recorded += 1;
        self.cached_output = None;
    }

    /// Compute the current output image for the active mode.
    fn current_output(&mut self) -> Option<Arc<ImageData>> {
        match self.mode {
            RecorderMode::Scroll => {
                if self.scroll_canvas.is_empty() {
                    return None;
                }
                let (w, h) = self.cached_scroll_dims;
                // Output ONLY the filled (rightmost) portion of the canvas.
                // The scroll write path shifts left and writes on the right,
                // so left of `start_col` is always unwritten zeros. Without
                // this trim, short recordings play back as long stretches of
                // silence followed by the actual audio at the end.
                let stride = self.scroll_stride.max(1);
                let filled_cols = (self.frames_recorded as u32).saturating_mul(stride).min(w);
                if filled_cols == 0 {
                    return None;
                }
                if filled_cols >= w {
                    return Some(Arc::new(ImageData::new(w, h, self.scroll_canvas.clone())));
                }
                let start_col = (w - filled_cols) as usize;
                let w_usize = w as usize;
                let row_bytes = w_usize * 4;
                let new_row_bytes = filled_cols as usize * 4;
                let mut trimmed = Vec::with_capacity(new_row_bytes * h as usize);
                for y in 0..(h as usize) {
                    let row_start = y * row_bytes + start_col * 4;
                    let row_end = (y + 1) * row_bytes;
                    trimmed.extend_from_slice(&self.scroll_canvas[row_start..row_end]);
                }
                Some(Arc::new(ImageData::new(filled_cols, h, trimmed)))
            }
            RecorderMode::Strip => {
                if self.strip_canvas.is_empty() {
                    return None;
                }
                let (cols, rows, cw, ch) = self.cached_strip_dims;
                Some(Arc::new(ImageData::new(
                    cols * cw,
                    rows * ch,
                    self.strip_canvas.clone(),
                )))
            }
            RecorderMode::Tape => {
                if self.tape.is_empty() {
                    return None;
                }
                let idx = (self.phase.clamp(0.0, 1.0) * self.tape.len() as f32)
                    .floor()
                    .min((self.tape.len() - 1) as f32) as usize;
                Some(self.tape[idx].clone())
            }
        }
    }

    fn progress(&self) -> f32 {
        match self.mode {
            RecorderMode::Scroll => {
                let total_w = self.scroll_w as u64;
                let filled = (self.frames_recorded * self.scroll_stride.max(1) as u64).min(total_w);
                filled as f32 / total_w.max(1) as f32
            }
            RecorderMode::Strip => {
                let total_cells = (self.strip_cols.max(1) * self.strip_rows.max(1)) as f32;
                (self.frames_recorded as f32 / total_cells).clamp(0.0, 1.0)
            }
            RecorderMode::Tape => self.phase.clamp(0.0, 1.0),
        }
    }
}

// ── NodeBehavior ────────────────────────────────────────────────────────────

impl NodeBehavior for FrameRecorderNode {
    fn title(&self) -> &str {
        "Frame Recorder"
    }

    fn type_tag(&self) -> &str {
        "frame_recorder"
    }

    fn color_hint(&self) -> [u8; 3] {
        match self.mode {
            RecorderMode::Scroll => [60, 200, 180], // teal
            RecorderMode::Strip => [230, 180, 60],  // amber
            RecorderMode::Tape => [220, 80, 200],   // magenta
        }
    }

    fn min_width(&self) -> Option<f32> {
        Some(240.0)
    }

    fn inline_ports(&self) -> bool {
        true
    }

    fn needs_cpu_image_input(&self, _port: usize) -> bool {
        true
    }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image", PortKind::Image),
            PortDef::new("Rec", PortKind::Gate),
            PortDef::new("Speed", PortKind::Number),
            PortDef::new("Seek", PortKind::Normalized),
            PortDef::new("Clear", PortKind::Trigger),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image", PortKind::Image),
            PortDef::new("Progress", PortKind::Normalized),
            PortDef::new("Frames", PortKind::Number),
        ]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Port reads. Unconnected inputs arrive as PortValue::None — we use
        // that to decide whether to honour the Rec port or the manual toggle.
        let rec_wired = !matches!(inputs.get(1), Some(PortValue::None) | None);
        let rec_gate = inputs
            .get(1)
            .map(|v| v.as_float())
            .unwrap_or(0.0);
        let speed_override = match inputs.get(2) {
            Some(PortValue::Float(f)) => Some(*f),
            _ => None,
        };
        let seek_override = match inputs.get(3) {
            Some(PortValue::Float(f)) => Some(f.clamp(0.0, 1.0)),
            _ => None,
        };
        let clear_in = inputs
            .get(4)
            .map(|v| v.as_float())
            .unwrap_or(0.0);

        // Clear: rising-edge trigger
        if clear_in > 0.5 && self.last_clear_input <= 0.5 {
            self.clear_all();
        }
        self.last_clear_input = clear_in;

        // Apply Speed port override (record speed multiplier for all modes)
        if let Some(s) = speed_override {
            self.speed = s;
        }

        // Timing: fire one or more record/advance steps based on dt vs base_fps*speed.
        let now = Instant::now();
        let dt = self
            .last_step_instant
            .map(|t| now.duration_since(t).as_secs_f32().min(1.0))
            .unwrap_or(0.0);
        let step_rate = (self.base_fps * self.speed.abs()).max(0.001);
        let step_interval = 1.0 / step_rate;
        let mut steps_owed = if dt >= step_interval {
            (dt / step_interval).floor() as u32
        } else {
            0
        };
        // Cap catch-up
        steps_owed = steps_owed.min(8);

        let is_recording = if rec_wired { rec_gate > 0.5 } else { self.manual_rec };

        // Extract input image (CPU path). Also stash it in `latest_input`
        // so the node UI can show a live preview of the incoming stream
        // regardless of recording state — this is how users verify that
        // camera / visualizer output is actually flowing in. Cleared to
        // None when input disconnects so the "Connect image" hint comes
        // back.
        let input_img: Option<Arc<ImageData>> = match inputs.first() {
            Some(PortValue::Image(img)) => Some(img.clone()),
            _ => None,
        };
        self.latest_input = input_img.clone();

        let mut output_changed = false;

        if steps_owed > 0 {
            self.last_step_instant = Some(now);

            for _ in 0..steps_owed {
                if is_recording {
                    if let Some(ref img) = input_img {
                        match self.mode {
                            RecorderMode::Scroll => self.record_step_scroll(img),
                            RecorderMode::Strip => self.record_step_strip(img),
                            RecorderMode::Tape => self.record_step_tape(img.clone()),
                        }
                        output_changed = true;
                    }
                } else if matches!(self.mode, RecorderMode::Tape) && !self.tape.is_empty() {
                    // Playback: advance phase unless Seek port is driving it
                    if seek_override.is_none() && self.speed.abs() > 0.0001 {
                        let delta = self.speed.signum() * step_interval / self.tape.len() as f32;
                        self.phase += delta;
                        if self.looping {
                            self.phase = self.phase.rem_euclid(1.0);
                        } else {
                            self.phase = self.phase.clamp(0.0, 1.0);
                        }
                        output_changed = true;
                    }
                }
            }
        } else if self.last_step_instant.is_none() {
            self.last_step_instant = Some(now);
        }

        // Seek override wins over speed-advancement for Tape.
        if let Some(s) = seek_override {
            if matches!(self.mode, RecorderMode::Tape) {
                if (self.phase - s).abs() > 1e-4 {
                    self.phase = s;
                    output_changed = true;
                }
            }
        }

        // Rebuild cached output if changed (or was cleared by ensure_* functions)
        if output_changed || self.cached_output.is_none() {
            self.cached_output = self.current_output();
            if output_changed {
                self.frame_stamp = self.frame_stamp.wrapping_add(1);
            }
        }

        let out_image = match &self.cached_output {
            Some(img) => PortValue::Image(img.clone()),
            None => PortValue::None,
        };

        vec![
            (0, out_image),
            (1, PortValue::Float(self.progress())),
            (2, PortValue::Float(self.frames_recorded as f32)),
        ]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<FrameRecorderNode>(state.clone()) {
            // Preserve runtime state; apply loaded parameters.
            self.mode = loaded.mode;
            self.base_fps = loaded.base_fps;
            self.speed = loaded.speed;
            self.looping = loaded.looping;
            self.scroll_w = loaded.scroll_w;
            self.scroll_h = loaded.scroll_h;
            self.scroll_stride = loaded.scroll_stride;
            self.strip_cols = loaded.strip_cols;
            self.strip_rows = loaded.strip_rows;
            self.cell_w = loaded.cell_w;
            self.cell_h = loaded.cell_h;
            self.max_frames = loaded.max_frames;
            self.phase = loaded.phase;
            self.manual_rec = loaded.manual_rec;
            self.frame_stamp = loaded.frame_stamp;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim = egui::Color32::from_rgb(110, 110, 120);
        let blue = egui::Color32::from_rgb(80, 170, 255);
        let accent = egui::Color32::from_rgb(self.color_hint()[0], self.color_hint()[1], self.color_hint()[2]);

        // ── Mode selector ───────────────────────────────────────────
        let prev_mode = self.mode;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Mode").small().color(dim));
            egui::ComboBox::from_id_salt(egui::Id::new(("frame_recorder_mode", node_id)))
                .selected_text(match self.mode {
                    RecorderMode::Scroll => "Scroll",
                    RecorderMode::Strip => "Strip",
                    RecorderMode::Tape => "Tape",
                })
                .width(90.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.mode, RecorderMode::Scroll, "Scroll");
                    ui.selectable_value(&mut self.mode, RecorderMode::Strip, "Strip");
                    ui.selectable_value(&mut self.mode, RecorderMode::Tape, "Tape");
                });
        });
        if self.mode != prev_mode {
            // Clear the other modes' buffers to free memory; keep current mode.
            match self.mode {
                RecorderMode::Scroll => {
                    self.strip_canvas = Vec::new();
                    self.cached_strip_dims = (0, 0, 0, 0);
                    self.tape.clear();
                }
                RecorderMode::Strip => {
                    self.scroll_canvas = Vec::new();
                    self.cached_scroll_dims = (0, 0);
                    self.tape.clear();
                }
                RecorderMode::Tape => {
                    self.scroll_canvas = Vec::new();
                    self.cached_scroll_dims = (0, 0);
                    self.strip_canvas = Vec::new();
                    self.cached_strip_dims = (0, 0, 0, 0);
                }
            }
            self.frames_recorded = 0;
            self.phase = 0.0;
            self.cached_output = None;
            self.frame_stamp = self.frame_stamp.wrapping_add(1);
        }

        // ── Input ports ─────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Image,
            );
            ui.label(egui::RichText::new("Image").small().color(dim));
        });

        let rec_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
        let rec_val = if rec_wired {
            crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 1).as_float()
        } else {
            0.0
        };
        let rec_active = if rec_wired { rec_val > 0.5 } else { self.manual_rec };
        let rec_red = egui::Color32::from_rgb(255, 80, 80);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Gate,
            );
            let label = if rec_active { "● Rec" } else { "Rec" };
            let color = if rec_active { rec_red } else { dim };
            ui.label(egui::RichText::new(label).small().color(color));
            if rec_wired {
                // Gate drives recording — disable the button but keep it visible
                // so the user sees the affordance even while the port is wired.
                ui.add_enabled(
                    false,
                    egui::Button::new(egui::RichText::new(if rec_active { "■" } else { "●" }).small()),
                );
            } else if ui
                .button(egui::RichText::new(if self.manual_rec { "■ Stop" } else { "● Record" })
                    .small()
                    .color(if self.manual_rec { rec_red } else { accent }))
                .clicked()
            {
                self.manual_rec = !self.manual_rec;
            }
        });

        let speed_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 2, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Number,
            );
            ui.label(egui::RichText::new("Speed").small().color(dim));
            if speed_wired {
                ui.label(egui::RichText::new(format!("{:.2}", self.speed)).small().monospace().color(blue));
            } else {
                ui.add(
                    egui::DragValue::new(&mut self.speed)
                        .speed(0.05)
                        .range(-8.0..=8.0)
                        .max_decimals(2),
                );
            }
        });

        if matches!(self.mode, RecorderMode::Tape) {
            let seek_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 3);
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(
                    ui, node_id, 3, true, ctx.connections,
                    ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                    PortKind::Normalized,
                );
                ui.label(egui::RichText::new("Seek").small().color(dim));
                if seek_wired {
                    ui.label(
                        egui::RichText::new(format!("{:.1}%", self.phase * 100.0))
                            .small()
                            .monospace()
                            .color(blue),
                    );
                } else {
                    ui.add(egui::Slider::new(&mut self.phase, 0.0..=1.0).show_value(false));
                    ui.label(
                        egui::RichText::new(format!("{:.0}%", self.phase * 100.0))
                            .small()
                            .monospace(),
                    );
                }
            });
        }

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 4, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Trigger,
            );
            ui.label(egui::RichText::new("Clear").small().color(dim));
            if ui.small_button("Clear").clicked() {
                self.clear_all();
            }
        });

        ui.add_space(2.0);
        ui.separator();

        // ── Mode-specific parameter rows ────────────────────────────
        match self.mode {
            RecorderMode::Scroll => {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Size").small().color(dim));
                    ui.add(egui::DragValue::new(&mut self.scroll_w).range(16..=4096).speed(4.0));
                    ui.label(egui::RichText::new("×").small().color(dim));
                    ui.add(egui::DragValue::new(&mut self.scroll_h).range(16..=2048).speed(2.0));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Stride").small().color(dim));
                    ui.add(egui::DragValue::new(&mut self.scroll_stride).range(1..=64));
                });
            }
            RecorderMode::Strip => {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Grid").small().color(dim));
                    ui.add(egui::DragValue::new(&mut self.strip_cols).range(1..=32));
                    ui.label(egui::RichText::new("×").small().color(dim));
                    ui.add(egui::DragValue::new(&mut self.strip_rows).range(1..=32));
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Cell").small().color(dim));
                    ui.add(egui::DragValue::new(&mut self.cell_w).range(8..=512).speed(2.0));
                    ui.label(egui::RichText::new("×").small().color(dim));
                    ui.add(egui::DragValue::new(&mut self.cell_h).range(8..=512).speed(2.0));
                });
            }
            RecorderMode::Tape => {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Frames").small().color(dim));
                    let mut mf = self.max_frames as i32;
                    if ui.add(egui::DragValue::new(&mut mf).range(2..=1024)).changed() {
                        self.max_frames = mf.max(2) as usize;
                        // Drop excess frames if cap was reduced
                        while self.tape.len() > self.max_frames {
                            self.tape.remove(0);
                        }
                    }
                    ui.label(
                        egui::RichText::new(format!("({}/{})", self.tape.len(), self.max_frames))
                            .small()
                            .monospace()
                            .color(dim),
                    );
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.looping, "Loop");
                });
            }
        }

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("FPS").small().color(dim));
            ui.add(
                egui::DragValue::new(&mut self.base_fps)
                    .range(1.0..=240.0)
                    .speed(0.5),
            );
        });

        ui.add_space(2.0);

        // ── Live preview ────────────────────────────────────────────
        let preview_w = ui.available_width().max(160.0).min(260.0);
        let preview_h = match self.mode {
            RecorderMode::Scroll => {
                preview_w * (self.scroll_h as f32 / self.scroll_w.max(1) as f32)
            }
            RecorderMode::Strip => {
                let total_w = (self.strip_cols * self.cell_w).max(1) as f32;
                let total_h = (self.strip_rows * self.cell_h).max(1) as f32;
                preview_w * (total_h / total_w)
            }
            RecorderMode::Tape => {
                // Prefer the recorded frame; fall back to live input so
                // the preview sizes to the camera/feed's aspect before
                // recording has started.
                if let Some(img) = self.cached_output.as_ref().or(self.latest_input.as_ref()) {
                    preview_w * (img.height as f32 / img.width.max(1) as f32)
                } else {
                    90.0
                }
            }
        };
        let preview_h = preview_h.clamp(40.0, 220.0);

        let (rect, _resp) =
            ui.allocate_exact_size(egui::vec2(preview_w, preview_h), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, egui::Color32::from_rgb(18, 18, 22));

        // Pick what to draw in the preview: recorded output if we have any,
        // otherwise the latest incoming frame (so users can see that input
        // IS flowing — camera / visualizer connected — even before hitting
        // Record). Dim the live-input preview to distinguish from recorded
        // content.
        let (preview_img, is_live_input) = match (&self.cached_output, &self.latest_input) {
            (Some(img), _) => (Some(img.clone()), false),
            (None, Some(img)) => (Some(img.clone()), true),
            _ => (None, false),
        };

        if let Some(ref img) = preview_img {
            if !img.pixels.is_empty() && img.width > 0 && img.height > 0 {
                let tex_id = egui::Id::new(("frame_rec_tex", node_id));
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width as usize, img.height as usize],
                    &img.pixels,
                );
                let mut tex = ui.ctx().data_mut(|d| d.get_temp::<egui::TextureHandle>(tex_id));
                let handle = match tex.take() {
                    Some(mut existing) => {
                        existing.set(color_image, egui::TextureOptions::LINEAR);
                        existing
                    }
                    None => ui.ctx().load_texture(
                        format!("frame_rec_{}", node_id),
                        color_image,
                        egui::TextureOptions::LINEAR,
                    ),
                };
                ui.ctx().data_mut(|d| d.insert_temp(tex_id, handle.clone()));
                let tint = if is_live_input {
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140) // dimmed
                } else {
                    egui::Color32::WHITE
                };
                painter.image(
                    handle.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    tint,
                );
                if is_live_input {
                    // Small badge so it's clear this is the incoming stream,
                    // not the recorded canvas.
                    painter.text(
                        egui::pos2(rect.min.x + 6.0, rect.min.y + 6.0),
                        egui::Align2::LEFT_TOP,
                        "LIVE",
                        egui::FontId::proportional(9.0),
                        egui::Color32::from_rgb(255, 180, 60),
                    );
                }
            }

            // Playhead for Tape mode
            if matches!(self.mode, RecorderMode::Tape) && !self.tape.is_empty() && !is_live_input {
                let ph_x = rect.min.x + self.phase.clamp(0.0, 1.0) * rect.width();
                painter.line_segment(
                    [egui::pos2(ph_x, rect.min.y), egui::pos2(ph_x, rect.max.y)],
                    egui::Stroke::new(1.5, accent),
                );
            }
        } else {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                match self.mode {
                    RecorderMode::Scroll => "Connect image — Scroll = slit-scan sweep",
                    RecorderMode::Strip  => "Connect image — Strip = contact-sheet grid",
                    RecorderMode::Tape   => "Connect image — Tape = full-frame scrub (camera-friendly)",
                },
                egui::FontId::proportional(11.0),
                dim,
            );
        }

        ui.add_space(2.0);
        ui.separator();

        // ── Output ports ────────────────────────────────────────────
        crate::nodes::output_port_row(
            ui, "Image", "",
            node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Image,
        );
        crate::nodes::output_port_row(
            ui, "Progress",
            &format!("{:.0}%", self.progress() * 100.0),
            node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Normalized,
        );
        crate::nodes::output_port_row(
            ui, "Frames",
            &format!("{}", self.frames_recorded),
            node_id, 2,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Number,
        );

        // Keep eval running while recording or while Tape is playing.
        let playing_tape = matches!(self.mode, RecorderMode::Tape)
            && !self.tape.is_empty()
            && self.speed.abs() > 0.0001;
        // Also repaint while input is flowing so the LIVE preview stays
        // current (otherwise the first-received frame would freeze on
        // screen until another unrelated UI event triggered a redraw).
        let has_live_input = self.latest_input.is_some();
        if rec_active || playing_tape || has_live_input {
            ui.ctx().request_repaint();
        }
    }
}

// ── Registration ────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("frame_recorder", |state| {
        let mut n = FrameRecorderNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
