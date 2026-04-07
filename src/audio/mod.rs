// Audio Module — VCV Rack-style per-node audio engine.
//
// Each audio node owns a persistent processor. The engine iterates all
// processors calling process(). Connections are buffer references.
// No mutex on audio thread. 1-block latency between nodes (inaudible).

pub mod smoothed;
pub mod buffers;
pub mod waveform;
pub mod biquad;
pub mod analysis;
pub mod decode;
pub mod manager;
pub mod processor;
pub mod processors;
pub mod params;
pub mod engine;
pub mod clap_host;

// Re-export what external code uses
pub use waveform::Waveform;
pub use biquad::{curve_to_eq_bands, eq_curve_hash};
pub use decode::probe_file_duration;
pub use manager::AudioManager;
