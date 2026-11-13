//! SpeakerProcessor — marker for the speaker output node.
//!
//! The actual L/R mixing, volume, and pan are handled by the engine's
//! speaker mix step, which reads per-port inputs and atomic params directly.
//! This processor is a no-op placeholder that satisfies the AudioProcessor trait.

use crate::audio::processor::{AudioProcessor, ProcessContext, ProcessorKind};

pub struct SpeakerProcessor;

impl SpeakerProcessor {
    pub fn new(_volume: f32) -> Self {
        Self
    }
}

impl AudioProcessor for SpeakerProcessor {
    fn type_id(&self) -> &str {
        "speaker"
    }
    fn kind(&self) -> ProcessorKind {
        ProcessorKind::Output
    }

    fn process_block(&mut self, _input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        // No-op. Engine handles speaker mixing directly.
        output[..ctx.block_size].fill(0.0);
    }

    fn set_params(&mut self, _params: &[f32]) {}
    fn param_count(&self) -> usize {
        4
    } // volume, active, pan, channel_offset
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {}
}
