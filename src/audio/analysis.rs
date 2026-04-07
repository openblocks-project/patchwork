/// FFT size for the spectrum analyzer. Power of two; ~21 ms at 48 kHz.
pub const SPECTRUM_FFT_SIZE: usize = 1024;
/// Number of log-spaced display bins shown by the spectrum analyzer.
pub const SPECTRUM_BINS: usize = 64;

/// Real-time spectrum analysis — log-spaced FFT magnitude bins (0..1).
///
/// Sits next to `AudioAnalysis` (which is 3-band scalar reactivity)
/// and is consumed by the Spectrum Analyzer node. Audio threads call
/// [`Spectrum::update`] every block; the UI thread reads `bins` via
/// `try_lock`.
#[derive(Clone, Debug)]
pub struct Spectrum {
    /// Log-spaced, normalized magnitudes (length = `SPECTRUM_BINS`).
    /// Indexed low → high frequency. Values are dB-scaled into 0..1.
    pub bins: Vec<f32>,
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
            buf: Vec::with_capacity(SPECTRUM_FFT_SIZE),
            window,
        }
    }
}

impl Spectrum {
    /// Push new audio samples; runs an FFT every `SPECTRUM_FFT_SIZE`
    /// samples and updates `bins`.
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
                self.buf.clear();
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

        // Half-spectrum magnitudes, normalized by N/2.
        let half = n / 2;
        let mut mags = vec![0.0f32; half];
        let norm = 1.0 / (n as f32 * 0.5);
        for i in 0..half {
            mags[i] = (re[i] * re[i] + im[i] * im[i]).sqrt() * norm;
        }

        // Log-spaced grouping into display bins, 30 Hz → Nyquist.
        let min_freq = 30.0f32.max(1.0);
        let max_freq = (sample_rate * 0.5).max(min_freq + 1.0);
        let log_min = min_freq.ln();
        let log_max = max_freq.ln();

        let attack = 0.7f32;
        let decay = 0.15f32;
        for b in 0..SPECTRUM_BINS {
            let f0 = (log_min + (log_max - log_min) * (b as f32) / (SPECTRUM_BINS as f32)).exp();
            let f1 = (log_min + (log_max - log_min) * ((b + 1) as f32) / (SPECTRUM_BINS as f32)).exp();
            let i0 = ((f0 / max_freq) * (half as f32)) as usize;
            let i1 = (((f1 / max_freq) * (half as f32)) as usize).max(i0 + 1).min(half);
            let mut sum = 0.0f32;
            let mut count = 0usize;
            for i in i0..i1 {
                sum += mags[i];
                count += 1;
            }
            let raw = if count == 0 { 0.0 } else { sum / count as f32 };
            // dB scale: 20·log10(m + ε) into roughly [-90 dB, 0 dB] → 0..1.
            let db = 20.0 * (raw + 1e-6).log10();
            let v = ((db + 90.0) / 90.0).clamp(0.0, 1.0);
            let old = self.bins[b];
            self.bins[b] = if v > old {
                old + attack * (v - old)
            } else {
                old + decay * (v - old)
            };
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
