use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::cell::UnsafeCell;

// ── Lock-free Ring Buffer for Live Audio Input ──────────────────────────────

/// Single-producer single-consumer lock-free ring buffer for passing audio
/// samples from a CPAL input callback thread to the audio output callback thread.
/// No mutex — uses atomic read/write positions for synchronization.
pub struct LiveInputBuffer {
    data: UnsafeCell<Vec<f32>>,
    pub capacity: usize,
    write_pos: AtomicUsize,
    read_pos: AtomicUsize,
}

// Safety: LiveInputBuffer is designed for single-producer (input callback)
// single-consumer (output callback) use. The atomic positions ensure that
// the producer and consumer never access the same region simultaneously.
unsafe impl Send for LiveInputBuffer {}
unsafe impl Sync for LiveInputBuffer {}

impl LiveInputBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: UnsafeCell::new(vec![0.0f32; capacity]),
            capacity,
            write_pos: AtomicUsize::new(0),
            read_pos: AtomicUsize::new(0),
        }
    }

    /// Write samples from the CPAL input callback (producer).
    /// Wraps around the ring buffer. If the buffer is full, overwrites old data.
    pub fn write(&self, samples: &[f32]) {
        let data = unsafe { &mut *self.data.get() };
        let mut wp = self.write_pos.load(Ordering::Relaxed);
        // For multi-channel input, mix down to mono by averaging channels
        for &s in samples {
            data[wp % self.capacity] = s;
            wp = wp.wrapping_add(1);
        }
        self.write_pos.store(wp, Ordering::Release);
    }

    /// Write interleaved multi-channel samples, mixing down to mono.
    pub fn write_interleaved(&self, samples: &[f32], channels: usize) {
        if channels <= 1 {
            self.write(samples);
            return;
        }
        let data = unsafe { &mut *self.data.get() };
        let mut wp = self.write_pos.load(Ordering::Relaxed);
        let inv_ch = 1.0 / channels as f32;
        for frame in samples.chunks_exact(channels) {
            let mono: f32 = frame.iter().sum::<f32>() * inv_ch;
            data[wp % self.capacity] = mono;
            wp = wp.wrapping_add(1);
        }
        self.write_pos.store(wp, Ordering::Release);
    }

    /// Read samples into the output buffer (consumer).
    /// If not enough samples are available, fills remainder with silence.
    pub fn read_into(&self, buf: &mut [f32], num_frames: usize) {
        let data = unsafe { &*self.data.get() };
        let wp = self.write_pos.load(Ordering::Acquire);
        let mut rp = self.read_pos.load(Ordering::Relaxed);

        let available = wp.wrapping_sub(rp);

        // Skip ahead if too much buffered — keeps latency low (~2 blocks).
        // Without this, the reader chases the writer from behind and
        // accumulated samples create audible delay.
        let max_buffer = num_frames * 3;
        if available > max_buffer {
            rp = wp.wrapping_sub(num_frames);
        }

        let available = wp.wrapping_sub(rp);
        let to_read = num_frames.min(available).min(buf.len());

        for i in 0..to_read {
            buf[i] = data[rp % self.capacity];
            rp = rp.wrapping_add(1);
        }
        // Fill remainder with silence if underrun
        for i in to_read..num_frames.min(buf.len()) {
            buf[i] = 0.0;
        }
        self.read_pos.store(rp, Ordering::Release);
    }
}


// ── Lock-free Ring Buffer for File Playback ──────────────────────────────────

/// SPSC ring buffer for streaming decoded audio file samples from a background
/// decode thread (Symphonia) into the audio output callback.
/// Same architecture as LiveInputBuffer, with additional control signals for
/// play/pause/seek/stop coordination between UI and decode thread.
pub struct FilePlayerBuffer {
    data: UnsafeCell<Vec<f32>>,
    pub capacity: usize,
    write_pos: AtomicUsize,
    read_pos: AtomicUsize,
    /// Decode thread sets when file reaches EOF
    pub finished: AtomicBool,
    /// UI sets to pause decode thread (audio callback returns silence)
    pub paused: AtomicBool,
    /// UI signals decode thread to seek
    pub seek_requested: AtomicBool,
    /// Target seek position in milliseconds (atomic — no mutex needed).
    /// Stored as u64 milliseconds to avoid needing AtomicF64.
    pub seek_target_ms: AtomicUsize,
    /// UI signals decode thread to stop
    pub stop_requested: AtomicBool,
    /// File's native sample rate (set once by decode thread on open)
    pub file_sample_rate: AtomicU32,
    /// Playback position in output samples (updated by audio callback consumer)
    pub playback_position: AtomicUsize,
    /// Decoded position in output samples (updated by decode thread as it writes).
    /// Used for playhead display when no Speaker is consuming.
    pub decoded_position: AtomicUsize,
    /// Total duration in file samples (set by decode thread from metadata)
    pub total_samples: AtomicUsize,
    /// Playback rate multiplier × 1000 (1000 = 1.0x, 500 = 0.5x, 2000 = 2.0x).
    /// Stored as integer to use AtomicU32. Set by UI, read by audio callback.
    pub playback_rate_x1000: AtomicU32,
    /// Fractional read position accumulator (only accessed by audio callback thread).
    /// Not atomic — only one consumer. Wrapped in UnsafeCell for interior mutability.
    frac_read_pos: UnsafeCell<f64>,
}

unsafe impl Send for FilePlayerBuffer {}
unsafe impl Sync for FilePlayerBuffer {}

impl FilePlayerBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: UnsafeCell::new(vec![0.0f32; capacity]),
            capacity,
            write_pos: AtomicUsize::new(0),
            read_pos: AtomicUsize::new(0),
            finished: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            seek_requested: AtomicBool::new(false),
            seek_target_ms: AtomicUsize::new(0),
            stop_requested: AtomicBool::new(false),
            file_sample_rate: AtomicU32::new(44100),
            playback_position: AtomicUsize::new(0),
            decoded_position: AtomicUsize::new(0),
            total_samples: AtomicUsize::new(0),
            playback_rate_x1000: AtomicU32::new(1000),
            frac_read_pos: UnsafeCell::new(0.0),
        }
    }

    /// Write decoded mono samples into the ring buffer (producer: decode thread).
    pub fn write(&self, samples: &[f32]) {
        let data = unsafe { &mut *self.data.get() };
        let mut wp = self.write_pos.load(Ordering::Relaxed);
        for &s in samples {
            data[wp % self.capacity] = s;
            wp = wp.wrapping_add(1);
        }
        self.write_pos.store(wp, Ordering::Release);
    }

    /// Read samples into the output buffer (consumer: audio callback).
    /// Supports variable playback rate via linear interpolation.
    /// Returns silence on underrun — never blocks.
    pub fn read_into(&self, buf: &mut [f32], num_frames: usize) {
        if self.paused.load(Ordering::Relaxed) {
            for i in 0..num_frames.min(buf.len()) { buf[i] = 0.0; }
            return;
        }
        let data = unsafe { &*self.data.get() };
        let rate = self.playback_rate_x1000.load(Ordering::Relaxed) as f64 / 1000.0;
        let wp = self.write_pos.load(Ordering::Acquire);
        let rp = self.read_pos.load(Ordering::Relaxed);
        let available = wp.wrapping_sub(rp);
        let frac_pos = unsafe { &mut *self.frac_read_pos.get() };

        if (rate - 1.0).abs() < 0.001 {
            // Fast path: 1x speed, no interpolation needed
            let to_read = num_frames.min(available).min(buf.len());
            let mut rp_local = rp;
            for i in 0..to_read {
                buf[i] = data[rp_local % self.capacity];
                rp_local = rp_local.wrapping_add(1);
            }
            for i in to_read..num_frames.min(buf.len()) { buf[i] = 0.0; }
            self.read_pos.store(rp_local, Ordering::Release);
        } else {
            // Variable rate with linear interpolation
            for i in 0..num_frames.min(buf.len()) {
                let int_pos = *frac_pos as usize;
                let consumed = int_pos; // how many whole samples we've advanced past rp
                if consumed + 1 >= available {
                    buf[i] = 0.0; // underrun
                    continue;
                }
                let frac = *frac_pos - int_pos as f64;
                let idx0 = (rp + int_pos) % self.capacity;
                let idx1 = (rp + int_pos + 1) % self.capacity;
                let s0 = data[idx0];
                let s1 = data[idx1];
                buf[i] = s0 + (s1 - s0) * frac as f32; // linear interpolation

                *frac_pos += rate;
            }
            // Advance read_pos by the integer part of what we consumed
            let consumed = *frac_pos as usize;
            let new_rp = rp.wrapping_add(consumed);
            *frac_pos -= consumed as f64;
            self.read_pos.store(new_rp, Ordering::Release);
        }
    }

    /// Reset buffer positions (called during seek to flush stale samples).
    pub fn reset(&self) {
        self.write_pos.store(0, Ordering::Release);
        self.read_pos.store(0, Ordering::Release);
        self.finished.store(false, Ordering::Release);
        unsafe { *self.frac_read_pos.get() = 0.0; }
    }

    /// Replay from the beginning without erasing data.
    /// Moves the read head back to 0 so the audio callback re-reads the existing
    /// waveform. Safe to call from the UI thread — the audio thread will pick up
    /// the updated read_pos on its next callback. `frac_read_pos` stays intact;
    /// on the next variable-rate read the int_pos=0 path picks up from the start.
    pub fn replay(&self) {
        self.read_pos.store(0, Ordering::Release);
        self.finished.store(false, Ordering::Release);
    }

    /// How many output samples are available but not yet consumed by the callback.
    pub fn buffered(&self) -> usize {
        let wp = self.write_pos.load(Ordering::Relaxed);
        let rp = self.read_pos.load(Ordering::Relaxed);
        wp.wrapping_sub(rp)
    }
}


// ── Sampler Buffer (Record + Playback) ───────────────────────────────────────

/// Lock-free buffer for recording audio from the audio callback and playing it back.
/// Unlike FilePlayerBuffer (decode thread → callback), this records FROM the callback
/// into a flat buffer, then plays back from that same buffer on demand.
///
/// Thread safety: all control fields are atomic. The sample data is only written
/// during recording (audio thread) and read during playback (audio thread) —
/// never simultaneously, so no data race.
pub struct SamplerBuffer {
    pub data: UnsafeCell<Vec<f32>>,
    pub capacity: usize,
    /// How many samples were actually recorded (set when recording stops).
    pub recorded_length: AtomicUsize,
    /// Current write position during recording.
    pub write_pos: AtomicUsize,
    /// Current read position during playback.
    pub read_pos: AtomicUsize,
    /// true while recording is active.
    pub recording: AtomicBool,
    /// true while playback is active.
    pub playing: AtomicBool,
    /// Loop playback.
    pub looping: AtomicBool,
    /// Trim start in samples (set by UI).
    pub trim_start: AtomicUsize,
    /// Trim end in samples (0 = use recorded_length).
    pub trim_end: AtomicUsize,
    /// Sample rate used for time↔sample conversions.
    pub sample_rate: AtomicU32,
    /// Play direction: false = forward, true = reverse.
    pub reverse: AtomicBool,
}

unsafe impl Send for SamplerBuffer {}
unsafe impl Sync for SamplerBuffer {}

impl SamplerBuffer {
    /// Create a buffer that can hold `max_seconds` of audio at `sample_rate`.
    pub fn new(sample_rate: u32, max_seconds: f32) -> Self {
        let capacity = (sample_rate as f32 * max_seconds).ceil() as usize;
        Self {
            data: UnsafeCell::new(vec![0.0f32; capacity]),
            capacity,
            recorded_length: AtomicUsize::new(0),
            write_pos: AtomicUsize::new(0),
            read_pos: AtomicUsize::new(0),
            recording: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            looping: AtomicBool::new(false),
            trim_start: AtomicUsize::new(0),
            trim_end: AtomicUsize::new(0),
            sample_rate: AtomicU32::new(sample_rate),
            reverse: AtomicBool::new(false),
        }
    }

    /// Start recording — resets write position, playhead, trim, and clears
    /// previous data so downstream nodes can't keep playing the old tail.
    pub fn start_recording(&self) {
        // Stop playback first so the audio thread observes `playing=false`
        // before `recording=true`. Then reset playhead and trim so there's
        // no stale loop-point state bleeding into the new take.
        self.playing.store(false, Ordering::Release);
        self.read_pos.store(0, Ordering::Release);
        self.trim_start.store(0, Ordering::Release);
        self.trim_end.store(0, Ordering::Release);
        self.write_pos.store(0, Ordering::Release);
        self.recorded_length.store(0, Ordering::Release);
        // Zero out the existing audio data so the old buffer tail can't
        // be heard if play is retriggered before a new recording finishes.
        let data = unsafe { &mut *self.data.get() };
        for s in data.iter_mut() { *s = 0.0; }
        self.recording.store(true, Ordering::Release);
    }

    /// Stop recording — stores the recorded length.
    pub fn stop_recording(&self) {
        self.recording.store(false, Ordering::Release);
        let len = self.write_pos.load(Ordering::Acquire);
        self.recorded_length.store(len, Ordering::Release);
        // Reset trim to full recording
        self.trim_start.store(0, Ordering::Release);
        self.trim_end.store(len, Ordering::Release);
    }

    /// Write input samples during recording (called from audio thread).
    /// Stops automatically when capacity is reached.
    pub fn record_from(&self, samples: &[f32]) {
        if !self.recording.load(Ordering::Relaxed) { return; }
        let data = unsafe { &mut *self.data.get() };
        let mut wp = self.write_pos.load(Ordering::Relaxed);
        for &s in samples {
            if wp >= self.capacity {
                // Auto-stop when buffer is full
                self.recording.store(false, Ordering::Release);
                self.recorded_length.store(wp, Ordering::Release);
                self.trim_end.store(wp, Ordering::Release);
                break;
            }
            data[wp] = s;
            wp += 1;
        }
        self.write_pos.store(wp, Ordering::Release);
    }

    /// Start playback from trim_start (or trim_end-1 if reverse).
    pub fn start_playback(&self) {
        let reverse = self.reverse.load(Ordering::Relaxed);
        if reverse {
            let end = self.effective_end();
            self.read_pos.store(if end > 0 { end - 1 } else { 0 }, Ordering::Release);
        } else {
            let start = self.trim_start.load(Ordering::Relaxed);
            self.read_pos.store(start, Ordering::Release);
        }
        self.recording.store(false, Ordering::Release);
        self.playing.store(true, Ordering::Release);
    }

    /// Stop playback.
    pub fn stop_playback(&self) {
        self.playing.store(false, Ordering::Release);
    }

    /// Read samples for playback (called from audio thread).
    /// Respects trim start/end and reverse direction. Returns silence when not playing.
    pub fn play_into(&self, buf: &mut [f32], num_frames: usize) {
        if !self.playing.load(Ordering::Relaxed) {
            for i in 0..num_frames.min(buf.len()) { buf[i] = 0.0; }
            return;
        }

        let data = unsafe { &*self.data.get() };
        let trim_end = self.effective_end();
        let trim_start = self.trim_start.load(Ordering::Relaxed);
        let looping = self.looping.load(Ordering::Relaxed);
        let reverse = self.reverse.load(Ordering::Relaxed);
        let mut rp = self.read_pos.load(Ordering::Relaxed);

        if reverse {
            for i in 0..num_frames.min(buf.len()) {
                if rp <= trim_start || rp == 0 {
                    if looping {
                        rp = if trim_end > 0 { trim_end - 1 } else { 0 };
                    } else {
                        for j in i..num_frames.min(buf.len()) { buf[j] = 0.0; }
                        self.playing.store(false, Ordering::Release);
                        break;
                    }
                }
                buf[i] = if rp < self.capacity { data[rp] } else { 0.0 };
                rp = rp.saturating_sub(1);
            }
        } else {
            for i in 0..num_frames.min(buf.len()) {
                if rp >= trim_end {
                    if looping {
                        rp = trim_start;
                    } else {
                        for j in i..num_frames.min(buf.len()) { buf[j] = 0.0; }
                        self.playing.store(false, Ordering::Release);
                        break;
                    }
                }
                buf[i] = if rp < self.capacity { data[rp] } else { 0.0 };
                rp += 1;
            }
        }
        self.read_pos.store(rp, Ordering::Release);
    }

    /// Effective end position (trim_end if set, else recorded_length).
    fn effective_end(&self) -> usize {
        let te = self.trim_end.load(Ordering::Relaxed);
        if te > 0 { te } else { self.recorded_length.load(Ordering::Relaxed) }
    }

    /// Current playback position in samples.
    pub fn playback_position(&self) -> usize {
        self.read_pos.load(Ordering::Relaxed)
    }

    /// Get recorded duration in seconds.
    pub fn recorded_duration_secs(&self) -> f32 {
        let sr = self.sample_rate.load(Ordering::Relaxed) as f32;
        if sr <= 0.0 { return 0.0; }
        self.recorded_length.load(Ordering::Relaxed) as f32 / sr
    }

    /// Get waveform snapshot using an explicit length (useful during recording
    /// when recorded_length hasn't been finalized yet — pass write_pos instead).
    pub fn waveform_snapshot_live(&self, num_bars: usize, len: usize) -> Vec<f32> {
        if len == 0 || num_bars == 0 { return vec![0.0; num_bars]; }
        let data = unsafe { &*self.data.get() };
        let len = len.min(self.capacity);
        let mut bars = Vec::with_capacity(num_bars);
        let samples_per_bar = (len as f32 / num_bars as f32).max(1.0);
        for i in 0..num_bars {
            let start = (i as f32 * samples_per_bar) as usize;
            let end = ((i + 1) as f32 * samples_per_bar) as usize;
            let end = end.min(len);
            if start >= end { bars.push(0.0); continue; }
            let mut peak: f32 = 0.0;
            for j in start..end {
                peak = peak.max(data[j].abs());
            }
            bars.push(peak);
        }
        bars
    }
}

