// ── Parameter Smoothing ─────────────────────────────────────────────────────

/// One-pole exponential smoother for audio parameters.
/// Eliminates zipper noise when sliders/knobs change at UI rate (~60Hz)
/// by interpolating toward the target value at audio rate (~44kHz).
///
/// Typical smooth_ms values:
///   5ms  — fast response (gain, amplitude)
///  10ms  — medium (filter cutoff, mix)
///  20ms  — slow (room size, large sweeps)
#[derive(Clone, Debug)]
pub struct SmoothedParam {
    pub current: f32,
    pub target: f32,
    coeff: f32, // per-sample smoothing coefficient
    #[allow(dead_code)]
    smooth_ms: f32, // stored to recompute coeff if sample rate changes
}

impl SmoothedParam {
    /// Create a new smoothed parameter starting at `value` with the given
    /// smoothing time in milliseconds.
    pub fn new(value: f32, smooth_ms: f32) -> Self {
        Self {
            current: value,
            target: value,
            // Default coeff for 44100Hz — will be updated on first tick if needed
            coeff: Self::compute_coeff(smooth_ms, 44100.0),
            smooth_ms,
        }
    }

    /// Compute the per-sample coefficient from smoothing time and sample rate.
    /// Uses: coeff = 1 - e^(-2π / (smooth_time_samples))
    fn compute_coeff(smooth_ms: f32, sample_rate: f32) -> f32 {
        let samples = (smooth_ms * 0.001 * sample_rate).max(1.0);
        1.0 - (-1.0 / samples).exp()
    }

    /// Set a new target value (called from UI thread via merge_params).
    #[inline]
    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    /// Advance one sample toward target. Call once per audio sample.
    #[inline]
    pub fn tick(&mut self) -> f32 {
        self.current += (self.target - self.current) * self.coeff;
        self.current
    }

    /// Snap immediately to target (no smoothing). Use on init or reset.
    #[inline]
    #[allow(dead_code)]
    pub fn snap(&mut self, value: f32) {
        self.current = value;
        self.target = value;
    }

    /// Update coefficient if sample rate changed.
    #[allow(dead_code)]
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.coeff = Self::compute_coeff(self.smooth_ms, sample_rate);
    }
}

impl Default for SmoothedParam {
    fn default() -> Self {
        Self::new(0.0, 5.0)
    }
}

// serde: serialize only the target value + smooth_ms (current state is transient)
impl serde::Serialize for SmoothedParam {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.target.serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for SmoothedParam {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f32::deserialize(deserializer)?;
        Ok(Self::new(value, 5.0))
    }
}
