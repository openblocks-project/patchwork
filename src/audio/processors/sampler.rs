#![allow(dead_code)]
//! SamplerProcessor — records audio from input, plays back from buffer.
//!
//! During recording: copies input samples into SamplerBuffer.
//! During playback: reads from SamplerBuffer into output.
//! Both modes are mutually exclusive (recording stops playback and vice versa).

use std::sync::Arc;
use crate::audio::processor::{AudioProcessor, ProcessorKind, ProcessContext};
use crate::audio::buffers::SamplerBuffer;

pub struct SamplerProcessor {
    pub buffer: Arc<SamplerBuffer>,
    pub volume: f32,
    /// Previous record trigger state (for edge detection).
    prev_record: bool,
    /// Previous play trigger state (for edge detection).
    prev_play: bool,
}

impl SamplerProcessor {
    pub fn new(buffer: Arc<SamplerBuffer>, volume: f32) -> Self {
        Self {
            buffer,
            volume,
            prev_record: false,
            prev_play: false,
        }
    }
}

impl AudioProcessor for SamplerProcessor {
    fn type_id(&self) -> &str { "sampler" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Source }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        let is_recording = self.buffer.recording.load(std::sync::atomic::Ordering::Relaxed);
        let is_playing = self.buffer.playing.load(std::sync::atomic::Ordering::Relaxed);

        if is_recording {
            // Record input samples into buffer
            self.buffer.record_from(&input[..ctx.block_size]);
            // Output silence during recording
            for s in output[..ctx.block_size].iter_mut() { *s = 0.0; }
        } else if is_playing {
            // Play back from buffer
            self.buffer.play_into(output, ctx.block_size);
            // Apply volume
            if (self.volume - 1.0).abs() > 0.001 {
                for s in output[..ctx.block_size].iter_mut() { *s *= self.volume; }
            }
        } else {
            // Idle — output silence
            for s in output[..ctx.block_size].iter_mut() { *s = 0.0; }
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        // params[0] = volume
        // params[1] = record trigger (>0.5 = record)
        // params[2] = play trigger (>0.5 = play)
        // params[3] = speed multiplier (1.0 = normal, 0.5 = half, 2.0 = double)
        // params[4] = seek normalized (0.0–1.0); negative = no seek this frame
        if let Some(&v) = params.first() { self.volume = v.max(0.0); }

        if params.len() >= 2 {
            let record_on = params[1] > 0.5;
            // Edge detection: only trigger on rising edge
            if record_on && !self.prev_record {
                self.buffer.start_recording();
            } else if !record_on && self.prev_record {
                self.buffer.stop_recording();
            }
            self.prev_record = record_on;
        }

        if params.len() >= 3 {
            let play_on = params[2] > 0.5;
            if play_on && !self.prev_play {
                self.buffer.start_playback();
            } else if !play_on && self.prev_play {
                self.buffer.stop_playback();
            }
            self.prev_play = play_on;
        }

        if params.len() >= 4 {
            let speed = params[3].clamp(0.1, 8.0);
            self.buffer.playback_rate_x1000.store(
                (speed * 1000.0) as u32,
                std::sync::atomic::Ordering::Relaxed,
            );
        }

        if params.len() >= 5 && params[4] >= 0.0 {
            self.buffer.seek_to_normalized(params[4]);
        }
    }

    fn param_count(&self) -> usize { 5 }
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {
        self.prev_record = false;
        self.prev_play = false;
    }
}
