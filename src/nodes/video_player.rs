use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::Instant;

/// Background video decoder using ffmpeg subprocess.
/// Uses a bounded channel (capacity 2) to prevent unbounded memory growth
/// when the UI thread can't keep up with frame production.
///
/// Pub so the new trait‑based `VideoInNode` (`src/nodes/video_in_node.rs`)
/// can spawn its own decoders without duplicating the ffmpeg plumbing.
/// The old enum‑variant `NodeType::Camera` render path
/// (`render_camera` below) still uses this via the module‑private
/// `VIDEO_DECODERS` thread‑local map.
pub struct VideoDecoder {
    process: Child,
    frame_rx: mpsc::Receiver<Arc<ImageData>>,
    /// ffmpeg's stderr, one line per recv. Used for diagnostics when
    /// frames never arrive (typical case: macOS camera TCC denied, or
    /// "device already in use by another process"). Drained by the
    /// node's poll loop into `stderr_log`.
    stderr_rx: mpsc::Receiver<String>,
    _width: u32,
    _height: u32,
    pub frame_changed: bool,
    /// `true` once the ffmpeg child/reader thread has EOF'd or died. The
    /// node layer reads this to surface a "Disconnected" status and stop
    /// drawing stale frames.
    pub disconnected: bool,
    /// When the decoder was spawned. Used by the node layer to decide
    /// whether "no frames yet" is a normal startup lull or a stuck
    /// permission wall (heuristic cutoff around 3 s).
    pub started_at: Instant,
    /// Accumulated ffmpeg stderr output (first ~1 KB). Only populated
    /// when ffmpeg has something to complain about; empty in the happy
    /// path. The node layer surfaces this into the user-visible status.
    pub stderr_log: String,
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        // Kill the ffmpeg process and wait for it to exit so we don't leak zombie processes.
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl VideoDecoder {
    pub fn open_file(path: &str, width: u32, height: u32, start_time: f32) -> Result<Self, String> {
        let mut args = vec![
            "-hide_banner".to_string(),
            "-loglevel".into(), "error".into(),
        ];
        if start_time > 0.0 {
            args.extend(["-ss".into(), format!("{:.3}", start_time)]);
        }
        args.extend([
            "-i".into(), path.to_string(),
            "-f".into(), "rawvideo".into(),
            "-pix_fmt".into(), "rgba".into(),
            "-s".into(), format!("{}x{}", width, height),
            "-r".into(), "30".into(),  // output at 30fps
            "pipe:1".into(),
        ]);

        let process = Command::new("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ffmpeg_install_hint()
                } else {
                    format!("Failed to start ffmpeg: {}", e)
                }
            })?;

        Self::start_reader(process, width, height)
    }

    /// Open a network stream (direct URL: mp4 / m3u8 / mpd / rtsp / rtmp / http
    /// audio that ffmpeg can decode). YouTube/Vimeo *page* URLs need to be
    /// resolved by yt-dlp first; the caller passes the resolved direct URL.
    ///
    /// `start_time` (seconds) seeks before decode begins. For HTTP URLs this
    /// triggers a byte-range request — no full re-download. 0.0 = play from
    /// the beginning.
    ///
    /// Reconnect flags are enabled so a transient HTTP hiccup retries instead
    /// of killing the stream. ffmpeg downscales server-side via `-s` so the
    /// reader thread doesn't have to deal with arbitrary network resolutions.
    pub fn open_url(url: &str, width: u32, height: u32, start_time: f32) -> Result<Self, String> {
        let mut args: Vec<String> = vec![
            "-hide_banner".into(), "-loglevel".into(), "error".into(),
            // HTTP resilience for direct URLs / HLS — harmless for non-HTTP inputs.
            "-reconnect".into(), "1".into(),
            "-reconnect_streamed".into(), "1".into(),
            "-reconnect_delay_max".into(), "2".into(),
            // Lower probe latency for live streams.
            "-fflags".into(), "nobuffer".into(),
            "-flags".into(), "low_delay".into(),
        ];
        if start_time > 0.0 {
            // `-ss` BEFORE `-i` is the fast input-side seek (uses byte-range
            // for HTTP). Keyframe-aligned, ~1–2 s precision on typical mp4.
            args.extend(["-ss".into(), format!("{:.3}", start_time)]);
        }
        args.extend([
            "-i".into(), url.to_string(),
            "-an".into(),                       // discard audio in this pipe; audio uses AudioPipeDecoder
            "-f".into(), "rawvideo".into(),
            "-pix_fmt".into(), "rgba".into(),
            "-s".into(), format!("{}x{}", width, height),
            "-r".into(), "30".into(),
            "pipe:1".into(),
        ]);

        let process = Command::new("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ffmpeg_install_hint()
                } else {
                    format!("Failed to start URL decoder: {}", e)
                }
            })?;

        Self::start_reader(process, width, height)
    }

    pub fn open_camera(device_index: u32, width: u32, height: u32) -> Result<Self, String> {
        // Platform-specific capture format and device input string.
        //
        // macOS (avfoundation) and Linux (v4l2) both accept numeric device
        // identifiers directly.
        //
        // Windows (dshow) does NOT accept an index — it needs the device's
        // friendly name from enumeration, passed as `video="<Name>"`. The
        // previous `device_pnp_<n>` form is a DirectShow internal identifier
        // that ffmpeg's `-i` doesn't accept. We re-enumerate at open time
        // and look up the name by the stored index. Enumeration adds ~200 ms
        // but only happens once per camera open, not per frame.
        #[cfg(target_os = "macos")]
        let (capture_fmt, device_input) = ("avfoundation", format!("{}:none", device_index));
        #[cfg(target_os = "linux")]
        let (capture_fmt, device_input) = ("v4l2", format!("/dev/video{}", device_index));
        #[cfg(target_os = "windows")]
        let (capture_fmt, device_input) = {
            let cams = list_cameras_dshow();
            let name = cams.iter()
                .find(|(idx, _)| *idx == device_index)
                .map(|(_, n)| n.clone())
                .ok_or_else(|| format!(
                    "Camera #{} not found (found {} camera(s). Reopen the Camera node's device selector.)",
                    device_index, cams.len()
                ))?;
            // Escape any embedded quotes in the device name. Rare but
            // possible; defensive because ffmpeg parses the -i argument
            // as a string literal.
            let safe_name = name.replace('"', "\\\"");
            ("dshow", format!("video={}", safe_name))
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        return Err("Camera capture not supported on this platform".into());

        let process = Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error",
                // Low-latency flags: skip probing/buffering for real-time capture
                "-fflags", "nobuffer",
                "-flags", "low_delay",
                "-probesize", "32",
                "-analyzeduration", "0",
                "-f", capture_fmt,
                "-framerate", "30",
                "-video_size", &format!("{}x{}", width, height),
                "-i", &device_input,
                "-f", "rawvideo",
                "-pix_fmt", "rgba",
                "-r", "30",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ffmpeg_install_hint()
                } else {
                    format!("Failed to start camera: {}", e)
                }
            })?;

        Self::start_reader(process, width, height)
    }

    /// Capture the whole (or a single) desktop into an RGBA stream.
    ///
    /// On macOS, AVFoundation represents screens as video devices —
    /// they show up in `list_cameras()` as "Capture screen N". So the
    /// caller passes the same kind of `device_index` they'd pass to
    /// `open_camera`, and this function just forwards to `open_camera`.
    /// `list_screens()` filters the full device list to just the screen
    /// entries for UI.
    ///
    /// On Windows, `gdigrab` captures the full virtual desktop (all
    /// monitors tiled together). Selecting a single display is a
    /// follow-up (use `-offset_x`, `-offset_y`, `-video_size`).
    ///
    /// On Linux, `x11grab` captures from the X display given by the
    /// `DISPLAY` env var (typically `:0.0`). Offset/size come from the
    /// caller via `origin_x`/`origin_y`.
    pub fn open_screen(
        device_index: u32,
        width: u32, height: u32,
        origin_x: i32, origin_y: i32,
    ) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            let _ = (origin_x, origin_y); // screens are one-per-device on macOS
            return Self::open_camera(device_index, width, height);
        }
        #[cfg(target_os = "linux")]
        {
            let _ = device_index;
            let display_env = std::env::var("DISPLAY").unwrap_or_else(|_| ":0.0".into());
            let input = format!("{}+{},{}", display_env, origin_x, origin_y);
            let process = Command::new("ffmpeg")
                .args([
                    "-hide_banner", "-loglevel", "error",
                    "-fflags", "nobuffer",
                    "-flags", "low_delay",
                    "-probesize", "32",
                    "-analyzeduration", "0",
                    "-f", "x11grab",
                    "-framerate", "30",
                    "-video_size", &format!("{}x{}", width, height),
                    "-i", &input,
                    "-f", "rawvideo",
                    "-pix_fmt", "rgba",
                    "-r", "30",
                    "pipe:1",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound { ffmpeg_install_hint() }
                    else { format!("Failed to start screen capture: {}", e) }
                })?;
            return Self::start_reader(process, width, height);
        }
        #[cfg(target_os = "windows")]
        {
            let _ = (device_index, origin_x, origin_y);
            let process = Command::new("ffmpeg")
                .args([
                    "-hide_banner", "-loglevel", "error",
                    "-fflags", "nobuffer",
                    "-flags", "low_delay",
                    "-probesize", "32",
                    "-analyzeduration", "0",
                    "-f", "gdigrab",
                    "-framerate", "30",
                    "-video_size", &format!("{}x{}", width, height),
                    "-i", "desktop",
                    "-f", "rawvideo",
                    "-pix_fmt", "rgba",
                    "-r", "30",
                    "pipe:1",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound { ffmpeg_install_hint() }
                    else { format!("Failed to start screen capture: {}", e) }
                })?;
            return Self::start_reader(process, width, height);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            let _ = (device_index, width, height, origin_x, origin_y);
            Err("Screen capture not supported on this platform".into())
        }
    }

    fn start_reader(mut process: Child, width: u32, height: u32) -> Result<Self, String> {
        let stdout = process.stdout.take().ok_or("No stdout")?;
        let stderr = process.stderr.take().ok_or("No stderr")?;
        // Bounded channel (capacity 1): minimal latency — at most 1 frame queued.
        // The producer blocks briefly if the UI hasn't consumed the last frame yet.
        let (tx, rx) = mpsc::sync_channel(1);
        let frame_size = (width * height * 4) as usize;

        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::with_capacity(frame_size, stdout);
            let mut buf = vec![0u8; frame_size];
            loop {
                // read_exact blocks until a full frame arrives from ffmpeg — this is
                // the natural pacing.  The bounded sync_channel(1) provides backpressure
                // if the UI hasn't consumed the previous frame yet, so no artificial
                // sleep is needed.  Removing the sleep eliminates up to 33ms of latency
                // per frame.
                match reader.read_exact(&mut buf) {
                    Ok(()) => {
                        // Zero-copy: swap the filled buffer into the frame and replace
                        // with a fresh buffer.  Avoids cloning ~8MB per frame at 1080p.
                        let pixels = std::mem::replace(&mut buf, vec![0u8; frame_size]);
                        let frame = Arc::new(ImageData { width, height, pixels });
                        if tx.send(frame).is_err() {
                            break; // Receiver dropped (node deleted)
                        }
                    }
                    Err(_) => break, // EOF or error
                }
            }
        });

        // Stderr reader — surfaces ffmpeg complaints ("camera in use",
        // "Operation not permitted", "no such device", codec errors).
        // Without this, a TCC-denied camera looks identical to a camera
        // that's happily running (both → no stdout bytes).
        let (err_tx, err_rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(|l| l.ok()) {
                // Send may fail if Decoder was dropped; fine, just stop.
                if err_tx.send(line).is_err() { break; }
            }
        });

        Ok(Self {
            process,
            frame_rx: rx,
            stderr_rx: err_rx,
            _width: width,
            _height: height,
            frame_changed: false,
            disconnected: false,
            started_at: Instant::now(),
            stderr_log: String::new(),
        })
    }

    /// Drain any queued ffmpeg stderr lines into `self.stderr_log`,
    /// capped at ~1 KB so a chatty ffmpeg doesn't eat memory. Call each
    /// frame from the node's render loop; cheap on the happy path.
    pub fn pump_stderr(&mut self) {
        while let Ok(line) = self.stderr_rx.try_recv() {
            if self.stderr_log.len() > 1024 { break; }
            if !self.stderr_log.is_empty() { self.stderr_log.push('\n'); }
            self.stderr_log.push_str(&line);
        }
    }

    pub fn try_recv_frame(&mut self) -> Option<Arc<ImageData>> {
        let mut latest = None;
        loop {
            match self.frame_rx.try_recv() {
                Ok(frame) => { latest = Some(frame); }
                Err(mpsc::TryRecvError::Empty) => break,       // No new frame, keep last
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Reader thread died (camera unplugged, ffmpeg crashed, EOF)
                    self.disconnected = true;
                    return None;
                }
            }
        }
        if latest.is_some() {
            self.frame_changed = true;
        }
        latest
    }
}

// ── Audio pipe decoder ──────────────────────────────────────────────────────
//
// Sibling of `VideoDecoder`. Spawns an ffmpeg subprocess that decodes the
// *audio* track from a network URL into mono `f32le` samples at a chosen
// sample rate, piped on stdout. A reader thread converts the byte stream
// into `Vec<f32>` chunks and ships them through a bounded mpsc channel,
// where `AudioManager` drains them into a `FilePlayerBuffer`.
//
// We use a separate ffmpeg invocation (rather than asking the video
// decoder for audio too) so the Audio output port on Video In can be
// optionally enabled — if nothing is wired to it, this whole pipeline
// never spawns and CPU stays low. Trade-off: the source is decoded
// twice and the two ffmpegs don't share a clock, so A/V can drift a
// few hundred ms. Acceptable for v1; a single-process refactor is
// queued as v2 work.
pub struct AudioPipeDecoder {
    process: Child,
    pub samples_rx: mpsc::Receiver<Vec<f32>>,
    pub disconnected: bool,
    stderr_rx: mpsc::Receiver<String>,
    pub stderr_log: String,
    pub started_at: Instant,
}

impl Drop for AudioPipeDecoder {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl AudioPipeDecoder {
    /// Decode the audio track from `source` (either a URL or a local file
    /// path — ffmpeg's `-i` accepts both) into mono f32 samples at
    /// `sample_rate`. `start_time` seeks before decode (`-ss` before
    /// `-i`, byte-range fast on HTTP, instant on disk). Reader thread
    /// pushes ~10 ms chunks into `samples_rx`.
    pub fn open_url(source: &str, sample_rate: f32, start_time: f32) -> Result<Self, String> {
        let mut args: Vec<String> = vec![
            "-hide_banner".into(), "-loglevel".into(), "error".into(),
            "-reconnect".into(), "1".into(),
            "-reconnect_streamed".into(), "1".into(),
            "-reconnect_delay_max".into(), "2".into(),
            "-fflags".into(), "nobuffer".into(),
            "-flags".into(), "low_delay".into(),
        ];
        if start_time > 0.0 {
            args.extend(["-ss".into(), format!("{:.3}", start_time)]);
        }
        args.extend([
            "-i".into(), source.to_string(),
            "-vn".into(),                                       // drop video
            "-ac".into(), "1".into(),                           // mono
            "-ar".into(), format!("{}", sample_rate as u32),    // resample server-side
            "-f".into(), "f32le".into(),                        // raw little-endian f32
            "pipe:1".into(),
        ]);
        let process = Command::new("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ffmpeg_install_hint()
                } else {
                    format!("Failed to start audio decoder: {}", e)
                }
            })?;

        Self::start_reader(process)
    }

    /// Decode audio from a capture device (microphone / line-in) into
    /// mono f32 samples at `sample_rate`. Sibling of `open_url`, but
    /// uses `-f <format> -i <input>` instead of a URL — needed because
    /// avfoundation / alsa / pulse / dshow inputs aren't URLs.
    ///
    /// Caller picks the platform-appropriate `format` and `input`:
    /// - macOS:   format="avfoundation", input=":<audio_idx>" (`:0` = default mic)
    /// - Linux:   format="alsa",         input="default" (or "pulse"/"default")
    /// - Windows: format="dshow",        input="audio=<friendly-name>"
    pub fn open_capture_input(
        format: &str,
        input: &str,
        sample_rate: f32,
    ) -> Result<Self, String> {
        let args: Vec<String> = vec![
            "-hide_banner".into(), "-loglevel".into(), "error".into(),
            "-fflags".into(), "nobuffer".into(),
            "-flags".into(), "low_delay".into(),
            "-probesize".into(), "32".into(),
            "-analyzeduration".into(), "0".into(),
            "-f".into(), format.to_string(),
            "-i".into(), input.to_string(),
            "-vn".into(),
            "-ac".into(), "1".into(),
            "-ar".into(), format!("{}", sample_rate as u32),
            "-f".into(), "f32le".into(),
            "pipe:1".into(),
        ];
        let process = Command::new("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ffmpeg_install_hint()
                } else {
                    format!("Failed to start mic capture: {}", e)
                }
            })?;
        Self::start_reader(process)
    }

    fn start_reader(mut process: Child) -> Result<Self, String> {
        let stdout = process.stdout.take().ok_or("No stdout")?;
        let stderr = process.stderr.take().ok_or("No stderr")?;

        // Bounded — backpressure if the consumer (audio decode thread) is slow.
        // Each chunk = ~10 ms of audio; capacity 8 = ~80 ms of slack.
        let (tx, rx) = mpsc::sync_channel::<Vec<f32>>(8);

        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::with_capacity(8192, stdout);
            // Read in chunks of 1024 frames (mono) ≈ 21 ms at 48 kHz.
            const CHUNK_FRAMES: usize = 1024;
            let mut byte_buf = vec![0u8; CHUNK_FRAMES * 4];
            loop {
                match reader.read_exact(&mut byte_buf) {
                    Ok(()) => {
                        let mut samples = Vec::with_capacity(CHUNK_FRAMES);
                        for chunk in byte_buf.chunks_exact(4) {
                            let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                            samples.push(s);
                        }
                        if tx.send(samples).is_err() { break; }
                    }
                    Err(_) => break, // EOF or error
                }
            }
        });

        let (err_tx, err_rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(|l| l.ok()) {
                if err_tx.send(line).is_err() { break; }
            }
        });

        Ok(Self {
            process,
            samples_rx: rx,
            disconnected: false,
            stderr_rx: err_rx,
            stderr_log: String::new(),
            started_at: Instant::now(),
        })
    }

    pub fn pump_stderr(&mut self) {
        while let Ok(line) = self.stderr_rx.try_recv() {
            if self.stderr_log.len() > 1024 { break; }
            if !self.stderr_log.is_empty() { self.stderr_log.push('\n'); }
            self.stderr_log.push_str(&line);
        }
    }
}

// Store decoders outside the graph (not serializable)
// Using thread_local since all access is from the main/GUI thread
use std::cell::RefCell;
thread_local! {
    static VIDEO_DECODERS: RefCell<HashMap<NodeId, VideoDecoder>> = RefCell::new(HashMap::new());
    /// Per-node preview texture cache. The second tuple element is the data
    /// pointer of the last-uploaded frame's Arc<ImageData> — if the next
    /// render sees the same pointer, we skip the downsample + GPU upload
    /// entirely. Without this, camera preview re-uploaded a 270 KB texture
    /// every UI repaint even when the underlying frame hadn't changed,
    /// causing visible stutter on the camera node itself.
    static VIDEO_TEXTURES: RefCell<HashMap<NodeId, (egui::TextureHandle, usize)>> = RefCell::new(HashMap::new());
    static CAMERA_LIST_CACHE: RefCell<(std::time::Instant, Vec<(u32, String)>)> = RefCell::new((std::time::Instant::now(), Vec::new()));
}

/// Get video / audio duration of a file or URL using ffprobe. Returns
/// `None` for live streams (no fixed duration), unsupported sources, or
/// when ffprobe isn't installed.
pub(crate) fn get_duration(path: &str) -> Option<f32> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration",
               "-of", "default=noprint_wrappers=1:nokey=1", path])
        .output().ok()?;
    let s = String::from_utf8_lossy(&output.stdout);
    s.trim().parse::<f32>().ok()
}

/// Per-OS ffmpeg install hint shown when the binary isn't found on PATH.
/// Called from both `open_file` and `open_camera` failure arms, plus
/// the file recorder's spawn failure arm in `video_io::file_recorder`.
pub fn ffmpeg_install_hint() -> String {
    #[cfg(target_os = "macos")]
    return "ffmpeg not found. Install with: brew install ffmpeg".into();
    #[cfg(target_os = "linux")]
    return "ffmpeg not found. Install via your package manager (e.g. apt install ffmpeg)".into();
    #[cfg(target_os = "windows")]
    return "ffmpeg not found. Download from https://ffmpeg.org/download.html and add ffmpeg.exe to PATH".into();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return "ffmpeg not found — required for camera/video capture".into();
}

/// List available camera devices (cross-platform via ffmpeg)
pub fn list_cameras() -> Vec<(u32, String)> {
    #[cfg(target_os = "macos")]
    return list_cameras_avfoundation();
    #[cfg(target_os = "linux")]
    return list_cameras_v4l2();
    #[cfg(target_os = "windows")]
    return list_cameras_dshow();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Vec::new();
}

#[cfg(target_os = "macos")]
fn list_cameras_avfoundation() -> Vec<(u32, String)> {
    let output = Command::new("ffmpeg")
        .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .output();
    let mut cameras = Vec::new();
    if let Ok(output) = output {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut in_video = false;
        for line in stderr.lines() {
            if line.contains("AVFoundation video devices") { in_video = true; continue; }
            if line.contains("AVFoundation audio devices") { break; }
            if in_video {
                if let Some(bracket_start) = line.find("] [") {
                    let rest = &line[bracket_start + 3..];
                    if let Some(bracket_end) = rest.find(']') {
                        if let Ok(idx) = rest[..bracket_end].parse::<u32>() {
                            let name = rest[bracket_end + 2..].trim().to_string();
                            cameras.push((idx, name));
                        }
                    }
                }
            }
        }
    }
    cameras
}

#[cfg(target_os = "linux")]
fn list_cameras_v4l2() -> Vec<(u32, String)> {
    let mut cameras = Vec::new();
    for i in 0..10u32 {
        let path = format!("/dev/video{}", i);
        if std::path::Path::new(&path).exists() {
            // Try to read device name from sysfs
            let name_path = format!("/sys/class/video4linux/video{}/name", i);
            let name = std::fs::read_to_string(&name_path)
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|_| format!("Camera {}", i));
            cameras.push((i, name));
        }
    }
    cameras
}

/// Run `ffmpeg -list_devices true -f dshow -i dummy` and parse the
/// stderr into video + audio device lists, returning the raw stderr
/// alongside so the UI can surface diagnostics when nothing is found.
///
/// One subprocess yields both video and audio device lists — saves
/// the ~200 ms spawn cost of a second invocation.
///
/// Hardening over the original parser:
/// - Section-aware: tracks "DirectShow video devices" vs
///   "DirectShow audio devices" headers so audio names don't leak into
///   the camera list.
/// - Skips `Alternative name "@device_pnp_..."` lines, which the old
///   parser mis-counted as separate cameras.
/// - Returns the raw stderr so the diagnostic panel can show the user
///   exactly what ffmpeg said when the list is empty.
#[cfg(target_os = "windows")]
fn enumerate_dshow_devices() -> (Vec<(u32, String)>, Vec<(u32, String)>, String) {
    let output = Command::new("ffmpeg")
        .args(["-f", "dshow", "-list_devices", "true", "-i", "dummy"])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .output();
    let stderr = match output {
        Ok(o) => String::from_utf8_lossy(&o.stderr).into_owned(),
        Err(e) => format!("ffmpeg invocation failed: {}", e),
    };
    let (videos, audios) = parse_dshow_devices(&stderr);
    let cameras: Vec<(u32, String)> = videos.into_iter().enumerate()
        .map(|(i, n)| (i as u32, n)).collect();
    let audio_inputs: Vec<(u32, String)> = audios.into_iter().enumerate()
        .map(|(i, n)| (i as u32, n)).collect();
    (cameras, audio_inputs, stderr)
}

/// Parse ffmpeg dshow `-list_devices` stderr into (video_names,
/// audio_names). Section-aware via "DirectShow video/audio devices"
/// header detection. Drops "Alternative name" alias lines.
#[cfg(target_os = "windows")]
fn parse_dshow_devices(stderr: &str) -> (Vec<String>, Vec<String>) {
    #[derive(Clone, Copy)]
    enum Section { None, Video, Audio }
    let mut section = Section::None;
    let mut videos = Vec::new();
    let mut audios = Vec::new();
    for line in stderr.lines() {
        let lower = line.to_lowercase();
        if lower.contains("directshow video") {
            section = Section::Video;
            continue;
        }
        if lower.contains("directshow audio") {
            section = Section::Audio;
            continue;
        }
        // "Alternative name" lines describe device aliases (the
        // `@device_pnp_...` form). Old parser counted these as
        // additional cameras.
        if line.contains("Alternative name") {
            continue;
        }
        let Some(name) = extract_first_quoted(line) else { continue };
        match section {
            Section::Video => videos.push(name),
            Section::Audio => audios.push(name),
            Section::None  => {}
        }
    }
    (videos, audios)
}

#[cfg(target_os = "windows")]
fn extract_first_quoted(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let after = &line[start + 1..];
    let end = after.find('"')?;
    let name = after[..end].to_string();
    if name.is_empty() { None } else { Some(name) }
}

#[cfg(target_os = "windows")]
fn list_cameras_dshow() -> Vec<(u32, String)> {
    enumerate_dshow_devices().0
}

// ── Video Player Node ────────────────────────────────────────────────────────

pub fn render_video(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _connections: &[Connection],
) {
    let (path, playing, looping, res_w, res_h, current_frame, duration, speed, status) = match node_type {
        NodeType::VideoPlayer { path, playing, looping, res_w, res_h, current_frame, duration, speed, status } =>
            (path, playing, looping, res_w, res_h, current_frame, duration, speed, status),
        _ => return,
    };

    // Open button
    ui.horizontal(|ui| {
        if ui.button("Open...").clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Video", &["mp4", "mov", "avi", "webm", "mkv", "gif"])
                .pick_file()
            {
                *path = p.display().to_string();
                *duration = get_duration(&path).unwrap_or(0.0);
                *playing = false;
                *status = "Loaded".into();
                // Stop existing decoder, drop the previously decoded frame,
                // and invalidate any cached GPU textures so downstream nodes
                // don't show one stale frame from the previous file.
                VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
                VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
                *current_frame = None;
                crate::gpu_image::request_node_invalidation(node_id);
            }
        }
    });

    if !path.is_empty() {
        let short = if path.len() > 35 { format!("...{}", &path[path.len()-35..]) } else { path.clone() };
        ui.label(egui::RichText::new(short).small().monospace());
    }

    // Resolution
    ui.horizontal(|ui| {
        ui.label("Res:");
        ui.add(egui::DragValue::new(res_w).range(120..=1920).speed(10).prefix("W:"));
        ui.add(egui::DragValue::new(res_h).range(90..=1080).speed(10).prefix("H:"));
    });

    // Play / Pause / Stop
    ui.horizontal(|ui| {
        let play_label = if *playing { "⏸ Pause" } else { "▶ Play" };
        if ui.button(play_label).clicked() && !path.is_empty() {
            if *playing {
                *playing = false;
                VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
                *status = "Paused".into();
            } else {
                *playing = true;
                match VideoDecoder::open_file(path, *res_w, *res_h, 0.0) {
                    Ok(dec) => {
                        VIDEO_DECODERS.with(|d| d.borrow_mut().insert(node_id, dec));
                        *status = "Playing".into();
                    }
                    Err(e) => *status = e,
                }
            }
        }
        if ui.button("⏹ Stop").clicked() {
            *playing = false;
            VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
            VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
            *current_frame = None;
            crate::gpu_image::request_node_invalidation(node_id);
            *status = "Stopped".into();
        }
        ui.checkbox(looping, "Loop");
    });

    // Speed
    ui.horizontal(|ui| {
        ui.label("Speed:");
        ui.add(egui::Slider::new(speed, 0.25..=4.0).step_by(0.25));
    });

    // Duration display
    if *duration > 0.0 {
        ui.label(egui::RichText::new(format!("Duration: {:.1}s", duration)).small());
    }

    // Status
    if !status.is_empty() {
        let color = if status.contains("Error") || status.contains("not found") {
            egui::Color32::from_rgb(255, 100, 100)
        } else if *playing {
            egui::Color32::from_rgb(80, 200, 80)
        } else {
            egui::Color32::from_rgb(150, 150, 150)
        };
        ui.colored_label(color, egui::RichText::new(&*status).small());
    }

    // Receive frame from decoder
    VIDEO_DECODERS.with(|d| {
        if let Some(decoder) = d.borrow_mut().get_mut(&node_id) {
            if let Some(frame) = decoder.try_recv_frame() {
                *current_frame = Some(frame);
            }
            if decoder.disconnected {
                *current_frame = None;
                *status = "Disconnected".to_string();
                *playing = false;
                crate::system_log::warn(format!("Video source disconnected (id:{})", node_id));
                decoder.disconnected = false;
            }
        }
    });

    // Preview — downsample + upload only when the frame actually changed.
    if let Some(frame) = current_frame.as_ref() {
        let max_w = ui.available_width().min(300.0);
        let aspect = frame.height as f32 / frame.width as f32;
        let preview_h = max_w * aspect;
        let frame_ptr = std::sync::Arc::as_ptr(frame) as usize;

        VIDEO_TEXTURES.with(|textures| {
            let mut textures = textures.borrow_mut();
            let cached_same = textures.get(&node_id).map(|(_, p)| *p == frame_ptr).unwrap_or(false);
            if !cached_same {
                let pw = max_w as u32;
                let ph = preview_h as u32;
                let preview_pixels = fast_downsample(frame, pw, ph);
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [pw as usize, ph as usize],
                    &preview_pixels,
                );
                match textures.get_mut(&node_id) {
                    Some(entry) => {
                        entry.0.set(color_image, egui::TextureOptions::LINEAR);
                        entry.1 = frame_ptr;
                    }
                    None => {
                        let tex = ui.ctx().load_texture(
                            format!("video_{}", node_id),
                            color_image,
                            egui::TextureOptions::LINEAR,
                        );
                        textures.insert(node_id, (tex, frame_ptr));
                    }
                }
            }
            if let Some((tex, _)) = textures.get(&node_id) {
                ui.image(egui::load::SizedTexture::new(tex.id(), egui::vec2(max_w, preview_h)));
            }
        });
    }

    if *playing {
        ui.ctx().request_repaint();
    }
}

/// Cached enumeration result. `cameras` is the cross-platform device
/// list (used everywhere); `audio_inputs` and `last_stderr` are
/// Windows-only diagnostics that stay empty on macOS / Linux.
struct DshowCache {
    refreshed_at: std::time::Instant,
    cameras: Vec<(u32, String)>,
    audio_inputs: Vec<(u32, String)>,
    last_stderr: String,
    refreshing: bool,
}

/// Get cached camera list — uses background thread to avoid blocking UI.
/// Returns stale data while refresh is in progress.
/// One shared cache for the ffmpeg device list. Both the legacy Camera
/// enum‑variant render path and the new `VideoInNode` read from here, so
/// a refresh in either UI benefits both.
fn camera_list_cache_handle()
    -> &'static std::sync::Mutex<DshowCache>
{
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<DshowCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(DshowCache {
        refreshed_at: std::time::Instant::now() - std::time::Duration::from_secs(100),
        cameras: Vec::new(),
        audio_inputs: Vec::new(),
        last_stderr: String::new(),
        refreshing: false,
    }))
}

/// Run a fresh enumeration. On Windows, calls `enumerate_dshow_devices`
/// for the full (cameras, audio_inputs, stderr) tuple. Elsewhere, just
/// fills the camera list — `audio_inputs` and `last_stderr` stay empty.
fn enumerate_blocking() -> (Vec<(u32, String)>, Vec<(u32, String)>, String) {
    #[cfg(target_os = "windows")]
    {
        return enumerate_dshow_devices();
    }
    #[cfg(not(target_os = "windows"))]
    {
        return (list_cameras(), Vec::new(), String::new());
    }
}

pub fn cached_camera_list() -> Vec<(u32, String)> {
    let cache = camera_list_cache_handle();
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    if guard.refreshed_at.elapsed().as_secs() >= 10 && !guard.refreshing {
        guard.refreshing = true;
        std::thread::spawn(move || {
            let (cams, audios, stderr) = enumerate_blocking();
            if let Ok(mut g) = cache.lock() {
                g.refreshed_at = std::time::Instant::now();
                g.cameras = cams;
                g.audio_inputs = audios;
                g.last_stderr = stderr;
                g.refreshing = false;
            }
        });
    }
    guard.cameras.clone()
}

/// Cached audio input list (Windows-only — populated by the same
/// `ffmpeg -list_devices` call that fills the camera list). On
/// macOS / Linux this is unused because the camera-mode audio path
/// uses `:0` (avfoundation) / `default` (alsa) directly rather than
/// a friendly-name lookup.
#[cfg(target_os = "windows")]
pub fn cached_audio_input_list_dshow() -> Vec<(u32, String)> {
    let cache = camera_list_cache_handle();
    let guard = match cache.lock() { Ok(g) => g, Err(_) => return Vec::new() };
    guard.audio_inputs.clone()
}

/// Last raw stderr from the dshow enumeration. Used by Video In's
/// Camera UI to show users what ffmpeg actually said when the device
/// list is empty. Windows-only debugging surface.
#[cfg(target_os = "windows")]
pub fn last_dshow_stderr() -> String {
    let cache = camera_list_cache_handle();
    let guard = match cache.lock() { Ok(g) => g, Err(_) => return String::new() };
    guard.last_stderr.clone()
}

/// Force the next `cached_camera_list()` call to re-enumerate devices
/// immediately, regardless of the 10‑second TTL. Hooked up to the "↻"
/// Refresh button in the Video In UI so plugging in OBS Virtual Camera
/// / Continuity Camera / a new USB webcam surfaces them without a wait.
///
/// Spawns one background re-enumeration; subsequent calls before the
/// refresh completes are coalesced (the `refreshing` flag is already
/// set). Safe to call from the UI thread.
pub fn refresh_camera_list_now() {
    let cache = camera_list_cache_handle();
    let mut guard = match cache.lock() { Ok(g) => g, Err(_) => return };
    if guard.refreshing { return; }
    guard.refreshing = true;
    drop(guard);
    std::thread::spawn(move || {
        let (cams, audios, stderr) = enumerate_blocking();
        if let Ok(mut g) = cache.lock() {
            g.refreshed_at = std::time::Instant::now();
            g.cameras = cams;
            g.audio_inputs = audios;
            g.last_stderr = stderr;
            g.refreshing = false;
        }
    });
}

/// The subset of `cached_camera_list()` entries that are screens, not
/// cameras. On macOS AVFoundation represents screens as video devices
/// named "Capture screen N" — we filter those by name. On Linux we
/// synthesise one entry per display from `crate::display::enumerate_displays()`.
/// On Windows we return a single "Desktop" entry for now (gdigrab
/// captures the virtual desktop; multi-monitor selection is a follow‑up).
pub fn list_screens() -> Vec<(u32, String)> {
    #[cfg(target_os = "macos")]
    {
        cached_camera_list()
            .into_iter()
            .filter(|(_, name)| {
                let lower = name.to_lowercase();
                lower.contains("capture screen") || lower.starts_with("screen")
            })
            .collect()
    }
    #[cfg(target_os = "linux")]
    {
        crate::display::enumerate_displays()
            .into_iter()
            .enumerate()
            .map(|(i, d)| (i as u32, d.name))
            .collect()
    }
    #[cfg(target_os = "windows")]
    { vec![(0, "Desktop".into())] }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    { Vec::new() }
}

// ── Camera Node ──────────────────────────────────────────────────────────────

pub fn render_camera(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _connections: &[Connection],
) {
    let (device_index, res_w, res_h, active, current_frame, status) = match node_type {
        NodeType::Camera { device_index, res_w, res_h, active, current_frame, status } =>
            (device_index, res_w, res_h, active, current_frame, status),
        _ => return,
    };

    // Device selector
    let cameras = cached_camera_list();
    ui.horizontal(|ui| {
        ui.label("Device:");
        egui::ComboBox::from_id_salt(egui::Id::new(("cam_device", node_id)))
            .selected_text(
                cameras.iter().find(|(i, _)| *i == *device_index)
                    .map(|(_, name)| name.as_str())
                    .unwrap_or("Select...")
            )
            .width(150.0)
            .show_ui(ui, |ui| {
                for (idx, name) in &cameras {
                    if ui.selectable_label(*device_index == *idx, name).clicked() && *device_index != *idx {
                        // Switching device — tear down the old decoder, drop
                        // the cached frame so downstream nodes can't see it
                        // for a single frame, and invalidate any GPU textures
                        // keyed by this node id. Restart capture if the
                        // camera was already running.
                        let was_active = *active;
                        VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
                        VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
                        *current_frame = None;
                        crate::gpu_image::request_node_invalidation(node_id);
                        *device_index = *idx;
                        if was_active {
                            match VideoDecoder::open_camera(*device_index, *res_w, *res_h) {
                                Ok(dec) => {
                                    VIDEO_DECODERS.with(|d| d.borrow_mut().insert(node_id, dec));
                                    *active = true;
                                    *status = "Capturing".into();
                                }
                                Err(e) => {
                                    *active = false;
                                    *status = e;
                                }
                            }
                        } else {
                            *status = "Stopped".into();
                        }
                    }
                }
            });
    });

    // Resolution
    ui.horizontal(|ui| {
        ui.label("Res:");
        ui.add(egui::DragValue::new(res_w).range(160..=1920).speed(10).prefix("W:"));
        ui.add(egui::DragValue::new(res_h).range(120..=1080).speed(10).prefix("H:"));
    });

    // Start / Stop
    ui.horizontal(|ui| {
        if *active {
            if ui.button("⏹ Stop").clicked() {
                *active = false;
                VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
                VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
                *current_frame = None;
                crate::gpu_image::request_node_invalidation(node_id);
                *status = "Stopped".into();
            }
            ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "● REC");
        } else {
            if ui.button("▶ Start").clicked() {
                match VideoDecoder::open_camera(*device_index, *res_w, *res_h) {
                    Ok(dec) => {
                        VIDEO_DECODERS.with(|d| d.borrow_mut().insert(node_id, dec));
                        *active = true;
                        *status = "Capturing".into();
                    }
                    Err(e) => *status = e,
                }
            }
        }
    });

    // Status
    if !status.is_empty() {
        let color = if status.contains("Error") || status.contains("Failed") {
            egui::Color32::from_rgb(255, 100, 100)
        } else if *active {
            egui::Color32::from_rgb(80, 200, 80)
        } else {
            egui::Color32::from_rgb(150, 150, 150)
        };
        ui.colored_label(color, egui::RichText::new(&*status).small());
    }

    // Receive frame — detect camera/source disconnect.
    //
    // Mirrors `VideoInNode::poll_decoder` (src/nodes/video_in_node.rs):
    // drain ffmpeg's stderr each tick and, if frames never start
    // flowing, surface either ffmpeg's last complaint (after a 1.5 s
    // grace for the normal pixel-format-negotiation chatter) or a
    // generic "camera busy / TCC" hint after 3 s. Without this the
    // node sits on green "Capturing" forever when ffmpeg wedges inside
    // AVFoundation — typical when a previous capture child is still
    // holding the device lock.
    VIDEO_DECODERS.with(|d| {
        if let Some(decoder) = d.borrow_mut().get_mut(&node_id) {
            decoder.pump_stderr();
            if let Some(frame) = decoder.try_recv_frame() {
                *current_frame = Some(frame);
                if status != "Capturing" { *status = "Capturing".into(); }
            } else if current_frame.is_none() {
                let elapsed = decoder.started_at.elapsed().as_secs_f32();
                if elapsed > 1.5 && !decoder.stderr_log.is_empty() {
                    let last = decoder.stderr_log.lines().last().unwrap_or("").to_string();
                    *status = format!("ffmpeg: {}", last);
                } else if elapsed > 3.0 {
                    *status =
                        "No frames — camera busy, or check System Settings → Privacy → Camera"
                            .into();
                }
            }
            if decoder.disconnected {
                *current_frame = None;
                *status = "Disconnected".to_string();
                *active = false;
                crate::system_log::warn(format!("Camera disconnected (id:{})", node_id));
                decoder.disconnected = false; // only log once
            }
        }
    });

    // Preview — downsample + upload only when the frame actually changed.
    // UI repaints at ~60 Hz but the camera produces at ~30 Hz, so without
    // the Arc-ptr cache we were redoing this work (a 270 KB GPU upload +
    // nearest-neighbour downsample) roughly twice per real frame.
    if let Some(frame) = current_frame.as_ref() {
        let max_w = ui.available_width().min(300.0);
        let aspect = frame.height as f32 / frame.width as f32;
        let preview_h = max_w * aspect;
        let frame_ptr = std::sync::Arc::as_ptr(frame) as usize;

        VIDEO_TEXTURES.with(|textures| {
            let mut textures = textures.borrow_mut();
            let cached_same = textures.get(&node_id).map(|(_, p)| *p == frame_ptr).unwrap_or(false);
            if !cached_same {
                let pw = max_w as u32;
                let ph = preview_h as u32;
                let preview_pixels = fast_downsample(frame, pw, ph);
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [pw as usize, ph as usize],
                    &preview_pixels,
                );
                match textures.get_mut(&node_id) {
                    Some(entry) => {
                        entry.0.set(color_image, egui::TextureOptions::LINEAR);
                        entry.1 = frame_ptr;
                    }
                    None => {
                        let tex = ui.ctx().load_texture(
                            format!("cam_{}", node_id),
                            color_image,
                            egui::TextureOptions::LINEAR,
                        );
                        textures.insert(node_id, (tex, frame_ptr));
                    }
                }
            }
            if let Some((tex, _)) = textures.get(&node_id) {
                ui.image(egui::load::SizedTexture::new(tex.id(), egui::vec2(max_w, preview_h)));
            }
        });
    }

    if *active {
        ui.ctx().request_repaint();
    }
}

/// Fast nearest-neighbor downsample for preview display
pub fn fast_downsample(img: &ImageData, target_w: u32, target_h: u32) -> Vec<u8> {
    if target_w == 0 || target_h == 0 { return vec![]; }

    // NOTE: an earlier fast path returned `img.pixels.clone()` directly
    // when `target_w >= img.width && target_h >= img.height` to skip
    // the inner loop. That's a bug — callers expect a buffer sized
    // `target_w * target_h * 4`, not `img.w * img.h * 4`. It crashed
    // `ColorImage::from_rgba_unmultiplied` with a left/right byte-count
    // mismatch any time a Transform node (or upstream processing) shrank
    // the image below the preview target. The downsample loop below
    // handles both downscale and upscale (nearest-neighbour; blocky but
    // correct) — removing the fast path fixes the crash with no measurable
    // perf hit since the real hot case is downscaling Camera 1920×1080
    // → preview 300×169.

    // u32-granular copy with a precomputed source-column map. Inner loop
    // becomes `dst[i] = src[col_map[i]]` — no per-pixel float math, no
    // 4-byte `copy_from_slice`, no bounds double-check. This runs on
    // every camera preview frame, so the old per-byte loop was a real
    // ~307k-iteration-per-frame overhead on debug builds.
    let out_bytes = (target_w * target_h * 4) as usize;
    let mut out: Vec<u8> = Vec::with_capacity(out_bytes);
    // SAFETY: every output pixel is written below; the u32-store loop
    // fills all `target_w * target_h` u32s.
    unsafe { out.set_len(out_bytes); }

    let x_ratio = img.width as f32 / target_w as f32;
    let y_ratio = img.height as f32 / target_h as f32;
    let w_in = img.width as usize;
    let tw = target_w as usize;

    // Precompute src column for each dst column.
    let mut col_map: Vec<usize> = Vec::with_capacity(tw);
    for x in 0..target_w {
        let sx = ((x as f32) * x_ratio) as u32 as usize;
        col_map.push(sx.min(w_in.saturating_sub(1)));
    }

    let src: &[u32] = bytemuck::cast_slice(&img.pixels);
    let dst: &mut [u32] = bytemuck::cast_slice_mut(&mut out);

    for y in 0..target_h {
        let sy = ((y as f32) * y_ratio) as u32 as usize;
        let sy = sy.min((img.height as usize).saturating_sub(1));
        let src_row_start = sy * w_in;
        let dst_row_start = y as usize * tw;
        for (dx, &sx) in col_map.iter().enumerate() {
            dst[dst_row_start + dx] = src[src_row_start + sx];
        }
    }
    out
}

/// Cleanup decoder and texture when node is deleted
pub fn cleanup_node(node_id: NodeId) {
    VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
    VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
    // Also drop GPU cache entries for this node so VRAM is released
    // immediately rather than waiting for the frame-LRU sweep.
    crate::gpu_image::request_node_invalidation(node_id);
}
