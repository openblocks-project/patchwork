//! SynthProcessor — oscillator with automatic FM modulation.
//!
//! If audio is connected to the input, it's used as FM modulation.
//! The fm_depth parameter controls modulation amount in Hz.
//! If no audio input, the synth generates a clean tone at params[0] Hz.

use crate::audio::processor::{AudioProcessor, ProcessContext, ProcessorKind};
use crate::audio::smoothed::SmoothedParam;
use crate::audio::waveform::Waveform;

pub struct SynthProcessor {
    pub waveform: Waveform,
    pub frequency: f32,
    pub amplitude: SmoothedParam,
    pub phase: f32,
    pub active: bool,
    pub fm_depth: f32,
}

impl SynthProcessor {
    pub fn new(waveform: Waveform, frequency: f32, amplitude: f32) -> Self {
        Self {
            waveform,
            frequency,
            amplitude: SmoothedParam::new(amplitude, 5.0),
            phase: 0.0,
            active: true,
            fm_depth: 0.0,
        }
    }
}

impl AudioProcessor for SynthProcessor {
    fn type_id(&self) -> &str {
        "synth"
    }
    fn kind(&self) -> ProcessorKind {
        ProcessorKind::Source
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        let target_amp = if self.active {
            self.amplitude.target
        } else {
            0.0
        };
        self.amplitude.set(target_amp);

        if !self.active && self.amplitude.current < 0.0001 {
            output[..ctx.block_size].fill(0.0);
            return;
        }

        // Detect if input has audio signal (for FM modulation)
        let has_input = input.iter().take(ctx.block_size).any(|&s| s.abs() > 0.0001);

        for i in 0..ctx.block_size {
            // If audio is connected, use it as FM modulation (input * depth)
            let freq = if has_input && self.fm_depth > 0.0 {
                self.frequency + input[i] * self.fm_depth
            } else {
                self.frequency
            };

            let amp = self.amplitude.tick();
            output[i] = self.waveform.sample(self.phase) * amp;
            self.phase += freq / ctx.sample_rate;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            if self.phase < 0.0 {
                self.phase += 1.0;
            }
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        // params[0] = frequency (Hz)
        // params[1] = amplitude (0-1)
        // params[2] = active/gate (0 or 1)
        // params[3] = fm_depth (Hz — how much input modulates frequency)
        // params[4] = waveform (0=Sine, 1=Square, 2=Saw, 3=Triangle, 4=Noise)
        if let Some(&v) = params.first() {
            self.frequency = v;
        }
        if let Some(&v) = params.get(1) {
            self.amplitude.set(v);
        }
        if let Some(&v) = params.get(2) {
            self.active = v > 0.5;
        }
        if let Some(&v) = params.get(3) {
            self.fm_depth = v;
        }
        if let Some(&v) = params.get(4) {
            self.waveform = match v as u32 {
                0 => Waveform::Sine,
                1 => Waveform::Square,
                2 => Waveform::Saw,
                3 => Waveform::Triangle,
                _ => Waveform::Noise,
            };
        }
    }

    fn param_count(&self) -> usize {
        5
    }

    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {
        self.amplitude = SmoothedParam::new(self.amplitude.target, 5.0);
    }

    fn reset(&mut self) {
        self.phase = 0.0;
    }
}
