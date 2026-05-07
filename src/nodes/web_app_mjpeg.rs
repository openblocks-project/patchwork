//! MJPEG encoder for the Web App node — pipe RGBA frames into ffmpeg
//! and publish the resulting JPEGs to a shared slot read by HTTP
//! `/stream` connection threads.
//!
//! Inverse of `video_player.rs::VideoDecoder`: writes RGBA to ffmpeg's
//! stdin and reads MJPEG from stdout. Spawn pattern mirrors
//! `video_io::file_recorder::FileRecorder`.
//!
//! ## Frame splitting
//!
//! ffmpeg's `-f mjpeg pipe:1` writes JPEGs concatenated back-to-back
//! with no length prefix. We use *next-frame's SOI marks the previous
//! frame's end* to split: when we see `FF D8 FF` we publish everything
//! since the last SOI as one complete JPEG. This sidesteps EOI marker
//! ambiguity inside embedded thumbnails. First publish is delayed by
//! one frame (~33 ms at 30 fps) — fine for our latency budget.
//!
//! ## Lifecycle
//!
//! Constructor spawns ffmpeg + sibling stdout/stderr threads. `Drop`
//! kills the child; the threads exit naturally on EOF and are not
//! joined (matches `FileRecorder` and `VideoDecoder` patterns).

use std::io::{BufRead, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Single-writer / multi-reader slot holding the most recently
/// encoded JPEG frame. Each `/stream` connection thread waits on the
/// condvar for a generation bump, then clones the `Arc<Vec<u8>>` —
/// fanout cost is one Arc ref-bump per phone, not a byte-copy.
pub struct JpegSlot {
    state: Mutex<JpegSlotState>,
    cond: Condvar,
}

struct JpegSlotState {
    generation: u64,
    bytes: Option<Arc<Vec<u8>>>,
}

impl JpegSlot {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(JpegSlotState { generation: 0, bytes: None }),
            cond: Condvar::new(),
        }
    }

    /// Replace the latest JPEG; bump generation; wake all waiters.
    pub fn publish(&self, jpeg: Vec<u8>) {
        if let Ok(mut s) = self.state.lock() {
            s.generation = s.generation.wrapping_add(1);
            s.bytes = Some(Arc::new(jpeg));
            self.cond.notify_all();
        }
    }

    /// Block until the slot's generation differs from `last_seen`, or
    /// until `timeout` elapses. Returns the new `(generation, bytes)`
    /// when a fresh frame is available; `None` on timeout (with no new
    /// frame to send — caller decides whether to keep the connection open).
    pub fn wait_next(
        &self,
        last_seen: u64,
        timeout: Duration,
    ) -> Option<(u64, Arc<Vec<u8>>)> {
        let s = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return None,
        };
        if s.generation != last_seen {
            if let Some(b) = s.bytes.as_ref() {
                return Some((s.generation, b.clone()));
            }
        }
        let res = self.cond.wait_timeout_while(s, timeout, |st| st.generation == last_seen).ok()?;
        let s = res.0;
        if res.1.timed_out() {
            return None;
        }
        s.bytes.as_ref().map(|b| (s.generation, b.clone()))
    }
}

impl Default for JpegSlot {
    fn default() -> Self { Self::new() }
}

/// 1×1 black JPEG sent immediately on `/stream` connect so the
/// browser's `<img>` paints something before the first encoded frame
/// arrives. Without this Chrome shows broken-image, Safari shows
/// blank, Firefox spins.
pub const PLACEHOLDER_JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08,
    0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12,
    0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20, 0x24, 0x2E, 0x27, 0x20,
    0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27,
    0x39, 0x3D, 0x38, 0x32, 0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01,
    0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04,
    0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0xFF, 0xC4, 0x00, 0xB5, 0x10, 0x00, 0x02, 0x01, 0x03,
    0x03, 0x02, 0x04, 0x03, 0x05, 0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7D, 0x01, 0x02, 0x03, 0x00,
    0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32,
    0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72,
    0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35,
    0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55,
    0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75,
    0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94,
    0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2,
    0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9,
    0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6,
    0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFF, 0xDA,
    0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0xFB, 0xD0, 0xFF, 0xD9,
];

/// Default frame rate cap for the encoder — matches the `RECORDING_FPS`
/// gate in `file_recorder.rs`. Higher would just stuff ffmpeg's stdin
/// pipe with frames the encoder can't emit fast enough.
const ENCODER_FPS: u32 = 30;

/// JPEG quality (`-q:v`). 1 = best, 31 = worst. 8 ≈ 200 KB / frame at
/// 1080p, well within Wi-Fi headroom.
const ENCODER_QUALITY: u32 = 8;

pub struct MjpegEncoder {
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    locked_dims: (u32, u32),
    last_write_at: Option<Instant>,
    /// Drained by a sibling thread directly into this Mutex<String>.
    /// `Mutex<String>` is `Sync`, unlike `mpsc::Receiver`, so the
    /// enclosing `WebAppNode` can satisfy `NodeBehavior: Sync`.
    stderr_log: Arc<Mutex<String>>,
    /// Wall-clock spawn time. Mirrors VideoDecoder/FileRecorder for
    /// future "encoder healthy?" UI checks; surfaced via `uptime()`.
    #[allow(dead_code)]
    started_at: Instant,
}

impl MjpegEncoder {
    /// Spawn ffmpeg locked to `(w, h)` and start the stdout/stderr
    /// reader threads. Stdout goes to `slot`. Returns Err if ffmpeg
    /// isn't on PATH (with install hint) or dimensions are invalid.
    pub fn new(w: u32, h: u32, slot: Arc<JpegSlot>) -> Result<Self, String> {
        if w == 0 || h == 0 {
            return Err(format!("invalid stream dimensions {w}×{h}"));
        }

        let dims = format!("{w}x{h}");
        let fps_str = format!("{ENCODER_FPS}");
        let quality_str = format!("{ENCODER_QUALITY}");

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-hide_banner",
            "-loglevel", "error",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "-s", &dims,
            "-framerate", &fps_str,
            "-i", "pipe:0",
            "-c:v", "mjpeg",
            "-q:v", &quality_str,
            "-f", "mjpeg",
            "-flush_packets", "1",
            "pipe:1",
        ]);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut process = match cmd.spawn() {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(crate::nodes::video_player::ffmpeg_install_hint());
            }
            Err(e) => return Err(format!("failed to spawn ffmpeg: {e}")),
        };

        let stdin = process.stdin.take()
            .ok_or_else(|| "ffmpeg stdin pipe missing".to_string())?;
        let stdout = process.stdout.take()
            .ok_or_else(|| "ffmpeg stdout pipe missing".to_string())?;
        let stderr = process.stderr.take()
            .ok_or_else(|| "ffmpeg stderr pipe missing".to_string())?;

        // Stderr drain — same shape as `file_recorder.rs:223–231`. If
        // we don't drain, ffmpeg eventually blocks on the full stderr
        // pipe and stops accepting frames. Writes go straight into a
        // shared `Mutex<String>` (capped at ~1 KB) so the encoder
        // itself can stay `Sync`.
        let stderr_log: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let stderr_log_clone = stderr_log.clone();
        thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(|l| l.ok()) {
                if let Ok(mut log) = stderr_log_clone.lock() {
                    if log.len() < 1024 {
                        if !log.is_empty() { log.push('\n'); }
                        log.push_str(&line);
                    }
                }
            }
        });

        // Stdout reader — scans for SOI markers and publishes complete
        // JPEGs to the shared slot. Exits on EOF when ffmpeg is killed.
        let slot_clone = slot.clone();
        thread::spawn(move || stdout_reader_loop(stdout, slot_clone));

        Ok(Self {
            process: Some(process),
            stdin: Some(stdin),
            locked_dims: (w, h),
            last_write_at: None,
            stderr_log,
            started_at: Instant::now(),
        })
    }

    /// Push one RGBA frame into ffmpeg. Returns `Err` if the
    /// dimensions changed (caller should drop and respawn) or the
    /// subprocess died (broken pipe). Throttled to `ENCODER_FPS` —
    /// faster calls are silently dropped (matches FileRecorder's
    /// `last_write_at` gate at `file_recorder.rs:284–292`).
    pub fn write_frame(&mut self, rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
        if (w, h) != self.locked_dims {
            return Err(format!(
                "dims changed {}×{} → {w}×{h}; respawn",
                self.locked_dims.0, self.locked_dims.1
            ));
        }
        let expected = (w as usize) * (h as usize) * 4;
        if rgba.len() < expected {
            return Ok(()); // partial buffer; drop frame, don't tear down
        }

        let now = Instant::now();
        let frame_period = Duration::from_secs_f32(1.0 / ENCODER_FPS as f32);
        if let Some(last) = self.last_write_at {
            if now.duration_since(last) < frame_period {
                return Ok(()); // throttled
            }
        }

        let stdin = match self.stdin.as_mut() {
            Some(s) => s,
            None => return Err("encoder stdin already closed".into()),
        };
        match stdin.write_all(&rgba[..expected]) {
            Ok(()) => {
                self.last_write_at = Some(now);
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                let stderr = self.stderr_log
                    .lock().map(|s| s.clone()).unwrap_or_default();
                Err(format!(
                    "ffmpeg exited{}",
                    if stderr.is_empty() { String::new() }
                    else { format!(": {}", stderr) }
                ))
            }
            Err(e) => Err(format!("write_frame failed: {e}")),
        }
    }

    #[allow(dead_code)]
    pub fn dims(&self) -> (u32, u32) {
        self.locked_dims
    }
}

impl Drop for MjpegEncoder {
    fn drop(&mut self) {
        // Close stdin first so ffmpeg sees EOF and starts flushing,
        // then SIGKILL to be sure it exits within a frame.
        self.stdin.take(); // drop ChildStdin → pipe close
        if let Some(mut p) = self.process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}

/// Read JPEGs from ffmpeg stdout and publish each to `slot`. Splits
/// frames on SOI (`FF D8 FF`) — bytes between two consecutive SOIs
/// are one complete JPEG. Exits on EOF.
fn stdout_reader_loop<R: Read>(mut stdout: R, slot: Arc<JpegSlot>) {
    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut frame_start: Option<usize> = None;
    let mut last_scanned: usize = 0;
    let mut chunk = [0u8; 16 * 1024];

    loop {
        match stdout.read(&mut chunk) {
            Ok(0) => break,                  // EOF — ffmpeg exited
            Err(_) => break,                 // pipe error — same path
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);

                // Scan for SOI starting a few bytes back to handle
                // a `FF D8 FF` split across read boundaries.
                let mut i = last_scanned.saturating_sub(2);
                while i + 2 < buf.len() {
                    if buf[i] == 0xFF && buf[i + 1] == 0xD8 && buf[i + 2] == 0xFF {
                        if let Some(start) = frame_start {
                            if i > start {
                                slot.publish(buf[start..i].to_vec());
                            }
                        }
                        frame_start = Some(i);
                        i += 3; // can't have another SOI inside this prefix
                        continue;
                    }
                    i += 1;
                }
                last_scanned = buf.len();

                // Periodically reclaim memory: if we have a frame
                // start and >64 KB sits before it, trim. Keeps the
                // buffer bounded over a long session.
                if let Some(start) = frame_start {
                    if start > 64 * 1024 {
                        buf.drain(..start);
                        last_scanned = last_scanned.saturating_sub(start);
                        frame_start = Some(0);
                    }
                }
            }
        }
    }
}
