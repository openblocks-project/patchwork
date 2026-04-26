//! NeuralAudioNode — host any `[1, N, 1] → audio` ONNX model as a Patchwork
//! audio source. Latent vector in (sliders), audio block out.
//!
//! Architecture:
//! - This UI struct holds the latent sliders and a shared `NeuralAudioBridge`.
//! - On first model load, a background thread is spawned that owns the
//!   `ort::Session` and runs decoder forward passes in a loop, pushing
//!   audio samples into the bridge's `LiveInputBuffer`.
//! - The corresponding `NeuralAudioProcessor` (audio thread) drains the same
//!   buffer in `process_block`. See `src/audio/processors/neural_audio.rs`.
//!
//! Runtime path: ort + ONNX. Drop in any ONNX file with input shape
//! `[1, N_latent, 1]` (auto-detected) and audio-tensor output. Train your
//! own with the sibling `patchwork-models` Python project, or use any
//! compatible third-party export.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::thread::JoinHandle;

use eframe::egui::{self, RichText};

use crate::audio::buffers::LiveInputBuffer;
use crate::graph::{PortDef, PortKind};
use crate::node_trait::{NodeBehavior, RenderContext};

const MAX_LATENT_DIM: usize = 32;
const DEFAULT_LATENT_DIM: usize = 8;
const RING_CAPACITY: usize = 32_768;

pub const STATUS_NO_MODEL: u8 = 0;
pub const STATUS_LOADING: u8 = 1;
pub const STATUS_READY: u8 = 2;
pub const STATUS_ERROR: u8 = 3;

/// Shared state between the GUI thread, the inference thread, and the
/// audio thread's `NeuralAudioProcessor`.
pub struct NeuralAudioBridge {
    pub audio_buffer: Arc<LiveInputBuffer>,
    pub latents: Mutex<Vec<f32>>,
    pub model_path: Mutex<Option<PathBuf>>,
    pub running: AtomicBool,
    pub status: AtomicU8,
    pub n_latent: AtomicUsize,
    pub samples_generated: AtomicU64,
    pub last_error: Mutex<String>,
}

impl NeuralAudioBridge {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            audio_buffer: Arc::new(LiveInputBuffer::new(RING_CAPACITY)),
            latents: Mutex::new(vec![0.0; DEFAULT_LATENT_DIM]),
            model_path: Mutex::new(None),
            running: AtomicBool::new(true),
            status: AtomicU8::new(STATUS_NO_MODEL),
            n_latent: AtomicUsize::new(DEFAULT_LATENT_DIM),
            samples_generated: AtomicU64::new(0),
            last_error: Mutex::new(String::new()),
        })
    }
}

pub struct NeuralAudioNode {
    pub bridge: Arc<NeuralAudioBridge>,
    pub latent_values: Vec<f32>,
    pub n_latent: usize,
    pub model_path: String,
    inference_thread: Option<JoinHandle<()>>,
}

impl Default for NeuralAudioNode {
    fn default() -> Self {
        Self {
            bridge: NeuralAudioBridge::new(),
            latent_values: vec![0.0; DEFAULT_LATENT_DIM],
            n_latent: DEFAULT_LATENT_DIM,
            model_path: String::new(),
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

    fn set_model_path(&mut self, path: PathBuf) {
        self.model_path = path.display().to_string();
        if let Ok(mut guard) = self.bridge.model_path.lock() {
            *guard = Some(path);
        }
        self.bridge.status.store(STATUS_LOADING, Ordering::Release);
        self.ensure_thread();
    }

    fn write_latents_to_bridge(&self) {
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

impl NodeBehavior for NeuralAudioNode {
    fn title(&self) -> &str { "Neural Audio" }
    fn type_tag(&self) -> &str { "neural_audio" }
    fn color_hint(&self) -> [u8; 3] { [180, 100, 255] }
    fn min_width(&self) -> Option<f32> { Some(240.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> { vec![] }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Audio", PortKind::Audio)]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim = egui::Color32::from_rgb(110, 110, 120);

        let detected = self.bridge.n_latent.load(Ordering::Acquire).clamp(1, MAX_LATENT_DIM);
        if detected != self.n_latent {
            self.n_latent = detected;
            if self.latent_values.len() < detected {
                self.latent_values.resize(detected, 0.0);
            }
            if let Ok(mut g) = self.bridge.latents.lock() {
                if g.len() != detected { g.resize(detected, 0.0); }
            }
        }

        let status = self.bridge.status.load(Ordering::Acquire);
        let (status_text, status_color) = match status {
            STATUS_NO_MODEL => ("No model", dim),
            STATUS_LOADING  => ("Loading…", egui::Color32::from_rgb(255, 200, 80)),
            STATUS_READY    => ("Ready", egui::Color32::from_rgb(120, 220, 140)),
            STATUS_ERROR    => ("Error", egui::Color32::from_rgb(255, 100, 100)),
            _               => ("Unknown", dim),
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").color(status_color));
            ui.label(RichText::new(status_text).small().color(status_color));
            let n_samples = self.bridge.samples_generated.load(Ordering::Relaxed);
            if n_samples > 0 {
                ui.label(RichText::new(format!("· {} k samples", n_samples / 1000))
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

        ui.horizontal(|ui| {
            if ui.small_button("📂 Model…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("ONNX model", &["onnx"])
                    .pick_file()
                {
                    self.set_model_path(path);
                }
            }
            if !self.model_path.is_empty() {
                let display = std::path::Path::new(&self.model_path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| self.model_path.clone());
                ui.label(RichText::new(display).small().color(dim));
            }
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        ui.label(RichText::new(format!("Latents ({})", self.n_latent)).small().color(dim));
        let mut changed = false;
        for i in 0..self.n_latent {
            if let Some(v) = self.latent_values.get_mut(i) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("z{}", i)).small().color(dim));
                    if ui.add(egui::Slider::new(v, -3.0..=3.0).show_value(false)).changed() {
                        changed = true;
                    }
                    ui.label(RichText::new(format!("{:+.2}", *v)).small().monospace());
                });
            }
        }
        if changed { self.write_latents_to_bridge(); }

        ui.horizontal(|ui| {
            if ui.small_button("Zero").clicked() {
                for v in self.latent_values.iter_mut() { *v = 0.0; }
                self.write_latents_to_bridge();
            }
            if ui.small_button("Random").clicked() {
                use rand::Rng;
                let mut rng = rand::rng();
                for v in self.latent_values.iter_mut() {
                    *v = rng.random_range(-1.5..=1.5);
                }
                self.write_latents_to_bridge();
            }
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        crate::nodes::output_port_row(
            ui, "Audio", "", node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections,
            ctx.pending_disconnects, PortKind::Audio,
        );

        if status == STATUS_LOADING {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "model_path": self.model_path,
            "latents": self.latent_values,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(arr) = state.get("latents").and_then(|v| v.as_array()) {
            self.latent_values = arr.iter()
                .filter_map(|v| v.as_f64().map(|x| x as f32))
                .collect();
            self.n_latent = self.latent_values.len().clamp(1, MAX_LATENT_DIM);
            self.write_latents_to_bridge();
        }
        if let Some(p) = state.get("model_path").and_then(|v| v.as_str()) {
            if !p.is_empty() {
                let path = PathBuf::from(p);
                if path.exists() {
                    self.set_model_path(path);
                } else {
                    self.model_path = p.to_string();
                }
            }
        }
    }
}

// ── Inference thread ──────────────────────────────────────────────────────────

fn run_inference_thread(bridge: Arc<NeuralAudioBridge>) {
    use ort::session::Session;

    let mut session: Option<Session> = None;
    let mut input_name = String::new();
    let mut current_path: Option<PathBuf> = None;
    let mut latent_dim: usize = DEFAULT_LATENT_DIM;

    while bridge.running.load(Ordering::Acquire) {
        let desired_path = bridge.model_path.lock().ok().and_then(|g| g.clone());
        let path_changed = match (&current_path, &desired_path) {
            (None, None) => false,
            (Some(a), Some(b)) => a != b,
            _ => true,
        };
        if path_changed {
            session = None;
            current_path = desired_path.clone();
            if let Some(path) = &desired_path {
                bridge.status.store(STATUS_LOADING, Ordering::Release);
                match build_session(path) {
                    Ok((s, name, n_lat)) => {
                        latent_dim = n_lat;
                        input_name = name;
                        bridge.n_latent.store(n_lat, Ordering::Release);
                        if let Ok(mut g) = bridge.latents.lock() {
                            g.resize(n_lat, 0.0);
                        }
                        if let Ok(mut e) = bridge.last_error.lock() { e.clear(); }
                        session = Some(s);
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

        let Some(sess) = session.as_mut() else {
            std::thread::sleep(std::time::Duration::from_millis(120));
            continue;
        };

        let buffered = bridge.audio_buffer.buffered();
        if buffered + 4096 > bridge.audio_buffer.capacity {
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }

        let latents: Vec<f32> = match bridge.latents.lock() {
            Ok(g) => {
                let mut v = vec![0.0f32; latent_dim];
                let n = latent_dim.min(g.len());
                v[..n].copy_from_slice(&g[..n]);
                v
            }
            Err(_) => vec![0.0f32; latent_dim],
        };

        let shape = vec![1i64, latent_dim as i64, 1i64];
        let tensor = match ort::value::Tensor::from_array((shape, latents.into_boxed_slice())) {
            Ok(t) => t,
            Err(e) => {
                set_error(&bridge, format!("tensor build: {e}"));
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
        };

        let outputs = match sess.run(ort::inputs![input_name.as_str() => tensor]) {
            Ok(o) => o,
            Err(e) => {
                set_error(&bridge, format!("inference: {e}"));
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
        };

        let Some(out) = outputs.into_iter().next() else {
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        };
        let value = out.1;
        let samples_view = match value.try_extract_tensor::<f32>() {
            Ok(t) => t,
            Err(e) => {
                set_error(&bridge, format!("extract: {e}"));
                std::thread::sleep(std::time::Duration::from_millis(200));
                continue;
            }
        };
        let samples: &[f32] = samples_view.1;
        if samples.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        bridge.audio_buffer.write(samples);
        bridge.samples_generated.fetch_add(samples.len() as u64, Ordering::Relaxed);
    }
}

fn set_error(bridge: &NeuralAudioBridge, msg: String) {
    if let Ok(mut e) = bridge.last_error.lock() { *e = msg; }
    bridge.status.store(STATUS_ERROR, Ordering::Release);
}

/// Open an ONNX file, attach execution providers, return (session, input_name, latent_dim).
/// The latent dim is taken from the second axis of the first input's shape;
/// if the model uses a symbolic dim, falls back to `DEFAULT_LATENT_DIM`.
fn build_session(path: &PathBuf) -> Result<(ort::session::Session, String, usize), String> {
    let mut builder = crate::ml::ep::session_builder()
        .map_err(|e| format!("session_builder: {e}"))?;
    let session = builder
        .commit_from_file(path)
        .map_err(|e| format!("load model: {e}"))?;

    let inputs = session.inputs();
    let input = inputs.first().ok_or_else(|| "model has no inputs".to_string())?;
    let name = input.name().to_string();

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
