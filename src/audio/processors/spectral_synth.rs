//! SpectralSynthProcessor — phase-coherent additive synthesis from 256 bins.
//!
//! Reads 256 magnitudes (params 0..256) + 256 phases (params 256..512) at
//! linear-spaced frequencies (0 → SPECTRUM_FREQ_MAX). On each new image
//! frame — detected via the monotonic `frame_seq` param — each oscillator's
//! phase is reset to the image's stored phase. This preserves the phase
//! coherence that makes voice sound vocal rather than saw-like.

use crate::audio::processor::{AudioProcessor, ProcessorKind, ProcessContext};

const NUM_BINS: usize = 256;
const PARAM_PHASE_OFFSET: usize = 256;
const PARAM_VOLUME: usize = 512;
const PARAM_GATE: usize = 513;
const PARAM_FRAME_SEQ: usize = 514;
const TOTAL_PARAMS: usize = 515;

pub struct SpectralSynthProcessor {
    /// Active oscillator bank phases, stored as cycle fraction (0..1).
    phases: [f64; NUM_BINS],
    /// Outgoing oscillator bank phases (previous frame's bank, fading out).
    phases_prev: [f64; NUM_BINS],
    /// Per-bin magnitudes, 0..1.
    magnitudes: [f32; NUM_BINS],
    /// Target phases from the image, normalized 0..1 (0.5 = 0 radians).
    target_phases: [f32; NUM_BINS],
    /// Image phase captured at the **previous** frame reset, stored as cycle
    /// fraction (matches `phases` convention). Used for phase-vocoder
    /// frequency estimation — we diff this against the new target to learn
    /// the true frequency of each bin's dominant tone, instead of always
    /// playing bin-center.
    prev_image_phases: [f64; NUM_BINS],
    /// Center freq per bin (Hz). Used as the anchor for phase-vocoder
    /// frequency refinement.
    freqs: [f64; NUM_BINS],
    /// Per-bin refined frequency (Hz) — bin center plus the phase-derived
    /// deviation. Recomputed on every frame reset. Oscillators advance at
    /// this rate so a 440 Hz tone in the "468 Hz" bin actually plays 440 Hz.
    refined_freqs: [f64; NUM_BINS],
    /// Samples between frame resets (= samples_per_hop). Learned from the
    /// time between set_params calls bumping frame_seq. Used for phase-to-
    /// frequency conversion. Initialized to an expected value in prepare().
    hop_samples: f64,
    /// Counts samples since the last frame reset; informs hop_samples.
    samples_since_frame: u32,
    volume: f32,
    gate: bool,
    sample_rate: f64,
    /// Last observed frame sequence counter. On change, crossfade begins.
    last_frame_seq: f32,
    /// Counts down while we linearly crossfade from `phases_prev` to
    /// `phases`. Zero ⇒ no crossfade in progress, output uses `phases` only.
    crossfade_samples: u32,
    /// Total crossfade length in samples. ~4 ms at 48 kHz softens the
    /// phase-reset click without obscuring fast consonants.
    crossfade_total: u32,
}

impl SpectralSynthProcessor {
    pub fn new() -> Self {
        Self {
            phases: [0.0; NUM_BINS],
            phases_prev: [0.0; NUM_BINS],
            magnitudes: [0.0; NUM_BINS],
            target_phases: [0.5; NUM_BINS],
            prev_image_phases: [0.0; NUM_BINS],
            freqs: [0.0; NUM_BINS],
            refined_freqs: [0.0; NUM_BINS],
            hop_samples: 1024.0,
            samples_since_frame: 0,
            volume: 0.8,
            gate: true,
            sample_rate: 44100.0,
            last_frame_seq: f32::NAN,
            crossfade_samples: 0,
            crossfade_total: 192, // will be re-computed in prepare()
        }
    }

    fn compute_frequencies(&mut self) {
        // Linear-spaced 0 → SPECTRUM_FREQ_MAX, matching analysis.rs bin layout.
        // Bin-center freq = (b + 0.5) * bin_width so oscillators sit at the
        // midpoint of each bin's frequency range. `refined_freqs` starts at
        // the center and gets pulled toward the true tone frequency at each
        // frame reset via phase-vocoder estimation.
        let bin_width = crate::audio::analysis::SPECTRUM_FREQ_MAX as f64
                      / NUM_BINS as f64;
        for b in 0..NUM_BINS {
            self.freqs[b] = (b as f64 + 0.5) * bin_width;
            self.refined_freqs[b] = self.freqs[b];
        }
    }
}

impl AudioProcessor for SpectralSynthProcessor {
    fn type_id(&self) -> &str { "spectral_synth" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Source }

    fn process_block(&mut self, _input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        if !self.gate || self.volume < 0.0001 {
            output[..ctx.block_size].fill(0.0);
            return;
        }

        // Normalization: /8 = /sqrt(64); conservative for sparse spectra,
        // allows some soft clipping when the whole spectrum lights up.
        let norm = self.volume / 8.0;
        let sr = self.sample_rate;
        let tau = std::f64::consts::TAU;
        let total = self.crossfade_total.max(1) as f64;

        for i in 0..ctx.block_size {
            // Crossfade weight: 0.0 at the start of a reset, 1.0 once we're
            // fully on the new bank. Smooths the phase-reset click into a
            // ~4 ms fade so the reset frequency (~46 Hz) isn't audible.
            let t = if self.crossfade_samples > 0 {
                self.crossfade_samples -= 1;
                1.0 - (self.crossfade_samples as f64 / total)
            } else {
                1.0
            };

            let mut sample = 0.0f64;
            for bin in 0..NUM_BINS {
                let mag = self.magnitudes[bin] as f64;
                if mag > 0.001 {
                    // cos (not sin) matches FFT's mag·e^(iφ) → mag·cos(…+φ).
                    let v_curr = (self.phases[bin] * tau).cos();
                    if t >= 1.0 {
                        sample += v_curr * mag;
                    } else {
                        let v_prev = (self.phases_prev[bin] * tau).cos();
                        sample += (v_prev * (1.0 - t) + v_curr * t) * mag;
                    }
                }
                // Advance both banks at the refined freq — this is what the
                // phase-vocoder estimation gives us: the actual dominant
                // tone within the bin, not the bin center. Holds pitch
                // steady across the 21 ms between frame resets instead of
                // letting the oscillator drift by up to ±30 Hz.
                self.phases[bin] += self.refined_freqs[bin] / sr;
                if self.phases[bin] >= 1.0 { self.phases[bin] -= 1.0; }
                if t < 1.0 {
                    self.phases_prev[bin] += self.refined_freqs[bin] / sr;
                    if self.phases_prev[bin] >= 1.0 { self.phases_prev[bin] -= 1.0; }
                }
            }
            output[i] = (sample * norm as f64) as f32;
            self.samples_since_frame = self.samples_since_frame.saturating_add(1);
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        for i in 0..NUM_BINS {
            if let Some(&v) = params.get(i) {
                self.magnitudes[i] = v.clamp(0.0, 1.0);
            }
            if let Some(&p) = params.get(PARAM_PHASE_OFFSET + i) {
                self.target_phases[i] = p.clamp(0.0, 1.0);
            }
        }
        if let Some(&v) = params.get(PARAM_VOLUME) { self.volume = v.clamp(0.0, 1.0); }
        if let Some(&v) = params.get(PARAM_GATE) { self.gate = v > 0.5; }
        if let Some(&seq) = params.get(PARAM_FRAME_SEQ) {
            // New frame → rebase each oscillator's phase so reconstructed
            // harmonics line up with the image's recorded phase. To avoid
            // a click at the discontinuity we retain the previous bank's
            // phases and crossfade between them for a few ms.
            if seq != self.last_frame_seq {
                self.phases_prev.copy_from_slice(&self.phases);

                // Measure hop: how many samples elapsed since the last frame
                // reset? Use this for the phase-vocoder math below. First
                // frame after startup has no meaningful prior, so skip.
                let observed_hop = self.samples_since_frame as f64;
                let have_prior = !self.last_frame_seq.is_nan() && observed_hop > 1.0;
                if have_prior {
                    // Exponentially track the hop between frames so short-
                    // term UI-rate variance doesn't send refined_freqs on a
                    // wild ride. The first valid observation seeds it.
                    self.hop_samples = 0.7 * self.hop_samples + 0.3 * observed_hop;
                }

                for i in 0..NUM_BINS {
                    // target_phases: encoded 0..1 where 0.5 = 0 rad.
                    // Offset so 0.5 → phase fraction 0 (cos peak at 1.0).
                    let frac = self.target_phases[i] as f64 - 0.5;
                    let new_phase = if frac < 0.0 { frac + 1.0 } else { frac };

                    if have_prior && self.magnitudes[i] > 0.01 {
                        // Phase-vocoder frequency estimation. Expected phase
                        // advance between frames at bin-center freq:
                        //   expected_cycles = freq_center * hop / sr
                        // Actual advance = new_phase − prev_image_phase
                        // (wrapped to nearest cycle). Deviation in cycles
                        // tells us the true tone's offset from bin center.
                        let hop_seconds = self.hop_samples / self.sample_rate;
                        let expected_advance = self.freqs[i] * hop_seconds;
                        let raw_advance = new_phase - self.prev_image_phases[i];
                        // Wrap the DEVIATION (not the advance) to nearest
                        // whole cycle so we pick the interpretation closest
                        // to bin-center freq.
                        let deviation = raw_advance - expected_advance;
                        let deviation = deviation - deviation.round();
                        let freq_delta_hz = deviation / hop_seconds.max(1e-9);
                        // Clamp to ±half-bin so one bin can't steal another
                        // bin's tone — a hard pitch lock would sound worse
                        // than the drift it's trying to fix.
                        let bin_width = crate::audio::analysis::SPECTRUM_FREQ_MAX as f64
                                      / NUM_BINS as f64;
                        let max_delta = bin_width * 0.5;
                        let clamped = freq_delta_hz.clamp(-max_delta, max_delta);
                        self.refined_freqs[i] = self.freqs[i] + clamped;
                    } else {
                        // No prior frame or silent bin — reset to center.
                        self.refined_freqs[i] = self.freqs[i];
                    }

                    self.prev_image_phases[i] = new_phase;
                    self.phases[i] = new_phase;
                }
                self.crossfade_samples = self.crossfade_total;
                self.last_frame_seq = seq;
                self.samples_since_frame = 0;
            }
        }
    }

    fn param_count(&self) -> usize { TOTAL_PARAMS }

    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.sample_rate = sample_rate as f64;
        self.compute_frequencies();
        // ~4 ms crossfade — shorter than 10 ms so fast consonants aren't
        // blurred. The audible buzz from phase resets at ~94 Hz now sits
        // squarely in the voice band, but a 4 ms fade still masks it
        // well and frames arrive 2× more often so each crossfade has
        // less to cover.
        self.crossfade_total = (sample_rate * 0.004) as u32;
        // Seed hop_samples with the expected value (analysis FFT rate of
        // ~93.75 Hz at 48 kHz = 512 samples/hop). The first real
        // observation overwrites this via exponential tracking.
        self.hop_samples = (sample_rate as f64 / 93.75).max(1.0);
    }

    fn reset(&mut self) {
        self.phases = [0.0; NUM_BINS];
        self.phases_prev = [0.0; NUM_BINS];
        self.prev_image_phases = [0.0; NUM_BINS];
        self.refined_freqs.copy_from_slice(&self.freqs);
        self.last_frame_seq = f32::NAN;
        self.crossfade_samples = 0;
        self.samples_since_frame = 0;
    }
}
