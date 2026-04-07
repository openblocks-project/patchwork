#![allow(dead_code)]
//! AudioProcessor trait — the DSP interface for all audio nodes.
//!
//! This trait runs ONLY on the audio thread. It must:
//! - Never allocate (no Vec::new, no Box::new, no String)
//! - Never lock (no Mutex, no RwLock)
//! - Never block (no I/O, no sleep, no channel recv)
//! - Be Send (moved to audio thread at chain swap)
//!
//! Separate from NodeBehavior (UI trait). Connected via ParamStore (AtomicF32).

/// Whether a processor generates audio (Source) or transforms it (Effect).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProcessorKind {
    /// Generates audio from scratch (Synth, FilePlayer, LiveInput)
    Source,
    /// Transforms audio from upstream (LowPass, Delay, Reverb, etc.)
    Effect,
    /// Mixes multiple upstream sources (Mixer)
    Mixer,
    /// Outputs audio to master (Speaker)
    Output,
}

/// Context passed to process_block — sample rate and block size.
pub struct ProcessContext {
    pub sample_rate: f32,
    pub block_size: usize,
}

/// The core audio processing trait.
///
/// Implemented by all audio processors (sources, effects, mixers).
/// Each processor is owned by a CompiledDspChain and lives on the audio thread.
pub trait AudioProcessor: Send {
    /// Stable type identifier (e.g. "synth", "lowpass", "delay").
    fn type_id(&self) -> &str;

    /// Source, Effect, or Mixer.
    fn kind(&self) -> ProcessorKind;

    /// Process a block of audio samples.
    ///
    /// For Effects: read from `input`, write processed audio to `output`.
    /// For Sources: ignore `input`, write generated audio to `output`.
    /// For Mixers: `input` contains the mixed result from upstream sources.
    ///
    /// Both buffers are pre-allocated and have exactly `context.block_size` elements.
    /// Never allocate inside this method.
    fn process_block(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        context: &ProcessContext,
    );

    /// Update parameters from atomic snapshot (control rate: once per block).
    ///
    /// Called before process_block each callback. The slice contains one f32
    /// per parameter in the order defined by param_count().
    /// Implementations should set SmoothedParam targets here.
    fn set_params(&mut self, params: &[f32]);

    /// Number of parameter slots this processor uses.
    fn param_count(&self) -> usize;

    /// Pre-allocate internal buffers for the given sample rate and max block size.
    /// Called once when the chain is compiled (on the UI thread, not audio thread).
    fn prepare(&mut self, sample_rate: f32, max_block_size: usize);

    /// Reset internal state (filter memory, delay buffers) without deallocating.
    fn reset(&mut self);

    /// Optionally receive the shared atomic params for bidirectional sync.
    /// Called by the engine after the processor is added. Default: no-op.
    /// Used by ClapProcessor to write back GUI param changes.
    fn set_shared_params(&mut self, _params: std::sync::Arc<Vec<super::params::AtomicF32>>) {}

    /// Number of output channels this processor produces.
    /// Most processors produce 1 (mono). Speaker produces 2 (interleaved stereo).
    /// The engine allocates `output_channels() * max_block_size` for the output buffer.
    fn output_channels(&self) -> usize { 1 }

    /// Replace the EQ band coefficients in-place.
    /// Default no-op; only `EqProcessor` overrides this.
    /// Called from the audio thread in response to `AudioCommand::UpdateEqBands`.
    /// The old `Vec` is dropped on the audio thread, but this only happens when
    /// the user moves an EQ point — i.e. interactively, not per-frame — so the
    /// occasional deallocation is acceptable (same pattern as add/remove
    /// processor commands).
    fn set_eq_bands(&mut self, _bands: Vec<super::biquad::BiquadFilter>, _hash: u64) {}

    /// Current EQ curve hash (only meaningful for `EqProcessor`). Used to skip
    /// redundant `UpdateEqBands` commands when the user hasn't actually moved
    /// a point. Default 0 means "always update".
    fn eq_curve_hash(&self) -> u64 { 0 }
}
