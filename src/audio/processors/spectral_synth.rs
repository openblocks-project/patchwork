//! SpectralSynthProcessor — additive synthesis from 64 frequency bin magnitudes.
//!
//! Reads 64 magnitude values (params 0-63) and generates audio by summing
//! 64 sine oscillators at log-spaced frequencies matching the spectrogram bins.
//! This allows converting spectrogram images back into audio.

use crate::audio::processor::{AudioProcessor, ProcessorKind, ProcessContext};

const NUM_BINS: usize = 64;
const PARAM_VOLUME: usize = 64;
const PARAM_GATE: usize = 65;
// Param 66 reserved for future use
const TOTAL_PARAMS: usize = 67;

pub struct SpectralSynthProcessor {
    phases: [f64; NUM_BINS],
    magnitudes: [f32; NUM_BINS],
    freqs: [f64; NUM_BINS],
    volume: f32,
    gate: bool,
    sample_rate: f64,
}

impl SpectralSynthProcessor {
    pub fn new() -> Self {
        Self {
            phases: [0.0; NUM_BINS],
            magnitudes: [0.0; NUM_BINS],
            freqs: [0.0; NUM_BINS],
            volume: 0.8,
            gate: true,
            sample_rate: 44100.0,
        }
    }

    fn compute_frequencies(&mut self) {
        // Log-spaced 30 Hz → Nyquist, matching analysis.rs spectrogram bins
        let min_f = 30.0f64.ln();
        let max_f = (self.sample_rate * 0.5).ln();
        for b in 0..NUM_BINS {
            self.freqs[b] = (min_f + (max_f - min_f) * (b as f64 + 0.5) / NUM_BINS as f64).exp();
        }
    }
}

impl AudioProcessor for SpectralSynthProcessor {
    fn type_id(&self) -> &str { "spectral_synth" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Source }

    fn process_block(&mut self, _input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        if !self.gate || self.volume < 0.0001 {
            output[..ctx.block_size].fill(0.0);
            return;
        }

        // Normalization: sqrt(64) ≈ 8 for uncorrelated sines
        let norm = self.volume / 8.0;
        let sr = self.sample_rate;

        for i in 0..ctx.block_size {
            let mut sample = 0.0f64;
            for bin in 0..NUM_BINS {
                let mag = self.magnitudes[bin] as f64;
                if mag > 0.001 {
                    sample += (self.phases[bin] * std::f64::consts::TAU).sin() * mag;
                }
                self.phases[bin] += self.freqs[bin] / sr;
                if self.phases[bin] >= 1.0 { self.phases[bin] -= 1.0; }
            }
            output[i] = (sample * norm as f64) as f32;
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        for i in 0..NUM_BINS {
            if let Some(&v) = params.get(i) {
                self.magnitudes[i] = v.clamp(0.0, 1.0);
            }
        }
        if let Some(&v) = params.get(PARAM_VOLUME) { self.volume = v.clamp(0.0, 1.0); }
        if let Some(&v) = params.get(PARAM_GATE) { self.gate = v > 0.5; }
    }

    fn param_count(&self) -> usize { TOTAL_PARAMS }

    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.sample_rate = sample_rate as f64;
        self.compute_frequencies();
    }

    fn reset(&mut self) {
        self.phases = [0.0; NUM_BINS];
    }
}
