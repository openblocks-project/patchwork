// Audio Module — VCV Rack-style per-node audio engine.
//
// Each audio node owns a persistent processor. The engine iterates all
// processors calling process(). Connections are buffer references.
// No mutex on audio thread. 1-block latency between nodes (inaudible).

pub mod analysis;
pub mod biquad;
pub mod buffers;
pub mod clap_host;
pub mod decode;
pub mod engine;
pub mod manager;
pub mod params;
pub mod processor;
pub mod processors;
pub mod smoothed;
pub mod waveform;

// Re-export what external code uses
pub use biquad::{curve_to_eq_bands, eq_curve_hash};
pub use decode::probe_file_duration;
pub use manager::AudioManager;
pub use waveform::Waveform;
