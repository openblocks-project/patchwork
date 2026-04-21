//! Voice Effects — 8 karaoke-style voice transformers in one processor.
//!
//! Dispatches on an `effect_id` param (rounded to int):
//!   0 Hall      — Schroeder reverb (size, damping, mix)
//!   1 Echo      — delay + lowpass in feedback (time, feedback, mix)
//!   2 Chorus    — 2 LFO-modulated delay taps (rate, depth, mix)
//!   3 Megaphone — HPF → LPF bandpass + soft clip (drive, tone, mix)
//!   4 Chipmunk  — naive varispeed pitch shift + tilt EQ (pitch, formant, mix)
//!   5 Robot     — ring modulator + soft clip (freq, drive, mix)
//!   6 Alien     — vibrato (LFO pitch) + ring mod (rate, amount, mix)
//!   7 Ghost     — reverb + octave-up parallel voice (size, shimmer, mix)
//!
//! DSP is intentionally simple — these are karaoke-style, not studio-grade.
//! All buffers sized once in `prepare()` from max_block_size and a 1 s
//! ceiling so nothing allocates inside `process_block`.

use super::super::biquad::BiquadFilter;
use super::super::processor::{AudioProcessor, ProcessContext, ProcessorKind};
use super::super::smoothed::SmoothedParam;

// ── Constants ───────────────────────────────────────────────────────────────
const MAX_DELAY_MS: f32 = 1000.0;   // 1 s — covers longest echo + safety
const PITCH_DELAY_MS: f32 = 200.0;  // 200 ms ring buffer for pitch shifter

// Schroeder reverb delay lengths in samples at 44.1 kHz. Scaled to actual SR
// in prepare(). Using pairwise-prime lengths reduces comb resonances.
const COMB_LENS_44K: [usize; 4] = [1557, 1617, 1491, 1422];
const AP_LENS_44K: [usize; 2] = [225, 556];

// ── Delay line helper ───────────────────────────────────────────────────────
struct DelayLine {
    buf: Vec<f32>,
    write: usize,
}

impl DelayLine {
    fn new() -> Self { Self { buf: Vec::new(), write: 0 } }

    fn resize(&mut self, len: usize) {
        self.buf = vec![0.0; len.max(1)];
        self.write = 0;
    }

    fn clear(&mut self) { self.buf.fill(0.0); self.write = 0; }

    #[inline]
    fn push(&mut self, x: f32) {
        if self.buf.is_empty() { return; }
        self.buf[self.write] = x;
        self.write = (self.write + 1) % self.buf.len();
    }

    /// Read `delay_samples` behind the write head with linear interpolation.
    #[inline]
    fn read(&self, delay_samples: f32) -> f32 {
        if self.buf.is_empty() { return 0.0; }
        let len = self.buf.len();
        let d = delay_samples.clamp(0.0, (len - 1) as f32);
        let read_f = self.write as f32 - d - 1.0;
        // Wrap negative reads into [0, len).
        let read_f = ((read_f % len as f32) + len as f32) % len as f32;
        let i0 = read_f.floor() as usize;
        let i1 = (i0 + 1) % len;
        let frac = read_f - read_f.floor();
        self.buf[i0] * (1.0 - frac) + self.buf[i1] * frac
    }
}

// ── Comb filter with lowpass in feedback (Schroeder reverb building block) ──
struct Comb {
    buf: Vec<f32>,
    idx: usize,
    feedback: f32,
    damp: f32,
    filter_state: f32,
}

impl Comb {
    fn new() -> Self { Self { buf: Vec::new(), idx: 0, feedback: 0.5, damp: 0.5, filter_state: 0.0 } }

    fn resize(&mut self, len: usize) {
        self.buf = vec![0.0; len.max(1)];
        self.idx = 0;
        self.filter_state = 0.0;
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.filter_state = 0.0;
        self.idx = 0;
    }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        if self.buf.is_empty() { return 0.0; }
        let out = self.buf[self.idx];
        // one-pole lowpass in the feedback path — higher `damp` → darker tail.
        self.filter_state = out * (1.0 - self.damp) + self.filter_state * self.damp;
        self.buf[self.idx] = x + self.filter_state * self.feedback;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

// ── All-pass for reverb diffusion ───────────────────────────────────────────
struct AllPass {
    buf: Vec<f32>,
    idx: usize,
    coeff: f32,
}

impl AllPass {
    fn new() -> Self { Self { buf: Vec::new(), idx: 0, coeff: 0.5 } }

    fn resize(&mut self, len: usize) {
        self.buf = vec![0.0; len.max(1)];
        self.idx = 0;
    }

    fn clear(&mut self) { self.buf.fill(0.0); self.idx = 0; }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        if self.buf.is_empty() { return x; }
        let bufout = self.buf[self.idx];
        let out = -x + bufout;
        self.buf[self.idx] = x + bufout * self.coeff;
        self.idx = (self.idx + 1) % self.buf.len();
        out
    }
}

// ── The processor ───────────────────────────────────────────────────────────
pub struct VoiceEffectsProcessor {
    // Control params (5 total)
    effect: SmoothedParam,   // 0..7, quantized to int per block
    p1: SmoothedParam,
    p2: SmoothedParam,
    p3: SmoothedParam,
    mix: SmoothedParam,

    sample_rate: f32,

    // Shared delay for echo/chorus
    echo_delay: DelayLine,
    chorus_delay: DelayLine,

    // Reverb
    combs: [Comb; 4],
    allpasses: [AllPass; 2],

    // Megaphone filters
    meg_hpf: BiquadFilter,
    meg_lpf_state: f32,

    // LFO phase (chorus, alien vibrato)
    lfo_phase: f32,

    // Ring mod carrier phase (robot, alien)
    rm_phase: f32,

    // Pitch shifter (used by chipmunk, ghost shimmer, alien vibrato via
    // fractional delay read — we reuse the same ring buffer for different
    // modes to keep footprint small).
    pitch_buf: Vec<f32>,
    pitch_write: usize,
    /// Two read heads (in samples back from write) for crossfading so pitch
    /// wraps don't click. Heads are offset by half the loop length.
    pitch_head_a: f32,
    pitch_head_b: f32,
}

impl VoiceEffectsProcessor {
    pub fn new(effect: f32, p1: f32, p2: f32, p3: f32, mix: f32) -> Self {
        Self {
            effect: SmoothedParam::new(effect, 20.0),
            p1: SmoothedParam::new(p1, 15.0),
            p2: SmoothedParam::new(p2, 15.0),
            p3: SmoothedParam::new(p3, 15.0),
            mix: SmoothedParam::new(mix, 10.0),
            sample_rate: 44100.0,
            echo_delay: DelayLine::new(),
            chorus_delay: DelayLine::new(),
            combs: [Comb::new(), Comb::new(), Comb::new(), Comb::new()],
            allpasses: [AllPass::new(), AllPass::new()],
            meg_hpf: BiquadFilter::high_pass(500.0, 0.707, 44100.0),
            meg_lpf_state: 0.0,
            lfo_phase: 0.0,
            rm_phase: 0.0,
            pitch_buf: Vec::new(),
            pitch_write: 0,
            pitch_head_a: 0.0,
            pitch_head_b: 0.0,
        }
    }

}

impl AudioProcessor for VoiceEffectsProcessor {
    fn type_id(&self) -> &str { "voice_effects" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.effect.set(v); }
        if let Some(&v) = params.get(1) { self.p1.set(v); }
        if let Some(&v) = params.get(2) { self.p2.set(v); }
        if let Some(&v) = params.get(3) { self.p3.set(v); }
        if let Some(&v) = params.get(4) { self.mix.set(v); }
    }

    fn param_count(&self) -> usize { 5 }

    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.sample_rate = sample_rate;

        // Delay lines — sized for the longest reasonable use.
        let max_delay_samples = (MAX_DELAY_MS * 0.001 * sample_rate) as usize + 16;
        self.echo_delay.resize(max_delay_samples);

        // Chorus runs in 5–30 ms range; allocate 50 ms for headroom.
        let chorus_max = ((50.0 * 0.001 * sample_rate) as usize).max(128);
        self.chorus_delay.resize(chorus_max);

        // Reverb: scale reference delay lengths to actual sample rate.
        let sr_ratio = sample_rate / 44100.0;
        for (i, comb) in self.combs.iter_mut().enumerate() {
            let len = ((COMB_LENS_44K[i] as f32) * sr_ratio).round().max(1.0) as usize;
            comb.resize(len);
        }
        for (i, ap) in self.allpasses.iter_mut().enumerate() {
            let len = ((AP_LENS_44K[i] as f32) * sr_ratio).round().max(1.0) as usize;
            ap.resize(len);
        }

        // Pitch shifter ring buffer (~200 ms).
        let pitch_len = ((PITCH_DELAY_MS * 0.001 * sample_rate) as usize).max(2048);
        self.pitch_buf = vec![0.0; pitch_len];
        self.pitch_write = 0;
        self.pitch_head_a = 0.0;
        self.pitch_head_b = pitch_len as f32 / 2.0;

        // Megaphone biquads
        self.meg_hpf = BiquadFilter::high_pass(500.0, 0.707, sample_rate);
        self.meg_lpf_state = 0.0;
    }

    fn reset(&mut self) {
        self.echo_delay.clear();
        self.chorus_delay.clear();
        for c in &mut self.combs { c.clear(); }
        for a in &mut self.allpasses { a.clear(); }
        self.meg_hpf.reset();
        self.meg_lpf_state = 0.0;
        self.lfo_phase = 0.0;
        self.rm_phase = 0.0;
        self.pitch_buf.fill(0.0);
        self.pitch_write = 0;
        self.pitch_head_a = 0.0;
        self.pitch_head_b = self.pitch_buf.len() as f32 / 2.0;
    }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], _ctx: &ProcessContext) {
        for i in 0..input.len() {
            // Tick all smoothed params once per sample.
            let eff_raw = self.effect.tick();
            let p1 = self.p1.tick();
            let p2 = self.p2.tick();
            let _p3 = self.p3.tick();
            let mix = self.mix.tick().clamp(0.0, 1.0);
            let effect_id = eff_raw.round().clamp(0.0, 7.0) as u32;

            let x = input[i];
            let wet = match effect_id {
                0 => self.tick_hall(x, p1, p2),
                1 => self.tick_echo(x, p1, p2),
                2 => self.tick_chorus(x, p1, p2),
                3 => self.tick_megaphone(x, p1, p2),
                4 => self.tick_pitchshift(x, p1, p2),   // Chipmunk/Demon
                5 => self.tick_robot(x, p1, p2),
                6 => self.tick_alien(x, p1, p2),
                7 => self.tick_ghost(x, p1, p2),
                _ => x,
            };
            output[i] = x * (1.0 - mix) + wet * mix;
        }
    }
}

// ── Effect implementations (per-sample ticks) ───────────────────────────────
impl VoiceEffectsProcessor {
    /// Hall reverb — Schroeder architecture.
    /// p1 = size (0..1 → feedback 0.7..0.95)
    /// p2 = damping (0..1 → lowpass coefficient 0.05..0.75)
    fn tick_hall(&mut self, x: f32, size: f32, damping: f32) -> f32 {
        let feedback = 0.7 + size.clamp(0.0, 1.0) * 0.25;
        let damp = 0.05 + damping.clamp(0.0, 1.0) * 0.70;

        let mut sum = 0.0;
        for c in &mut self.combs {
            c.feedback = feedback;
            c.damp = damp;
            sum += c.tick(x);
        }
        let mut y = sum * 0.25;
        for a in &mut self.allpasses {
            a.coeff = 0.5;
            y = a.tick(y);
        }
        y
    }

    /// Echo — single delay with lowpass in feedback.
    /// p1 = time seconds (0.04..0.8)
    /// p2 = feedback (0..0.9)
    fn tick_echo(&mut self, x: f32, time_s: f32, feedback: f32) -> f32 {
        let time = time_s.clamp(0.04, MAX_DELAY_MS * 0.001);
        let delay_samples = time * self.sample_rate;
        let fb = feedback.clamp(0.0, 0.9);
        let tap = self.echo_delay.read(delay_samples);
        // One-pole lowpass in the feedback path to tame repeats.
        self.meg_lpf_state = 0.4 * tap + 0.6 * self.meg_lpf_state;
        self.echo_delay.push(x + self.meg_lpf_state * fb);
        tap
    }

    /// Chorus — 2 LFO-modulated delay taps summed with dry.
    /// p1 = rate Hz (0.1..6)
    /// p2 = depth 0..1 (controls modulation range in ms)
    fn tick_chorus(&mut self, x: f32, rate: f32, depth: f32) -> f32 {
        let rate = rate.clamp(0.1, 6.0);
        self.lfo_phase += rate / self.sample_rate;
        if self.lfo_phase >= 1.0 { self.lfo_phase -= 1.0; }
        let lfo1 = (self.lfo_phase * std::f32::consts::TAU).sin();
        let lfo2 = ((self.lfo_phase + 0.25) * std::f32::consts::TAU).sin();

        // Delay range: 8 ms center ± (depth * 6) ms.
        let center_ms = 8.0;
        let range_ms = 6.0 * depth.clamp(0.0, 1.0);
        let d1 = ((center_ms + lfo1 * range_ms) * 0.001 * self.sample_rate).max(1.0);
        let d2 = ((center_ms + lfo2 * range_ms) * 0.001 * self.sample_rate).max(1.0);
        let t1 = self.chorus_delay.read(d1);
        let t2 = self.chorus_delay.read(d2);
        self.chorus_delay.push(x);
        (t1 + t2) * 0.5
    }

    /// Megaphone — HPF 500 Hz → LPF 3 kHz → soft clip.
    /// p1 = drive (1..20)
    /// p2 = tone (0..1 → LPF coeff 0.1..0.85 — higher = brighter)
    fn tick_megaphone(&mut self, x: f32, drive: f32, tone: f32) -> f32 {
        let drive = drive.clamp(1.0, 20.0);
        let tone = tone.clamp(0.0, 1.0);
        // HPF (500 Hz) to cut bass rumble characteristic of big cones.
        let after_hpf = self.meg_hpf.process(x);
        // Drive + soft clip (tanh) for that buzzy bullhorn bite.
        let driven = (after_hpf * drive).tanh();
        // Simple one-pole LPF as the tone control.
        let a = 0.1 + tone * 0.75;
        self.meg_lpf_state = a * driven + (1.0 - a) * self.meg_lpf_state;
        self.meg_lpf_state * 0.6  // pad gain back after drive
    }

    /// Pitch shifter (Chipmunk/Demon) — naive varispeed with two crossfaded
    /// read heads to hide the loop point.
    /// p1 = semitones (-12..+12)
    /// p2 = formant/tone (0..1 → brighter..darker tilt EQ)
    fn tick_pitchshift(&mut self, x: f32, semitones: f32, tone: f32) -> f32 {
        let ratio = 2f32.powf(semitones.clamp(-12.0, 12.0) / 12.0);
        if self.pitch_buf.is_empty() { return x; }
        let len = self.pitch_buf.len() as f32;

        self.pitch_buf[self.pitch_write] = x;
        self.pitch_write = (self.pitch_write + 1) % self.pitch_buf.len();

        // Advance read heads relative to write. They move at `ratio` — so
        // ratio > 1 (higher pitch) → heads fall behind faster, wrapping often.
        self.pitch_head_a = (self.pitch_head_a - (1.0 - ratio)).rem_euclid(len);
        self.pitch_head_b = (self.pitch_head_b - (1.0 - ratio)).rem_euclid(len);

        let sample_a = read_interp(&self.pitch_buf, self.pitch_head_a);
        let sample_b = read_interp(&self.pitch_buf, self.pitch_head_b);

        // Crossfade based on how close each head is to the write head —
        // the closer, the more "fresh" it is. Use a triangular window.
        let dist_a = head_distance(self.pitch_head_a, self.pitch_write as f32, len);
        let dist_b = head_distance(self.pitch_head_b, self.pitch_write as f32, len);
        let w_a = (dist_a / (len * 0.5)).clamp(0.0, 1.0);
        let w_b = (dist_b / (len * 0.5)).clamp(0.0, 1.0);
        let shifted = sample_a * w_a + sample_b * w_b;

        // Tilt EQ as a lightweight "formant" shaper. tone=0 is brighter
        // (treble boost via differentiation), tone=1 is darker (integration).
        let t = tone.clamp(0.0, 1.0);
        let a = 0.15 + t * 0.75;
        self.meg_lpf_state = a * shifted + (1.0 - a) * self.meg_lpf_state;
        self.meg_lpf_state
    }

    /// Robot — ring modulator at a single carrier frequency.
    /// p1 = carrier Hz (40..800)
    /// p2 = drive (0..1 → soft clip amount)
    fn tick_robot(&mut self, x: f32, freq: f32, drive: f32) -> f32 {
        let freq = freq.clamp(40.0, 800.0);
        self.rm_phase += freq / self.sample_rate;
        if self.rm_phase >= 1.0 { self.rm_phase -= 1.0; }
        let carrier = (self.rm_phase * std::f32::consts::TAU).sin();
        let rm = x * carrier;
        let d = 1.0 + drive.clamp(0.0, 1.0) * 6.0;
        (rm * d).tanh()
    }

    /// Alien — vibrato (LFO-modulated pitch via fractional delay read) +
    /// mild ring mod at a low carrier. Distinct from Robot via slower,
    /// wobbling quality.
    /// p1 = rate Hz (0.1..3)
    /// p2 = amount (0..1)
    fn tick_alien(&mut self, x: f32, rate: f32, amount: f32) -> f32 {
        let rate = rate.clamp(0.1, 3.0);
        let amt = amount.clamp(0.0, 1.0);
        self.lfo_phase += rate / self.sample_rate;
        if self.lfo_phase >= 1.0 { self.lfo_phase -= 1.0; }
        let lfo = (self.lfo_phase * std::f32::consts::TAU).sin();
        // Vibrato via a modulated tap on the chorus delay.
        let center_ms = 10.0;
        let range_ms = 10.0 * amt;
        let d = ((center_ms + lfo * range_ms) * 0.001 * self.sample_rate).max(1.0);
        let tap = self.chorus_delay.read(d);
        self.chorus_delay.push(x);

        // Mild ring mod on top (freq scales with amount so "0" = clean vibrato).
        self.rm_phase += (40.0 + amt * 120.0) / self.sample_rate;
        if self.rm_phase >= 1.0 { self.rm_phase -= 1.0; }
        let carrier = 0.5 + 0.5 * (self.rm_phase * std::f32::consts::TAU).sin();
        tap * (1.0 - amt * 0.5) + (tap * carrier) * (amt * 0.5)
    }

    /// Ghost — hall reverb + a parallel octave-up copy mixed into the wet
    /// path for that haunting airy shimmer.
    /// p1 = size (0..1) — reverb feedback
    /// p2 = shimmer (0..1) — how much of the +oct voice is fed into reverb
    fn tick_ghost(&mut self, x: f32, size: f32, shimmer: f32) -> f32 {
        let sz = size.clamp(0.0, 1.0);
        let sh = shimmer.clamp(0.0, 1.0);

        // Octave-up copy (ratio = 2) via the pitch shifter's ring buffer.
        // This shares state with Chipmunk mode — if the user just switched
        // from Chipmunk the buffer contents will be stale for a brief
        // moment, which is musically fine for a ghostly tail.
        if self.pitch_buf.is_empty() { return self.tick_hall(x, sz, 0.3); }
        let len = self.pitch_buf.len() as f32;
        self.pitch_buf[self.pitch_write] = x;
        self.pitch_write = (self.pitch_write + 1) % self.pitch_buf.len();
        let ratio = 2.0;
        self.pitch_head_a = (self.pitch_head_a - (1.0 - ratio)).rem_euclid(len);
        self.pitch_head_b = (self.pitch_head_b - (1.0 - ratio)).rem_euclid(len);
        let s_a = read_interp(&self.pitch_buf, self.pitch_head_a);
        let s_b = read_interp(&self.pitch_buf, self.pitch_head_b);
        let d_a = head_distance(self.pitch_head_a, self.pitch_write as f32, len);
        let d_b = head_distance(self.pitch_head_b, self.pitch_write as f32, len);
        let w_a = (d_a / (len * 0.5)).clamp(0.0, 1.0);
        let w_b = (d_b / (len * 0.5)).clamp(0.0, 1.0);
        let oct_up = (s_a * w_a + s_b * w_b) * sh;

        // Feed dry + shimmer into reverb for the haunting wash.
        self.tick_hall(x + oct_up * 0.6, sz, 0.2)
    }
}

// ── Free helpers ────────────────────────────────────────────────────────────

#[inline]
fn read_interp(buf: &[f32], pos: f32) -> f32 {
    let len = buf.len();
    if len == 0 { return 0.0; }
    let i0 = pos.floor() as usize % len;
    let i1 = (i0 + 1) % len;
    let frac = pos - pos.floor();
    buf[i0] * (1.0 - frac) + buf[i1] * frac
}

/// Shortest distance from read head to write head on a ring (used for
/// crossfade weighting in the pitch shifter).
#[inline]
fn head_distance(head: f32, write: f32, len: f32) -> f32 {
    let d = (write - head).rem_euclid(len);
    // Treat "close to write" and "far from write" symmetrically — a head
    // right behind the write pointer is freshest.
    d.min(len - d)
}
