//! Waveform oscillator math — used by SynthProcessor.

#[derive(Clone, Copy, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum Waveform {
    #[default]
    Sine,
    Saw,
    Square,
    Triangle,
    Noise,
}

impl Waveform {
    pub fn all() -> &'static [Waveform] {
        &[
            Waveform::Sine,
            Waveform::Saw,
            Waveform::Square,
            Waveform::Triangle,
            Waveform::Noise,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Waveform::Sine => "Sine",
            Waveform::Saw => "Saw",
            Waveform::Square => "Square",
            Waveform::Triangle => "Triangle",
            Waveform::Noise => "Noise",
        }
    }

    /// Generate sample at phase (0..1)
    pub fn sample(&self, phase: f32) -> f32 {
        match self {
            Waveform::Sine => (phase * std::f32::consts::TAU).sin(),
            Waveform::Saw => 2.0 * phase - 1.0,
            Waveform::Square => {
                if phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            Waveform::Triangle => {
                if phase < 0.25 {
                    phase * 4.0
                } else if phase < 0.75 {
                    2.0 - phase * 4.0
                } else {
                    phase * 4.0 - 4.0
                }
            }
            Waveform::Noise => fastrand_f32() * 2.0 - 1.0,
        }
    }
}

fn fastrand_f32() -> f32 {
    use std::cell::Cell;
    thread_local! {
        static SEED: Cell<u32> = const { Cell::new(12345) };
    }
    SEED.with(|s| {
        let v = s.get().wrapping_mul(1103515245).wrapping_add(12345);
        s.set(v);
        (v >> 16) as f32 / 32768.0
    })
}
