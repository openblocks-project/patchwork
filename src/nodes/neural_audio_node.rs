//! NeuralAudioNode — host an ONNX decoder (latent → audio) plus an optional
//! ONNX encoder (audio → latent) for live timbre transfer.
//!
//! Modes are implicit, determined by what's wired + the Freeze toggle:
//!
//!   Generate  — nothing wired               · sliders drive latent absolutely
//!   Transfer  — Audio In wired + encoder    · incoming audio encoded to latent;
//!                                             sliders bias around it
//!   Frozen    — Freeze toggled on           · last-encoded latent locked;
//!                                             sliders bias around it
//!   Driven    — any Latent In port wired    · those dims taken from upstream;
//!                                             sliders still bias all dims
//!
//! Per-dim composition each iteration:
//!     base[i] = upstream[i]       if Latent_In[i] wired
//!             | encoded[i]        if Audio_In active & encoder & !frozen
//!             | frozen_latent[i]  if frozen
//!             | 0                 otherwise
//!     z[i]    = base[i] + slider[i]   (sliders are always BIAS)
//!
//! Threads:
//!   GUI         — slider state, file pickers, freeze, reads upstream Latent In
//!   Inference   — owns both ort::Sessions, runs in a loop encoding/decoding
//!   Audio       — drains output ring, writes input audio to input ring
//! All shared state lives in `NeuralAudioBridge`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::thread::JoinHandle;

use eframe::egui::{self, RichText};

use crate::audio::buffers::LiveInputBuffer;
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

const MAX_LATENT_DIM: usize = 32;
const DEFAULT_LATENT_DIM: usize = 8;
const N_LATENT_PORTS: usize = 8;       // hardcoded in/out latent ports per direction
const ENCODER_BLOCK_SIZE: usize = 2048; // samples per encoder forward pass
const OUTPUT_RING_CAPACITY: usize = 32_768;
const INPUT_RING_CAPACITY: usize = 16_384; // ~340 ms at 48 kHz; encoder block + slack

pub const STATUS_NO_MODEL: u8 = 0;
pub const STATUS_LOADING: u8 = 1;
pub const STATUS_READY: u8 = 2;
pub const STATUS_ERROR: u8 = 3;

pub const MODE_GENERATE: u8 = 0;
pub const MODE_TRANSFER: u8 = 1;
pub const MODE_FROZEN: u8 = 2;
pub const MODE_DRIVEN: u8 = 3;

pub struct NeuralAudioBridge {
    pub output_buffer: Arc<LiveInputBuffer>,
    pub input_buffer: Arc<LiveInputBuffer>,
    pub latents: Mutex<Vec<f32>>,
    pub decoder_path: Mutex<Option<PathBuf>>,
    pub encoder_path: Mutex<Option<PathBuf>>,
    pub running: AtomicBool,
    pub status: AtomicU8,
    pub mode: AtomicU8,
    pub n_latent: AtomicUsize,
    pub frozen: AtomicBool,
    pub frozen_latent: Mutex<Vec<f32>>,
    pub last_encoded_latent: Mutex<Vec<f32>>,
    pub upstream_latent: Mutex<Vec<f32>>,
    pub upstream_mask: Mutex<Vec<bool>>,
    pub live_latent: Mutex<Vec<f32>>,
    pub samples_generated: AtomicU64,
    pub last_error: Mutex<String>,
}

impl NeuralAudioBridge {
    fn new() -> Arc<Self> {
        let n = DEFAULT_LATENT_DIM;
        Arc::new(Self {
            output_buffer: Arc::new(LiveInputBuffer::new(OUTPUT_RING_CAPACITY)),
            input_buffer: Arc::new(LiveInputBuffer::new(INPUT_RING_CAPACITY)),
            latents: Mutex::new(vec![0.0; n]),
            decoder_path: Mutex::new(None),
            encoder_path: Mutex::new(None),
            running: AtomicBool::new(true),
            status: AtomicU8::new(STATUS_NO_MODEL),
            mode: AtomicU8::new(MODE_GENERATE),
            n_latent: AtomicUsize::new(n),
            frozen: AtomicBool::new(false),
            frozen_latent: Mutex::new(vec![0.0; n]),
            last_encoded_latent: Mutex::new(vec![0.0; n]),
            upstream_latent: Mutex::new(vec![0.0; N_LATENT_PORTS]),
            upstream_mask: Mutex::new(vec![false; N_LATENT_PORTS]),
            live_latent: Mutex::new(vec![0.0; n]),
            samples_generated: AtomicU64::new(0),
            last_error: Mutex::new(String::new()),
        })
    }
}

pub struct NeuralAudioNode {
    pub bridge: Arc<NeuralAudioBridge>,
    pub latent_values: Vec<f32>,
    pub n_latent: usize,
    pub decoder_path: String,
    pub encoder_path: String,
    inference_thread: Option<JoinHandle<()>>,
}

impl Default for NeuralAudioNode {
    fn default() -> Self {
        Self {
            bridge: NeuralAudioBridge::new(),
            latent_values: vec![0.0; DEFAULT_LATENT_DIM],
            n_latent: DEFAULT_LATENT_DIM,
            decoder_path: String::new(),
            encoder_path: String::new(),
            inference_thread: None,
        }
    }
}

impl NeuralAudioNode {
    fn ensure_thread(&mut self) {
        if self.inference_thread.is_some() { return; }
        let bridge = self.bridge.clone();
        self.inference_thread = std::thread::Builder::new()
            .name("neural-audio-inference".to_string())
            .spawn(move || run_inference_thread(bridge))
            .ok();
    }

    fn set_decoder_path(&mut self, path: PathBuf) {
        self.decoder_path = path.display().to_string();
        if let Ok(mut g) = self.bridge.decoder_path.lock() {
            *g = Some(path);
        }
        self.bridge.status.store(STATUS_LOADING, Ordering::Release);
        self.ensure_thread();
    }

    fn set_encoder_path(&mut self, path: PathBuf) {
        self.encoder_path = path.display().to_string();
        if let Ok(mut g) = self.bridge.encoder_path.lock() {
            *g = Some(path);
        }
        self.bridge.status.store(STATUS_LOADING, Ordering::Release);
        self.ensure_thread();
    }

    fn write_slider_bias_to_bridge(&self) {
        if let Ok(mut g) = self.bridge.latents.lock() {
            let n = self.n_latent.min(self.latent_values.len()).min(g.len());
            for i in 0..n { g[i] = self.latent_values[i]; }
        }
    }
}

impl Drop for NeuralAudioNode {
    fn drop(&mut self) {
        self.bridge.running.store(false, Ordering::Release);
    }
}

fn short_filename(path: &str) -> String {
    if path.is_empty() { return String::new(); }
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}

impl NodeBehavior for NeuralAudioNode {
    fn title(&self) -> &str { "Neural Audio" }
    fn type_tag(&self) -> &str { "neural_audio" }
    fn color_hint(&self) -> [u8; 3] { [180, 100, 255] }
    fn min_width(&self) -> Option<f32> { Some(260.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        let mut v = Vec::with_capacity(1 + N_LATENT_PORTS);
        v.push(PortDef::new("Audio", PortKind::Audio));
        for i in 0..N_LATENT_PORTS {
            v.push(PortDef::dynamic(format!("z{}", i), PortKind::Number));
        }
        v
    }

    fn outputs(&self) -> Vec<PortDef> {
        let mut v = Vec::with_capacity(1 + N_LATENT_PORTS);
        v.push(PortDef::new("Audio", PortKind::Audio));
        for i in 0..N_LATENT_PORTS {
            v.push(PortDef::dynamic(format!("z{}", i), PortKind::Number));
        }
        v
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Read per-dim Latent In ports (input ports 1..=N_LATENT_PORTS)
        let mut upstream = vec![0.0f32; N_LATENT_PORTS];
        let mut mask = vec![false; N_LATENT_PORTS];
        for i in 0..N_LATENT_PORTS {
            if let Some(PortValue::Float(v)) = inputs.get(1 + i) {
                upstream[i] = *v;
                mask[i] = true;
            }
        }
        if let Ok(mut g) = self.bridge.upstream_latent.lock() { *g = upstream; }
        if let Ok(mut g) = self.bridge.upstream_mask.lock() { *g = mask; }

        // Emit Latent Out values from the live latent (output ports 1..=N_LATENT_PORTS).
        let live = self.bridge.live_latent.lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| vec![0.0; self.n_latent]);
        let mut out = Vec::with_capacity(N_LATENT_PORTS);
        for i in 0..N_LATENT_PORTS {
            let v = live.get(i).copied().unwrap_or(0.0);
            out.push((1 + i, PortValue::Float(v)));
        }
        out
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim = egui::Color32::from_rgb(110, 110, 120);

        // Sync detected latent dim
        let detected = self.bridge.n_latent.load(Ordering::Acquire).clamp(1, MAX_LATENT_DIM);
        if detected != self.n_latent {
            self.n_latent = detected;
            if self.latent_values.len() < detected {
                self.latent_values.resize(detected, 0.0);
            }
            if let Ok(mut g) = self.bridge.latents.lock() {
                if g.len() != detected { g.resize(detected, 0.0); }
            }
            if let Ok(mut g) = self.bridge.frozen_latent.lock() {
                if g.len() != detected { g.resize(detected, 0.0); }
            }
            if let Ok(mut g) = self.bridge.last_encoded_latent.lock() {
                if g.len() != detected { g.resize(detected, 0.0); }
            }
            if let Ok(mut g) = self.bridge.live_latent.lock() {
                if g.len() != detected { g.resize(detected, 0.0); }
            }
        }

        // ── Status row ───────────────────────────────────────────────
        let status = self.bridge.status.load(Ordering::Acquire);
        let mode = self.bridge.mode.load(Ordering::Acquire);
        let (status_text, status_color) = match status {
            STATUS_NO_MODEL => ("No model".to_string(), dim),
            STATUS_LOADING  => ("Loading…".to_string(), egui::Color32::from_rgb(255, 200, 80)),
            STATUS_READY    => {
                let label = match mode {
                    MODE_TRANSFER => "Transfer",
                    MODE_FROZEN   => "Frozen",
                    MODE_DRIVEN   => "Driven",
                    _             => "Generate",
                };
                (label.to_string(), egui::Color32::from_rgb(120, 220, 140))
            }
            STATUS_ERROR    => ("Error".to_string(), egui::Color32::from_rgb(255, 100, 100)),
            _               => ("Unknown".to_string(), dim),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").color(status_color));
            ui.label(RichText::new(status_text).small().color(status_color));
            let n = self.bridge.samples_generated.load(Ordering::Relaxed);
            if n > 0 {
                ui.label(RichText::new(format!("· {} k samples", n / 1000))
                    .small().color(dim));
            }
        });
        if status == STATUS_ERROR {
            if let Ok(err) = self.bridge.last_error.lock() {
                if !err.is_empty() {
                    ui.label(RichText::new(err.as_str()).small()
                        .color(egui::Color32::from_rgb(255, 100, 100)));
                }
            }
        }

        // ── Decoder + Encoder pickers ────────────────────────────────
        ui.horizontal(|ui| {
            if ui.small_button("📂 Decoder…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("ONNX model", &["onnx"])
                    .pick_file()
                {
                    self.set_decoder_path(path);
                }
            }
            ui.label(RichText::new(short_filename(&self.decoder_path)).small().color(dim));
        });
        ui.horizontal(|ui| {
            let encoder_btn = ui.small_button("📂 Encoder…");
            if encoder_btn.clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("ONNX model", &["onnx"])
                    .pick_file()
                {
                    self.set_encoder_path(path);
                }
            }
            ui.label(RichText::new(short_filename(&self.encoder_path)).small().color(dim));
        });

        // ── Freeze toggle ───────────────────────────────────────────
        ui.horizontal(|ui| {
            let frozen = self.bridge.frozen.load(Ordering::Acquire);
            let label = if frozen { "❄ Frozen" } else { "❄ Freeze" };
            let resp = ui.small_button(label);
            if resp.clicked() {
                if !frozen {
                    // Snapshot last_encoded_latent into frozen_latent
                    if let (Ok(src), Ok(mut dst)) = (
                        self.bridge.last_encoded_latent.lock(),
                        self.bridge.frozen_latent.lock(),
                    ) {
                        let n = self.n_latent.min(src.len()).min(dst.len());
                        for i in 0..n { dst[i] = src[i]; }
                    }
                }
                self.bridge.frozen.store(!frozen, Ordering::Release);
            }
            if ui.small_button("Zero").clicked() {
                for v in self.latent_values.iter_mut() { *v = 0.0; }
                self.write_slider_bias_to_bridge();
            }
            if ui.small_button("Random").clicked() {
                use rand::Rng;
                let mut rng = rand::rng();
                for v in self.latent_values.iter_mut() {
                    *v = rng.random_range(-1.5..=1.5);
                }
                self.write_slider_bias_to_bridge();
            }
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Per-dim latent rows (in port + slider + value + out port) ─
        let live = self.bridge.live_latent.lock().map(|g| g.clone()).unwrap_or_default();
        let upstream_mask = self.bridge.upstream_mask.lock().map(|g| g.clone()).unwrap_or_default();
        let bias_label_color = if mode == MODE_GENERATE { dim } else { egui::Color32::from_rgb(140, 200, 240) };
        let bias_header = if mode == MODE_GENERATE { "Latents" } else { "Latents — bias" };
        ui.label(RichText::new(format!("{} ({})", bias_header, self.n_latent))
            .small().color(bias_label_color));

        let mut changed = false;
        for i in 0..self.n_latent {
            let port_idx = 1 + i; // ports 1..N_LATENT_PORTS+1
            ui.horizontal(|ui| {
                // Latent In port (only show for first N_LATENT_PORTS dims)
                if i < N_LATENT_PORTS {
                    crate::nodes::inline_port_circle(
                        ui, node_id, port_idx, true,
                        ctx.connections, ctx.port_positions, ctx.dragging_from,
                        ctx.pending_disconnects, PortKind::Number,
                    );
                } else {
                    ui.add_space(20.0);
                }
                ui.label(RichText::new(format!("z{}", i)).small().color(dim));
                if let Some(v) = self.latent_values.get_mut(i) {
                    if ui.add(egui::Slider::new(v, -3.0..=3.0).show_value(false)).changed() {
                        changed = true;
                    }
                }
                let live_v = live.get(i).copied().unwrap_or(0.0);
                let driven = upstream_mask.get(i).copied().unwrap_or(false);
                let val_color = if driven {
                    egui::Color32::from_rgb(140, 200, 240)
                } else {
                    egui::Color32::from_rgb(180, 180, 180)
                };
                ui.label(RichText::new(format!("{:+.2}", live_v))
                    .small().monospace().color(val_color));
                // Latent Out port (only show for first N_LATENT_PORTS dims)
                if i < N_LATENT_PORTS {
                    crate::nodes::inline_port_circle(
                        ui, node_id, port_idx, false,
                        ctx.connections, ctx.port_positions, ctx.dragging_from,
                        ctx.pending_disconnects, PortKind::Number,
                    );
                } else {
                    ui.add_space(20.0);
                }
            });
        }
        if changed { self.write_slider_bias_to_bridge(); }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Audio In + Audio Out rows ────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from,
                ctx.pending_disconnects, PortKind::Audio,
            );
            let has_encoder = !self.encoder_path.is_empty();
            let label_color = if has_encoder { dim } else { egui::Color32::from_rgb(80, 80, 90) };
            ui.label(RichText::new(if has_encoder { "Audio in" } else { "Audio in (load encoder)" })
                .small().color(label_color));
        });
        crate::nodes::output_port_row(
            ui, "Audio", "", node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections,
            ctx.pending_disconnects, PortKind::Audio,
        );

        if status == STATUS_LOADING {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
        }
        // Repaint while playing so live latent values update on the UI.
        if status == STATUS_READY {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "decoder_path": self.decoder_path,
            "encoder_path": self.encoder_path,
            "latents": self.latent_values,
            "frozen": self.bridge.frozen.load(Ordering::Acquire),
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(arr) = state.get("latents").and_then(|v| v.as_array()) {
            self.latent_values = arr.iter()
                .filter_map(|v| v.as_f64().map(|x| x as f32))
                .collect();
            self.n_latent = self.latent_values.len().clamp(1, MAX_LATENT_DIM);
            self.write_slider_bias_to_bridge();
        }
        // Backwards-compat: old saves used `model_path` for the decoder.
        let decoder = state.get("decoder_path").and_then(|v| v.as_str())
            .or_else(|| state.get("model_path").and_then(|v| v.as_str()))
            .unwrap_or("");
        if !decoder.is_empty() {
            let p = PathBuf::from(decoder);
            if p.exists() { self.set_decoder_path(p); }
            else { self.decoder_path = decoder.to_string(); }
        }
        if let Some(s) = state.get("encoder_path").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                let p = PathBuf::from(s);
                if p.exists() { self.set_encoder_path(p); }
                else { self.encoder_path = s.to_string(); }
            }
        }
        if let Some(b) = state.get("frozen").and_then(|v| v.as_bool()) {
            self.bridge.frozen.store(b, Ordering::Release);
        }
    }
}

// ── Inference thread ──────────────────────────────────────────────────────────

fn run_inference_thread(bridge: Arc<NeuralAudioBridge>) {
    use ort::session::Session;

    let mut decoder: Option<Session> = None;
    let mut decoder_input: String = String::new();
    let mut decoder_path_cache: Option<PathBuf> = None;
    let mut latent_dim: usize = DEFAULT_LATENT_DIM;

    let mut encoder: Option<Session> = None;
    let mut encoder_input: String = String::new();
    let mut encoder_path_cache: Option<PathBuf> = None;

    let mut input_block: Vec<f32> = vec![0.0; ENCODER_BLOCK_SIZE];

    while bridge.running.load(Ordering::Acquire) {
        // ── Reload decoder if path changed ────────────────────────────
        let want_decoder = bridge.decoder_path.lock().ok().and_then(|g| g.clone());
        if want_decoder != decoder_path_cache {
            decoder = None;
            decoder_path_cache = want_decoder.clone();
            if let Some(path) = &want_decoder {
                bridge.status.store(STATUS_LOADING, Ordering::Release);
                match build_session(path) {
                    Ok((s, name, n_lat)) => {
                        latent_dim = n_lat;
                        decoder_input = name;
                        bridge.n_latent.store(n_lat, Ordering::Release);
                        // Resize all latent vectors in bridge
                        for m in [&bridge.latents, &bridge.frozen_latent,
                                  &bridge.last_encoded_latent, &bridge.live_latent] {
                            if let Ok(mut g) = m.lock() { g.resize(n_lat, 0.0); }
                        }
                        if let Ok(mut e) = bridge.last_error.lock() { e.clear(); }
                        decoder = Some(s);
                        bridge.status.store(STATUS_READY, Ordering::Release);
                    }
                    Err(err) => {
                        if let Ok(mut e) = bridge.last_error.lock() { *e = err; }
                        bridge.status.store(STATUS_ERROR, Ordering::Release);
                    }
                }
            } else {
                bridge.status.store(STATUS_NO_MODEL, Ordering::Release);
            }
        }

        // ── Reload encoder if path changed ────────────────────────────
        let want_encoder = bridge.encoder_path.lock().ok().and_then(|g| g.clone());
        if want_encoder != encoder_path_cache {
            encoder = None;
            encoder_path_cache = want_encoder.clone();
            if let Some(path) = &want_encoder {
                match build_session(path) {
                    Ok((s, name, _n)) => {
                        encoder_input = name;
                        encoder = Some(s);
                    }
                    Err(err) => {
                        if let Ok(mut e) = bridge.last_error.lock() { *e = err; }
                    }
                }
            }
        }

        let Some(decoder) = decoder.as_mut() else {
            std::thread::sleep(std::time::Duration::from_millis(120));
            continue;
        };

        // Throttle when output buffer is mostly full
        let buffered = bridge.output_buffer.buffered();
        if buffered + 4096 > bridge.output_buffer.capacity {
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }

        // ── Determine base latent ─────────────────────────────────────
        let frozen = bridge.frozen.load(Ordering::Acquire);
        let mut base = vec![0.0f32; latent_dim];
        let mut active_mode = MODE_GENERATE;

        let input_avail = bridge.input_buffer.buffered() >= ENCODER_BLOCK_SIZE;
        if !frozen && encoder.is_some() && input_avail {
            // Drain ENCODER_BLOCK_SIZE samples from input ring
            input_block.resize(ENCODER_BLOCK_SIZE, 0.0);
            bridge.input_buffer.read_into(&mut input_block, ENCODER_BLOCK_SIZE);
            // Detect non-silence (we don't run encoder on dead air)
            let energy = input_block.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);
            if energy > 1e-4 {
                if let Some(enc) = encoder.as_mut() {
                    match run_encoder(enc, &encoder_input, &input_block, latent_dim) {
                        Ok(z) => {
                            base.copy_from_slice(&z[..latent_dim.min(z.len())]);
                            if let Ok(mut g) = bridge.last_encoded_latent.lock() {
                                let n = latent_dim.min(g.len());
                                g[..n].copy_from_slice(&base[..n]);
                            }
                            active_mode = MODE_TRANSFER;
                        }
                        Err(e) => {
                            set_error(&bridge, format!("encoder: {e}"));
                        }
                    }
                }
            }
        }
        if frozen {
            if let Ok(g) = bridge.frozen_latent.lock() {
                let n = latent_dim.min(g.len());
                base[..n].copy_from_slice(&g[..n]);
            }
            active_mode = MODE_FROZEN;
        }

        // ── Compose with upstream Latent In overrides + slider bias ───
        let upstream = bridge.upstream_latent.lock().map(|g| g.clone()).unwrap_or_default();
        let mask     = bridge.upstream_mask.lock().map(|g| g.clone()).unwrap_or_default();
        let sliders  = bridge.latents.lock().map(|g| g.clone()).unwrap_or_default();

        let mut z = vec![0.0f32; latent_dim];
        let mut any_driven = false;
        for i in 0..latent_dim {
            let driven = mask.get(i).copied().unwrap_or(false);
            let b = if driven {
                any_driven = true;
                upstream.get(i).copied().unwrap_or(0.0)
            } else {
                base[i]
            };
            let bias = sliders.get(i).copied().unwrap_or(0.0);
            z[i] = b + bias;
        }
        if any_driven { active_mode = MODE_DRIVEN; }
        bridge.mode.store(active_mode, Ordering::Release);

        // Publish live latent for UI + Latent Out ports
        if let Ok(mut g) = bridge.live_latent.lock() {
            let n = latent_dim.min(g.len());
            g[..n].copy_from_slice(&z[..n]);
        }

        // ── Run decoder ───────────────────────────────────────────────
        let shape = vec![1i64, latent_dim as i64, 1i64];
        let tensor = match ort::value::Tensor::from_array((shape, z.into_boxed_slice())) {
            Ok(t) => t,
            Err(e) => {
                set_error(&bridge, format!("tensor build: {e}"));
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
        };
        let outputs = match decoder.run(ort::inputs![decoder_input.as_str() => tensor]) {
            Ok(o) => o,
            Err(e) => {
                set_error(&bridge, format!("decoder: {e}"));
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
        };
        let Some(out) = outputs.into_iter().next() else { continue };
        let value = out.1;
        let view = match value.try_extract_tensor::<f32>() {
            Ok(t) => t,
            Err(e) => { set_error(&bridge, format!("extract: {e}")); continue; }
        };
        let samples: &[f32] = view.1;
        if samples.is_empty() { continue; }
        bridge.output_buffer.write(samples);
        bridge.samples_generated.fetch_add(samples.len() as u64, Ordering::Relaxed);
    }
}

fn run_encoder(
    sess: &mut ort::session::Session,
    input_name: &str,
    audio: &[f32],
    latent_dim: usize,
) -> Result<Vec<f32>, String> {
    let shape = vec![1i64, 1i64, audio.len() as i64];
    let tensor = ort::value::Tensor::from_array((shape, audio.to_vec().into_boxed_slice()))
        .map_err(|e| format!("tensor: {e}"))?;
    let outputs = sess.run(ort::inputs![input_name => tensor])
        .map_err(|e| format!("run: {e}"))?;
    let out = outputs.into_iter().next().ok_or_else(|| "no outputs".to_string())?;
    let view = out.1.try_extract_tensor::<f32>().map_err(|e| format!("extract: {e}"))?;
    let data: &[f32] = view.1;
    let n = latent_dim.min(data.len());
    Ok(data[..n].to_vec())
}

fn set_error(bridge: &NeuralAudioBridge, msg: String) {
    if let Ok(mut e) = bridge.last_error.lock() { *e = msg; }
    bridge.status.store(STATUS_ERROR, Ordering::Release);
}

fn build_session(path: &PathBuf) -> Result<(ort::session::Session, String, usize), String> {
    let mut builder = crate::ml::ep::session_builder()
        .map_err(|e| format!("session_builder: {e}"))?;
    let session = builder.commit_from_file(path)
        .map_err(|e| format!("load model: {e}"))?;

    let inputs = session.inputs();
    let input = inputs.first().ok_or_else(|| "model has no inputs".to_string())?;
    let name = input.name().to_string();

    // Try to detect latent dim from input shape's axis-1 (decoder convention).
    // If symbolic / not present, fall back to default.
    let n_latent = match input.dtype() {
        ort::value::ValueType::Tensor { shape, .. } => shape
            .get(1)
            .copied()
            .filter(|d| *d > 0)
            .map(|d| (d as usize).clamp(1, MAX_LATENT_DIM))
            .unwrap_or(DEFAULT_LATENT_DIM),
        _ => DEFAULT_LATENT_DIM,
    };

    Ok((session, name, n_latent))
}

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("neural_audio", |state| {
        let mut n = NeuralAudioNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
