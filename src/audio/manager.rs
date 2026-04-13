use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crate::graph::NodeId;
use super::buffers::{LiveInputBuffer, FilePlayerBuffer};
use super::decode::{decode_file_thread, probe_file_duration};

// ── Secondary Output (for per-speaker device routing) ────────────────────────

/// A secondary CPAL output stream for routing speakers to non-primary devices.
#[allow(dead_code)]
struct SecondaryOutput {
    stream: cpal::Stream,
    /// Shared buffer: UI/engine writes interleaved stereo, CPAL callback reads.
    buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    channels: usize,
    sample_rate: f32,
}

// ── Audio Manager ────────────────────────────────────────────────────────────

pub struct AudioManager {
    stream: Option<cpal::Stream>,
    pub output_device_name: String,
    pub _input_device_name: String,
    // File playback via Symphonia decode thread → FilePlayerBuffer → CPAL callback
    pub file_buffers: HashMap<NodeId, Arc<FilePlayerBuffer>>,
    file_threads: HashMap<NodeId, std::thread::JoinHandle<()>>,
    file_looping: HashMap<NodeId, Arc<AtomicBool>>,
    pub file_playing: HashMap<NodeId, bool>,
    pub file_durations: HashMap<NodeId, f64>,  // seconds
    // Live audio input streams (one per AudioInput node)
    input_streams: HashMap<NodeId, cpal::Stream>,
    pub input_buffers: HashMap<NodeId, Arc<LiveInputBuffer>>,
    // Sampler buffers (one per AudioSampler node)
    pub sampler_buffers: HashMap<NodeId, Arc<super::buffers::SamplerBuffer>>,
    /// Resolved upstream audio source for each sampler node.
    #[allow(dead_code)]
    pub sampler_input_sources: HashMap<NodeId, NodeId>,
    // Cached device lists (refreshed periodically, not every frame)
    pub cached_output_devices: Vec<String>,
    pub cached_input_devices: Vec<String>,
    /// Set to true by audio error callback when device disconnects or errors.
    /// UI checks this each frame to show warning and disable DSP.
    pub audio_error: Arc<AtomicBool>,
    /// Dropout counter — lives outside the Mutex so the audio callback can
    /// increment it even when try_lock fails (which is the whole point).
    pub dropout_count: Arc<AtomicU32>,
    /// Master volume — shared atomic between UI and engine. No command needed.
    pub master_volume: Arc<super::params::AtomicF32>,

    // ── New engine (VCV Rack style) ──────────────────────────────────────
    /// Command sender for the new audio engine (UI → audio thread).
    pub engine_tx: Option<crossbeam_channel::Sender<super::engine::AudioCommand>>,
    /// Per-node atomic parameter storage. UI writes directly, audio reads.
    pub node_params: HashMap<NodeId, Arc<Vec<super::params::AtomicF32>>>,
    /// Cached sample rate for processor preparation.
    pub engine_sample_rate: f32,
    /// Whether rebuild_engine_from_graph needs to run for the current engine.
    pub engine_needs_rebuild: bool,
    /// Per-analyzer shared analysis results. UI reads, engine writes.
    pub analyzer_results: HashMap<NodeId, Arc<std::sync::Mutex<super::analysis::AudioAnalysis>>>,
    /// Per-spectrum-analyzer FFT bins. UI reads, engine writes.
    pub spectrum_results: HashMap<NodeId, Arc<std::sync::Mutex<super::analysis::Spectrum>>>,
    /// CLAP plugin GUI handles — stored here so we can close all GUIs before stopping DSP.
    pub clap_gui_handles: HashMap<NodeId, Arc<std::sync::Mutex<super::clap_host::ClapGuiHandle>>>,
    /// Number of output channels on the current primary device.
    pub output_channel_count: usize,
    /// Cached channel counts per device name (for Speaker UI dropdowns).
    pub device_channel_counts: HashMap<String, usize>,
    /// Secondary output streams for speakers targeting non-primary devices.
    /// Key = device name. The stream reads from a shared buffer filled by the engine.
    #[allow(dead_code)]
    secondary_streams: HashMap<String, SecondaryOutput>,
    /// Node IDs whose processors were registered mid-session AFTER the
    /// engine had already done its initial connection walk. The eval loop
    /// drains this set on the next frame and re-runs `connect_audio` for
    /// every connection involving these nodes, so a Microphone (or Sampler)
    /// activated after DSP starts gets correctly wired into the chain.
    /// Without this, `start_input` adds a processor but the audio edges
    /// from this node remain dangling, producing silence downstream.
    pub pending_connection_sync: std::collections::HashSet<NodeId>,
    // TTS synthesis
    pub tts_buffers: HashMap<NodeId, Arc<FilePlayerBuffer>>,
    pub tts_running: std::collections::HashSet<NodeId>,
}

impl AudioManager {
    pub fn new() -> Self {
        Self {
            stream: None,
            output_device_name: String::new(),
            _input_device_name: String::new(),
            file_buffers: HashMap::new(),
            file_threads: HashMap::new(),
            file_looping: HashMap::new(),
            file_playing: HashMap::new(),
            file_durations: HashMap::new(),
            input_streams: HashMap::new(),
            input_buffers: HashMap::new(),
            sampler_buffers: HashMap::new(),
            sampler_input_sources: HashMap::new(),
            cached_output_devices: Vec::new(),
            cached_input_devices: Vec::new(),
            audio_error: Arc::new(AtomicBool::new(false)),
            dropout_count: Arc::new(AtomicU32::new(0)),
            master_volume: Arc::new(super::params::AtomicF32::new(0.8)),
            engine_tx: None, node_params: HashMap::new(),
            engine_sample_rate: 44100.0, engine_needs_rebuild: false,
            analyzer_results: HashMap::new(),
            spectrum_results: HashMap::new(),
            clap_gui_handles: HashMap::new(),
            output_channel_count: 2,
            device_channel_counts: HashMap::new(),
            secondary_streams: HashMap::new(),
            pending_connection_sync: std::collections::HashSet::new(),
            tts_buffers: HashMap::new(),
            tts_running: std::collections::HashSet::new(),
        }
    }

    /// Create a cheap placeholder (used during mem::replace swap)
    pub fn placeholder() -> Self {
        Self {
            stream: None,
            output_device_name: String::new(),
            _input_device_name: String::new(),
            file_buffers: HashMap::new(),
            file_threads: HashMap::new(),
            file_looping: HashMap::new(),
            file_playing: HashMap::new(),
            file_durations: HashMap::new(),
            input_streams: HashMap::new(),
            input_buffers: HashMap::new(),
            sampler_buffers: HashMap::new(),
            sampler_input_sources: HashMap::new(),
            cached_output_devices: Vec::new(),
            cached_input_devices: Vec::new(),
            audio_error: Arc::new(AtomicBool::new(false)),
            dropout_count: Arc::new(AtomicU32::new(0)),
            master_volume: Arc::new(super::params::AtomicF32::new(0.8)),
            engine_tx: None, node_params: HashMap::new(),
            engine_sample_rate: 44100.0, engine_needs_rebuild: false,
            analyzer_results: HashMap::new(),
            spectrum_results: HashMap::new(),
            clap_gui_handles: HashMap::new(),
            output_channel_count: 2,
            device_channel_counts: HashMap::new(),
            secondary_streams: HashMap::new(),
            pending_connection_sync: std::collections::HashSet::new(),
            tts_buffers: HashMap::new(),
            tts_running: std::collections::HashSet::new(),
        }
    }

    /// Refresh cached device lists and channel counts (call every ~60 frames, not every frame)
    #[allow(dead_code)]
    pub fn refresh_devices(&mut self) {
        let host = cpal::default_host();
        self.device_channel_counts.clear();
        if let Ok(devs) = host.output_devices() {
            let mut names = Vec::new();
            for d in devs {
                if let Ok(name) = d.name() {
                    if let Ok(cfg) = d.default_output_config() {
                        self.device_channel_counts.insert(name.clone(), cfg.channels() as usize);
                    }
                    names.push(name);
                }
            }
            self.cached_output_devices = names;
        }
        self.cached_input_devices = host.input_devices()
            .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default();
    }

    pub fn set_device_lists(&mut self, output: Vec<String>, input: Vec<String>) {
        self.cached_output_devices = output;
        self.cached_input_devices = input;
    }

    /// Start audio output on the default (or named) device.
    /// Creates an AudioEngine on the audio thread, connected via command channel.
    pub fn start_output(&mut self, device_name: Option<&str>) -> Result<(), String> {
        // Stop existing stream
        self.stream = None;

        let host = cpal::default_host();
        let device = if let Some(name) = device_name {
            host.output_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.name().ok().as_deref() == Some(name))
                .ok_or_else(|| format!("Device '{}' not found", name))?
        } else {
            host.default_output_device()
                .ok_or("No default output device")?
        };

        self.output_device_name = device.name().unwrap_or_default();

        let config = device.default_output_config()
            .map_err(|e| format!("No output config: {}", e))?;

        let sample_rate = config.sample_rate().0 as f32;
        let channels = config.channels() as usize;
        self.engine_sample_rate = sample_rate;
        self.output_channel_count = channels;


        // Create command channel for the new engine.
        // Clear stale state from any previous engine — node_params pointed to
        // the old engine's processors which no longer exist.
        let (tx, rx) = crossbeam_channel::unbounded();
        self.engine_tx = Some(tx);
        self.node_params.clear();
        self.engine_needs_rebuild = true;

        // Create engine — owned by the audio thread closure
        let master_vol = self.master_volume.clone();
        let mut engine = super::engine::AudioEngine::new(rx, sample_rate, master_vol);

        let stream = device.build_output_stream(
            &config.into(),
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                engine.execute(data, channels);
            },
            {
                let error_flag = self.audio_error.clone();
                move |err| {
                    crate::system_log::error(format!("Audio error: {}", err));
                    error_flag.store(true, Ordering::Relaxed);
                }
            },
            None,
        ).map_err(|e| format!("Build stream failed: {}", e))?;

        stream.play().map_err(|e| format!("Play failed: {}", e))?;
        self.stream = Some(stream);

        crate::system_log::log(format!("DSP started: {} @ {}Hz", self.output_device_name, sample_rate));
        Ok(())
    }

    /// Close all open CLAP plugin GUI windows. Call before stopping DSP.
    pub fn close_all_guis(&mut self) {
        for (_, handle) in &self.clap_gui_handles {
            if let Ok(mut h) = handle.try_lock() {
                h.close();
            }
        }
    }

    /// Stop audio output
    pub fn stop_output(&mut self) {
        self.close_all_guis(); // Close plugin GUIs before dropping the engine
        // Pause the stream before dropping — CPAL's drop can have a slight delay,
        // during which the audio callback may fire one more time.
        if let Some(stream) = self.stream.take() {
            use cpal::traits::StreamTrait;
            let _ = stream.pause();
            drop(stream);
        }
        crate::system_log::log("DSP stopped");
        self.engine_tx = None;
        self.node_params.clear();
    }

    pub fn is_running(&self) -> bool {
        self.stream.is_some()
    }

    // ── Engine API ────────────────────────────────────────────────────────

    /// Send a command to the audio engine (non-blocking).
    pub fn send_command(&self, cmd: super::engine::AudioCommand) {
        if let Some(tx) = &self.engine_tx {
            let _ = tx.send(cmd);
        }
    }

    /// Add a processor to the audio engine. Returns the shared param handle
    /// so node renders can write parameters directly via atomics.
    pub fn add_processor(
        &mut self,
        node_id: NodeId,
        processor: Box<dyn super::processor::AudioProcessor>,
        param_count: usize,
    ) -> Arc<Vec<super::params::AtomicF32>> {
        // Create per-node atomic param storage
        let params: Vec<super::params::AtomicF32> = (0..param_count)
            .map(|_| super::params::AtomicF32::new(0.0))
            .collect();
        let params = Arc::new(params);
        self.node_params.insert(node_id, params.clone());

        // Send to engine
        self.send_command(super::engine::AudioCommand::AddProcessor {
            node_id,
            processor,
            params: params.clone(),
        });

        params
    }

    /// Remove a processor from the audio engine.
    pub fn remove_processor(&mut self, node_id: NodeId) {
        self.node_params.remove(&node_id);
        self.send_command(super::engine::AudioCommand::RemoveProcessor { node_id });
    }

    /// Connect one processor's output to another's input port.
    pub fn connect_audio(&self, from_node: NodeId, to_node: NodeId, to_port: usize) {
        self.send_command(super::engine::AudioCommand::Connect { from_node, to_node, to_port });
    }

    /// Disconnect an input port.
    pub fn disconnect_audio(&self, to_node: NodeId, to_port: usize) {
        self.send_command(super::engine::AudioCommand::Disconnect { to_node, to_port });
    }

    /// Hot-swap the band coefficients of an existing EQ processor.
    /// `points` is the raw curve from the `AudioEq` node; this function
    /// computes the biquad bank and ships it to the audio thread.
    /// Cheap enough to call every frame because the engine drops the
    /// command on the floor if `node_id` doesn't refer to an EQ processor.
    pub fn update_eq_bands(&self, node_id: NodeId, points: &[[f32; 2]]) {
        let bands = crate::audio::curve_to_eq_bands(points, self.engine_sample_rate);
        let hash = crate::audio::eq_curve_hash(points);
        self.send_command(super::engine::AudioCommand::UpdateEqBands {
            node_id,
            bands,
            hash,
        });
    }

    /// Write a parameter value directly to the node's atomic storage.
    /// No channel, no lock — just an atomic store. Called from node UI code.
    pub fn engine_write_param(&self, node_id: NodeId, param_index: usize, value: f32) {
        if let Some(params) = self.node_params.get(&node_id) {
            if param_index < params.len() {
                params[param_index].store(value);
            }
        }
    }

    /// Check if a node has a processor registered in the engine.
    pub fn has_processor(&self, node_id: NodeId) -> bool {
        self.node_params.contains_key(&node_id)
    }

    /// Create a processor for a given NodeType. Returns None for non-audio nodes.
    pub fn create_processor_for_node(&mut self, node_type: &crate::graph::NodeType, nid: NodeId) -> Option<(Box<dyn super::processor::AudioProcessor>, usize)> {
        use crate::graph::NodeType;
        use super::processors::*;

        match node_type {
            NodeType::Synth { waveform, frequency, amplitude, active, fm_depth } => {
                let mut p = synth::SynthProcessor::new(*waveform, *frequency, *amplitude);
                p.active = *active;
                p.fm_depth = *fm_depth;
                Some((Box::new(p), 5))
            }
            NodeType::Speaker { volume, .. } => {
                Some((Box::new(speaker::SpeakerProcessor::new(*volume)), 4))
            }
            NodeType::AudioDelay { time_ms, feedback } => {
                Some((Box::new(effects::DelayProcessor::new(*time_ms, *feedback)), 2))
            }
            NodeType::AudioDistortion { drive } => {
                Some((Box::new(effects::DistortionProcessor::new(*drive)), 1))
            }
            NodeType::AudioPitchShift { semitones } => {
                Some((Box::new(effects::PitchShiftProcessor::new(*semitones)), 1))
            }
            NodeType::AudioReverb { room_size, damping, mix } => {
                Some((Box::new(effects::ReverbProcessor::new(*room_size, *damping, *mix)), 3))
            }
            NodeType::AudioLowPass { cutoff } => {
                Some((Box::new(effects::LowPassProcessor::new(*cutoff)), 1))
            }
            NodeType::AudioHighPass { cutoff } => {
                Some((Box::new(effects::HighPassProcessor::new(*cutoff)), 1))
            }
            NodeType::AudioGain { level } => {
                Some((Box::new(effects::GainProcessor::new(*level)), 1))
            }
            NodeType::AudioEq { points } => {
                let bands = crate::audio::curve_to_eq_bands(points, self.engine_sample_rate);
                let hash = crate::audio::eq_curve_hash(points);
                Some((Box::new(effects::EqProcessor::new(bands, hash)), 0))
            }
            NodeType::AudioMixer { .. } => {
                // Allocate max 8 param slots so adding channels doesn't require re-registration
                Some((Box::new(mixer::MixerProcessor::new()), 8))
            }
            NodeType::AudioAnalyzer => {
                let (proc, analysis) = analyzer::AnalyzerProcessor::new();
                self.analyzer_results.insert(nid, analysis);
                Some((Box::new(proc), 0))
            }
            NodeType::Dynamic { inner } if inner.node.type_tag() == "audio_analyzer" => {
                let (proc, analysis) = analyzer::AnalyzerProcessor::new();
                self.analyzer_results.insert(nid, analysis);
                Some((Box::new(proc), 0))
            }
            NodeType::SpectrumAnalyzer => {
                let (proc, spectrum) = spectrum::SpectrumProcessor::new();
                self.spectrum_results.insert(nid, spectrum);
                Some((Box::new(proc), 0))
            }
            NodeType::AudioInput { gain, .. } => {
                if let Some(buf) = self.input_buffers.get(&nid) {
                    Some((Box::new(input::LiveInputProcessor { buffer: buf.clone(), gain: *gain }), 1))
                } else { None }
            }
            NodeType::AudioPlayer { volume, .. } => {
                if let Some(buf) = self.file_buffers.get(&nid) {
                    Some((Box::new(input::FilePlayerProcessor { buffer: buf.clone(), volume: *volume }), 1))
                } else { None }
            }
            NodeType::AudioSampler { volume, record_duration, .. } => {
                // Make sure a buffer exists so the processor can be registered
                // during auto-registration of newly-added nodes. Without this,
                // a freshly created Sampler node has no processor/connections
                // until the user toggles DSP off/on.
                let buf = if let Some(buf) = self.sampler_buffers.get(&nid) {
                    buf.clone()
                } else {
                    let sr = self.engine_sample_rate as u32;
                    let buf = std::sync::Arc::new(super::buffers::SamplerBuffer::new(sr, *record_duration));
                    self.sampler_buffers.insert(nid, buf.clone());
                    buf
                };
                Some((Box::new(sampler::SamplerProcessor::new(buf, *volume)), 5))
            }
            _ => None,
        }
    }

    /// Rebuild the audio engine from the full graph state.
    /// Called after start_output() or project load to re-register all audio nodes.
    pub fn rebuild_engine_from_graph(&mut self, graph: &crate::graph::Graph) {
        self.node_params.clear();

        for (&nid, node) in &graph.nodes {
            if let Some((processor, param_count)) = self.create_processor_for_node(&node.node_type, nid) {
                self.add_processor(nid, processor, param_count);
                if let crate::graph::NodeType::Speaker { active, .. } = &node.node_type {
                    self.send_command(super::engine::AudioCommand::SetSpeaker {
                        node_id: nid, active: *active,
                    });
                }
            }
        }

        // Re-establish all audio connections
        for conn in &graph.connections {
            let from_audio = self.node_params.contains_key(&conn.from_node);
            let to_audio = self.node_params.contains_key(&conn.to_node);
            if !from_audio || !to_audio { continue; }

            // For mixer nodes: only connect audio ports (even indices: 0, 2, 4).
            // Odd indices (1, 3, 5) are gain control ports — handled by graph-layer
            // values, not engine connections.
            let is_mixer = matches!(graph.nodes.get(&conn.to_node).map(|n| &n.node_type),
                Some(crate::graph::NodeType::AudioMixer { .. }));
            if is_mixer {
                if conn.to_port % 2 != 0 { continue; } // skip gain ports
                self.connect_audio(conn.from_node, conn.to_node, conn.to_port / 2);
            } else {
                self.connect_audio(conn.from_node, conn.to_node, conn.to_port);
            }
        }
    }

    // ── Sampler ─────────────────────────────────────────────────────────────

    /// Get or create a sampler buffer for the given node.
    /// Buffer capacity is based on record_duration × sample rate.
    pub fn get_or_create_sampler_buffer(&mut self, node_id: NodeId, record_duration: f32) -> std::sync::Arc<super::buffers::SamplerBuffer> {
        if let Some(buf) = self.sampler_buffers.get(&node_id) {
            return buf.clone();
        }
        let sr = self.engine_sample_rate as u32;
        let buf = std::sync::Arc::new(super::buffers::SamplerBuffer::new(sr, record_duration));
        self.sampler_buffers.insert(node_id, buf.clone());

        // Register processor in the engine (if running)
        if self.engine_tx.is_some() && !self.has_processor(node_id) {
            let processor = Box::new(super::processors::sampler::SamplerProcessor::new(buf.clone(), 1.0));
            self.add_processor(node_id, processor, 5);
        }

        buf
    }

    // ── Live Audio Input ────────────────────────────────────────────────────

    /// Start capturing audio from the named input device (or default).
    /// Creates a CPAL input stream that writes into a lock-free ring buffer.
    pub fn start_input(&mut self, node_id: NodeId, device_name: Option<&str>) -> Result<(), String> {
        // Stop existing input for this node
        self.stop_input(node_id);

        let host = cpal::default_host();
        let device = if let Some(name) = device_name {
            host.input_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.name().ok().as_deref() == Some(name))
                .ok_or_else(|| format!("Input device '{}' not found", name))?
        } else {
            host.default_input_device()
                .ok_or("No default input device")?
        };

        let config = device.default_input_config()
            .map_err(|e| format!("No input config: {}", e))?;

        let sample_rate = config.sample_rate().0 as usize;
        let channels = config.channels() as usize;

        // Ring buffer holds 1 second of mono audio
        let buffer = Arc::new(LiveInputBuffer::new(sample_rate));
        let buf_clone = buffer.clone();

        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                buf_clone.write_interleaved(data, channels);
            },
            |err| {
                crate::system_log::error(format!("Audio input error: {}", err));
            },
            None,
        ).map_err(|e| format!("Build input stream failed: {}", e))?;

        stream.play().map_err(|e| format!("Input play failed: {}", e))?;

        self.input_streams.insert(node_id, stream);
        self.input_buffers.insert(node_id, buffer.clone());

        // Register processor in the engine (if engine is running).
        // If engine_tx is None, the auto-register in build_audio_chains will catch it.
        if self.engine_tx.is_some() && !self.has_processor(node_id) {
            let processor = Box::new(super::processors::input::LiveInputProcessor {
                buffer: buffer.clone(),
                gain: 1.0,
            });
            self.add_processor(node_id, processor, 1);
            // The engine's initial connection walk happened before this
            // processor existed, so any mic→sampler edges were silently
            // dropped. Mark this node for connection replay on the next
            // eval frame.
            self.pending_connection_sync.insert(node_id);
        }

        Ok(())
    }

    /// Stop capturing audio for a node.
    pub fn stop_input(&mut self, node_id: NodeId) {
        self.input_streams.remove(&node_id);
        self.input_buffers.remove(&node_id);
        self.remove_processor(node_id);
    }

    // ── TTS (Piper ONNX) ────────────────────────────────────────────────────

    /// Start a TTS synthesis job for the given node.
    /// Creates a FilePlayerBuffer, spawns the synthesis thread, and registers
    /// a FilePlayerProcessor so the synthesized audio flows into the audio engine.
    pub fn start_tts_synthesis(
        &mut self,
        node_id: NodeId,
        req: &crate::nodes::tts_node::TtsSynthRequest,
        tts_tx: &std::sync::mpsc::Sender<crate::nodes::tts_node::TtsSynthResult>,
    ) {
        // Stop any existing TTS playback for this node
        if let Some(old_buf) = self.tts_buffers.get(&node_id) {
            old_buf.stop_requested.store(true, Ordering::Relaxed);
        }

        // 30s capacity at engine sample rate
        let capacity = (self.engine_sample_rate * 30.0) as usize;
        let buffer = Arc::new(FilePlayerBuffer::new(capacity));
        let buf_clone = buffer.clone();

        let mut req_owned = req.clone();
        req_owned.engine_sample_rate = self.engine_sample_rate;
        let tx = tts_tx.clone();

        std::thread::Builder::new()
            .name(format!("tts-synth-{}", node_id))
            .spawn(move || {
                let result = crate::nodes::tts_node::run_tts_synthesis(buf_clone, req_owned);
                let _ = tx.send(result);
            })
            .ok();

        self.tts_buffers.insert(node_id, buffer.clone());

        // Register or hot-swap processor in the audio engine
        if self.engine_tx.is_some() {
            if self.has_processor(node_id) {
                // Hot-swap: remove old, add fresh with new buffer
                self.send_command(super::engine::AudioCommand::RemoveProcessor { node_id });
                self.node_params.remove(&node_id);
            }
            let processor = Box::new(super::processors::input::FilePlayerProcessor {
                buffer: buffer.clone(),
                volume: 1.0,
            });
            // Register with param_count=1, then immediately write volume=1.0.
            // add_processor() initialises all param slots to AtomicF32(0.0).
            // The engine reads params[0] and calls set_params([params[0]]) every block,
            // which would set FilePlayerProcessor::volume = 0.0 → silence.
            // Writing 1.0 here ensures the engine always reads 1.0.
            let tts_params = self.add_processor(node_id, processor, 1);
            if let Some(p) = tts_params.get(0) { p.store(1.0); }
            self.pending_connection_sync.insert(node_id);
        }

        self.tts_running.insert(node_id);
    }

    /// Play an audio file — if paused, resume; if stopped/new, start fresh.
    /// Decoded by Symphonia in a background thread → FilePlayerBuffer → CPAL callback.
    pub fn play_file(&mut self, node_id: NodeId, path: &str) -> Result<(), String> {
        // If paused, just resume
        if let Some(buf) = self.file_buffers.get(&node_id) {
            if buf.paused.load(Ordering::Relaxed) {
                buf.paused.store(false, Ordering::Release);
                self.file_playing.insert(node_id, true);
                return Ok(());
            }
            if !buf.finished.load(Ordering::Relaxed) {
                // Already playing
                self.file_playing.insert(node_id, true);
                return Ok(());
            }
        }

        // Stop any existing playback for this node
        self.stop_file(node_id);

        let output_sr = self.engine_sample_rate;

        // Probe file for duration (fast metadata read, no full decode)
        if !self.file_durations.contains_key(&node_id) {
            if let Some(dur) = probe_file_duration(path) {
                self.file_durations.insert(node_id, dur);
            }
        }

        // Create ring buffer (2 seconds at output sample rate)
        let capacity = (output_sr * 2.0) as usize;
        let buffer = Arc::new(FilePlayerBuffer::new(capacity));

        // Looping flag shared with decode thread
        let looping = Arc::new(AtomicBool::new(false));
        self.file_looping.insert(node_id, looping.clone());

        // Spawn decode thread
        let buf_clone = buffer.clone();
        let path_owned = path.to_string();
        let looping_clone = looping.clone();
        let handle = std::thread::Builder::new()
            .name(format!("file-decode-{}", node_id))
            .spawn(move || {
                decode_file_thread(path_owned, buf_clone, output_sr, looping_clone);
            })
            .map_err(|e| format!("Spawn decode thread: {}", e))?;

        self.file_buffers.insert(node_id, buffer.clone());
        self.file_threads.insert(node_id, handle);
        self.file_playing.insert(node_id, true);

        // Register processor in the engine (if running).
        // If engine_tx is None, the auto-register in build_audio_chains will catch it.
        if self.engine_tx.is_some() && !self.has_processor(node_id) {
            let processor = Box::new(super::processors::input::FilePlayerProcessor {
                buffer: buffer.clone(),
                volume: 1.0,
            });
            self.add_processor(node_id, processor, 1);
            // The engine's initial connection walk happened before this
            // processor existed, so any AudioPlayer→Speaker edges were
            // silently dropped. Mark this node for connection replay on
            // the next eval frame so the chain wires up without the user
            // having to toggle DSP off/on.
            self.pending_connection_sync.insert(node_id);
        }

        Ok(())
    }

    /// Pause file playback (keeps position, decode thread sleeps)
    pub fn pause_file(&mut self, node_id: NodeId) {
        if let Some(buf) = self.file_buffers.get(&node_id) {
            buf.paused.store(true, Ordering::Release);
        }
        self.file_playing.insert(node_id, false);
    }

    /// Seek file playback to a specific position (seconds).
    /// Signals the decode thread to seek — no thread restart needed.
    pub fn seek_file(&mut self, node_id: NodeId, _path: &str, position_secs: f64) -> Result<(), String> {
        if let Some(buf) = self.file_buffers.get(&node_id) {
            buf.seek_target_ms.store((position_secs * 1000.0) as usize, Ordering::Release);
            buf.seek_requested.store(true, Ordering::Release);
            self.file_playing.insert(node_id, true);
            Ok(())
        } else {
            Err("No active file player".into())
        }
    }

    /// Check if paused
    pub fn is_file_paused(&self, node_id: NodeId) -> bool {
        self.file_buffers.get(&node_id)
            .map(|b| b.paused.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// Check if playback has finished (EOF reached and buffer drained)
    pub fn is_file_finished(&self, node_id: NodeId) -> bool {
        self.file_buffers.get(&node_id)
            .map(|b| b.finished.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// Get duration of a file for a node (in seconds)
    pub fn get_file_duration(&self, node_id: NodeId) -> f64 {
        self.file_durations.get(&node_id).copied().unwrap_or(0.0)
    }

    /// Get current playback position in seconds.
    /// Uses callback-consumed position when a Speaker is connected (accurate),
    /// falls back to decoded position when no Speaker (playhead still moves).
    pub fn get_playback_position(&self, node_id: NodeId) -> f64 {
        if let Some(buf) = self.file_buffers.get(&node_id) {
            let callback_pos = buf.playback_position.load(Ordering::Relaxed);
            let decoded_pos = buf.decoded_position.load(Ordering::Relaxed);
            // Use whichever is further ahead — callback_pos when Speaker is consuming,
            // decoded_pos when no Speaker is connected
            let pos = callback_pos.max(decoded_pos);
            let sr = self.engine_sample_rate as f64;
            if sr > 0.0 { pos as f64 / sr } else { 0.0 }
        } else {
            0.0
        }
    }

    /// Set volume for a specific node's file player
    pub fn set_file_volume(&self, node_id: NodeId, volume: f32) {
        self.engine_write_param(node_id, 0, volume);
    }

    /// Set playback speed (turntable style — affects both tempo and pitch).
    /// 1.0 = normal, 0.5 = half speed (lower pitch), 2.0 = double speed (higher pitch).
    pub fn set_file_speed(&self, node_id: NodeId, speed: f32) {
        if let Some(buf) = self.file_buffers.get(&node_id) {
            let rate = (speed.clamp(0.1, 4.0) * 1000.0) as u32;
            buf.playback_rate_x1000.store(rate, Ordering::Relaxed);
        }
    }

    /// Update looping flag for a specific node's file player
    pub fn set_file_looping(&self, node_id: NodeId, looping: bool) {
        if let Some(flag) = self.file_looping.get(&node_id) {
            flag.store(looping, Ordering::Release);
        }
    }

    /// Stop file playback completely (signals thread, joins, removes source)
    pub fn stop_file(&mut self, node_id: NodeId) {
        // Signal decode thread to stop
        if let Some(buf) = self.file_buffers.get(&node_id) {
            buf.stop_requested.store(true, Ordering::Release);
        }
        // Join the thread (with timeout to avoid blocking)
        if let Some(handle) = self.file_threads.remove(&node_id) {
            let _ = handle.join();
        }
        // Remove buffer, source, and engine processor
        self.file_buffers.remove(&node_id);
        self.file_looping.remove(&node_id);
        self.file_playing.remove(&node_id);
        self.remove_processor(node_id);
    }

    /// Cleanup when a node is deleted
    pub fn cleanup_node(&mut self, node_id: NodeId) {
        self.stop_file(node_id);
        self.stop_input(node_id);
        self.sampler_buffers.remove(&node_id);
        self.sampler_input_sources.remove(&node_id);
        self.analyzer_results.remove(&node_id);
        self.spectrum_results.remove(&node_id);
        self.clap_gui_handles.remove(&node_id);
    }

}

