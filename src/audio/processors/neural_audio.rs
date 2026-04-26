//! NeuralAudioProcessor — audio-thread half of the Neural Audio node.
//!
//! All ONNX inference happens on a background thread spawned by `NeuralAudioNode`
//! (see `src/nodes/neural_audio_node.rs`). The processor:
//! - drains decoded samples from the shared `output` ring into the engine's
//!   output buffer
//! - copies upstream `input` audio into the shared `input` ring so the
//!   inference thread's encoder can read it (Transfer mode)
//!
//! No allocation, no inference, no locks on the audio thread.

use std::sync::Arc;

use crate::audio::buffers::LiveInputBuffer;
use crate::audio::processor::{AudioProcessor, ProcessContext, ProcessorKind};

pub struct NeuralAudioProcessor {
    output_buffer: Arc<LiveInputBuffer>,
    input_buffer: Arc<LiveInputBuffer>,
}

impl NeuralAudioProcessor {
    pub fn new(output_buffer: Arc<LiveInputBuffer>, input_buffer: Arc<LiveInputBuffer>) -> Self {
        Self { output_buffer, input_buffer }
    }
}

impl AudioProcessor for NeuralAudioProcessor {
    fn type_id(&self) -> &str { "neural_audio" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Source }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        // Capture upstream audio for the encoder (silently no-op if empty).
        // The engine fills `input` with zeros when no upstream connection
        // exists; the inference thread's gate drops empty/silent blocks.
        if !input.is_empty() {
            self.input_buffer.write(&input[..ctx.block_size.min(input.len())]);
        }
        self.output_buffer.read_into(output, ctx.block_size);
    }

    fn set_params(&mut self, _params: &[f32]) {}
    fn param_count(&self) -> usize { 0 }
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {}
}
