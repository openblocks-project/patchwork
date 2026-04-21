// ── Biquad Filter (2nd-order IIR) ────────────────────────────────────────────

/// Standard biquad filter — the building block for parametric EQ.
/// Coefficients from Robert Bristow-Johnson's Audio EQ Cookbook.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BiquadFilter {
    pub b0: f32, pub b1: f32, pub b2: f32,
    pub a1: f32, pub a2: f32,
    #[serde(skip)] x1: f32,
    #[serde(skip)] x2: f32,
    #[serde(skip)] y1: f32,
    #[serde(skip)] y2: f32,
}

impl BiquadFilter {
    /// Create a peaking EQ biquad filter.
    /// freq: center frequency (Hz), gain_db: boost/cut in dB, q: bandwidth, sr: sample rate
    pub fn peaking_eq(freq: f32, gain_db: f32, q: f32, sr: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * freq / sr;
        let sin_w0 = w0.sin();
        let cos_w0 = w0.cos();
        let alpha = sin_w0 / (2.0 * q);

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;

        Self {
            b0: b0 / a0, b1: b1 / a0, b2: b2 / a0,
            a1: a1 / a0, a2: a2 / a0,
            x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0,
        }
    }

    /// Create a low shelf biquad filter.
    pub fn low_shelf(freq: f32, gain_db: f32, sr: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * freq / sr;
        let sin_w0 = w0.sin();
        let cos_w0 = w0.cos();
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / 0.7 - 1.0) + 2.0).sqrt(); // S=0.7
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

        Self {
            b0: b0 / a0, b1: b1 / a0, b2: b2 / a0,
            a1: a1 / a0, a2: a2 / a0,
            x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0,
        }
    }

    /// Create a high shelf biquad filter.
    pub fn high_shelf(freq: f32, gain_db: f32, sr: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w0 = std::f32::consts::TAU * freq / sr;
        let sin_w0 = w0.sin();
        let cos_w0 = w0.cos();
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / 0.7 - 1.0) + 2.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

        Self {
            b0: b0 / a0, b1: b1 / a0, b2: b2 / a0,
            a1: a1 / a0, a2: a2 / a0,
            x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0,
        }
    }

    /// Bypass filter (unity pass-through).
    #[allow(dead_code)]
    pub fn bypass() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    /// 2nd-order high-pass biquad (RBJ cookbook). Q=0.707 gives Butterworth
    /// response — maximally flat passband, 12 dB/octave rolloff.
    pub fn high_pass(freq: f32, q: f32, sr: f32) -> Self {
        let w0 = std::f32::consts::TAU * freq / sr;
        let sin_w0 = w0.sin();
        let cos_w0 = w0.cos();
        let alpha = sin_w0 / (2.0 * q.max(0.0001));

        let b0 = (1.0 + cos_w0) / 2.0;
        let b1 = -(1.0 + cos_w0);
        let b2 = (1.0 + cos_w0) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0, b1: b1 / a0, b2: b2 / a0,
            a1: a1 / a0, a2: a2 / a0,
            x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0,
        }
    }

    /// Process a single sample through this biquad.
    #[inline]
    pub fn reset(&mut self) {
        self.x1 = 0.0; self.x2 = 0.0;
        self.y1 = 0.0; self.y2 = 0.0;
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
                            - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Build a bank of biquad filters from EQ curve points.
/// Points are in normalized coords: x=0..1 (mapped to 20Hz..20kHz log), y=0..1 (mapped to -24..+24 dB).
/// Samples the curve at 10 log-spaced frequency bands.
pub fn curve_to_eq_bands(points: &[[f32; 2]], sample_rate: f32) -> Vec<BiquadFilter> {
    // 10 frequency bands on log scale (Hz)
    let freqs: [f32; 10] = [31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0];
    let q = 1.4; // moderate bandwidth

    let mut bands = Vec::new();
    for (i, &freq) in freqs.iter().enumerate() {
        // Convert frequency to normalized x (0-1, log scale)
        let x = (freq.ln() - 20.0_f32.ln()) / (20000.0_f32.ln() - 20.0_f32.ln());
        // Evaluate the curve at this x position
        let y = evaluate_curve_at(points, x);
        // Convert y (0-1) to gain in dB (-24 to +24)
        let gain_db = (y - 0.5) * 48.0; // 0.5 = 0dB, 0.0 = -24dB, 1.0 = +24dB

        if gain_db.abs() > 0.5 {
            let filter = if i == 0 {
                BiquadFilter::low_shelf(freq, gain_db, sample_rate)
            } else if i == freqs.len() - 1 {
                BiquadFilter::high_shelf(freq, gain_db, sample_rate)
            } else {
                BiquadFilter::peaking_eq(freq, gain_db, q, sample_rate)
            };
            bands.push(filter);
        }
    }
    bands
}

/// Deterministic hash of a list of EQ curve points. Used to detect when the
/// user has actually moved a point so we can skip redundant `UpdateEqBands`
/// commands. Must be stable across calls within a session — std `DefaultHasher`
/// is randomly seeded, so we roll a tiny FNV-1a here instead.
/// f32 → bits cast is fine: NaN/±0 don't occur for UI-driven points clamped to [0,1].
pub fn eq_curve_hash(points: &[[f32; 2]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    let prime: u64 = 0x100_0000_01b3;        // FNV prime
    let mut mix = |word: u32, h: &mut u64| {
        for byte in word.to_le_bytes() {
            *h ^= byte as u64;
            *h = h.wrapping_mul(prime);
        }
    };
    mix(points.len() as u32, &mut h);
    for p in points {
        mix(p[0].to_bits(), &mut h);
        mix(p[1].to_bits(), &mut h);
    }
    h
}

/// Simple cubic Hermite interpolation of a curve defined by sorted [x,y] points.
fn evaluate_curve_at(points: &[[f32; 2]], x: f32) -> f32 {
    if points.is_empty() { return 0.5; } // flat (0dB)
    if points.len() == 1 { return points[0][1]; }
    if x <= points[0][0] { return points[0][1]; }
    if let Some(last) = points.last() { if x >= last[0] { return last[1]; } }

    // Find the segment
    for i in 0..points.len() - 1 {
        let x0 = points[i][0];
        let x1 = points[i + 1][0];
        if x >= x0 && x <= x1 {
            let t = if (x1 - x0).abs() < 1e-6 { 0.0 } else { (x - x0) / (x1 - x0) };
            let y0 = points[i][1];
            let y1 = points[i + 1][1];
            // Cubic Hermite smooth interpolation
            let t2 = t * t;
            let t3 = t2 * t;
            let h = 2.0 * t3 - 3.0 * t2 + 1.0;
            return y0 * h + y1 * (1.0 - h);
        }
    }
    0.5
}

