#![allow(dead_code)]
//! Input processors — LiveInput (microphone) and FilePlayer (audio files).
//! Both read from lock-free ring buffers filled by their respective threads.

use std::sync::Arc;
use crate::audio::processor::{AudioProcessor, ProcessorKind, ProcessContext};
use crate::audio::buffers::{LiveInputBuffer, FilePlayerBuffer};

// ── Live Input (Microphone) ───────────────────────────────────────────────────

pub struct LiveInputProcessor {
    pub buffer: Arc<LiveInputBuffer>,
    pub gain: f32,
    /// Auto-level gain control enabled. When true, signal is scaled toward
    /// `AGC_TARGET_RMS` before the manual gain trim is applied.
    agc_enabled: bool,
    /// RMS envelope follower state (0..1). Tracks block-level input RMS.
    agc_env: f32,
    /// Smoothed gain multiplier applied to samples when AGC is on. Slews
    /// toward `target_rms / agc_env`.
    agc_gain: f32,
}

const AGC_TARGET_RMS: f32 = 0.15;      // ≈ -16 dBFS, leaves headroom
const AGC_MIN_GAIN: f32 = 0.2;         // -14 dB — won't over-attenuate
const AGC_MAX_GAIN: f32 = 50.0;        // +34 dB — handles -40 dBFS mics
const AGC_ENV_ATTACK: f32 = 0.3;       // fast rise when signal surges
const AGC_ENV_RELEASE: f32 = 0.02;     // slow fall through silences
const AGC_GAIN_ATTACK: f32 = 0.02;     // slow rise so silence→speech doesn't jump
const AGC_GAIN_RELEASE: f32 = 0.2;     // faster drop to prevent clipping
/// Initial gain assumed for the first block after start/prepare. Picked so a
/// typical built-in mic (~ -30 dBFS raw speech) is immediately audible when
/// routed through a simple Mic → Effect → Speaker chain, rather than taking
/// ~1 second to ramp up from unity. Loud sources release down to their
/// correct gain within ~100 ms.
const AGC_INIT_GAIN: f32 = 8.0;

impl LiveInputProcessor {
    pub fn new(buffer: Arc<LiveInputBuffer>, gain: f32) -> Self {
        Self {
            buffer,
            gain,
            agc_enabled: true,
            agc_env: AGC_TARGET_RMS,
            agc_gain: AGC_INIT_GAIN,
        }
    }
}

impl AudioProcessor for LiveInputProcessor {
    fn type_id(&self) -> &str { "live_input" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Source }

    fn process_block(&mut self, _input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        self.buffer.read_into(output, ctx.block_size);

        if self.agc_enabled && ctx.block_size > 0 {
            // Per-block RMS
            let mut sumsq = 0.0f32;
            for &s in &output[..ctx.block_size] { sumsq += s * s; }
            let block_rms = (sumsq / ctx.block_size as f32).sqrt();
            // Envelope: fast attack to catch peaks, slow release so it doesn't
            // collapse during silences and then blast noise when speech resumes.
            let env_coeff = if block_rms > self.agc_env { AGC_ENV_ATTACK } else { AGC_ENV_RELEASE };
            self.agc_env += env_coeff * (block_rms - self.agc_env);
            // Target gain to hit AGC_TARGET_RMS; clamped to sane range.
            let target_gain = if self.agc_env > 1.0e-5 {
                (AGC_TARGET_RMS / self.agc_env).clamp(AGC_MIN_GAIN, AGC_MAX_GAIN)
            } else { 1.0 };
            // Slew the gain: slow when rising (prevents pumping up silence),
            // faster when falling (prevents clipping on a surge).
            let gain_coeff = if target_gain < self.agc_gain { AGC_GAIN_RELEASE } else { AGC_GAIN_ATTACK };
            self.agc_gain += gain_coeff * (target_gain - self.agc_gain);
            for s in output.iter_mut() { *s *= self.agc_gain; }
        }

        // Manual gain trim on top (applies regardless of AGC state).
        if (self.gain - 1.0).abs() > 0.001 {
            for s in output.iter_mut() { *s *= self.gain; }
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.gain = v; }
        if let Some(&v) = params.get(1) { self.agc_enabled = v > 0.5; }
    }

    fn param_count(&self) -> usize { 2 }
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {
        self.agc_env = AGC_TARGET_RMS;
        self.agc_gain = AGC_INIT_GAIN;
    }
    fn reset(&mut self) {
        self.agc_env = AGC_TARGET_RMS;
        self.agc_gain = AGC_INIT_GAIN;
    }
}

// ── File Player ───────────────────────────────────────────────────────────────

pub struct FilePlayerProcessor {
    pub buffer: Arc<FilePlayerBuffer>,
    pub volume: f32,
}

impl FilePlayerProcessor {
    pub fn new(buffer: Arc<FilePlayerBuffer>, volume: f32) -> Self {
        Self { buffer, volume }
    }
}

impl AudioProcessor for FilePlayerProcessor {
    fn type_id(&self) -> &str { "file_player" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Source }

    fn process_block(&mut self, _input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        self.buffer.read_into(output, ctx.block_size);
        if (self.volume - 1.0).abs() > 0.001 {
            for s in output.iter_mut() { *s *= self.volume; }
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.volume = v; }
    }

    fn param_count(&self) -> usize { 1 }
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {}
}
