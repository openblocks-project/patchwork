//! AnalyzerProcessor — computes real-time audio analysis on its input.
//!
//! Acts as a pass-through effect: input goes to output unchanged,
//! but analysis (amplitude, peak, bass, mid, treble) is computed
//! and stored in a shared struct for the UI to read.

use crate::audio::analysis::AudioAnalysis;
use crate::audio::processor::{AudioProcessor, ProcessContext, ProcessorKind};
use std::sync::{Arc, Mutex};

pub struct AnalyzerProcessor {
    /// Shared analysis results — UI reads via try_lock.
    pub analysis: Arc<Mutex<AudioAnalysis>>,
}

impl AnalyzerProcessor {
    pub fn new() -> (Self, Arc<Mutex<AudioAnalysis>>) {
        let analysis = Arc::new(Mutex::new(AudioAnalysis::default()));
        let proc = Self {
            analysis: analysis.clone(),
        };
        (proc, analysis)
    }
}

impl AudioProcessor for AnalyzerProcessor {
    fn type_id(&self) -> &str {
        "analyzer"
    }
    fn kind(&self) -> ProcessorKind {
        ProcessorKind::Effect
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        // Pass audio through unchanged
        output[..ctx.block_size].copy_from_slice(&input[..ctx.block_size]);

        // Compute analysis on the input (mono, channels=1)
        if let Ok(mut a) = self.analysis.try_lock() {
            a.update(input, 1, ctx.sample_rate);
        }
    }

    fn set_params(&mut self, _params: &[f32]) {}
    fn param_count(&self) -> usize {
        0
    }
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {
        if let Ok(mut a) = self.analysis.try_lock() {
            *a = AudioAnalysis::default();
        }
    }
}
