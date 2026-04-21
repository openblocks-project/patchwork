/// FFT size for the spectrum analyzer. Power of two; ~43 ms at 48 kHz.
/// Bumped from 1024 → 2048 to double raw FFT frequency resolution, which
/// matches the doubled display bin count below. Larger window is the
/// "better freq / worse time" half of the trade-off — paired with a
/// hop that runs 4× per window (see `SPECTRUM_HOP_SIZE`) to recover time
/// resolution via overlap.
pub const SPECTRUM_FFT_SIZE: usize = 2048;
/// Number of samples between FFT frames. With FFT size 2048 and hop 512,
/// consecutive windows overlap 75% and update rate = `sample_rate / hop`
/// ≈ 93.75 Hz at 48 kHz — roughly twice the previous rate. Frame Recorder
/// `base_fps` and Spectral Synth `playback_rate` defaults match this.
pub const SPECTRUM_HOP_SIZE: usize = 512;
/// Number of linear-spaced display bins shown by the spectrum analyzer.
/// Linear spacing preserves voice-harmonic integer ratios on resynthesis
/// through the Music Visualizer → Frame Recorder → Spectral Synth chain.
/// 256 bins × 31.25 Hz/bin across 0–8 kHz; matched against the 2048-point
/// FFT's ~23 Hz native bin width so display bins aggregate ~1.3 FFT bins
/// each — enough averaging for stability, not so much that harmonics
/// blend.
pub const SPECTRUM_BINS: usize = 256;
/// Upper frequency bound for the binned spectrum. Independent of sample rate
/// — keeps bin centers deterministic so the Spectral Synth processor can use
/// the same mapping without knowing the audio sample rate ahead of time.
/// 8 kHz covers the voice band (fundamentals, formants, sibilance) while
/// sacrificing >8 kHz music content (cymbal air, high harmonics).
pub const SPECTRUM_FREQ_MAX: f32 = 8000.0;

/// Real-time spectrum analysis — linear-spaced FFT magnitude bins (0..1).
///
/// Sits next to `AudioAnalysis` (which is 3-band scalar reactivity)
/// and is consumed by the Spectrum Analyzer node. Audio threads call
/// [`Spectrum::update`] every block; the UI thread reads `bins` via
/// `try_lock`.
#[derive(Clone, Debug)]
pub struct Spectrum {
    /// Linear-spaced, sqrt-encoded magnitudes (length = `SPECTRUM_BINS`).
    /// Indexed low → high frequency. Smoothed with attack/decay.
    pub bins: Vec<f32>,
    /// Per-bin phase, normalized 0..1 where 0.5 = 0 radians. Decoded as
    /// `phase_rad = (phases[b] - 0.5) * 2π`. Never smoothed — averaging
    /// angles across wraparound produces nonsense.
    pub phases: Vec<f32>,
    /// Sliding sample buffer accumulated from the audio thread until
    /// `SPECTRUM_FFT_SIZE` samples are collected, then drained.
    buf: Vec<f32>,
    /// Precomputed Hann window.
    window: Vec<f32>,
}

impl Default for Spectrum {
    fn default() -> Self {
        let window: Vec<f32> = (0..SPECTRUM_FFT_SIZE)
            .map(|i| {
                let x = (i as f32) / ((SPECTRUM_FFT_SIZE - 1) as f32);
                0.5 - 0.5 * (std::f32::consts::TAU * x).cos()
            })
            .collect();
        Self {
            bins: vec![0.0; SPECTRUM_BINS],
            phases: vec![0.5; SPECTRUM_BINS],
            buf: Vec::with_capacity(SPECTRUM_FFT_SIZE),
            window,
        }
    }
}

impl Spectrum {
    /// Push new audio samples; runs an FFT every `SPECTRUM_HOP_SIZE`
    /// samples using the last `SPECTRUM_FFT_SIZE` samples as the window,
    /// then drops the oldest hop-worth of samples so the next FFT overlaps
    /// with the previous one. Overlap percentage =
    /// `(FFT_SIZE - HOP_SIZE) / FFT_SIZE` — currently 75%.
    pub fn update(&mut self, data: &[f32], channels: usize, sample_rate: f32) {
        let ch = channels.max(1);
        let num_frames = data.len() / ch;
        if num_frames == 0 || sample_rate <= 0.0 {
            return;
        }
        for frame in 0..num_frames {
            let mut s = 0.0f32;
            for c in 0..ch {
                s += data[frame * ch + c];
            }
            s /= ch as f32;
            self.buf.push(s);
            if self.buf.len() >= SPECTRUM_FFT_SIZE {
                self.compute(sample_rate);
                // Slide the window: drop HOP_SIZE oldest samples, keep the
                // rest so the next FFT happens as soon as HOP_SIZE new
                // samples arrive. This is the overlap that doubles time
                // resolution without shrinking the FFT window.
                self.buf.drain(0..SPECTRUM_HOP_SIZE);
            }
        }
    }

    fn compute(&mut self, sample_rate: f32) {
        let n = SPECTRUM_FFT_SIZE;
        let mut re = vec![0.0f32; n];
        let mut im = vec![0.0f32; n];
        for i in 0..n {
            re[i] = self.buf[i] * self.window[i];
        }
        fft_radix2(&mut re, &mut im);

        // Half-spectrum, normalized by N/2. Keep re/im separate so we can
        // recover phase per display bin (atan2 of summed re/im) — Spectral
        // Synth needs phase to reconstruct voice rather than a saw-like
        // magnitude envelope.
        let half = n / 2;
        let norm = 1.0 / (n as f32 * 0.5);

        // Linear-spaced grouping into display bins, 0 → SPECTRUM_FREQ_MAX.
        // Bin centers sit at arithmetic multiples of bin_width, so voice
        // harmonics (integer multiples of a fundamental) map onto integer
        // multiples of a bin index — their ratios are preserved when
        // the Spectral Synth resynthesizes them as additive sines.
        let bin_width = SPECTRUM_FREQ_MAX / SPECTRUM_BINS as f32;
        let nyquist = (sample_rate * 0.5).max(1.0);

        // Faster attack/decay than before — ~50 ms decay half-life at the
        // ~47 Hz FFT rate. Preserves phoneme transients; previous decay=0.15
        // had a 130 ms half-life that smeared consonants into vowels.
        let attack = 0.9f32;
        let decay = 0.5f32;
        for b in 0..SPECTRUM_BINS {
            let f0 = (b as f32) * bin_width;
            let f1 = ((b + 1) as f32) * bin_width;
            let i0 = ((f0 / nyquist) * (half as f32)) as usize;
            let i1 = (((f1 / nyquist) * (half as f32)) as usize).max(i0 + 1).min(half);
            let mut re_sum = 0.0f32;
            let mut im_sum = 0.0f32;
            let mut count = 0usize;
            for i in i0..i1 {
                re_sum += re[i];
                im_sum += im[i];
                count += 1;
            }
            let (raw, phase_rad) = if count == 0 {
                (0.0, 0.0)
            } else {
                let re_avg = re_sum / count as f32;
                let im_avg = im_sum / count as f32;
                let mag = (re_avg * re_avg + im_avg * im_avg).sqrt() * norm;
                (mag, im_avg.atan2(re_avg))
            };
            // Bin 0 (0–62 Hz) is a trap: it collects DC offset, AC mains
            // hum, mic-handling thumps, and generally rumble that's not
            // intentional signal. With the 2× pregain below it clamps to
            // v=1 and the Spectral Synth plays a subsonic 31 Hz sine at
            // full amplitude — masking any real high-pitch content the
            // user sang. Just zero it.
            let raw = if b == 0 { 0.0 } else { raw };
            // sqrt encoding with 2× pregain. Mic/line FFT magnitudes
            // typically land at 0.01–0.1; a plain sqrt maps those to
            // dim-looking gray and quiet resynth output. 2× lifts normal
            // speech into visible and audible ranges without over-
            // amplifying ambient bass. Loud signals saturate cleanly at
            // v=1.0. Spectral Synth's v² decode inverts sqrt; the 2×
            // carries through as an effective round-trip gain factor.
            let v = (raw * 2.0).sqrt().clamp(0.0, 1.0);
            let old = self.bins[b];
            self.bins[b] = if v > old {
                old + attack * (v - old)
            } else {
                old + decay * (v - old)
            };
            // Store phase normalized to 0..1 (0.5 = 0 radians). Unsmoothed:
            // averaging angles across the ±π wrap produces nonsense, and the
            // Spectral Synth needs the freshest phase to lock each bin's
            // oscillator on every new frame. For zeroed bin 0, phase is
            // irrelevant (magnitude is zero).
            self.phases[b] = (phase_rad / std::f32::consts::TAU + 0.5)
                .clamp(0.0, 1.0);
        }
    }
}

/// In-place radix-2 Cooley-Tukey FFT. `re.len()` must equal `im.len()`
/// and be a power of two. Sufficient for the small (1024-point)
/// per-block FFT the Spectrum node needs — no external FFT crate.
fn fft_radix2(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert_eq!(n, im.len());
    debug_assert!(n.is_power_of_two());
    let bits = n.trailing_zeros();

    // Bit-reversal permutation.
    for i in 0..n {
        let mut x = i;
        let mut j = 0usize;
        for _ in 0..bits {
            j = (j << 1) | (x & 1);
            x >>= 1;
        }
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    // Butterflies.
    let mut size = 2usize;
    while size <= n {
        let half = size / 2;
        let theta = -std::f32::consts::TAU / size as f32;
        let wr_step = theta.cos();
        let wi_step = theta.sin();
        let mut k = 0;
        while k < n {
            let mut wr = 1.0f32;
            let mut wi = 0.0f32;
            for j in 0..half {
                let tr = wr * re[k + j + half] - wi * im[k + j + half];
                let ti = wr * im[k + j + half] + wi * re[k + j + half];
                re[k + j + half] = re[k + j] - tr;
                im[k + j + half] = im[k + j] - ti;
                re[k + j] += tr;
                im[k + j] += ti;
                let nwr = wr * wr_step - wi * wi_step;
                wi = wr * wi_step + wi * wr_step;
                wr = nwr;
            }
            k += size;
        }
        size *= 2;
    }
}

/// Real-time audio analysis — computed per audio block.
/// All values are 0.0–1.0 (smoothed).
#[derive(Clone, Debug)]
pub struct AudioAnalysis {
    /// RMS amplitude (overall volume level)
    pub amplitude: f32,
    /// Peak sample value
    pub peak: f32,
    /// Low frequency energy (bass, ~0–300 Hz)
    pub bass: f32,
    /// Mid frequency energy (~300–2000 Hz)
    pub mid: f32,
    /// High frequency energy (treble, ~2000 Hz+)
    pub treble: f32,
    // Internal filter states for band splitting (not exposed)
    bass_state: f32,
    #[allow(dead_code)]
    mid_state: f32,
    treble_state: f32,
}

impl Default for AudioAnalysis {
    fn default() -> Self {
        Self {
            amplitude: 0.0, peak: 0.0,
            bass: 0.0, mid: 0.0, treble: 0.0,
            bass_state: 0.0, mid_state: 0.0, treble_state: 0.0,
        }
    }
}

impl AudioAnalysis {
    /// Update analysis from an audio buffer. Uses one-pole band-split
    /// filters and exponential smoothing for stable, reactive values.
    pub fn update(&mut self, data: &[f32], channels: usize, sample_rate: f32) {
        let num_frames = data.len() / channels;
        if num_frames == 0 { return; }

        let mut sum_sq = 0.0f32;
        let mut peak = 0.0f32;
        let mut bass_energy = 0.0f32;
        let mut mid_energy = 0.0f32;
        let mut treble_energy = 0.0f32;

        let bass_coeff = (std::f32::consts::TAU * 300.0 / sample_rate).min(1.0);
        let treble_coeff = (std::f32::consts::TAU * 2000.0 / sample_rate).min(1.0);

        for frame in 0..num_frames {
            let mut sample = 0.0f32;
            for ch in 0..channels {
                sample += data[frame * channels + ch];
            }
            sample /= channels as f32;

            sum_sq += sample * sample;
            peak = peak.max(sample.abs());

            self.bass_state += bass_coeff * (sample - self.bass_state);
            let bass_sample = self.bass_state;

            self.treble_state += treble_coeff * (sample - self.treble_state);
            let treble_sample = sample - self.treble_state;
            let mid_sample = self.treble_state - self.bass_state;

            bass_energy += bass_sample * bass_sample;
            mid_energy += mid_sample * mid_sample;
            treble_energy += treble_sample * treble_sample;
        }

        let rms = (sum_sq / num_frames as f32).sqrt();
        let bass_rms = (bass_energy / num_frames as f32).sqrt();
        let mid_rms = (mid_energy / num_frames as f32).sqrt();
        let treble_rms = (treble_energy / num_frames as f32).sqrt();

        let attack = 0.6;
        let decay = 0.05;
        let smooth = |old: f32, new: f32| {
            if new > old { old + attack * (new - old) }
            else { old + decay * (new - old) }
        };

        self.amplitude = smooth(self.amplitude, rms.min(1.0));
        self.peak = smooth(self.peak, peak.min(1.0));
        self.bass = smooth(self.bass, (bass_rms * 3.0).min(1.0));
        self.mid = smooth(self.mid, (mid_rms * 4.0).min(1.0));
        self.treble = smooth(self.treble, (treble_rms * 5.0).min(1.0));
    }
}
