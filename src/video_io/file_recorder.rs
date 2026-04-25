//! File Recorder — pipe RGBA frames to an ffmpeg subprocess.
//!
//! Phase 6 sink for `VideoOutNode`. Wraps a `Command::new("ffmpeg")`
//! child whose stdin accepts raw RGBA8 pixels at the locked first-frame
//! resolution and whose output goes to the user-picked file. Codec +
//! container are picked from the `Codec` enum at record start and
//! cannot change mid-recording.
//!
//! ## Design notes
//!
//! - **Cross-platform.** Pure `std::process::Command`; no platform-
//!   specific code. macOS gets HW codecs (`hevc_videotoolbox`,
//!   `prores_videotoolbox`); other OSes will use the same enum but the
//!   HW codecs will fail to spawn — surface the error to status.
//! - **Two teardown paths.** `stop_graceful(self)` closes stdin and
//!   waits up to 5 s so ffmpeg can flush the moov atom; `Drop` SIGKILLs
//!   immediately for the node-removal / sink-change path where the UI
//!   can't block. `+faststart+frag_keyframe+empty_moov` makes
//!   SIGKILL-truncated MP4s playable to the last keyframe.
//! - **Locked dimensions.** First frame's `(w, h)` defines the encoder
//!   format. Subsequent mismatched frames are dropped with a status
//!   warning — the recording stays alive (a Camera resolution wobble
//!   shouldn't lose 5 minutes of footage).
//! - **Stderr drained on a sibling thread.** Mirror of
//!   `video_player.rs:278–308`. ffmpeg's stderr can fill its pipe and
//!   block the encoder otherwise.
//! - **Pause = drop frames, single output file.** No segmentation; user
//!   can splice in post if they need a cut.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Encoder choice. Container is implied (`.mp4` for H.264/HEVC, `.mov`
/// for ProRes). VP9 / AV1 are deferred — niche on macOS, slow on the
/// software path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// `libx264` software encode. Maximum compatibility; works on every
    /// platform with ffmpeg installed. Slowest of the three at 1080p+.
    H264Sw,
    /// `hevc_videotoolbox` hardware encode. Apple Silicon / Intel mac
    /// HW path; ~half the file size of H.264 at the same quality.
    HevcVt,
    /// `prores_videotoolbox` hardware encode. Apple intermediate codec
    /// — large files but trivial to scrub in Final Cut / DaVinci /
    /// Premiere. The right pick if the recording is going into an
    /// editor.
    ProResVt,
}

impl Codec {
    pub fn extension(self) -> &'static str {
        match self {
            Self::H264Sw | Self::HevcVt => "mp4",
            Self::ProResVt => "mov",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::H264Sw => "H.264 (libx264)",
            Self::HevcVt => "HEVC (VideoToolbox)",
            Self::ProResVt => "ProRes 422 (VideoToolbox)",
        }
    }

    /// Hardware-only encoders reject odd dimensions on the first frame
    /// (VideoToolbox wants even width AND height). Software libx264
    /// handles any dim.
    pub fn requires_even_dims(self) -> bool {
        matches!(self, Self::HevcVt | Self::ProResVt)
    }

    /// Codec-specific ffmpeg args appended after the input spec. Quality
    /// presets here are tuned for "looks good, no special config" rather
    /// than max-compression — recording sessions are usually short.
    fn encoder_args(self) -> Vec<&'static str> {
        match self {
            Self::H264Sw => vec![
                "-c:v", "libx264",
                "-preset", "veryfast",
                "-crf", "20",
                "-pix_fmt", "yuv420p", // Quicktime / web compatibility
            ],
            Self::HevcVt => vec![
                "-c:v", "hevc_videotoolbox",
                "-q:v", "60",          // VT scale 1–100; ~60 ≈ visually lossless
                "-pix_fmt", "yuv420p",
                "-tag:v", "hvc1",      // QuickTime needs hvc1 (not hev1)
            ],
            Self::ProResVt => vec![
                "-c:v", "prores_videotoolbox",
                "-profile:v", "3",     // ProRes 422 HQ
            ],
        }
    }

    fn parse(byte: u8) -> Self {
        match byte {
            1 => Self::HevcVt,
            2 => Self::ProResVt,
            _ => Self::H264Sw,
        }
    }
}

/// Numeric byte → enum helper for the persisted `recorder_codec: u8`
/// field on `VideoOutNode`. Unknown values fall back to H.264.
pub fn codec_from_byte(b: u8) -> Codec {
    Codec::parse(b)
}

/// A running ffmpeg encode session. Construct via `FileRecorder::new`,
/// feed `write_frame` per UI tick, end with `stop_graceful` (clean) or
/// `Drop` (fast).
pub struct FileRecorder {
    /// Wrapped in `Option` so `stop_graceful` can move the `Child` into
    /// a reaper thread without violating `Drop` semantics. After
    /// `take()`, `Drop` skips the kill+wait (the reaper owns the
    /// process).
    process: Option<Child>,
    /// Wrapped in `Option` so `stop_graceful` can drop it (signalling
    /// EOF to ffmpeg, which triggers moov flush) before waiting for
    /// the child to exit.
    stdin: Option<ChildStdin>,
    stderr_rx: mpsc::Receiver<String>,
    locked_dims: (u32, u32),
    codec: Codec,
    output_path: PathBuf,
    paused: bool,
    started_at: Instant,
    frames_written: u64,
    bytes_written: u64,
    /// Latched from the stderr drainer. UI surfaces this in the status
    /// row. 1 KB cap prevents unbounded growth from a chatty ffmpeg.
    stderr_log: String,
}

impl FileRecorder {
    /// Spawn ffmpeg + lock dimensions to `(w, h)`. Returns `Err` if
    /// ffmpeg isn't on PATH (with a brew install hint), or if the
    /// codec needs even dims and `(w, h)` are odd.
    pub fn new(path: &Path, codec: Codec, w: u32, h: u32) -> Result<Self, String> {
        if w == 0 || h == 0 {
            return Err(format!("invalid recording dimensions {w}×{h}"));
        }
        if codec.requires_even_dims() && (w & 1 != 0 || h & 1 != 0) {
            return Err(format!(
                "{} needs even dimensions; got {w}×{h}. Switch codec to H.264 \
                 or pick an even-dim source.",
                codec.label()
            ));
        }

        let mut cmd = Command::new("ffmpeg");
        cmd.args([
            "-y",
            "-hide_banner",
            "-loglevel", "error",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "-s", &format!("{w}x{h}"),
            // -framerate is a hint; actual rate is whatever we feed.
            // ffmpeg interpolates if write_frame is slower than this.
            "-framerate", "60",
            "-i", "pipe:0",
        ]);
        cmd.args(codec.encoder_args());
        // Crash-survivable MP4: faststart moves moov to the front for
        // streaming-friendly playback; frag_keyframe + empty_moov mean a
        // file truncated by SIGKILL is still playable to the last
        // keyframe (no moov → otherwise unplayable).
        if codec.extension() == "mp4" {
            cmd.args(["-movflags", "+faststart+frag_keyframe+empty_moov"]);
        }
        cmd.arg(path.as_os_str());
        cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());

        let mut process = match cmd.spawn() {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(crate::nodes::video_player::ffmpeg_install_hint());
            }
            Err(e) => return Err(format!("failed to spawn ffmpeg: {e}")),
        };

        let stdin = process.stdin.take()
            .ok_or_else(|| "ffmpeg stdin pipe missing".to_string())?;
        let stderr = process.stderr.take()
            .ok_or_else(|| "ffmpeg stderr pipe missing".to_string())?;

        // Drain stderr on a sibling thread — same shape as
        // `video_player.rs:278`. If we don't drain, ffmpeg eventually
        // blocks on the full stderr pipe and stops accepting frames.
        let (err_tx, err_rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(|l| l.ok()) {
                if err_tx.send(line).is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            process: Some(process),
            stdin: Some(stdin),
            stderr_rx: err_rx,
            locked_dims: (w, h),
            codec,
            output_path: path.to_path_buf(),
            paused: false,
            started_at: Instant::now(),
            frames_written: 0,
            bytes_written: 0,
            stderr_log: String::new(),
        })
    }

    /// Feed one RGBA frame. Returns `Err` only on a real subprocess
    /// failure (broken pipe = ffmpeg crashed); a paused recorder or a
    /// dimension mismatch returns `Ok(())` after recording the issue
    /// in `stderr_log` so the UI can warn without tearing down.
    pub fn write_frame(&mut self, rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
        if self.paused {
            return Ok(());
        }
        if (w, h) != self.locked_dims {
            // Don't tear down — a 1-frame Camera wobble shouldn't lose
            // the recording. UI surfaces the warning via stderr_log.
            self.set_warning(format!(
                "Resolution changed mid-recording: locked {}×{}, got {w}×{h}. \
                 Frame dropped.",
                self.locked_dims.0, self.locked_dims.1,
            ));
            return Ok(());
        }
        let expected = (w as usize) * (h as usize) * 4;
        if rgba.len() < expected {
            self.set_warning(format!(
                "Frame buffer too small: {} bytes for {w}×{h}×4 = {expected}.",
                rgba.len(),
            ));
            return Ok(());
        }

        let stdin = match self.stdin.as_mut() {
            Some(s) => s,
            None => return Err("recorder stdin already closed".into()),
        };
        match stdin.write_all(&rgba[..expected]) {
            Ok(()) => {
                self.frames_written += 1;
                self.bytes_written += expected as u64;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                Err(format!(
                    "ffmpeg exited unexpectedly{}",
                    if self.stderr_log.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", self.stderr_log)
                    },
                ))
            }
            Err(e) => Err(format!("write_frame failed: {e}")),
        }
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn frames_written(&self) -> u64 {
        self.frames_written
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Drain any queued ffmpeg stderr lines into `stderr_log`, capped
    /// at ~1 KB so a chatty ffmpeg doesn't eat memory. Mirror of
    /// `VideoDecoder::pump_stderr`. Call each UI tick.
    pub fn pump_stderr(&mut self) {
        while let Ok(line) = self.stderr_rx.try_recv() {
            self.set_warning(line);
        }
    }

    pub fn stderr_log(&self) -> &str {
        &self.stderr_log
    }

    fn set_warning(&mut self, line: String) {
        if self.stderr_log.len() > 1024 {
            return;
        }
        if !self.stderr_log.is_empty() {
            self.stderr_log.push('\n');
        }
        self.stderr_log.push_str(&line);
    }

    /// Graceful shutdown. Closes stdin so ffmpeg flushes the moov atom
    /// (so the file is fully playable) then waits up to 5 s for the
    /// child to exit. On timeout, falls back to SIGKILL — file remains
    /// playable to the last keyframe via `+frag_keyframe+empty_moov`.
    ///
    /// Consumes `self` because the recorder is single-shot — a new
    /// session starts from `FileRecorder::new`.
    pub fn stop_graceful(mut self) -> Result<(), String> {
        // Close stdin → ffmpeg sees EOF → finalises and exits.
        drop(self.stdin.take());

        // Move the Child into a reaper thread so the wait can run
        // without blocking the caller for the full timeout. Caller is
        // expected to run `stop_graceful` itself on a worker thread
        // (see `video_out_node.rs::render_filerec_sink`); the
        // `recv_timeout` here is a per-call safety net.
        let mut process = match self.process.take() {
            Some(p) => p,
            None => return Ok(()), // already reaped (e.g. double-stop)
        };
        let (tx, rx) = mpsc::channel::<std::io::Result<std::process::ExitStatus>>();
        let kill_handle = process.id();
        thread::spawn(move || {
            let r = process.wait();
            let _ = tx.send(r);
        });

        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(_status)) => Ok(()),
            Ok(Err(e)) => Err(format!("ffmpeg wait failed: {e}")),
            Err(_) => {
                // Timeout. Force-terminate via PID — the reaper thread
                // still owns the `Child` so we can't call `.kill()` on
                // it directly. SIGKILL via libc is the cross-platform
                // best we can do; the reaper thread will then notice
                // the exit and finalise.
                #[cfg(unix)]
                unsafe {
                    libc::kill(kill_handle as i32, libc::SIGKILL);
                }
                #[cfg(not(unix))]
                {
                    // Windows: process.id() is the OS handle id; without
                    // the Child handle we can't TerminateProcess cleanly.
                    // Accept the truncation — `+frag_keyframe` keeps the
                    // file playable to the last keyframe.
                    let _ = kill_handle;
                }
                Err("Stop timed out after 5 s — file may be truncated".into())
            }
        }
    }
}

impl Drop for FileRecorder {
    /// Fast teardown for the node-removal / sink-change path. Mirrors
    /// `VideoDecoder::Drop` at `video_player.rs:43–49` — kill +
    /// reap. The output file is whatever ffmpeg flushed before SIGKILL;
    /// `+frag_keyframe+empty_moov` keeps it playable to the last
    /// keyframe.
    ///
    /// Callers wanting a clean file should consume the recorder via
    /// `stop_graceful` instead.
    fn drop(&mut self) {
        // Drop stdin first — for a graceful Stop the caller already
        // dropped it via `stop_graceful`; for the kill path, dropping
        // stdin gives ffmpeg a brief window to start finalising before
        // SIGKILL hits. If `stop_graceful` already moved the Child to
        // its reaper thread, `process` is `None` and there's nothing
        // to kill here.
        let _ = self.stdin.take();
        if let Some(mut p) = self.process.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: spawn a recorder, write 30 frames of solid red,
    /// stop gracefully, assert the file exists and is non-trivial size.
    /// Gated on `PATCHWORK_FFMPEG_TEST=1` so CI without ffmpeg passes.
    #[test]
    fn round_trip_smoke() {
        if std::env::var("PATCHWORK_FFMPEG_TEST").ok().as_deref() != Some("1") {
            eprintln!("skipping — set PATCHWORK_FFMPEG_TEST=1 to run");
            return;
        }
        let tmp = std::env::temp_dir().join("patchwork_filerec_smoke.mp4");
        let _ = std::fs::remove_file(&tmp);

        const W: u32 = 160;
        const H: u32 = 120;
        let mut rec = FileRecorder::new(&tmp, Codec::H264Sw, W, H)
            .expect("spawn ffmpeg");
        let mut frame = vec![0u8; (W * H * 4) as usize];
        for px in frame.chunks_exact_mut(4) {
            px[0] = 220; px[1] = 30; px[2] = 30; px[3] = 255;
        }
        for _ in 0..30 {
            rec.write_frame(&frame, W, H).expect("write_frame");
            std::thread::sleep(Duration::from_millis(33));
        }
        rec.stop_graceful().expect("stop_graceful");

        let meta = std::fs::metadata(&tmp).expect("output file exists");
        assert!(meta.len() > 1024, "output too small: {} bytes", meta.len());

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn even_dim_check_rejects_odd_for_hevc() {
        let r = FileRecorder::new(Path::new("/tmp/nope.mp4"), Codec::HevcVt, 1023, 768);
        assert!(r.is_err());
        let r = FileRecorder::new(Path::new("/tmp/nope.mp4"), Codec::HevcVt, 1024, 769);
        assert!(r.is_err());
    }

    #[test]
    fn h264_accepts_odd_dims_in_validation() {
        // Validation only — actual spawn would still need ffmpeg.
        // Stop short of spawn by using a path that ffmpeg would reject
        // anyway. The point is the codec.requires_even_dims() check
        // doesn't gate H.264.
        assert!(!Codec::H264Sw.requires_even_dims());
    }

    #[test]
    fn codec_extensions_match_container_rules() {
        assert_eq!(Codec::H264Sw.extension(), "mp4");
        assert_eq!(Codec::HevcVt.extension(), "mp4");
        assert_eq!(Codec::ProResVt.extension(), "mov");
    }
}
