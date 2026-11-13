//! ClapProcessor — wraps a ClapInstance as an AudioProcessor.
//!
//! Adapts to plugin type:
//! - Effect: passes audio through, ProcessorKind::Effect
//! - Instrument: generates audio from note events, ProcessorKind::Source
//!   Uses 3 virtual params (Note, Velocity, Gate) at the end of the param array
//!   to receive MIDI data via the existing atomic param system.

use crate::audio::clap_host::{ClapInstance, ClapPluginType};
use crate::audio::params::AtomicF32;
use crate::audio::processor::{AudioProcessor, ProcessContext, ProcessorKind};
use std::sync::Arc;

pub struct ClapProcessor {
    instance: ClapInstance,
    plugin_type: ClapPluginType,
    /// Cached param IDs (same order as our param slots)
    param_ids: Vec<u32>,
    /// Cached param ranges for scaling
    param_ranges: Vec<(f64, f64)>,
    /// Previous param values — only send events when values actually change.
    prev_params: Vec<f32>,
    /// Shared atomic params with the UI (for writing back GUI changes)
    shared_params: Option<Arc<Vec<AtomicF32>>>,
    /// For instruments: previous gate state (for edge detection)
    prev_gate: f32,
    /// For instruments: currently held note (-1 = none)
    current_note: i16,
}

impl ClapProcessor {
    pub fn new(instance: ClapInstance) -> Self {
        let plugin_type = instance.info.plugin_type;
        let param_ids: Vec<u32> = instance.info.params.iter().map(|p| p.id).collect();
        let param_ranges: Vec<(f64, f64)> = instance
            .info
            .params
            .iter()
            .map(|p| (p.min, p.max))
            .collect();
        // For instruments, add 3 extra slots for virtual note params (Note, Vel, Gate)
        let total_params = param_ids.len()
            + if plugin_type == ClapPluginType::Instrument {
                3
            } else {
                0
            };
        let prev_params = vec![f32::NAN; total_params];
        Self {
            instance,
            plugin_type,
            param_ids,
            param_ranges,
            prev_params,
            shared_params: None,
            prev_gate: 0.0,
            current_note: -1,
        }
    }
}

impl AudioProcessor for ClapProcessor {
    fn type_id(&self) -> &str {
        "clap_plugin"
    }

    fn kind(&self) -> ProcessorKind {
        match self.plugin_type {
            ClapPluginType::Instrument => ProcessorKind::Source,
            ClapPluginType::Effect => ProcessorKind::Effect,
        }
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        self.instance.process_audio(input, output, ctx.block_size);

        // Read back GUI param changes and update shared atomics
        if let Some(ref shared) = self.shared_params {
            for &(param_id, value) in &self.instance.gui_param_changes {
                if let Some(idx) = self.param_ids.iter().position(|&id| id == param_id) {
                    let (min, max) = self.param_ranges[idx];
                    let normalized = if (max - min).abs() > f64::EPSILON {
                        ((value - min) / (max - min)) as f32
                    } else {
                        0.0
                    };
                    if idx < shared.len() {
                        shared[idx].store(normalized);
                    }
                    if idx < self.prev_params.len() {
                        self.prev_params[idx] = normalized;
                    }
                }
            }
        }
        self.instance.gui_param_changes.clear();
    }

    fn set_params(&mut self, params: &[f32]) {
        let real_param_count = self.param_ids.len();

        // Send real plugin params (only when changed)
        for (i, &value) in params.iter().enumerate().take(real_param_count) {
            if i < self.prev_params.len() && (self.prev_params[i] - value).abs() < 0.0001 {
                continue;
            }
            let (min, max) = self.param_ranges[i];
            let scaled = min + (value as f64) * (max - min);
            self.instance.set_param(self.param_ids[i], scaled);
            if i < self.prev_params.len() {
                self.prev_params[i] = value;
            }
        }

        // For instruments: handle virtual note params (Note, Velocity, Gate)
        if self.plugin_type == ClapPluginType::Instrument && params.len() > real_param_count {
            let note_idx = real_param_count;
            let vel_idx = real_param_count + 1;
            let gate_idx = real_param_count + 2;

            let note = params.get(note_idx).copied().unwrap_or(60.0);
            let velocity = params.get(vel_idx).copied().unwrap_or(0.8);
            let gate = params.get(gate_idx).copied().unwrap_or(0.0);

            // Gate edge detection: rising = note on, falling = note off
            if gate > 0.5 && self.prev_gate <= 0.5 {
                // Note on — send note-off for previous note first (if any)
                if self.current_note >= 0 {
                    self.instance.note_off(self.current_note, 0.0, 0);
                }
                let key = note.round() as i16;
                self.instance.note_on(key, velocity as f64, 0);
                self.current_note = key;
            } else if gate <= 0.5 && self.prev_gate > 0.5 {
                // Note off
                if self.current_note >= 0 {
                    self.instance.note_off(self.current_note, 0.0, 0);
                    self.current_note = -1;
                }
            } else if gate > 0.5 && self.current_note >= 0 {
                // Note is held — check if note number changed (glide/legato)
                let key = note.round() as i16;
                if key != self.current_note {
                    self.instance.note_off(self.current_note, 0.0, 0);
                    self.instance.note_on(key, velocity as f64, 0);
                    self.current_note = key;
                }
            }

            self.prev_gate = gate;
        }
    }

    fn param_count(&self) -> usize {
        let base = self.param_ids.len();
        if self.plugin_type == ClapPluginType::Instrument {
            base + 3
        } else {
            base
        }
    }

    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {
        self.prev_params.fill(f32::NAN);
        self.prev_gate = 0.0;
        self.current_note = -1;
    }

    fn set_shared_params(&mut self, params: Arc<Vec<AtomicF32>>) {
        self.shared_params = Some(params);
    }
}
