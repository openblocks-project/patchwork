//! Audio processor implementations.
//!
//! Each struct wraps existing DSP math from the AudioEffect/AudioSource enums.
//! The old enums continue to work — these processors exist alongside them
//! and will replace them when the CompiledDspChain is wired up (Phase 4).

pub mod analyzer;
pub mod clap;
pub mod effects;
pub mod input;
pub mod mixer;
pub mod neural_audio;
pub mod sampler;
pub mod speaker;
pub mod spectral_synth;
pub mod spectrum;
pub mod synth;
pub mod voice_effects;
