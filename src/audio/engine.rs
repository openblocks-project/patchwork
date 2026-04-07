//! AudioEngine — VCV Rack-style per-node audio processing.
//!
//! The engine is owned entirely by the audio thread. The UI communicates via:
//! - AudioCommand channel (topology changes: add/remove/connect/disconnect)
//! - Arc<Vec<AtomicF32>> per processor (parameter updates: lock-free atomic writes)
//!
//! Each processor owns its output buffer. Connections are just indices into other
//! processors' output buffers. 1-block latency between connected nodes (inaudible).
//! No mutex, no topological sort, no compilation step.

use std::collections::HashMap;
use crossbeam_channel::Receiver;
use crate::graph::NodeId;
use super::processor::{AudioProcessor, ProcessorKind, ProcessContext};
use super::analysis::AudioAnalysis;
use super::params::AtomicF32;
use std::sync::Arc;

// ── Commands (UI → Audio Thread) ─────────────────────────────────────────────

/// Typed commands sent from the UI thread to the audio engine.
/// Processed non-blocking at the start of each audio callback.
pub enum AudioCommand {
    /// Add a new processor to the engine.
    AddProcessor {
        node_id: NodeId,
        processor: Box<dyn AudioProcessor>,
        params: Arc<Vec<AtomicF32>>,
    },
    /// Remove a processor and all its connections.
    RemoveProcessor {
        node_id: NodeId,
    },
    /// Connect one processor's output to another's input port.
    Connect {
        from_node: NodeId,
        to_node: NodeId,
        to_port: usize,
    },
    /// Disconnect an input port.
    Disconnect {
        to_node: NodeId,
        to_port: usize,
    },
    /// Mark/unmark a processor as a speaker (mixes to master output).
    SetSpeaker {
        node_id: NodeId,
        active: bool,
    },
    /// Hot-swap the band coefficients of an existing EQ processor.
    /// Used by `AudioEq` nodes when the user drags points on the curve —
    /// the alternative (remove + re-add the processor) would drop all
    /// connections and create a click.
    UpdateEqBands {
        node_id: NodeId,
        bands: Vec<super::biquad::BiquadFilter>,
        hash: u64,
    },
}

// ── Engine Internals ─────────────────────────────────────────────────────────

/// A single processor slot in the engine.
struct ProcessorSlot {
    processor: Box<dyn AudioProcessor>,
    /// This processor's output buffer (persists across callbacks).
    /// For mono processors: `max_block_size` samples.
    /// For stereo processors: `2 * max_block_size` samples (interleaved L, R).
    output_buffer: Vec<f32>,
    /// Number of output channels (1 = mono, 2 = interleaved stereo).
    output_channels: usize,
    /// Input connections: port index → source node ID.
    /// The engine reads from the source node's output_buffer.
    inputs: Vec<Option<NodeId>>,  // indexed by port
    /// Whether this processor is an active speaker (mixes to master).
    is_speaker: bool,
    /// Shared atomic parameters. UI writes, audio reads.
    params: Arc<Vec<AtomicF32>>,
    /// Scratch buffer for reading params into f32 slice.
    param_scratch: Vec<f32>,
}

/// The audio engine. Lives entirely on the audio thread.
///
/// UI sends commands via crossbeam channel. Parameters update via atomics.
/// No mutex. No compilation. No snapshots. Just iterate and process.
pub struct AudioEngine {
    slots: HashMap<NodeId, ProcessorSlot>,
    commands: Receiver<AudioCommand>,
    /// Shared silence buffer (all zeros, read by unconnected inputs).
    silence: Vec<f32>,
    /// Scratch buffer for building mixed input from multiple sources.
    input_scratch: Vec<f32>,
    sample_rate: f32,
    master_volume: Arc<AtomicF32>,
    pub master_analysis: AudioAnalysis,
    max_block_size: usize,
    /// Pre-collected speaker IDs (rebuilt when speakers change).
    speaker_ids: Vec<NodeId>,
    /// Pre-collected processor IDs for iteration (avoids HashMap key collect each frame).
    processor_ids: Vec<NodeId>,
    ids_dirty: bool,
}

impl AudioEngine {
    /// Create a new engine. The Receiver end of the command channel is moved in.
    pub fn new(commands: Receiver<AudioCommand>, sample_rate: f32, master_volume: Arc<AtomicF32>) -> Self {
        let max_block_size = 2048;
        Self {
            slots: HashMap::new(),
            commands,
            silence: vec![0.0f32; max_block_size],
            input_scratch: vec![0.0f32; max_block_size],
            sample_rate,
            master_volume,
            master_analysis: AudioAnalysis::default(),
            max_block_size,
            speaker_ids: Vec::new(),
            processor_ids: Vec::new(),
            ids_dirty: true,
        }
    }

    /// Drain all pending commands from the UI (non-blocking).
    fn process_commands(&mut self) {
        while let Ok(cmd) = self.commands.try_recv() {
            match cmd {
                AudioCommand::AddProcessor { node_id, mut processor, params } => {
                    processor.prepare(self.sample_rate, self.max_block_size);
                    processor.set_shared_params(params.clone());
                    let param_count = processor.param_count();
                    let is_speaker = processor.kind() == ProcessorKind::Output;
                    let output_channels = processor.output_channels();
                    self.slots.insert(node_id, ProcessorSlot {
                        processor,
                        output_buffer: vec![0.0f32; self.max_block_size * output_channels],
                        output_channels,
                        inputs: Vec::new(),
                        is_speaker,
                        params,
                        param_scratch: vec![0.0f32; param_count],
                    });
                    self.ids_dirty = true;
                }
                AudioCommand::RemoveProcessor { node_id } => {
                    self.slots.remove(&node_id);
                    // Remove any connections TO this node from other nodes
                    for slot in self.slots.values_mut() {
                        for input in slot.inputs.iter_mut() {
                            if *input == Some(node_id) {
                                *input = None;
                            }
                        }
                    }
                    self.ids_dirty = true;
                }
                AudioCommand::Connect { from_node, to_node, to_port } => {
                    if let Some(slot) = self.slots.get_mut(&to_node) {
                        // Grow inputs vec if needed
                        while slot.inputs.len() <= to_port {
                            slot.inputs.push(None);
                        }
                        slot.inputs[to_port] = Some(from_node);
                    }
                }
                AudioCommand::Disconnect { to_node, to_port } => {
                    if let Some(slot) = self.slots.get_mut(&to_node) {
                        if to_port < slot.inputs.len() {
                            slot.inputs[to_port] = None;
                        }
                    }
                }
                AudioCommand::SetSpeaker { node_id, active } => {
                    if let Some(slot) = self.slots.get_mut(&node_id) {
                        slot.is_speaker = active;
                    }
                    self.ids_dirty = true;
                }
                AudioCommand::UpdateEqBands { node_id, bands, hash } => {
                    if let Some(slot) = self.slots.get_mut(&node_id) {
                        slot.processor.set_eq_bands(bands, hash);
                    }
                }
            }
        }

        // Rebuild cached ID lists if topology changed
        if self.ids_dirty {
            self.processor_ids = self.slots.keys().copied().collect();
            self.speaker_ids = self.slots.iter()
                .filter(|(_, slot)| slot.is_speaker)
                .map(|(&id, _)| id)
                .collect();
            self.ids_dirty = false;
        }
    }

    /// Execute one audio callback. Called by CPAL on the audio thread.
    ///
    /// 1. Drain commands
    /// 2. For each processor: read params, build input, process
    /// 3. Mix speaker outputs to master
    /// 4. Clamp and analyze
    pub fn execute(&mut self, data: &mut [f32], channels: usize) {
        let num_frames = data.len() / channels;
        if num_frames == 0 { return; }

        // Zero master output
        data.fill(0.0);

        // 1. Process pending commands
        self.process_commands();

        let ctx = ProcessContext {
            sample_rate: self.sample_rate,
            block_size: num_frames,
        };

        // 2. Process each node
        // Two-phase approach to satisfy borrow checker:
        //   Phase A: for each node, read connected inputs into scratch buffer (immutable borrows)
        //   Phase B: call process_block with scratch as input, node's output_buffer as output (mutable borrow)
        for idx in 0..self.processor_ids.len() {
            let node_id = self.processor_ids[idx];

            // Phase A: Build input buffer from connections
            // Read from connected sources' output buffers (written in previous callback or
            // earlier this callback — 1-block latency, which is fine)
            self.input_scratch[..num_frames].fill(0.0);
            let mut has_input = false;

            // Read input connections into stack array (zero heap allocation)
            const MAX_INPUTS: usize = 16;
            let mut conn_buf: [(Option<NodeId>, f32); MAX_INPUTS] = [(None, 1.0); MAX_INPUTS];
            let mut conn_count = 0usize;

            if let Some(slot) = self.slots.get(&node_id) {
                let is_mixer = slot.processor.kind() == ProcessorKind::Mixer;
                for (i, src) in slot.inputs.iter().enumerate() {
                    if i >= MAX_INPUTS { break; }
                    let gain = if is_mixer {
                        if i < slot.params.len() { slot.params[i].load() } else { 1.0 }
                    } else { 1.0 };
                    conn_buf[i] = (*src, gain);
                    conn_count = i + 1;
                }
            }

            for i in 0..conn_count {
                let (maybe_src, gain) = conn_buf[i];
                if let Some(src_id) = maybe_src {
                    if let Some(src_slot) = self.slots.get(&src_id) {
                        for s in 0..num_frames {
                            self.input_scratch[s] += src_slot.output_buffer[s] * gain;
                        }
                        has_input = true;
                    }
                }
            }

            // Phase B: Read params and process
            if let Some(slot) = self.slots.get_mut(&node_id) {
                // Read atomic params
                let pc = slot.param_scratch.len();
                if pc > 0 {
                    for i in 0..pc {
                        slot.param_scratch[i] = if i < slot.params.len() {
                            slot.params[i].load()
                        } else {
                            0.0
                        };
                    }
                    slot.processor.set_params(&slot.param_scratch);
                }

                // Process
                let input = if has_input {
                    &self.input_scratch[..num_frames]
                } else {
                    &self.silence[..num_frames]
                };
                let out_len = num_frames * slot.output_channels;
                slot.processor.process_block(input, &mut slot.output_buffer[..out_len], &ctx);
            }
        }

        // 3. Mix active speakers to master output
        // Speaker reads L (port 0) and R (port 1) inputs separately.
        // Params: [0] = volume, [1] = active (>0.5), [2] = pan (-1..+1), [3] = channel_offset (0,2,4,...)
        // channel_offset routes output to specific hardware channel pair.
        // e.g. offset=0 → ch 0-1, offset=2 → ch 2-3 (for multi-channel sound cards)
        let master_vol = self.master_volume.load();
        for &spk_id in &self.speaker_ids {
            // Read speaker params
            let (l_source, r_source, volume, active, pan, ch_offset) = {
                if let Some(slot) = self.slots.get(&spk_id) {
                    let l = slot.inputs.get(0).and_then(|id| *id);
                    let r = slot.inputs.get(1).and_then(|id| *id);
                    let vol = if slot.params.len() > 0 { slot.params[0].load().clamp(0.0, 1.0) } else { 0.8 };
                    let act = if slot.params.len() > 1 { slot.params[1].load() > 0.5 } else { true };
                    let pan = if slot.params.len() > 2 { slot.params[2].load().clamp(-1.0, 1.0) } else { 0.0 };
                    // Channel offset: round to even number (channel pairs)
                    let offset = if slot.params.len() > 3 {
                        let raw = slot.params[3].load() as usize;
                        raw & !1  // round down to even
                    } else { 0 };
                    (l, r, vol, act, pan, offset)
                } else {
                    continue;
                }
            };

            if !active { continue; }
            // Skip if channel offset is beyond device capability
            if ch_offset >= channels { continue; }

            // Get L and R source buffers (mono)
            let l_buf = l_source.and_then(|id| self.slots.get(&id).map(|s| &s.output_buffer));
            let r_buf = r_source.and_then(|id| self.slots.get(&id).map(|s| &s.output_buffer));

            // Equal-power pan coefficients
            let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4; // 0 to PI/2
            let pan_l = angle.cos();
            let pan_r = angle.sin();

            for frame in 0..num_frames {
                let l_in = l_buf.map(|b| b[frame]).unwrap_or(0.0);
                let r_in = r_buf.map(|b| b[frame]).unwrap_or(0.0);

                let (l_out, r_out) = if l_buf.is_some() && r_buf.is_some() {
                    // True stereo: pan acts as balance
                    (l_in * pan_l * volume, r_in * pan_r * volume)
                } else if l_buf.is_some() {
                    // Mono from L: pan positions in stereo field
                    (l_in * pan_l * volume, l_in * pan_r * volume)
                } else {
                    // Only R (or nothing): R on right channel
                    (0.0, r_in * volume)
                };

                let base = frame * channels + ch_offset;
                if ch_offset + 1 < channels {
                    // Stereo pair at channel offset
                    data[base] += l_out * master_vol;
                    data[base + 1] += r_out * master_vol;
                } else if ch_offset < channels {
                    // Only one channel available at this offset
                    data[base] += (l_out + r_out) * 0.5 * master_vol;
                }
            }
        }

        // 4. Clamp
        for s in data.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }

        // 5. Master analysis
        self.master_analysis.update(data, channels, self.sample_rate);
    }
}
