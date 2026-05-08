//! Audio path for the Web Stream node.
//!
//! Two pieces:
//! 1. `AudioTapProcessor` — registered with the audio engine when the
//!    Audio input port is wired. Real-time-safe: copies upstream
//!    samples into a SPSC ring buffer (`LiveInputBuffer` from
//!    `audio/buffers.rs`) and emits silence on its own output (we're
//!    a sink, not a passthrough).
//! 2. `OpusPump` — a non-RT thread that drains the ring buffer in
//!    20 ms chunks, encodes each chunk with `opus::Encoder`, and
//!    hands the bytes off to a `PacketSink` callback (which the
//!    WebRTC layer wires to a `TrackLocalStaticSample`).
//!
//! Why the ring-buffer indirection: `process_block` runs on the
//! audio thread and must not allocate / lock / call into ffmpeg or
//! webrtc. `opus::Encoder::encode_vec` allocates output, so it can't
//! run there. Hand-off via SPSC keeps the audio thread real-time-
//! safe and lets the encoder catch up at its own pace on a sibling
//! thread.

use crate::audio::buffers::LiveInputBuffer;
use crate::audio::processor::{AudioProcessor, ProcessContext, ProcessorKind};
use crate::audio::params::AtomicF32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// 48 kHz mono is the de facto WebRTC standard for Opus. The audio
/// engine's sample rate may be 44.1 kHz on some hardware — in that
/// case we resample server-side via simple linear interp inside the
/// pump (the encoder is the latency-critical piece, not pristine
/// resampling). Latency win > resample-quality loss for a [WIP] node.
pub const OPUS_SAMPLE_RATE: u32 = 48_000;

/// 20 ms at 48 kHz = 960 samples. WebRTC's accepted range is
/// 2.5/5/10/20/40/60 ms; 20 ms balances latency and per-packet
/// overhead well for live monitor-style audio.
pub const OPUS_FRAME_SAMPLES: usize = (OPUS_SAMPLE_RATE / 50) as usize; // 960

/// Ring-buffer capacity. ~200 ms of headroom: even at peak burst-
/// decode mismatch the encoder keeps up. Larger only delays drift
/// detection.
const RING_CAPACITY_SAMPLES: usize = OPUS_SAMPLE_RATE as usize / 5; // 9600

/// Callback fired by the pump for every encoded Opus frame, in the
/// same byte form `TrackLocalStaticSample::write_sample` accepts.
pub type PacketSink = Arc<dyn Fn(&[u8], Duration) + Send + Sync>;

// ── RT processor: copies input → ring buffer ────────────────────────────────

pub struct AudioTapProcessor {
    pub buffer: Arc<LiveInputBuffer>,
}

impl AudioTapProcessor {
    pub fn new(buffer: Arc<LiveInputBuffer>) -> Self {
        Self { buffer }
    }
}

impl AudioProcessor for AudioTapProcessor {
    fn type_id(&self) -> &str { "web_stream_tap" }
    fn kind(&self) -> ProcessorKind { ProcessorKind::Effect }

    fn process_block(&mut self, input: &[f32], output: &mut [f32], _ctx: &ProcessContext) {
        // SPSC write — RT-safe (no allocation, no locks, no I/O).
        // Clamp NaN / Inf / out-of-range to [-1, 1]; libopus encodes
        // gracefully but a single bad sample can produce a malformed
        // packet that the receiver drops, which presents as random
        // audio glitches and (over time) consent-freshness failures.
        // RT-safe: the loop has fixed cost per sample, no allocation.
        let n = input.len().min(output.len());
        let buf_in = &input[..n];
        // We can't write directly into the SPSC buffer one sample at
        // a time without performance overhead, but we can use the
        // `output` slice as scratch (we'll overwrite it with silence
        // after — we're a sink). Reuse it to avoid heap allocation.
        for (i, s) in buf_in.iter().enumerate() {
            let v = if s.is_finite() { s.clamp(-1.0, 1.0) } else { 0.0 };
            output[i] = v;
        }
        self.buffer.write(&output[..n]);
        // Sink: emit silence on our (unused) audio output port.
        for s in output.iter_mut() { *s = 0.0; }
    }

    fn set_params(&mut self, _params: &[f32]) {}
    fn param_count(&self) -> usize { 0 }
    fn prepare(&mut self, _sample_rate: f32, _max_block_size: usize) {}
    fn reset(&mut self) {}
    fn set_shared_params(&mut self, _params: Arc<Vec<AtomicF32>>) {}
}

// ── Non-RT pump: ring buffer → Opus → PacketSink ────────────────────────────

/// Owns the SPSC ring buffer + the encoder thread. Drop signals the
/// thread to stop; it joins on the next loop iteration.
pub struct OpusPump {
    pub buffer: Arc<LiveInputBuffer>,
    shutdown: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl OpusPump {
    /// Start the encoder thread. `engine_sample_rate` is the audio
    /// engine's mono rate (44.1 k or 48 k); we resample to 48 k for
    /// Opus inside the pump.
    pub fn start(engine_sample_rate: u32, sink: PacketSink) -> Result<Self, String> {
        let buffer = Arc::new(LiveInputBuffer::new(RING_CAPACITY_SAMPLES));
        let shutdown = Arc::new(AtomicBool::new(false));

        let buffer_clone = buffer.clone();
        let shutdown_clone = shutdown.clone();
        let thread = thread::Builder::new()
            .name("web_stream_opus".into())
            .spawn(move || {
                pump_loop(buffer_clone, shutdown_clone, engine_sample_rate, sink);
            })
            .map_err(|e| format!("opus pump thread spawn failed: {e}"))?;

        Ok(Self {
            buffer,
            shutdown,
            thread: Some(thread),
        })
    }
}

impl Drop for OpusPump {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            // Don't block UI — pump checks shutdown every 5 ms.
            let _ = t.join();
        }
    }
}

fn pump_loop(
    buffer: Arc<LiveInputBuffer>,
    shutdown: Arc<AtomicBool>,
    engine_sr: u32,
    sink: PacketSink,
) {
    // Build Opus encoder. Voip mode = lower latency, slightly worse
    // music quality — fine for a monitor-style stream.
    let mut encoder = match opus::Encoder::new(
        OPUS_SAMPLE_RATE,
        opus::Channels::Mono,
        opus::Application::Voip,
    ) {
        Ok(e) => e,
        Err(e) => {
            crate::system_log::error(format!("Opus encoder init failed: {e}"));
            return;
        }
    };
    // Bitrate cap; default "auto" can spike to 510 kbps which is
    // overkill for monitor-style audio and bursty on Wi-Fi.
    let _ = encoder.set_bitrate(opus::Bitrate::Bits(64_000));
    // Forward Error Correction. Opus packets carry a low-bandwidth
    // copy of the previous frame so a single dropped packet doesn't
    // glitch the audio. ~10% bitrate cost. Critical on lossy Wi-Fi
    // — without it, a single packet loss hits the receiver as a
    // 20 ms hole, several of which can drop the connection.
    let _ = encoder.set_inband_fec(true);
    // Tell the encoder to assume up to 10% packet loss so it sizes
    // its FEC redundancy appropriately.
    let _ = encoder.set_packet_loss_perc(10);

    // Working buffers. For the resample path we read N input-rate
    // samples, then linearly interp to 960 output-rate samples. For
    // the no-resample path we read 960 directly.
    let resample = engine_sr != OPUS_SAMPLE_RATE;
    let read_chunk = if resample {
        // engine_sr * 20ms / 1000
        ((engine_sr as f64) * 0.020) as usize
    } else {
        OPUS_FRAME_SAMPLES
    };
    let mut input_scratch = vec![0.0f32; read_chunk];
    let mut output_scratch = vec![0.0f32; OPUS_FRAME_SAMPLES];
    let mut opus_packet = vec![0u8; 1500]; // MTU-ish

    let frame_dur = Duration::from_millis(20);
    let poll_period = Duration::from_millis(5);

    loop {
        if shutdown.load(Ordering::Acquire) {
            break;
        }

        // Wait until enough samples are buffered. read_into fills
        // missing samples with silence — we want to avoid that, so
        // poll buffered() until we have a full chunk.
        if buffer.buffered() < read_chunk {
            thread::sleep(poll_period);
            continue;
        }

        buffer.read_into(&mut input_scratch, read_chunk);

        let frame: &[f32] = if resample {
            // Linear-interp resample. Quality is mediocre but latency
            // is ~zero and a [WIP] streaming node isn't a mastering
            // target. Replace with libsamplerate / rubato if quality
            // becomes a real complaint.
            for i in 0..OPUS_FRAME_SAMPLES {
                let src_pos = (i as f64) * (read_chunk as f64) / (OPUS_FRAME_SAMPLES as f64);
                let lo = src_pos as usize;
                let hi = (lo + 1).min(read_chunk - 1);
                let frac = src_pos - lo as f64;
                output_scratch[i] = input_scratch[lo] * (1.0 - frac as f32)
                    + input_scratch[hi] * (frac as f32);
            }
            &output_scratch
        } else {
            &input_scratch
        };

        match encoder.encode_float(frame, &mut opus_packet) {
            Ok(n) if n > 0 => {
                sink(&opus_packet[..n], frame_dur);
            }
            Ok(_) => {
                // 0-byte packet = "DTX" silence; skip.
            }
            Err(e) => {
                crate::system_log::error(format!("opus encode failed: {e}"));
                break;
            }
        }
    }
}
