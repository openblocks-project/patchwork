#![allow(dead_code)]
//! Effect processors — wrap existing DSP math from AudioEffect enum.
//! Each struct implements AudioProcessor for one effect type.

use crate::audio::processor::{AudioProcessor, ProcessorKind, ProcessContext};
use crate::audio::smoothed::SmoothedParam;
use crate::audio::biquad::BiquadFilter;

// ── Gain ──────────────────────────────────────────────────────────────────────

pub struct GainProcessor {
    level: SmoothedParam,
}

impl GainProcessor {
    pub fn new(level: f32) -> Self {
        Self { level: SmoothedParam::new(level, 5.0) }
    }
}

impl AudioProcessor for GainProcessor {
    fn type_id(&self) -> &str { "gain" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], _ctx: &ProcessContext) {
        for i in 0..input.len() {
            output[i] = input[i] * self.level.tick();
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.level.set(v); }
    }

    fn param_count(&self) -> usize { 1 }

    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {
        self.level = SmoothedParam::new(self.level.target, 5.0);
    }

    fn reset(&mut self) { self.level.current = self.level.target; }
}

// ── LowPass ───────────────────────────────────────────────────────────────────

pub struct LowPassProcessor {
    pub cutoff: SmoothedParam,
    pub state: f32,
}

impl LowPassProcessor {
    pub fn new(cutoff: f32) -> Self {
        Self { cutoff: SmoothedParam::new(cutoff, 10.0), state: 0.0 }
    }
}

impl AudioProcessor for LowPassProcessor {
    fn type_id(&self) -> &str { "lowpass" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        for i in 0..input.len() {
            let c = self.cutoff.tick().max(20.0);
            let rc = 1.0 / (std::f32::consts::TAU * c);
            let dt = 1.0 / ctx.sample_rate;
            let alpha = dt / (rc + dt);
            self.state += alpha * (input[i] - self.state);
            output[i] = self.state;
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.cutoff.set(v); }
    }

    fn param_count(&self) -> usize { 1 }

    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {
        self.cutoff = SmoothedParam::new(self.cutoff.target, 10.0);
    }

    fn reset(&mut self) { self.state = 0.0; }
}

// ── HighPass ──────────────────────────────────────────────────────────────────

pub struct HighPassProcessor {
    pub cutoff: SmoothedParam,
    pub state: f32,
}

impl HighPassProcessor {
    pub fn new(cutoff: f32) -> Self {
        Self { cutoff: SmoothedParam::new(cutoff, 10.0), state: 0.0 }
    }
}

impl AudioProcessor for HighPassProcessor {
    fn type_id(&self) -> &str { "highpass" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], ctx: &ProcessContext) {
        for i in 0..input.len() {
            let c = self.cutoff.tick().max(20.0);
            let rc = 1.0 / (std::f32::consts::TAU * c);
            let dt = 1.0 / ctx.sample_rate;
            let alpha = rc / (rc + dt);
            let out = alpha * (self.state + input[i] - self.state);
            self.state = input[i];
            output[i] = out;
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.cutoff.set(v); }
    }

    fn param_count(&self) -> usize { 1 }

    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {
        self.cutoff = SmoothedParam::new(self.cutoff.target, 10.0);
    }

    fn reset(&mut self) { self.state = 0.0; }
}

// ── Delay ─────────────────────────────────────────────────────────────────────

pub struct DelayProcessor {
    time_ms: f32,
    feedback: SmoothedParam,
    buffer: Vec<f32>,
    write_pos: usize,
    max_delay_samples: usize,
    sample_rate: f32,
}

impl DelayProcessor {
    pub fn new(time_ms: f32, feedback: f32) -> Self {
        Self {
            time_ms, feedback: SmoothedParam::new(feedback, 10.0),
            buffer: Vec::new(), write_pos: 0, max_delay_samples: 0, sample_rate: 44100.0,
        }
    }
}

impl AudioProcessor for DelayProcessor {
    fn type_id(&self) -> &str { "delay" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], _ctx: &ProcessContext) {
        if self.buffer.is_empty() { return; }

        let delay_samples = (self.time_ms * self.sample_rate / 1000.0) as usize;
        let delay_samples = delay_samples.clamp(1, self.max_delay_samples - 1);

        for i in 0..input.len() {
            let read_pos = if self.write_pos >= delay_samples {
                self.write_pos - delay_samples
            } else {
                self.max_delay_samples - (delay_samples - self.write_pos)
            };
            let delayed = self.buffer[read_pos];
            let fb = self.feedback.tick();
            let out = input[i] + delayed * fb;
            self.buffer[self.write_pos] = out;
            self.write_pos = (self.write_pos + 1) % self.max_delay_samples;
            output[i] = out;
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.time_ms = v; }
        if let Some(&v) = params.get(1) { self.feedback.set(v); }
    }

    fn param_count(&self) -> usize { 2 } // time_ms, feedback

    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.sample_rate = sample_rate;
        self.max_delay_samples = (2.0 * sample_rate) as usize; // 2 second max
        self.buffer = vec![0.0; self.max_delay_samples];
        self.write_pos = 0;
        self.feedback = SmoothedParam::new(self.feedback.target, 10.0);
    }

    fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.write_pos = 0;
    }
}

// ── Distortion ────────────────────────────────────────────────────────────────

pub struct DistortionProcessor {
    drive: SmoothedParam,
}

impl DistortionProcessor {
    pub fn new(drive: f32) -> Self {
        Self { drive: SmoothedParam::new(drive, 5.0) }
    }
}

impl AudioProcessor for DistortionProcessor {
    fn type_id(&self) -> &str { "distortion" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], _ctx: &ProcessContext) {
        for i in 0..input.len() {
            let d = self.drive.tick();
            output[i] = (input[i] * d).tanh();
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.drive.set(v); }
    }

    fn param_count(&self) -> usize { 1 }

    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {
        self.drive = SmoothedParam::new(self.drive.target, 5.0);
    }

    fn reset(&mut self) {}
}

// ── Reverb (Schroeder) ────────────────────────────────────────────────────────

pub struct ReverbProcessor {
    room_size: SmoothedParam,
    damping: SmoothedParam,
    mix: SmoothedParam,
    comb_buffers: [Vec<f32>; 4],
    comb_pos: [usize; 4],
    comb_filter_state: [f32; 4],
    allpass_buffers: [Vec<f32>; 2],
    allpass_pos: [usize; 2],
}

impl ReverbProcessor {
    pub fn new(room_size: f32, damping: f32, mix: f32) -> Self {
        Self {
            room_size: SmoothedParam::new(room_size, 20.0),
            damping: SmoothedParam::new(damping, 20.0),
            mix: SmoothedParam::new(mix, 20.0),
            comb_buffers: [vec![], vec![], vec![], vec![]], comb_pos: [0; 4], comb_filter_state: [0.0; 4],
            allpass_buffers: [vec![], vec![]], allpass_pos: [0; 2],
        }
    }
}

impl AudioProcessor for ReverbProcessor {
    fn type_id(&self) -> &str { "reverb" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], _ctx: &ProcessContext) {
        for i in 0..input.len() {
            let sample = input[i];
            let room = self.room_size.tick();
            let damp = self.damping.tick();
            let wet = self.mix.tick();

            let feedback = room.clamp(0.0, 1.0) * 0.28 + 0.7;
            let damp1 = damp;
            let damp2 = 1.0 - damp;

            let mut comb_out = 0.0f32;
            for j in 0..4 {
                let buf = &mut self.comb_buffers[j];
                if buf.is_empty() { continue; }
                let pos = &mut self.comb_pos[j];
                let filt = &mut self.comb_filter_state[j];
                let delayed = buf[*pos];
                *filt = delayed * damp2 + *filt * damp1;
                buf[*pos] = sample + *filt * feedback;
                *pos = (*pos + 1) % buf.len();
                comb_out += delayed;
            }
            comb_out *= 0.25;

            let mut out = comb_out;
            for j in 0..2 {
                let buf = &mut self.allpass_buffers[j];
                if buf.is_empty() { continue; }
                let pos = &mut self.allpass_pos[j];
                let delayed = buf[*pos];
                let ap_out = -out + delayed;
                buf[*pos] = out + delayed * 0.5;
                *pos = (*pos + 1) % buf.len();
                out = ap_out;
            }

            output[i] = sample * (1.0 - wet) + out * wet;
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.room_size.set(v); }
        if let Some(&v) = params.get(1) { self.damping.set(v); }
        if let Some(&v) = params.get(2) { self.mix.set(v); }
    }

    fn param_count(&self) -> usize { 3 }

    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        let sr_scale = (sample_rate / 44100.0).max(0.5);
        let comb_lengths = [
            (1116.0 * sr_scale) as usize,
            (1188.0 * sr_scale) as usize,
            (1277.0 * sr_scale) as usize,
            (1356.0 * sr_scale) as usize,
        ];
        let allpass_lengths = [
            (556.0 * sr_scale) as usize,
            (441.0 * sr_scale) as usize,
        ];
        for (i, &len) in comb_lengths.iter().enumerate() {
            self.comb_buffers[i] = vec![0.0; len.max(1)];
            self.comb_pos[i] = 0;
            self.comb_filter_state[i] = 0.0;
        }
        for (i, &len) in allpass_lengths.iter().enumerate() {
            self.allpass_buffers[i] = vec![0.0; len.max(1)];
            self.allpass_pos[i] = 0;
        }
        self.room_size = SmoothedParam::new(self.room_size.target, 20.0);
        self.damping = SmoothedParam::new(self.damping.target, 20.0);
        self.mix = SmoothedParam::new(self.mix.target, 20.0);
    }

    fn reset(&mut self) {
        for buf in &mut self.comb_buffers { buf.fill(0.0); }
        for buf in &mut self.allpass_buffers { buf.fill(0.0); }
        self.comb_pos = [0; 4];
        self.allpass_pos = [0; 2];
        self.comb_filter_state = [0.0; 4];
    }
}

// ── Noise Removal (2nd-order Butterworth HPF + optional noise gate) ──────────
//
// Designed for voice: removes low-frequency rumble (mic handling, AC mains,
// HVAC, room boom below ~80 Hz) with a 12 dB/octave cutoff, and silences
// quiet-room hiss via a simple envelope-tracking gate when enabled.

pub struct NoiseRemovalProcessor {
    cutoff_hz: SmoothedParam,
    gate_thresh: SmoothedParam, // linear amplitude threshold; 0 = gate off
    hpf: BiquadFilter,
    last_cutoff_hz: f32,
    envelope: f32,
    sample_rate: f32,
}

impl NoiseRemovalProcessor {
    pub fn new(cutoff_hz: f32, gate_thresh: f32) -> Self {
        Self {
            cutoff_hz: SmoothedParam::new(cutoff_hz, 15.0),
            gate_thresh: SmoothedParam::new(gate_thresh, 15.0),
            hpf: BiquadFilter::high_pass(cutoff_hz.max(20.0), 0.707, 44100.0),
            last_cutoff_hz: cutoff_hz,
            envelope: 0.0,
            sample_rate: 44100.0,
        }
    }
}

impl AudioProcessor for NoiseRemovalProcessor {
    fn type_id(&self) -> &str { "noise_removal" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], _ctx: &ProcessContext) {
        // Only rebuild biquad coeffs when the cutoff moves enough to matter
        // — cheap guard, avoids trig every sample.
        let target_cutoff = self.cutoff_hz.tick().max(20.0);
        if (target_cutoff - self.last_cutoff_hz).abs() > 0.5 {
            self.hpf = BiquadFilter::high_pass(target_cutoff, 0.707, self.sample_rate);
            self.last_cutoff_hz = target_cutoff;
        }

        // Gate envelope: slow-falling peak follower. Below-threshold signal
        // gets muted via an exponential ramp (avoids clicks on gate events).
        let gate_thresh = self.gate_thresh.tick().max(0.0);
        let gate_on = gate_thresh > 0.0;
        let env_attack = 0.25f32;   // fast rise
        let env_release = 0.0005f32; // slow fall

        for i in 0..input.len() {
            let y = self.hpf.process(input[i]);
            if !gate_on {
                output[i] = y;
                continue;
            }
            let abs_y = y.abs();
            if abs_y > self.envelope {
                self.envelope += env_attack * (abs_y - self.envelope);
            } else {
                self.envelope += env_release * (abs_y - self.envelope);
            }
            // Soft gate: multiply by a sigmoidish gain based on how far the
            // envelope is above threshold. Prevents hard on/off clicks.
            let margin = (self.envelope / gate_thresh.max(1e-6)).clamp(0.0, 4.0);
            let gain = (margin * margin).min(1.0); // ramps 0→1 over 0..1×threshold
            output[i] = y * gain;
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() { self.cutoff_hz.set(v); }
        if let Some(&v) = params.get(1) { self.gate_thresh.set(v); }
    }

    fn param_count(&self) -> usize { 2 }

    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        self.sample_rate = sample_rate;
        self.cutoff_hz = SmoothedParam::new(self.cutoff_hz.target, 15.0);
        self.gate_thresh = SmoothedParam::new(self.gate_thresh.target, 15.0);
        self.hpf = BiquadFilter::high_pass(self.cutoff_hz.target.max(20.0), 0.707, sample_rate);
        self.last_cutoff_hz = self.cutoff_hz.target;
        self.envelope = 0.0;
    }

    fn reset(&mut self) {
        self.hpf.reset();
        self.envelope = 0.0;
    }
}

// ── Parametric EQ ─────────────────────────────────────────────────────────────

pub struct EqProcessor {
    bands: Vec<BiquadFilter>,
    curve_hash: u64,
}

impl EqProcessor {
    pub fn new(bands: Vec<BiquadFilter>, curve_hash: u64) -> Self {
        Self { bands, curve_hash }
    }
}

impl AudioProcessor for EqProcessor {
    fn type_id(&self) -> &str { "eq" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], _ctx: &ProcessContext) {
        for i in 0..input.len() {
            let mut s = input[i];
            for band in self.bands.iter_mut() {
                s = band.process(s);
            }
            output[i] = s;
        }
    }

    fn set_params(&mut self, _params: &[f32]) {
        // EQ params are the curve points — updated via replace, not atomic slots
    }

    fn param_count(&self) -> usize { 0 }

    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}

    fn reset(&mut self) {
        for band in &mut self.bands { band.reset(); }
    }

    fn set_eq_bands(&mut self, bands: Vec<crate::audio::biquad::BiquadFilter>, hash: u64) {
        // Swap in the new band set. The old Vec is dropped on the audio thread,
        // but this only happens when the user actually drags an EQ point — at
        // human-interaction rate, not per-block — so the cost is negligible.
        // Filter state (x1/x2/y1/y2) is intentionally NOT carried across:
        // each new BiquadFilter starts with zero history. With biquads at
        // moderate Q this produces a very brief envelope ramp on parameter
        // change rather than the audible click you'd get from a coefficient
        // jump on a stateful filter.
        self.bands = bands;
        self.curve_hash = hash;
    }

    fn eq_curve_hash(&self) -> u64 { self.curve_hash }
}

// ── Pitch Shift (granular overlap-add) ───────────────────────────────────────
//
// Two overlapping Hann-windowed grains read from a circular ring buffer.
// Each grain's read pointer advances at pitch_ratio = 2^(semitones/12) per
// sample, while the write pointer always advances at 1.0 per sample — so
// the tempo is preserved and only the pitch changes.
// Grain size ≈ 46 ms, 50% overlap → continuous coverage with no clicks.

pub struct PitchShiftProcessor {
    semitones: SmoothedParam,
    ring: Vec<f32>,
    write: usize,
    grain_size: usize,
    /// Phase (0..grain_size) and fractional read offset for each of the 2 grains.
    grain_phase: [f64; 2],
    grain_read:  [f64; 2],
    /// Absolute ring position (in samples) where each grain started.
    /// Captured when the grain (re)wraps, then frozen — `read_pos`
    /// is `grain_start + grain_read`. This is what makes ratio=1 a
    /// passthrough: read advances at `ratio` per output sample, so
    /// at ratio=1 read tracks write at a constant gs-sample delay.
    /// Anchoring against the *current* write head instead — which
    /// the previous version did — added an extra +1 sample per
    /// output sample to the effective read rate, so ratio=1
    /// pitched up an octave.
    grain_start: [f64; 2],
}

impl PitchShiftProcessor {
    pub fn new(semitones: f32) -> Self {
        Self {
            semitones: SmoothedParam::new(semitones, 30.0),
            ring: Vec::new(),
            write: 0,
            grain_size: 2048,
            grain_phase: [0.0, 0.0],
            grain_read:  [0.0, 0.0],
            grain_start: [0.0, 0.0],
        }
    }

    #[inline]
    fn read_lerp(&self, pos: f64) -> f32 {
        let n = self.ring.len();
        let pos = pos.rem_euclid(n as f64);
        let i0 = pos as usize % n;
        let i1 = (i0 + 1) % n;
        let frac = (pos - pos.floor()) as f32;
        self.ring[i0] * (1.0 - frac) + self.ring[i1] * frac
    }
}

impl AudioProcessor for PitchShiftProcessor {
    fn type_id(&self) -> &str { "pitchshift" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], _ctx: &ProcessContext) {
        if self.ring.is_empty() { return; }
        let n = self.ring.len();
        let gs = self.grain_size as f64;
        let nf = n as f64;

        for i in 0..input.len() {
            // Write input into ring
            self.ring[self.write] = input[i];
            let write_f = self.write as f64;
            self.write = (self.write + 1) % n;

            let ratio = 2.0f64.powf(self.semitones.tick() as f64 / 12.0);
            let mut out = 0.0f32;

            for g in 0..2 {
                // Hann window: rises from 0 → 1 → 0 over the grain
                let t = self.grain_phase[g] / gs;
                let win = (std::f64::consts::PI * t).sin() as f32;
                let win = win * win;

                // Read from ring: grain_start is the absolute position the
                // grain anchored to when it last (re)started; grain_read
                // advances at `ratio` per output sample so the effective
                // read-rate vs. write-rate is exactly `ratio`.
                let read_pos = (self.grain_start[g] + self.grain_read[g]).rem_euclid(nf);
                out += self.read_lerp(read_pos) * win;

                self.grain_phase[g] += 1.0;
                self.grain_read[g]  += ratio;

                // When grain completes, restart it gs samples behind the
                // current write head (write_f is the position we just wrote
                // to, so write_f - gs + 1 would be the oldest "safe" sample
                // — close enough; off-by-one is masked by the Hann window).
                if self.grain_phase[g] >= gs {
                    self.grain_phase[g] = 0.0;
                    self.grain_read[g]  = 0.0;
                    self.grain_start[g] = (write_f - gs).rem_euclid(nf);
                }
            }

            output[i] = out;
        }
    }

    fn set_params(&mut self, params: &[f32]) {
        if let Some(&v) = params.first() {
            self.semitones.set(v.clamp(-12.0, 12.0));
        }
    }

    fn param_count(&self) -> usize { 1 }

    fn prepare(&mut self, sample_rate: f32, _max_block_size: usize) {
        // ~46 ms grain, rounded to next power of two
        self.grain_size = ((sample_rate * 0.046) as usize).next_power_of_two().max(512);
        // Ring must fit at least 2× the grain so the read head never overtakes write
        let ring_size = self.grain_size * 4;
        self.ring = vec![0.0; ring_size];
        self.write = 0;
        // Stagger the two grains by half a grain so one is always at full window
        self.grain_phase = [0.0, self.grain_size as f64 / 2.0];
        self.grain_read  = [0.0, self.grain_size as f64 / 2.0];
        // Anchor each grain `grain_size` samples behind the start of the
        // ring, modulo n. With write=0 that lands at n - grain_size,
        // which is in the silent prefill — the first ~gs samples of
        // output are silence (natural latency) until the ring fills.
        let nf = ring_size as f64;
        let gsf = self.grain_size as f64;
        self.grain_start = [
            (-gsf).rem_euclid(nf),
            (-gsf).rem_euclid(nf),
        ];
        self.semitones = SmoothedParam::new(self.semitones.target, 30.0);
    }

    fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write = 0;
        self.grain_phase = [0.0, self.grain_size as f64 / 2.0];
        self.grain_read  = [0.0, self.grain_size as f64 / 2.0];
        let nf = self.ring.len() as f64;
        let gsf = self.grain_size as f64;
        self.grain_start = [
            (-gsf).rem_euclid(nf),
            (-gsf).rem_euclid(nf),
        ];
    }
}
