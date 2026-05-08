//! H.264 encoder for the Web Stream node — pipe RGBA frames into
//! ffmpeg and publish each emitted NAL unit to the WebRTC video
//! track via a shared callback.
//!
//! Inverse of `video_player.rs::VideoDecoder` (and a sibling of
//! `web_app_mjpeg.rs::MjpegEncoder`): writes RGBA to ffmpeg's stdin
//! and reads concatenated H.264 Annex-B NAL units from stdout. The
//! NAL splitter scans for the start code `00 00 00 01` (or
//! `00 00 01`) and emits one chunk per NAL boundary.
//!
//! Codec selection is platform-aware:
//! - macOS: `h264_videotoolbox` (HW encode, Apple Silicon + Intel)
//! - other: `libx264 -tune zerolatency -preset ultrafast`
//!
//! Output is piped as `-bsf:v h264_mp4toannexb -f h264 pipe:1`. The
//! `-flush_packets 1` flag is critical for sub-100ms latency — without
//! it ffmpeg buffers a couple frames before emitting anything.

use std::io::{BufRead, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const ENCODER_FPS: u32 = 30;
const KEYFRAME_EVERY_N_FRAMES: u32 = 30; // 1 keyframe/sec @ 30fps

/// Callback fired once per complete H.264 access unit (one full
/// frame's worth of NALs concatenated in Annex-B form, start codes
/// included). webrtc-rs's `TrackLocalStaticSample::write_sample`
/// expects one sample = one frame; calling it per-NAL gave wrong
/// RTP timestamps and the browser decoder rejected the stream.
///
/// A keyframe AU is `SPS + PPS + IDR` (3 NALs). A P-frame AU is
/// just one slice NAL. The reader buffers NALs until it sees a VCL
/// slice (nal_unit_type 1 or 5) — that NAL ends the AU.
pub type NalSink = Arc<dyn Fn(&[u8]) + Send + Sync>;

pub struct H264Encoder {
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    locked_dims: (u32, u32),
    last_write_at: Option<Instant>,
    stderr_log: Arc<Mutex<String>>,
    #[allow(dead_code)]
    started_at: Instant,
}

impl H264Encoder {
    /// Spawn ffmpeg locked to `(w, h)`, route NAL units to `sink`. Returns
    /// `Err` if ffmpeg isn't on PATH (with install hint) or dims are
    /// invalid.
    pub fn new(w: u32, h: u32, sink: NalSink) -> Result<Self, String> {
        if w == 0 || h == 0 {
            return Err(format!("invalid stream dimensions {w}×{h}"));
        }
        // h264_videotoolbox requires even dims (encoder limitation).
        // libx264 handles any dims but yuv420p output also wants even.
        if w & 1 != 0 || h & 1 != 0 {
            return Err(format!(
                "H.264 needs even dimensions; got {w}×{h}. \
                 Insert a Crop / Resize node before Web Stream."
            ));
        }

        let dims = format!("{w}x{h}");
        let fps_str = format!("{ENCODER_FPS}");
        let gop_str = format!("{KEYFRAME_EVERY_N_FRAMES}");

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-hide_banner",
            "-loglevel", "error",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "-s", &dims,
            "-framerate", &fps_str,
            "-i", "pipe:0",
        ]);
        // Codec args differ per platform.
        // Bitrate cap. WebRTC over LAN Wi-Fi tolerates ~2 Mbps reliably;
        // beyond that, bursts can saturate the phone's Wi-Fi for a
        // moment, ICE consent freshness fails, and the connection
        // bounces Connected→Disconnected→Connected. 1.5 Mbps is a
        // good headroom margin for 720p baseline H.264.
        #[cfg(target_os = "macos")]
        cmd.args([
            "-c:v", "h264_videotoolbox",
            "-realtime", "1",
            "-allow_sw", "1",
            "-profile:v", "baseline",
            "-pix_fmt", "yuv420p",
            "-b:v", "1500k",
            "-maxrate", "2000k",
            "-g", &gop_str,
            "-bf", "0",
        ]);
        #[cfg(not(target_os = "macos"))]
        cmd.args([
            "-c:v", "libx264",
            "-tune", "zerolatency",
            "-preset", "ultrafast",
            "-profile:v", "baseline",
            "-pix_fmt", "yuv420p",
            "-b:v", "1500k",
            "-maxrate", "2000k",
            "-bufsize", "1500k",
            "-g", &gop_str,
            "-bf", "0",
        ]);
        cmd.args([
            "-bsf:v", "h264_mp4toannexb",
            "-flush_packets", "1",
            "-f", "h264",
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

        // Stderr drain — same shape as web_app_mjpeg.rs and
        // file_recorder.rs. Cap log at ~1 KB.
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

        // Stdout reader — splits the byte stream on Annex-B start
        // codes, hands each complete NAL to `sink`. Exits on EOF
        // when ffmpeg dies.
        thread::spawn(move || nal_reader_loop(stdout, sink));

        Ok(Self {
            process: Some(process),
            stdin: Some(stdin),
            locked_dims: (w, h),
            last_write_at: None,
            stderr_log,
            started_at: Instant::now(),
        })
    }

    /// Push one RGBA frame to ffmpeg. Errors signal "respawn me":
    /// dim mismatch (caller drops + recreates with new dims) or broken
    /// pipe (ffmpeg crashed; stderr is included).
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

        // FPS gate: writes within 1/fps of the previous accepted one
        // are silently skipped.
        let now = Instant::now();
        let frame_period = Duration::from_secs_f32(1.0 / ENCODER_FPS as f32);
        if let Some(last) = self.last_write_at {
            if now.duration_since(last) < frame_period {
                return Ok(());
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
                let stderr = self.stderr_log.lock()
                    .map(|s| s.clone()).unwrap_or_default();
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

impl Drop for H264Encoder {
    fn drop(&mut self) {
        self.stdin.take(); // drop ChildStdin → pipe close → ffmpeg flush
        if let Some(mut p) = self.process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}

/// Read Annex-B NALs from ffmpeg stdout, group them into access
/// units, hand each AU to `sink`. NAL boundaries are start-code-
/// delimited (`00 00 00 01` or `00 00 01`). An AU ends when a VCL
/// slice (`nal_unit_type` 1 = non-IDR, 5 = IDR) is appended; non-VCL
/// NALs (SPS=7, PPS=8, SEI=6, AUD=9) preceding it accumulate into
/// the same AU.
fn nal_reader_loop<R: Read>(mut stdout: R, sink: NalSink) {
    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut nal_start: Option<usize> = None;
    let mut last_scanned: usize = 0;
    let mut chunk = [0u8; 16 * 1024];
    // Accumulator for the current access unit. Flushed when a VCL
    // slice NAL is appended.
    let mut au: Vec<u8> = Vec::with_capacity(128 * 1024);

    let flush_nal = |buf: &[u8], au: &mut Vec<u8>, sink: &NalSink| {
        if buf.is_empty() { return; }
        // Find NAL header (first byte after the start code). Start
        // code is the leading sequence of 0x00 followed by one 0x01.
        let mut i = 0;
        while i < buf.len() && buf[i] == 0x00 { i += 1; }
        if i >= buf.len() || buf[i] != 0x01 { return; }
        let header_idx = i + 1;
        if header_idx >= buf.len() { return; }
        let nal_type = buf[header_idx] & 0x1F;

        // VCL slice ends the AU. Append (slice + any preceding
        // non-VCL NALs already in `au`) and flush.
        if nal_type == 1 || nal_type == 5 {
            au.extend_from_slice(buf);
            if !au.is_empty() {
                sink(au);
                au.clear();
            }
        } else {
            // Non-VCL: parameter set, SEI, AUD, etc. Buffer for the
            // next AU. Skip access-unit-delimiter (type 9) — webrtc-rs
            // packetizes its own and a duplicate confuses the decoder.
            if nal_type != 9 {
                au.extend_from_slice(buf);
            }
        }
    };

    loop {
        match stdout.read(&mut chunk) {
            Ok(0) => break,    // EOF
            Err(_) => break,   // pipe error
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);

                // Scan for start codes from a few bytes back to handle
                // a code split across two read chunks.
                let mut i = last_scanned.saturating_sub(3);
                while i + 2 < buf.len() {
                    let three = buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1;
                    let four = i + 3 < buf.len()
                        && buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 0 && buf[i + 3] == 1;
                    if four || three {
                        if let Some(start) = nal_start {
                            if i > start {
                                flush_nal(&buf[start..i], &mut au, &sink);
                            }
                        }
                        nal_start = Some(i);
                        i += if four { 4 } else { 3 };
                        continue;
                    }
                    i += 1;
                }
                last_scanned = buf.len();

                if let Some(start) = nal_start {
                    if start > 256 * 1024 {
                        buf.drain(..start);
                        last_scanned = last_scanned.saturating_sub(start);
                        nal_start = Some(0);
                    }
                }
            }
        }
    }
}
