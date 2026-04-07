//! SpectrumProcessor — runs FFT-based spectrum analysis on its input.
//!
//! Pass-through effect: input is forwarded to output unchanged, but
//! a windowed FFT is run on the incoming samples and the resulting
//! log-spaced bins are stored in a shared `Spectrum` struct that the
//! UI thread reads via `try_lock`.

use std::sync::{Arc, Mutex};
use crate::audio::processor::{AudioProcessor, ProcessorKind, ProcessContext};
use crate::audio::analysis::Spectrum;

pub struct SpectrumProcessor {
    /// Shared spectrum bins — UI reads via try_lock.
    pub spectrum: Arc<Mutex<Spectrum>>,
}

impl SpectrumProcessor {
    pub fn new() -> (Self, Arc<Mutex<Spectrum>>) {
        let spectrum = Arc::new(Mutex::new(Spectrum::default()));
        let proc = Self { spectrum: spectrum.clone() };
        (proc, spectrum)
    }
}

impl AudioProcessor for SpectrumProcessor {
    fn type_id(&self) -> &str { "spectrum" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        // Pass audio through unchanged
        output[..ctx.block_size].copy_from_slice(&input[..ctx.block_size]);

        // Update spectrum (mono, channels=1)
        if let Ok(mut s) = self.spectrum.try_lock() {
            s.update(input, 1, ctx.sample_rate);
        }
    }

    fn set_params(&mut self, _params: &[f32]) {}
    fn param_count(&self) -> usize { 0 }
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {
        if let Ok(mut s) = self.spectrum.try_lock() {
            *s = Spectrum::default();
        }
    }
}
