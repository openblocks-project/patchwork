//! MixerProcessor — pass-through for the VCV Rack engine.
//!
//! The engine pre-mixes all connected inputs with per-port gains
//! before calling process_block. The mixer just copies input → output.

use crate::audio::processor::{AudioProcessor, ProcessContext, ProcessorKind};

pub struct MixerProcessor;

impl MixerProcessor {
    pub fn new() -> Self {
        Self
    }
}

impl AudioProcessor for MixerProcessor {
    fn type_id(&self) -> &str {
        "mixer"
    }
    fn kind(&self) -> ProcessorKind {
        ProcessorKind::Mixer
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        output[..ctx.block_size].copy_from_slice(&input[..ctx.block_size]);
    }

    fn set_params(&mut self, _params: &[f32]) {}
    fn param_count(&self) -> usize {
        0
    }
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {}
}
