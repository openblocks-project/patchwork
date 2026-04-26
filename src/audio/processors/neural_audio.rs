//! NeuralAudioProcessor — audio-thread half of the Neural Audio node.
//!
//! All ONNX inference happens on a background thread spawned by `NeuralAudioNode`
//! (see `src/nodes/neural_audio_node.rs`). That thread pushes decoded f32 audio
//! samples into a shared `LiveInputBuffer` (lock-free SPSC ring). This
//! processor only drains the ring on each audio callback. No allocation,
//! no inference, no locks on the audio thread.
//!
//! On underrun the ring serves silence; users hear gaps when the host
//! can't keep up with the model's compute budget. That's intentional v1
//! feedback — better an obvious dropout than a hidden lock.

use std::sync::Arc;

use crate::audio::buffers::LiveInputBuffer;
use crate::audio::processor::{AudioProcessor, ProcessContext, ProcessorKind};

pub struct NeuralAudioProcessor {
    buffer: Arc<LiveInputBuffer>,
}

impl NeuralAudioProcessor {
    pub fn new(buffer: Arc<LiveInputBuffer>) -> Self {
        Self { buffer }
    }
}

impl AudioProcessor for NeuralAudioProcessor {
    fn type_id(&self) -> &str { "neural_audio" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Source }

    fn process_block(&mut self, _input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        self.buffer.read_into(output, ctx.block_size);
    }

    fn set_params(&mut self, _params: &[f32]) {}
    fn param_count(&self) -> usize { 0 }
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {}
}
