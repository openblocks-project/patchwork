//! Video In — unified video source node.
//!
//! Source type picked via a dropdown (Camera / Screen / Video File /
//! Syphon / NDI / Spout). Phase 0 ships with Camera working; all other
//! sources appear in the dropdown but are stubbed — they'll land in
//! Phase 1 (Screen, File) and Phase 2+ (Syphon, NDI, Spout) per
//! `docs/video_io_spec.md`.
//!
//! This node is intentionally a sibling to the legacy `NodeType::Camera`
//! enum variant (`src/nodes/video_player.rs::render_camera`). The old
//! Camera stays on the palette and is fully functional until Phase 1
//! deprecates it.

use crate::graph::{ImageData, NodeId, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::video_player::{
    cached_camera_list, fast_downsample, list_screens, refresh_camera_list_now, VideoDecoder,
};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Source‑type dropdown for `VideoInNode`. Phase 0 only implements
/// `Camera`; Phase 1+ fills in the rest per the spec.
///
/// `Hash` is needed so `(VideoSource, u32)` works as a `HashMap` key
/// in the `CAMERA_OWNERS` registry (prevents two Video Ins from
/// fighting for the same AVFoundation device).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum VideoSource {
    #[default]
    Camera,
    Screen,
    File,
    Syphon,
    Ndi,
    Spout,
    /// Network stream — a direct media URL that ffmpeg can open
    /// (mp4 / m3u8 / mpd / rtsp / rtmp / etc.) OR a YouTube / Vimeo
    /// page URL that gets routed through `yt-dlp` to extract the
    /// real stream URL before ffmpeg sees it.
    Url,
}

impl VideoSource {
    fn label(self) -> &'static str {
        match self {
            Self::Camera => "Camera",
            Self::Screen => "Screen",
            Self::File => "Video File",
            Self::Syphon => "Syphon (mac)",
            Self::Ndi => "NDI",
            Self::Spout => "Spout (win)",
            Self::Url => "URL",
        }
    }
    /// Whether this source is wired up in the current build + env.
    /// Phase 1 added Screen. Phase 2 added Syphon on macOS. Phase 3
    /// adds NDI — availability depends on `ndi::library()` because the
    /// NDI Runtime is user-installed (EULA, can't bundle). URL arrived
    /// in Phase 4 and is always available where ffmpeg is installed
    /// (which is the same prerequisite as Camera / Screen / File). File /
    /// Spout arrive in later phases and show up disabled with a
    /// "(soon)" tag.
    fn available(self) -> bool {
        match self {
            Self::Camera | Self::Screen | Self::File | Self::Url => true,
            #[cfg(target_os = "macos")]
            Self::Syphon => true,
            #[cfg(target_os = "macos")]
            Self::Ndi => crate::video_io::ndi::library().is_ok(),
            _ => false,
        }
    }
    fn all() -> [Self; 7] {
        [Self::Camera, Self::Screen, Self::File, Self::Url, Self::Syphon, Self::Ndi, Self::Spout]
    }
}

fn default_res_w() -> u32 { 640 }
fn default_res_h() -> u32 { 480 }
fn default_volume() -> f32 { 1.0 }

#[derive(Serialize, Deserialize)]
pub struct VideoInNode {
    #[serde(default)] pub source: VideoSource,

    /// Encodes source‑specific identity. Camera → device index as a
    /// string ("0", "1", …). File → absolute path. Screen → display
    /// index. Syphon/NDI/Spout → publish name. Single field so every
    /// source type serialises the same way.
    #[serde(default)] pub device_id: String,

    #[serde(default = "default_res_w")] pub res_w: u32,
    #[serde(default = "default_res_h")] pub res_h: u32,

    /// Mirror the frame horizontally before emit. Persisted per-project.
    /// macOS front cameras return a mirrored stream by default (selfie
    /// convention), so toggling this on un-mirrors the downstream feed
    /// for Syphon / Visual Output / any consumer. Preview also mirrors.
    #[serde(default)] pub mirror_h: bool,
    /// Mirror the frame vertically before emit. Rarely needed — usually
    /// only on camera mounts that ship the signal upside-down.
    #[serde(default)] pub mirror_v: bool,

    /// Reflects whether the decoder is currently open. Deliberately not
    /// restored from project files — user presses Start to capture.
    #[serde(skip)] pub active: bool,

    #[serde(skip)] pub status: String,

    /// Resolved URL for the URL source. For direct URLs this is just a
    /// copy of `device_id`. For YouTube / Vimeo URLs this is the real
    /// stream URL extracted by `yt-dlp` — typically temporary (expires
    /// in a few hours). Cleared on Stop. Never persisted: the user-typed
    /// page URL stays in `device_id` so save/load round-trips correctly.
    #[serde(skip)] pub effective_url: String,

    /// When the URL source EOFs (finite mp4 / podcast / a YouTube video
    /// that ended), should we transparently re-open and play it again?
    /// No-op for live streams (HLS / RTSP — those don't EOF). Persisted.
    #[serde(default)] pub loop_playback: bool,

    /// Audio volume for the URL stream's audio output, 0.0–2.0.
    /// Written to the FilePlayerProcessor's volume param atomic each
    /// frame so the slider feels responsive. Persisted.
    #[serde(default = "default_volume")] pub volume: f32,

    /// Path to a cached local copy of the URL's media, **relative** to the
    /// project folder (e.g. "assets/video_in/<id>.mp4"). Empty when not
    /// cached. Persisted; survives save/load alongside the project.
    #[serde(default)] pub cached_path: String,

    /// Resolved absolute path of `cached_path` against the current project
    /// directory. Set on first render after load (and on Cache-success);
    /// cleared on Stop / source change. When the file at this path exists,
    /// it takes precedence over `effective_url` / `device_id` — no network
    /// fetch, no yt-dlp.
    #[serde(skip)] pub cached_abs: Option<std::path::PathBuf>,

    // ── Playhead / scrubber state ───────────────────────────────────
    /// Source duration in seconds. 0.0 means unknown / live (no fixed
    /// length, scrubber hidden). Probed via ffprobe at file pick / URL
    /// resolve. Persisted so projects re-open with the cached value
    /// (saves a probe on Start), but re-probed on the actual Start to
    /// catch length changes.
    #[serde(default)] pub duration_secs: f32,

    /// `Instant` that corresponds to the playhead being at
    /// `position_anchor_secs`. `None` while paused / stopped — in those
    /// states we read `paused_at_secs` instead. Set on Start, on Resume,
    /// and on every Seek so live playhead = anchor + (now - anchor_at).
    #[serde(skip)] pub position_anchor_instant: Option<std::time::Instant>,
    #[serde(skip)] pub position_anchor_secs: f32,

    /// Captured playhead while paused. None when actively playing.
    #[serde(skip)] pub paused_at_secs: Option<f32>,

    /// User's current scrub-drag intent. Read by the slider's value
    /// during a drag (so the thumb tracks the cursor smoothly without
    /// the live-position calculation overwriting it). Committed to a
    /// real seek on `drag_stopped()`.
    #[serde(skip)] pub scrub_value: f32,
    #[serde(skip)] pub scrubbing: bool,

    // ── Runtime state (never serialised) ────────────────────────────
    #[serde(skip)] pub current_frame: Option<Arc<ImageData>>,

    /// The ffmpeg decoder. `Arc<Mutex<…>>` gets us `Send + Sync`
    /// without forcing `VideoDecoder` to be Sync (its `Child` isn't).
    /// `Drop` on the Box<dyn NodeBehavior> drops this Arc → drops the
    /// last strong ref → drops the decoder → kills the ffmpeg child.
    #[serde(skip)] decoder: Arc<Mutex<Option<VideoDecoder>>>,

    /// Preview texture + last uploaded Arc pointer. Mirrors the
    /// `VIDEO_TEXTURES` cache inside `render_camera` so we don't
    /// re‑upload an unchanged frame on every repaint.
    #[serde(skip)] preview_tex: Option<(egui::TextureHandle, usize)>,

    /// Syphon client — connected when `source == Syphon` and the user
    /// has picked a server. `Arc<Mutex<…>>` so the node stays
    /// `Send + Sync`; SyphonClient itself is `Send` but `!Sync`.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    syphon_client: Arc<Mutex<Option<crate::video_io::syphon::SyphonClient>>>,

    /// Last known dimensions of the incoming Syphon texture. Used for
    /// status UI; the actual texture lives in the GPU cache. Public
    /// because `app/mod.rs` reads it to emit `PortValue::GpuImage`.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    pub syphon_last_dims: Option<(u32, u32)>,

    /// Display-only cache of discovered Syphon servers, refreshed on a
    /// ~1 s TTL. Calling `syphon::servers()` every frame at 60 Hz was
    /// a measurable source of CFString leaks (M6 soak) — capping call
    /// frequency to 1 Hz cuts that by ~60×. `(app_name, name)` is all
    /// the UI needs; the full `SyphonServerInfo` (with its Retained
    /// NSDictionary) is re-queried only at connect time.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    syphon_servers_cache: Option<(std::time::Instant, Vec<(String, String)>)>,

    // ── NDI source state (Phase 3) ──────────────────────────────────
    //
    // The NDI story parallels Syphon — a discovery handle (`NdiFinder`),
    // a cache of discovered sources, and a background-thread-driven
    // receiver that funnels frames into `current_frame`. Unlike Syphon,
    // NDI traffics CPU pixels (BGRA bytes), so there's no GPU handoff
    // on receive — the frame lands in `current_frame` like Camera's
    // output and the existing app/mod.rs arm uploads it to the GPU
    // cache for downstream GPU consumers.
    /// NDI finder instance. Created lazily on the first `render_ndi_ui`
    /// tick when the NDI source is selected; dropped when the user
    /// switches sources or removes the node. Keeping it alive for the
    /// whole source-picker lifetime lets NDI's internal mDNS directory
    /// populate (the first few seconds after creation return 0 sources
    /// as the network announces converge).
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    ndi_finder: Arc<Mutex<Option<crate::video_io::ndi::NdiFinder>>>,

    /// 1 s-TTL snapshot of discovered NDI source names, for the UI
    /// dropdown. Same performance rationale as `syphon_servers_cache` —
    /// we don't need 60 Hz refresh for what's effectively a slowly-
    /// changing directory.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    ndi_sources_cache: Option<(std::time::Instant, Vec<crate::video_io::ndi::NdiSourceInfo>)>,

    /// Worker thread handle + shared frame slot + stop flag. `None`
    /// when not receiving; `Some(...)` for the duration of a capture
    /// session. `Drop` on this struct (via `close_decoder`) flips the
    /// stop flag and joins the thread so no zombies survive node
    /// removal / Stop click.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    ndi_worker: Option<NdiWorker>,

    /// Last dimensions pulled off an NDI frame — purely for the "WxH"
    /// label in the UI status row. Updated on every successful
    /// capture; cleared on Stop.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    pub ndi_last_dims: Option<(u32, u32)>,

    /// `Instant` of the most-recent successful NDI frame. Used by the
    /// UI to tell the user when a Receiving chain has gone stale —
    /// e.g. the sender on the other end was killed mid-stream. The
    /// worker thread deliberately stays alive through source
    /// disappearances so it auto-reconnects if the sender comes back,
    /// but without this we'd leave a green "● RX / Receiving" label
    /// up forever against a frozen preview. Cleared by `close_decoder`.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    ndi_last_frame_at: Option<std::time::Instant>,
}

/// Handle on the per-node receiver thread. Kept inside `VideoInNode`
/// as `Option<NdiWorker>`.
#[cfg(target_os = "macos")]
struct NdiWorker {
    /// Flip to `true` to ask the worker thread to exit at its next
    /// capture-timeout boundary (~100 ms latency worst-case).
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// Most-recent frame the worker swizzled out of NDI. Main thread
    /// drains this in `poll_ndi_receiver` and moves it into
    /// `current_frame`.
    latest: Arc<Mutex<Option<Arc<ImageData>>>>,
    /// JoinHandle — kept so `Drop::drop` can wait on the thread.
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "macos")]
impl Drop for NdiWorker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.thread.take() {
            // Join timeout-free; worst case ~100 ms because the worker
            // loop checks the flag between capture_video calls and
            // each call has a 100 ms timeout.
            let _ = h.join();
        }
    }
}

impl Clone for VideoInNode {
    /// Cloned copy starts cold: new decoder slot, no cached frame,
    /// no preview texture. Matches how DynNode's own Clone works
    /// (serialise → deserialise, so `#[serde(skip)]` fields default).
    fn clone(&self) -> Self {
        Self {
            source: self.source,
            device_id: self.device_id.clone(),
            res_w: self.res_w,
            res_h: self.res_h,
            mirror_h: self.mirror_h,
            mirror_v: self.mirror_v,
            active: false,
            status: String::new(),
            current_frame: None,
            decoder: Arc::new(Mutex::new(None)),
            preview_tex: None,
            #[cfg(target_os = "macos")]
            syphon_client: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            syphon_last_dims: None,
            #[cfg(target_os = "macos")]
            syphon_servers_cache: None,
            #[cfg(target_os = "macos")]
            ndi_finder: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            ndi_sources_cache: None,
            #[cfg(target_os = "macos")]
            ndi_worker: None,
            #[cfg(target_os = "macos")]
            ndi_last_dims: None,
            #[cfg(target_os = "macos")]
            ndi_last_frame_at: None,
            effective_url: String::new(),
            loop_playback: self.loop_playback,
            volume: self.volume,
            cached_path: self.cached_path.clone(),
            cached_abs: None,
            duration_secs: self.duration_secs,
            position_anchor_instant: None,
            position_anchor_secs: 0.0,
            paused_at_secs: None,
            scrub_value: 0.0,
            scrubbing: false,
        }
    }
}

impl Default for VideoInNode {
    fn default() -> Self {
        Self {
            source: VideoSource::Camera,
            device_id: "0".into(),
            res_w: default_res_w(),
            res_h: default_res_h(),
            mirror_h: false,
            mirror_v: false,
            active: false,
            status: String::new(),
            current_frame: None,
            decoder: Arc::new(Mutex::new(None)),
            preview_tex: None,
            #[cfg(target_os = "macos")]
            syphon_client: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            syphon_last_dims: None,
            #[cfg(target_os = "macos")]
            syphon_servers_cache: None,
            #[cfg(target_os = "macos")]
            ndi_finder: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            ndi_sources_cache: None,
            #[cfg(target_os = "macos")]
            ndi_worker: None,
            #[cfg(target_os = "macos")]
            ndi_last_dims: None,
            #[cfg(target_os = "macos")]
            ndi_last_frame_at: None,
            effective_url: String::new(),
            loop_playback: false,
            volume: default_volume(),
            cached_path: String::new(),
            cached_abs: None,
            duration_secs: 0.0,
            position_anchor_instant: None,
            position_anchor_secs: 0.0,
            paused_at_secs: None,
            scrub_value: 0.0,
            scrubbing: false,
        }
    }
}

/// Process-wide registry of which `VideoInNode` owns which AVFoundation
/// / v4l2 camera device. macOS's AVFoundation (and most Linux v4l2
/// devices) only let one process at a time have exclusive access to a
/// camera; if two `VideoInNode`s both spawn ffmpeg against device 0,
/// the second one either silently fails or preempts the first,
/// leaving the user with "only the latest one turned on works" and
/// intermittent glitches from both ffmpegs racing for frames.
///
/// We detect the conflict at `open_decoder` time and refuse the
/// second opener, pointing the user at the correct graph pattern:
/// wire the first Video In's Frame output into multiple downstream
/// consumers rather than duplicating the Video In. Symmetric with
/// how Audio In, MIDI In, etc. handle device ownership in PatchWork.
///
/// Keyed by `(source_kind, device_index)` so Screen device 0 and
/// Camera device 0 don't conflict (different AVFoundation
/// subsystems). NDI / Syphon don't use this registry — they're
/// publish-subscribe and support any number of concurrent receivers.
static CAMERA_OWNERS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<(VideoSource, u32), NodeId>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

impl VideoInNode {
    fn device_index(&self) -> u32 {
        self.device_id.parse::<u32>().unwrap_or(0)
    }

    /// Attempt to claim ownership of the current (source, device_index)
    /// for `node_id`. Returns `Err(existing_owner)` if another node
    /// already has it. Only applies to Camera / Screen — network
    /// sources (NDI / Syphon) are shareable.
    fn claim_camera(&self, node_id: NodeId) -> Result<(), NodeId> {
        if !matches!(self.source, VideoSource::Camera | VideoSource::Screen) {
            return Ok(());
        }
        let key = (self.source, self.device_index());
        let mut owners = match CAMERA_OWNERS.lock() {
            Ok(o) => o,
            Err(_) => return Ok(()), // mutex poisoned — don't block the user
        };
        match owners.get(&key).copied() {
            Some(existing) if existing != node_id => Err(existing),
            _ => {
                owners.insert(key, node_id);
                Ok(())
            }
        }
    }

    /// Drop our claim on the device. Safe to call when we don't hold
    /// one; the entry only gets removed if it still points at us.
    fn release_camera(&self, node_id: NodeId) {
        if !matches!(self.source, VideoSource::Camera | VideoSource::Screen) {
            return;
        }
        let key = (self.source, self.device_index());
        if let Ok(mut owners) = CAMERA_OWNERS.lock() {
            if owners.get(&key).copied() == Some(node_id) {
                owners.remove(&key);
            }
        }
    }

    fn open_decoder(&mut self, node_id: NodeId, start_time: f32) {
        // Enforce single-owner-per-device before spawning ffmpeg. Better
        // to fail cleanly here with a clear message than to let the
        // second ffmpeg race the first, silently stealing the camera
        // and causing "only latest works" + glitch complaints.
        if let Err(existing) = self.claim_camera(node_id) {
            self.active = false;
            self.status = format!(
                "Device in use by Video In #{existing}. Wire that node's \
                 output into multiple downstream nodes instead of adding \
                 a second Video In for the same camera."
            );
            return;
        }

        let result = match self.source {
            VideoSource::Camera => {
                VideoDecoder::open_camera(self.device_index(), self.res_w, self.res_h)
            }
            VideoSource::Screen => {
                // Reuses the camera pathway on macOS (AVFoundation treats
                // screens as video devices). On Linux/Windows the
                // open_screen helper picks the right ffmpeg input format.
                // Origin (0,0) means capture the whole desktop for
                // x11grab; ignored on macOS/Windows.
                VideoDecoder::open_screen(self.device_index(), self.res_w, self.res_h, 0, 0)
            }
            VideoSource::Url => {
                // Precedence:
                //   1. cached_abs (local file copy) — fastest, survives
                //      yt-dlp expiry, makes the project portable.
                //   2. effective_url (direct URL or yt-dlp resolved).
                //   3. device_id (the raw user-pasted URL — fallback).
                let from_cache = self.cached_abs.as_ref()
                    .filter(|p| p.exists())
                    .map(|p| p.to_string_lossy().into_owned());
                let url = if let Some(p) = from_cache {
                    p
                } else if !self.effective_url.is_empty() {
                    self.effective_url.clone()
                } else {
                    self.device_id.trim().to_string()
                };
                if url.is_empty() {
                    Err("URL is empty".into())
                } else {
                    VideoDecoder::open_url(&url, self.res_w, self.res_h, start_time)
                }
            }
            VideoSource::File => {
                // device_id holds the absolute file path (set by the
                // file picker in render_file_ui).
                let path = self.device_id.trim();
                if path.is_empty() {
                    Err("No file selected".into())
                } else {
                    VideoDecoder::open_file(path, self.res_w, self.res_h, start_time)
                }
            }
            _ => Err(format!("{} source not yet implemented", self.source.label())),
        };
        match result {
            Ok(dec) => {
                if let Ok(mut guard) = self.decoder.lock() { *guard = Some(dec); }
                self.active = true;
                self.status = "Capturing".into();
            }
            Err(e) => {
                // Release the claim so the user can retry against a
                // different device without being gated by a ghost entry.
                self.release_camera(node_id);
                self.active = false;
                self.status = e;
            }
        }
    }

    fn close_decoder(&mut self, node_id: NodeId) {
        // Release the camera-ownership claim before tearing the
        // decoder down, so a second Video In can reclaim the device
        // on the very next frame. Idempotent if we never held it.
        self.release_camera(node_id);
        if let Ok(mut guard) = self.decoder.lock() { *guard = None; }
        #[cfg(target_os = "macos")]
        {
            // Drop any Syphon client too. SyphonClient::Drop sends
            // `-stop` which unsubscribes cleanly so the server knows
            // we're gone.
            if let Ok(mut g) = self.syphon_client.lock() { *g = None; }
            self.syphon_last_dims = None;

            // NDI: drop the worker first (its `Drop` joins the thread
            // and flips the stop flag), then drop the finder.
            self.ndi_worker = None;
            if let Ok(mut g) = self.ndi_finder.lock() { *g = None; }
            self.ndi_last_dims = None;
            self.ndi_last_frame_at = None;
        }
        self.current_frame = None;
        self.preview_tex = None;
        self.active = false;
        self.status = "Stopped".into();
        // Drop the resolved URL so a Restart re-resolves YouTube / Vimeo
        // (those URLs are signed and expire). For direct URLs the next
        // Start just re-copies device_id into effective_url anyway.
        self.effective_url.clear();
        // Release VRAM now instead of waiting for the frame-LRU sweep.
        crate::gpu_image::request_node_invalidation(node_id);
    }

    fn poll_decoder(&mut self, node_id: NodeId, ctx: Option<&mut RenderContext>) {
        let mut guard = match self.decoder.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Some(decoder) = guard.as_mut() else { return; };
        // Drain any ffmpeg stderr into the decoder's log so we can
        // surface a meaningful error if frames never start flowing.
        decoder.pump_stderr();
        if let Some(frame) = decoder.try_recv_frame() {
            // Apply optional mirror before storing. Zero cost when both
            // toggles are off; ~0.5 ms at 640×480 when either is on.
            self.current_frame = Some(if self.mirror_h || self.mirror_v {
                mirror_frame(&frame, self.mirror_h, self.mirror_v)
            } else {
                frame
            });
            // Got a frame — clear any startup/timeout message.
            if self.status != "Capturing" {
                self.status = "Capturing".into();
            }
        } else if self.current_frame.is_none() {
            // No frames yet. ffmpeg often prints a transient format-
            // negotiation warning during the first second (e.g.
            // "Selected pixel format (yuv420p) is not supported...")
            // before retrying successfully — so only surface stderr
            // after a grace period, and only the last line (most
            // recent complaint is more informative than the first).
            // After 3 s with still no frame and no stderr, assume TCC
            // permission and point the user at System Settings.
            let elapsed = decoder.started_at.elapsed().as_secs_f32();
            if elapsed > 1.5 && !decoder.stderr_log.is_empty() {
                let last_line = decoder
                    .stderr_log
                    .lines()
                    .last()
                    .unwrap_or("")
                    .to_string();
                self.status = format!("ffmpeg: {}", last_line);
            } else if elapsed > 3.0 {
                self.status =
                    "No frames — check System Settings → Privacy → Camera".into();
            }
        }
        if decoder.disconnected {
            // For File / URL streams with Loop on, an EOF means "play
            // it again" — drop the dead decoder, mark for restart on
            // the next frame. The actual `open_decoder` call has to
            // happen outside this borrow scope (we're holding the
            // decoder lock guard here), so we set a transient flag and
            // restart below the unlock.
            let want_restart = self.loop_playback
                && matches!(self.source, VideoSource::Url | VideoSource::File);
            self.current_frame = None;
            self.active = false;
            decoder.disconnected = false;
            *guard = None; // free the slot so next Start spawns fresh
            drop(guard);
            crate::gpu_image::request_node_invalidation(node_id);
            if want_restart {
                self.status = "Looping".into();
                self.open_decoder(node_id, 0.0);
                // Audio restart — only possible when called from a
                // render_*_ui (which passes ctx). poll_decoder is also
                // called from elsewhere; in those cases the audio
                // restart waits for the next render frame.
                if let Some(ctx) = ctx {
                    ctx.audio.stop_file(node_id);
                    let source = self.effective_input();
                    if !source.is_empty() {
                        let _ = ctx.audio.play_pipe_audio(node_id, &source, 0.0);
                        ctx.audio.engine_write_param(node_id, 0, self.volume);
                    }
                }
                self.anchor_position(0.0);
            } else {
                self.status = "Disconnected".into();
                crate::system_log::warn(format!("Video In disconnected (id:{})", node_id));
            }
        }
    }
}

impl NodeBehavior for VideoInNode {
    fn title(&self) -> &str { "Video In" }
    fn inputs(&self) -> Vec<PortDef> { vec![] }
    fn outputs(&self) -> Vec<PortDef> {
        // Audio port is always present — for non-URL sources it just stays
        // disconnected (Camera / Screen / Syphon / NDI don't carry audio
        // in v1; that's a follow-up). Wiring it for URL streams kicks off
        // a parallel audio-only ffmpeg subprocess via `play_url_audio`.
        vec![
            PortDef::new("Frame", PortKind::Image),
            PortDef::new("Audio", PortKind::Audio),
        ]
    }
    fn color_hint(&self) -> [u8; 3] { [80, 200, 140] }
    fn type_tag(&self) -> &str { "video_in" }
    /// The Frame output port is rendered inline via `output_port_row`
    /// inside `render_with_ctx`, so the framework must skip its default
    /// bottom-port rendering — otherwise the port shows twice and only
    /// the framework one is wired up.
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        if let Some(frame) = self.current_frame.as_ref() {
            vec![(0, PortValue::Image(frame.clone()))]
        } else {
            vec![]
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<VideoInNode>(state.clone()) {
            self.source = l.source;
            self.device_id = l.device_id;
            self.res_w = l.res_w;
            self.res_h = l.res_h;
            self.loop_playback = l.loop_playback;
            self.volume = l.volume;
            self.cached_path = l.cached_path;
            self.duration_secs = l.duration_secs;
            self.mirror_h = l.mirror_h;
            self.mirror_v = l.mirror_v;
            // `active` / `status` intentionally not restored — user clicks Start.
            // `cached_abs` resolves on first render once we have the project dir.
        }
    }

    fn on_removed(&mut self, node_id: NodeId) {
        self.close_decoder(node_id);
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // ── Source dropdown ────────────────────────────────────────
        let mut source_changed = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Source").small());
            let prev = self.source;
            egui::ComboBox::from_id_salt(egui::Id::new(("vin_src", ctx.node_id)))
                .selected_text(self.source.label())
                .width(140.0)
                .show_ui(ui, |ui| {
                    for s in VideoSource::all() {
                        // Unavailable-reason is source-specific: NDI
                        // has a 1:1 install story (libndi missing);
                        // everything else is "not yet implemented".
                        // The type annotation on `tooltip` is needed
                        // because on non-macOS the Some(...) arm below
                        // is cfg'd out, leaving only `None` with no
                        // inference target.
                        let (text, tooltip): (String, Option<&'static str>) = if s.available() {
                            (s.label().to_string(), None)
                        } else {
                            #[cfg(target_os = "macos")]
                            {
                                if matches!(s, VideoSource::Ndi) {
                                    (
                                        format!("{}  (install NDI Runtime)", s.label()),
                                        Some(
                                            "NDI Runtime not installed.\n\
                                             Install the free NDI Tools package\n\
                                             from https://ndi.video/tools/.",
                                        ),
                                    )
                                } else {
                                    (format!("{}  (soon)", s.label()), None)
                                }
                            }
                            #[cfg(not(target_os = "macos"))]
                            {
                                (format!("{}  (soon)", s.label()), None)
                            }
                        };
                        ui.add_enabled_ui(s.available(), |ui| {
                            let resp = ui.selectable_value(&mut self.source, s, text);
                            if let Some(t) = tooltip {
                                resp.on_hover_text(t);
                            }
                        });
                    }
                });
            if self.source != prev { source_changed = true; }
        });
        if source_changed {
            self.close_decoder(ctx.node_id);
            // Reset identity because device_id format differs per source.
            self.device_id = match self.source {
                VideoSource::Camera => "0".into(),
                _ => String::new(),
            };
        }

        ui.separator();

        // ── Source‑specific UI ─────────────────────────────────────
        match self.source {
            VideoSource::Camera => self.render_camera_ui(ui, ctx),
            VideoSource::Screen => self.render_screen_ui(ui, ctx),
            VideoSource::File => self.render_file_ui(ui, ctx),
            VideoSource::Url => self.render_url_ui(ui, ctx),
            #[cfg(target_os = "macos")]
            VideoSource::Syphon => self.render_syphon_ui(ui, ctx),
            #[cfg(target_os = "macos")]
            VideoSource::Ndi => self.render_ndi_ui(ui, ctx),
            _ => {
                ui.add_space(4.0);
                ui.colored_label(
                    dim,
                    egui::RichText::new(format!(
                        "{} source — coming in a later phase.\nSee docs/video_io_spec.md",
                        self.source.label()
                    )).small(),
                );
                ui.add_space(4.0);
            }
        }

        // ── Output port row (always visible) ───────────────────────
        ui.separator();
        let out_val = if self.current_frame.is_some() { "●" } else { "—" };
        crate::nodes::output_port_row(
            ui, "Frame", out_val, ctx.node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Image,
        );
        // Audio port — only meaningful for URL streams in v1, but always
        // shown so the visual port shape is stable across source switches.
        // For non-URL sources it stays disconnected at the engine level.
        let audio_active = self.active
            && matches!(self.source, VideoSource::Url)
            && ctx.audio.file_buffers.contains_key(&ctx.node_id);
        crate::nodes::output_port_row(
            ui, "Audio", if audio_active { "●" } else { "—" }, ctx.node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Audio,
        );

        if self.active { ui.ctx().request_repaint(); }
    }
}

impl VideoInNode {
    fn render_camera_ui(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let cameras = cached_camera_list();
        // Hide screen-capture devices from the Camera picker — they
        // belong under Source = Screen. Without this filter OBS / Camo
        // users end up with "Capture screen 0" in their camera list.
        let cameras: Vec<(u32, String)> = cameras
            .into_iter()
            .filter(|(_, name)| {
                let lower = name.to_lowercase();
                !(lower.contains("capture screen") || lower.starts_with("screen"))
            })
            .collect();
        let current_idx = self.device_index();

        // Device dropdown + Refresh
        let mut new_idx: Option<u32> = None;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Device").small());
            egui::ComboBox::from_id_salt(egui::Id::new(("vin_dev", ctx.node_id)))
                .selected_text(
                    cameras
                        .iter()
                        .find(|(i, _)| *i == current_idx)
                        .map(|(_, n)| n.as_str())
                        .unwrap_or("Select…"),
                )
                .width(170.0)
                .show_ui(ui, |ui| {
                    for (idx, name) in &cameras {
                        let selected = *idx == current_idx;
                        if ui.selectable_label(selected, name).clicked() && !selected {
                            new_idx = Some(*idx);
                        }
                    }
                });
            // "↻" — bypass the 10s TTL so OBS / Continuity / fresh USB
            // cams show up instantly after plugging in.
            if ui.small_button("↻").on_hover_text("Refresh device list").clicked() {
                refresh_camera_list_now();
            }
        });
        if let Some(idx) = new_idx {
            let was_active = self.active;
            self.close_decoder(ctx.node_id);
            self.device_id = idx.to_string();
            if was_active { self.open_decoder(ctx.node_id, 0.0); }
        }

        // Windows-only diagnostics: when ffmpeg's dshow enumeration
        // returns nothing (the historical pain point), show what it
        // actually said so the user can see the cause without a
        // rebuild. Hidden by default once any device is found.
        #[cfg(target_os = "windows")]
        {
            let audio_inputs = crate::nodes::video_player::cached_audio_input_list_dshow();
            if cameras.is_empty() || audio_inputs.is_empty() {
                let stderr = crate::nodes::video_player::last_dshow_stderr();
                let header = if cameras.is_empty() && audio_inputs.is_empty() {
                    "🪟 No cameras or mics enumerated"
                } else if cameras.is_empty() {
                    "🪟 No cameras enumerated"
                } else {
                    "🪟 No mics enumerated"
                };
                egui::CollapsingHeader::new(
                    egui::RichText::new(header)
                        .small()
                        .color(egui::Color32::from_rgb(255, 180, 80)),
                )
                .id_salt(egui::Id::new(("vin_dshow_diag", ctx.node_id)))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("ffmpeg said:").small());
                    let max = stderr.len().saturating_sub(1024);
                    let tail = if max > 0 { &stderr[max..] } else { stderr.as_str() };
                    let mut s = tail.to_string();
                    egui::ScrollArea::vertical()
                        .id_salt(egui::Id::new(("vin_dshow_stderr", ctx.node_id)))
                        .max_height(160.0)
                        .show(ui, |ui| {
                            ui.add(egui::TextEdit::multiline(&mut s)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace)
                                .interactive(false));
                        });
                    ui.label(egui::RichText::new("Try running this in cmd.exe:").small());
                    let cmd = "ffmpeg -list_devices true -f dshow -i dummy";
                    ui.horizontal(|ui| {
                        ui.code(cmd);
                        if ui.small_button("📋").on_hover_text("Copy command").clicked() {
                            ui.ctx().copy_text(cmd.to_string());
                        }
                    });
                });
            }
        }

        // Resolution
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Res").small());
            ui.add(egui::DragValue::new(&mut self.res_w).range(160..=1920).speed(10).prefix("W:"));
            ui.add(egui::DragValue::new(&mut self.res_h).range(120..=1080).speed(10).prefix("H:"));
        });

        // Mirror toggles — applied in `poll_decoder` before emit, so
        // preview + downstream (Visual Output, Syphon, …) all see the
        // mirrored frame. macOS front cameras return a mirrored stream
        // by default; flipping Mirror H here un-mirrors it for TD/OBS.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Mirror").small());
            ui.checkbox(&mut self.mirror_h, "H");
            ui.checkbox(&mut self.mirror_v, "V");
        });

        // Start / Stop
        let mut toggle_start = false;
        let mut toggle_stop = false;
        ui.horizontal(|ui| {
            if self.active {
                if ui.button("⏹ Stop").clicked() { toggle_stop = true; }
                ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "● REC");
            } else if ui.button("▶ Start").clicked() {
                toggle_start = true;
            }
        });
        if toggle_start {
            self.open_decoder(ctx.node_id, 0.0);
            // Camera mode previously skipped audio entirely (open_camera
            // uses `:none` for the audio device). Spawn a sibling ffmpeg
            // pulling the system default mic so wiring the Audio port
            // produces sound. Same buffer + processor pipeline as URL /
            // File audio — wiring the port AFTER Start still works
            // because `pending_connection_sync` picks up the new edge.
            //
            // macOS will prompt for mic permission on first open; that's
            // the OS-level safeguard. If the user denies, ffmpeg exits
            // and the pump thread sees disconnected → silence.
            // Picks the platform-appropriate ffmpeg capture format +
            // input string. The Windows arm enumerates dshow audio
            // devices via the cached list (filled by the same
            // ffmpeg -list_devices call as cameras) and picks the
            // first one — there's no UI for choosing yet, "first mic
            // found" matches the macOS `:0` / Linux `default` defaults.
            #[cfg(target_os = "macos")]
            let capture: Option<(&str, String)> = Some(("avfoundation", ":0".into()));
            #[cfg(target_os = "linux")]
            let capture: Option<(&str, String)> = Some(("alsa", "default".into()));
            #[cfg(target_os = "windows")]
            let capture: Option<(&str, String)> = {
                let inputs = crate::nodes::video_player::cached_audio_input_list_dshow();
                inputs.first()
                    .map(|(_, name)| ("dshow", format!("audio={}", name)))
            };
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            let capture: Option<(&str, String)> = None;
            if let Some((fmt, input)) = capture {
                if self.active {
                    if let Err(e) = ctx.audio.play_capture_audio(ctx.node_id, fmt, &input) {
                        crate::system_log::warn(format!("Camera audio: {}", e));
                    }
                    ctx.audio.engine_write_param(ctx.node_id, 0, self.volume);
                }
            } else if self.active {
                // Windows with no enumerated audio device — surface a
                // hint instead of silently giving up. The diagnostics
                // panel below shows ffmpeg's stderr for debugging.
                #[cfg(target_os = "windows")]
                {
                    self.status = "No audio input found — see diagnostics".into();
                }
            }
        }
        if toggle_stop {
            // Tear down audio first — close_decoder drops the camera
            // claim but doesn't touch the audio pipeline. Symmetric
            // with the URL/File close paths.
            ctx.audio.stop_file(ctx.node_id);
            self.close_decoder(ctx.node_id);
        }

        // Status line
        if !self.status.is_empty() {
            let color = if self.status.contains("Error") || self.status.contains("Failed") {
                egui::Color32::from_rgb(255, 100, 100)
            } else if self.active {
                egui::Color32::from_rgb(80, 200, 80)
            } else {
                egui::Color32::from_rgb(150, 150, 150)
            };
            ui.colored_label(color, egui::RichText::new(&self.status).small());
        }

        // Pull any new frames from the decoder thread
        self.poll_decoder(ctx.node_id, Some(ctx));

        // Preview (shared with Screen source)
        self.render_preview(ui, ctx);
    }
}

// ── URL source helpers ──────────────────────────────────────────────────────

/// Hosts that need URL extraction via `yt-dlp` before ffmpeg can ingest.
/// Cheap substring check — no need for full URL parsing.
fn needs_yt_dlp(url: &str) -> bool {
    let l = url.to_lowercase();
    l.contains("youtube.com") || l.contains("youtu.be") || l.contains("vimeo.com")
}

/// Pick a sanitised filename for caching the URL's media. YouTube and
/// Vimeo videos use their stable ID (so re-caching the same video twice
/// is idempotent); direct URLs use the last path segment, scrubbed of
/// anything non-alphanumeric. Falls back to "video.mp4" when nothing
/// usable can be extracted.
fn cache_filename(url: &str) -> String {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    if let Some(id) = extract_youtube_id(no_query) { return format!("{}.mp4", id); }
    if let Some(id) = extract_vimeo_id(no_query)   { return format!("{}.mp4", id); }
    let last = no_query.rsplit('/').next().unwrap_or("video");
    let sanitised: String = last.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .collect();
    if sanitised.is_empty() {
        "video.mp4".to_string()
    } else if !sanitised.contains('.') {
        format!("{}.mp4", sanitised)
    } else {
        sanitised
    }
}

fn extract_youtube_id(url: &str) -> Option<String> {
    let take_id = |rest: &str| -> Option<String> {
        let id: String = rest.chars().take(11)
            .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
            .collect();
        if id.len() == 11 { Some(id) } else { None }
    };
    if let Some(idx) = url.find("youtu.be/")  { return take_id(&url[idx + "youtu.be/".len()..]); }
    if let Some(idx) = url.find("v=")          { return take_id(&url[idx + 2..]); }
    if let Some(idx) = url.find("/embed/")     { return take_id(&url[idx + "/embed/".len()..]); }
    None
}

fn extract_vimeo_id(url: &str) -> Option<String> {
    let after_host = url.split("vimeo.com/").nth(1)?;
    let after_video = after_host.strip_prefix("video/").unwrap_or(after_host);
    let id: String = after_video.chars().take_while(|c| c.is_ascii_digit()).collect();
    if id.len() >= 5 { Some(id) } else { None }
}

/// Worker-thread entry: download `url` to `dest`. For YouTube/Vimeo URLs
/// we go through yt-dlp; for direct URLs ffmpeg's stream copy (`-c copy`)
/// gives us a byte-accurate file with no transcoding cost.
fn download_to_disk(url: &str, dest: &std::path::Path) -> Result<(), String> {
    // Make sure the parent dir exists — caller passes
    // <project>/assets/video_in/<file>, so create that tree.
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Create assets/video_in: {}", e))?;
    }
    let status = if needs_yt_dlp(url) {
        std::process::Command::new("yt-dlp")
            .args([
                "--no-playlist",
                "-f", "best[ext=mp4]/best",
                "-o",
            ])
            .arg(dest)
            .arg(url)
            .status()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "yt-dlp not found. `brew install yt-dlp` to enable YouTube/Vimeo cache.".to_string()
                } else { format!("yt-dlp launch failed: {}", e) }
            })?
    } else {
        std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error",
                "-y",                       // overwrite if exists
                "-i", url,
                "-c", "copy",               // byte-exact, no transcode
                "-bsf:a", "aac_adtstoasc",  // safety for HLS-AAC → mp4
            ])
            .arg(dest)
            .status()
            .map_err(|e| format!("ffmpeg launch failed: {}", e))?
    };
    if !status.success() {
        return Err(format!("download failed (exit {})", status.code().unwrap_or(-1)));
    }
    if !dest.exists() {
        return Err("download succeeded but no file written".into());
    }
    Ok(())
}

/// Run `yt-dlp -g` to resolve a page URL into a direct stream URL ffmpeg
/// can play. Blocking — call from a worker thread. The 30 s wait covers
/// network roundtrips even on slow connections; longer than that almost
/// always means yt-dlp is hung against an unsupported site.
fn run_yt_dlp(url: &str) -> Result<String, String> {
    let output = std::process::Command::new("yt-dlp")
        .args([
            "--no-playlist",                   // a single video, not the whole channel
            "-f", "best[ext=mp4]/best",
            "-g",                              // print resolved URL(s) to stdout
            url,
        ])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "yt-dlp not found. Install with `brew install yt-dlp` (mac) \
                 or `pip install yt-dlp`. Direct URLs still work without it."
                    .to_string()
            } else {
                format!("yt-dlp launch failed: {}", e)
            }
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.lines().last().unwrap_or("yt-dlp failed").to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let resolved = stdout.lines().next().unwrap_or("").trim().to_string();
    if resolved.is_empty() {
        Err("yt-dlp returned no URL".into())
    } else {
        Ok(resolved)
    }
}

impl VideoInNode {
    /// URL-source UI. Single text field for any HTTP/HLS/RTSP/RTMP URL or
    /// a YouTube / Vimeo page URL. The latter goes through `yt-dlp` in a
    /// worker thread so the UI doesn't freeze during resolution.
    fn render_url_ui(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let node_id = ctx.node_id;
        let pending_id        = egui::Id::new(("vin_url_pending",         node_id));
        let status_id         = egui::Id::new(("vin_url_status",          node_id));
        // Two keys instead of `Result<String, String>` because egui 0.31's
        // remove_temp requires `Default`, which Result doesn't implement.
        let resolved_ok_id    = egui::Id::new(("vin_url_resolved_ok",     node_id));
        let resolved_err_id   = egui::Id::new(("vin_url_resolved_err",    node_id));
        let cache_pending_id  = egui::Id::new(("vin_url_cache_pending",   node_id));
        let cache_done_id     = egui::Id::new(("vin_url_cache_done",      node_id));
        let cache_err_id      = egui::Id::new(("vin_url_cache_err",       node_id));

        // ── Lazy-resolve cached_abs from cached_path on first render ──
        // load_state can't see the project_dir; render_with_context can.
        // First time we render after load (or after a download), we
        // promote `cached_path` (relative) to `cached_abs` (absolute)
        // so the open_decoder precedence chain finds it.
        if !self.cached_path.is_empty() && self.cached_abs.is_none() {
            if let Some(pdir) = ctx.project_dir {
                let abs = pdir.join(&self.cached_path);
                if abs.exists() {
                    self.cached_abs = Some(abs);
                }
            }
        }

        // ── URL field ────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("URL").small());
            ui.add(
                egui::TextEdit::singleline(&mut self.device_id)
                    .hint_text("https://… or YouTube / Vimeo URL")
                    .desired_width(ui.available_width() - 6.0),
            );
        });

        // ── Resolution sliders (matches camera/screen) ───────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Size").small().color(dim));
            ui.add(egui::DragValue::new(&mut self.res_w).range(64..=3840).speed(8));
            ui.label(egui::RichText::new("×").small().color(dim));
            ui.add(egui::DragValue::new(&mut self.res_h).range(64..=2160).speed(8));
        });

        // ── Loop checkbox (URL-only feature) ─────────────────────────
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.loop_playback, "Loop")
              .on_hover_text(
                "When the source ends, restart it. No effect on live \
                 streams. For YouTube/Vimeo, the resolved URL may expire \
                 after a few hours — re-press Start in that case."
              );
        });

        // ── Drain cache-worker result if one arrived this frame ──────
        let cache_done: Option<String> = ui.ctx().data_mut(|d| d.remove_temp(cache_done_id));
        let cache_err:  Option<String> = ui.ctx().data_mut(|d| d.remove_temp(cache_err_id));
        if cache_done.is_some() || cache_err.is_some() {
            ui.ctx().data_mut(|d| d.remove_temp::<bool>(cache_pending_id));
        }
        if let Some(rel_path) = cache_done {
            // Worker wrote the file at <project>/<rel_path>. Promote it
            // to cached_abs so the next Start picks the local file.
            self.cached_path = rel_path.clone();
            if let Some(pdir) = ctx.project_dir {
                self.cached_abs = Some(pdir.join(&rel_path));
            }
        } else if let Some(e) = cache_err {
            ui.ctx().data_mut(|d|
                d.insert_temp(status_id, format!("Cache failed: {}", e)));
        }

        // ── Drain yt-dlp result if one arrived this frame ────────────
        let resolved_ok:  Option<String> = ui.ctx().data_mut(|d| d.remove_temp(resolved_ok_id));
        let resolved_err: Option<String> = ui.ctx().data_mut(|d| d.remove_temp(resolved_err_id));
        if resolved_ok.is_some() || resolved_err.is_some() {
            ui.ctx().data_mut(|d| d.remove_temp::<bool>(pending_id));
        }
        if let Some(resolved) = resolved_ok {
            self.effective_url = resolved.clone();
            // Probe duration of the resolved URL so the scrubber knows
            // its range. ffprobe HEADs the moov atom; cheap on most
            // CDN-backed mp4s. Returns None for live streams (HLS).
            self.duration_secs = crate::nodes::video_player::get_duration(&resolved)
                .unwrap_or(0.0);
            ui.ctx().data_mut(|d|
                d.insert_temp(status_id, "Connecting…".to_string()));
            self.open_decoder(node_id, 0.0);
            // Always spawn audio when a URL source starts. The samples
            // flow into the engine immediately; whether sound reaches
            // the speaker depends on the user wiring Audio→Speaker.
            // Wiring after Start works without restart — the engine's
            // pending_connection_sync picks up the new edge.
            if self.active {
                if let Err(e) = ctx.audio.play_pipe_audio(node_id, &resolved, 0.0) {
                    crate::system_log::warn(format!("Video In audio: {}", e));
                }
                ctx.audio.engine_write_param(node_id, 0, self.volume);
                self.anchor_position(0.0);
            }
            if self.active {
                ui.ctx().data_mut(|d|
                    d.insert_temp(status_id, "Streaming".to_string()));
            }
        } else if let Some(e) = resolved_err {
            ui.ctx().data_mut(|d|
                d.insert_temp(status_id, format!("Error: {}", e)));
        }

        // ── Status label ─────────────────────────────────────────────
        let status: Option<String> = ui.ctx().data_mut(|d| d.get_temp(status_id));
        if let Some(s) = status {
            let color = if s.starts_with("Error") {
                egui::Color32::from_rgb(255, 100, 100)
            } else if s == "Streaming" {
                egui::Color32::from_rgb(80, 200, 120)
            } else {
                dim
            };
            ui.colored_label(color, egui::RichText::new(s).small());
        } else if !self.status.is_empty() {
            ui.colored_label(dim, egui::RichText::new(&self.status).small());
        }

        // ── Start / Resolving / Stop ─────────────────────────────────
        let pending: bool = ui.ctx().data_mut(|d| d.get_temp(pending_id).unwrap_or(false));
        ui.horizontal(|ui| {
            if pending {
                ui.add_enabled_ui(false, |ui| {
                    let _ = ui.add(egui::Button::new(
                        egui::RichText::new("Resolving…").small()
                    ));
                });
                if ui.small_button("✕").on_hover_text("Cancel resolve").clicked() {
                    ui.ctx().data_mut(|d| {
                        d.remove_temp::<bool>(pending_id);
                        d.remove_temp::<String>(status_id);
                        d.remove_temp::<String>(resolved_ok_id);
                        d.remove_temp::<String>(resolved_err_id);
                    });
                }
            } else if !self.active {
                let can_start = !self.device_id.trim().is_empty();
                if ui.add_enabled(can_start, egui::Button::new("▶ Start")).clicked() {
                    let url = self.device_id.trim().to_string();
                    if needs_yt_dlp(&url) {
                        // Async path — yt-dlp resolves on a worker thread.
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(status_id, "Resolving…".to_string());
                            d.insert_temp(pending_id, true);
                        });
                        let ectx = ui.ctx().clone();
                        let url2 = url.clone();
                        std::thread::spawn(move || {
                            match run_yt_dlp(&url2) {
                                Ok(u) => ectx.data_mut(|d| d.insert_temp(resolved_ok_id, u)),
                                Err(e) => ectx.data_mut(|d| d.insert_temp(resolved_err_id, e)),
                            }
                            ectx.request_repaint();
                        });
                    } else {
                        // Synchronous path — direct URL straight to ffmpeg.
                        self.effective_url = url.clone();
                        self.duration_secs = crate::nodes::video_player::get_duration(&url)
                            .unwrap_or(0.0);
                        ui.ctx().data_mut(|d|
                            d.insert_temp(status_id, "Connecting…".to_string()));
                        self.open_decoder(node_id, 0.0);
                        if self.active {
                            if let Err(e) = ctx.audio.play_pipe_audio(node_id, &url, 0.0) {
                                crate::system_log::warn(format!("Video In audio: {}", e));
                            }
                            ctx.audio.engine_write_param(node_id, 0, self.volume);
                            self.anchor_position(0.0);
                            ui.ctx().data_mut(|d|
                                d.insert_temp(status_id, "Streaming".to_string()));
                        }
                    }
                }
            } else {
                // ── Pause / Resume ─────────────────────────────────
                let is_paused = ctx.audio.is_file_paused(node_id);
                if is_paused {
                    if ui.button("▶ Resume").clicked() {
                        ctx.audio.resume_file(node_id);
                        let pos = self.paused_at_secs.unwrap_or(0.0);
                        self.anchor_position(pos);
                        ui.ctx().data_mut(|d|
                            d.insert_temp(status_id, "Streaming".to_string()));
                    }
                } else {
                    if ui.button("⏸ Pause").clicked() {
                        ctx.audio.pause_file(node_id);
                        self.pause_position();
                        ui.ctx().data_mut(|d|
                            d.insert_temp(status_id, "Paused".to_string()));
                    }
                }
                if ui.button("⏹ Stop").clicked() {
                    self.close_decoder(node_id);
                    ctx.audio.stop_file(node_id);
                    self.clear_position();
                    ui.ctx().data_mut(|d| {
                        d.remove_temp::<String>(status_id);
                    });
                }
            }

            // ── Cache button ─────────────────────────────────────────
            // Three states:
            //   • Already cached → show ✓ Cached chip, no button.
            //   • No project dir → "Save project first" disabled hint.
            //   • Otherwise      → Cache button (or "Caching…" while pending).
            let already_cached = self.cached_abs.as_ref()
                .map(|p| p.exists()).unwrap_or(false);
            let cache_pending: bool = ui.ctx()
                .data_mut(|d| d.get_temp(cache_pending_id).unwrap_or(false));
            if already_cached {
                ui.colored_label(
                    egui::Color32::from_rgb(80, 200, 120),
                    egui::RichText::new("✓ Cached").small(),
                );
            } else if cache_pending {
                ui.add_enabled_ui(false, |ui| {
                    let _ = ui.button(egui::RichText::new("Caching…").small());
                });
            } else if ctx.project_dir.is_none() {
                ui.add_enabled_ui(false, |ui| {
                    let _ = ui.button(egui::RichText::new("Save project to cache").small())
                        .on_disabled_hover_text(
                            "Cache writes into <project>/assets/video_in/. \
                             Save the project at least once to enable.",
                        );
                });
            } else if !self.device_id.trim().is_empty() {
                if ui.button(egui::RichText::new("⬇ Cache").small())
                    .on_hover_text("Download to <project>/assets/video_in/ and play from disk on subsequent Starts.")
                    .clicked()
                {
                    let url     = self.device_id.trim().to_string();
                    let pdir    = ctx.project_dir.unwrap().to_path_buf();
                    let rel     = std::path::PathBuf::from("assets/video_in")
                        .join(cache_filename(&url));
                    let abs_dst = pdir.join(&rel);
                    let rel_str = rel.to_string_lossy().into_owned();
                    ui.ctx().data_mut(|d| {
                        d.insert_temp(cache_pending_id, true);
                        d.insert_temp(status_id, "Caching…".to_string());
                    });
                    let ectx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        match download_to_disk(&url, &abs_dst) {
                            Ok(()) => ectx.data_mut(|d|
                                d.insert_temp(cache_done_id, rel_str)),
                            Err(e) => ectx.data_mut(|d|
                                d.insert_temp(cache_err_id, e)),
                        }
                        ectx.request_repaint();
                    });
                }
            }
        });

        // ── Active-state controls (Volume / Scrubber / Audio hint) ──
        if self.active {
            self.render_active_controls(ui, ctx);
        }

        // Pull any new frames from the decoder thread (same dance as
        // render_camera_ui / render_screen_ui — without this call the
        // ffmpeg subprocess runs but its frames are never read into
        // `self.current_frame` and the preview stays empty).
        self.poll_decoder(node_id, Some(ctx));
        self.render_preview(ui, ctx);

        // If decode failed (Open returned Err inside open_decoder),
        // surface that error in the URL-flow status display so the user
        // knows what happened. self.status holds the message; we copy
        // it into the temp_data status so the URL section shows it
        // instead of a stale "Connecting…".
        if !self.active && !self.status.is_empty()
            && self.status != "Stopped"
            && self.status != "Capturing"
        {
            ui.ctx().data_mut(|d|
                d.insert_temp(status_id, format!("Error: {}", self.status)));
        }
    }

    /// True if anything is wired to our Audio output port (port index 1).
    /// Used to decide whether to spawn the audio-extraction ffmpeg —
    /// nothing wired = no point burning CPU on a second decode.
    fn audio_port_wired(ctx: &RenderContext, node_id: NodeId) -> bool {
        ctx.connections.iter().any(|c| c.from_node == node_id && c.from_port == 1)
    }

    /// Current playhead in seconds. Wall-clock estimate accumulated since
    /// the last anchor (Start / Resume / Seek). Returns the captured
    /// position when paused, or zero before the first Start.
    fn current_position(&self) -> f32 {
        if let Some(p) = self.paused_at_secs { return p; }
        if let Some(t) = self.position_anchor_instant {
            return self.position_anchor_secs + t.elapsed().as_secs_f32();
        }
        self.position_anchor_secs
    }

    /// Anchor the playhead at `secs` and start the wall clock. Called on
    /// Start (with 0.0), on Resume (with the paused value), and on every
    /// successful Seek.
    fn anchor_position(&mut self, secs: f32) {
        self.position_anchor_secs = secs;
        self.position_anchor_instant = Some(std::time::Instant::now());
        self.paused_at_secs = None;
    }

    /// Capture current position into `paused_at_secs` and stop the wall
    /// clock. Idempotent.
    fn pause_position(&mut self) {
        let p = self.current_position();
        self.paused_at_secs = Some(p);
        self.position_anchor_instant = None;
    }

    /// Reset position state to "not playing" (Stop).
    fn clear_position(&mut self) {
        self.position_anchor_instant = None;
        self.position_anchor_secs = 0.0;
        self.paused_at_secs = None;
        self.scrub_value = 0.0;
        self.scrubbing = false;
    }

    /// Seek the active stream to `target` seconds. Closes the decoders
    /// and reopens with `-ss target` for both video and audio. ~1–2 s
    /// glitch on URL streams (HTTP byte range), instant on local files.
    fn seek_to(&mut self, node_id: NodeId, target: f32, ctx: &mut RenderContext) {
        self.close_decoder(node_id);
        ctx.audio.stop_file(node_id);
        self.open_decoder(node_id, target);
        if self.active {
            let source = self.effective_input();
            if !source.is_empty() {
                if let Err(e) = ctx.audio.play_pipe_audio(node_id, &source, target) {
                    crate::system_log::warn(format!("Video In seek audio: {}", e));
                }
                ctx.audio.engine_write_param(node_id, 0, self.volume);
            }
        }
        self.anchor_position(target);
    }

    /// The string passed to ffmpeg `-i` — same precedence chain
    /// open_decoder uses internally, but exposed for play_pipe_audio
    /// and seek calls.
    fn effective_input(&self) -> String {
        match self.source {
            VideoSource::Url => {
                if let Some(p) = self.cached_abs.as_ref().filter(|p| p.exists()) {
                    return p.to_string_lossy().into_owned();
                }
                if !self.effective_url.is_empty() { return self.effective_url.clone(); }
                self.device_id.trim().to_string()
            }
            VideoSource::File => self.device_id.clone(),
            _ => String::new(),
        }
    }

    /// Render the slice of UI shared by File and URL sources while
    /// playback is active: Volume slider, Scrubber (when duration is
    /// known), and the "wire Audio" hint. Caller is responsible for the
    /// source-specific bits above this (URL field / File picker / Start
    /// button) and for `poll_decoder` + `render_preview` after.
    fn render_active_controls(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let node_id = ctx.node_id;

        // Volume — pushed every frame (not just on slider change) so a
        // DSP off/on cycle, which clears node_params and re-creates the
        // volume atomic at default 1.0, picks the user's actual setting
        // back up on the next render. The atomic write is cheap.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Volume").small().color(dim));
            ui.add(egui::Slider::new(&mut self.volume, 0.0..=2.0).show_value(false));
        });
        ctx.audio.engine_write_param(node_id, 0, self.volume);

        // Scrubber — only when duration is known (i.e. not a live stream
        // and we successfully ffprobe'd it).
        if self.duration_secs > 0.0 {
            let pos = self.current_position();
            // While not actively dragging, the slider value tracks the
            // computed playhead so the thumb advances during playback.
            // While dragging, scrub_value floats with the cursor and pos
            // is unused.
            if !self.scrubbing { self.scrub_value = pos; }
            let display_left = if self.scrubbing { self.scrub_value } else { pos };
            let dur = self.duration_secs;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format_time(display_left))
                        .small().monospace().color(dim),
                );
                let resp = ui.add(
                    egui::Slider::new(&mut self.scrub_value, 0.0..=dur).show_value(false),
                );
                ui.label(
                    egui::RichText::new(format_time(dur))
                        .small().monospace().color(dim),
                );
                if resp.dragged() { self.scrubbing = true; }
                if resp.drag_stopped() {
                    let target = self.scrub_value;
                    self.seek_to(node_id, target, ctx);
                    self.scrubbing = false;
                }
            });
        }

        // Wire-audio hint
        if !Self::audio_port_wired(ctx, node_id) {
            ui.colored_label(
                egui::Color32::from_rgb(255, 180, 80),
                egui::RichText::new("🔇 Wire Audio → Speaker to hear sound").small(),
            );
        }
    }
}

/// Format `s` seconds as either "M:SS" (under an hour) or "H:MM:SS".
fn format_time(s: f32) -> String {
    let s = s.max(0.0);
    let h = (s / 3600.0) as u32;
    let m = ((s % 3600.0) / 60.0) as u32;
    let sec = (s % 60.0) as u32;
    if h > 0 { format!("{}:{:02}:{:02}", h, m, sec) }
    else { format!("{}:{:02}", m, sec) }
}

impl VideoInNode {
    /// File-source UI. Picker via rfd, ffprobe duration probe on pick,
    /// Start / Pause / Resume / Stop / Loop / Volume / Scrubber. Audio
    /// goes through the same AudioPipeDecoder path the URL source uses,
    /// so wiring Audio → Speaker is identical.
    fn render_file_ui(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let node_id = ctx.node_id;
        let status_id = egui::Id::new(("vin_url_status", node_id));

        // ── File picker + path ───────────────────────────────────────
        ui.horizontal(|ui| {
            if ui.button(egui::RichText::new("📁 Open").small()).clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Video", &["mp4", "mov", "m4v", "avi", "webm", "mkv", "gif", "mp3", "wav", "ogg", "flac", "aac"])
                    .pick_file()
                {
                    let path = p.display().to_string();
                    self.device_id = path.clone();
                    // Probe duration immediately so the scrubber knows
                    // its range before Start. Cheap (reads moov atom).
                    self.duration_secs = crate::nodes::video_player::get_duration(&path)
                        .unwrap_or(0.0);
                    // Reset position state since we have a new source.
                    self.clear_position();
                    self.close_decoder(node_id);
                    ctx.audio.stop_file(node_id);
                    ui.ctx().data_mut(|d| d.remove_temp::<String>(status_id));
                }
            }
            if !self.device_id.is_empty() {
                let short = if self.device_id.len() > 38 {
                    format!("…{}", &self.device_id[self.device_id.len() - 38..])
                } else {
                    self.device_id.clone()
                };
                ui.label(
                    egui::RichText::new(short).small().monospace().color(dim),
                ).on_hover_text(&self.device_id);
                if ui.small_button("✕").on_hover_text("Clear file").clicked() {
                    self.device_id.clear();
                    self.duration_secs = 0.0;
                    self.clear_position();
                    self.close_decoder(node_id);
                    ctx.audio.stop_file(node_id);
                    ui.ctx().data_mut(|d| d.remove_temp::<String>(status_id));
                }
            }
        });

        // ── Resolution sliders ───────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Size").small().color(dim));
            ui.add(egui::DragValue::new(&mut self.res_w).range(64..=3840).speed(8));
            ui.label(egui::RichText::new("×").small().color(dim));
            ui.add(egui::DragValue::new(&mut self.res_h).range(64..=2160).speed(8));
        });

        // ── Loop checkbox ────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.loop_playback, "Loop")
              .on_hover_text("When the file ends, restart from the beginning.");
        });

        // ── Status label ─────────────────────────────────────────────
        let status: Option<String> = ui.ctx().data_mut(|d| d.get_temp(status_id));
        if let Some(s) = status {
            let color = if s.starts_with("Error") {
                egui::Color32::from_rgb(255, 100, 100)
            } else if s == "Streaming" {
                egui::Color32::from_rgb(80, 200, 120)
            } else {
                dim
            };
            ui.colored_label(color, egui::RichText::new(s).small());
        } else if !self.status.is_empty() {
            ui.colored_label(dim, egui::RichText::new(&self.status).small());
        }

        // ── Start / Pause-Resume / Stop ──────────────────────────────
        ui.horizontal(|ui| {
            if !self.active {
                let can_start = !self.device_id.trim().is_empty();
                if ui.add_enabled(can_start, egui::Button::new("▶ Start")).clicked() {
                    let path = self.device_id.clone();
                    ui.ctx().data_mut(|d|
                        d.insert_temp(status_id, "Loading…".to_string()));
                    self.open_decoder(node_id, 0.0);
                    if self.active {
                        if let Err(e) = ctx.audio.play_pipe_audio(node_id, &path, 0.0) {
                            crate::system_log::warn(format!("Video In audio: {}", e));
                        }
                        ctx.audio.engine_write_param(node_id, 0, self.volume);
                        self.anchor_position(0.0);
                        ui.ctx().data_mut(|d|
                            d.insert_temp(status_id, "Streaming".to_string()));
                    }
                }
            } else {
                let is_paused = ctx.audio.is_file_paused(node_id);
                if is_paused {
                    if ui.button("▶ Resume").clicked() {
                        ctx.audio.resume_file(node_id);
                        let pos = self.paused_at_secs.unwrap_or(0.0);
                        self.anchor_position(pos);
                        ui.ctx().data_mut(|d|
                            d.insert_temp(status_id, "Streaming".to_string()));
                    }
                } else {
                    if ui.button("⏸ Pause").clicked() {
                        ctx.audio.pause_file(node_id);
                        self.pause_position();
                        ui.ctx().data_mut(|d|
                            d.insert_temp(status_id, "Paused".to_string()));
                    }
                }
                if ui.button("⏹ Stop").clicked() {
                    self.close_decoder(node_id);
                    ctx.audio.stop_file(node_id);
                    self.clear_position();
                    ui.ctx().data_mut(|d| d.remove_temp::<String>(status_id));
                }
            }
        });

        // ── Active-state controls (Volume / Scrubber / Audio hint) ──
        if self.active {
            self.render_active_controls(ui, ctx);
        }

        // Pull frames + render preview (same dance as camera/screen/url).
        self.poll_decoder(node_id, Some(ctx));
        self.render_preview(ui, ctx);

        // Surface decoder open errors via the URL-flow status display.
        if !self.active && !self.status.is_empty()
            && self.status != "Stopped"
            && self.status != "Capturing"
        {
            ui.ctx().data_mut(|d|
                d.insert_temp(status_id, format!("Error: {}", self.status)));
        }
    }

    /// Screen-source UI. Symmetric to `render_camera_ui` but uses
    /// `list_screens()`, which on macOS filters to "Capture screen N"
    /// AVFoundation entries and on Linux synthesises one entry per
    /// attached display.
    fn render_screen_ui(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let screens = list_screens();
        let current_idx = self.device_index();

        let mut new_idx: Option<u32> = None;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Display").small());
            egui::ComboBox::from_id_salt(egui::Id::new(("vin_scr", ctx.node_id)))
                .selected_text(
                    screens
                        .iter()
                        .find(|(i, _)| *i == current_idx)
                        .map(|(_, n)| n.as_str())
                        .unwrap_or("Select…"),
                )
                .width(170.0)
                .show_ui(ui, |ui| {
                    for (idx, name) in &screens {
                        let selected = *idx == current_idx;
                        if ui.selectable_label(selected, name).clicked() && !selected {
                            new_idx = Some(*idx);
                        }
                    }
                });
            if ui.small_button("↻").on_hover_text("Refresh display list").clicked() {
                // Screens come from the camera list on macOS, so the
                // same refresh helper serves both.
                refresh_camera_list_now();
            }
        });
        if let Some(idx) = new_idx {
            let was_active = self.active;
            self.close_decoder(ctx.node_id);
            self.device_id = idx.to_string();
            if was_active { self.open_decoder(ctx.node_id, 0.0); }
        }

        if screens.is_empty() {
            ui.colored_label(
                ui.visuals().widgets.noninteractive.fg_stroke.color,
                egui::RichText::new("No screens detected — check display permissions").small(),
            );
        }

        // Resolution (same as camera)
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Res").small());
            ui.add(egui::DragValue::new(&mut self.res_w).range(320..=3840).speed(10).prefix("W:"));
            ui.add(egui::DragValue::new(&mut self.res_h).range(240..=2160).speed(10).prefix("H:"));
        });

        // Start / Stop — deferred-apply to avoid borrowing self twice
        let mut toggle_start = false;
        let mut toggle_stop = false;
        ui.horizontal(|ui| {
            if self.active {
                if ui.button("⏹ Stop").clicked() { toggle_stop = true; }
                ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "● CAP");
            } else if ui.button("▶ Start").clicked() {
                toggle_start = true;
            }
        });
        if toggle_start { self.open_decoder(ctx.node_id, 0.0); }
        if toggle_stop { self.close_decoder(ctx.node_id); }

        // Status
        if !self.status.is_empty() {
            let color = if self.status.contains("Error") || self.status.contains("Failed") {
                egui::Color32::from_rgb(255, 100, 100)
            } else if self.active {
                egui::Color32::from_rgb(80, 200, 80)
            } else {
                egui::Color32::from_rgb(150, 150, 150)
            };
            ui.colored_label(color, egui::RichText::new(&self.status).small());
        }

        // Poll + preview — same code path as camera so changes stay
        // localised if the two UIs ever diverge further.
        self.poll_decoder(ctx.node_id, Some(ctx));
        self.render_preview(ui, ctx);
    }

    /// Phase 2 M5 Syphon source UI. Discovers servers via
    /// `SyphonServerDirectory`, lets the user pick one, connects a
    /// `SyphonClient` on Start. Incoming frames are imported as
    /// `wgpu::Texture`s and published to the GPU cache via
    /// `queue_publish_node_output` so downstream GPU consumers (and
    /// Visual Output's GPU preview callback) pick them up with no
    /// readback.
    ///
    /// Unlike Camera / Screen, there's no `current_frame: Arc<ImageData>`
    /// here — the frame is GPU-only. The node's output port therefore
    /// emits `PortValue::GpuImage` (handled in `evaluate_with_ctx`).
    #[cfg(target_os = "macos")]
    fn render_syphon_ui(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        use crate::video_io::wgpu_metal;

        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // ── Discovered servers dropdown (cached, ~1 Hz refresh) ────
        // Calling `syphon::servers()` every frame at 60 Hz was a
        // measurable CFString leak source in M6 soak. The dict of
        // running servers barely changes from one frame to the next,
        // so 1 Hz is plenty for UI feedback. The ↻ button forces
        // immediate refresh. Connect path re-queries fresh regardless.
        let cache_stale = self
            .syphon_servers_cache
            .as_ref()
            .map(|(t, _)| t.elapsed().as_secs_f32() > 1.0)
            .unwrap_or(true);
        if cache_stale {
            let fresh: Vec<(String, String)> = crate::video_io::syphon::servers()
                .into_iter()
                .map(|info| (info.app_name, info.name))
                .collect();
            self.syphon_servers_cache = Some((std::time::Instant::now(), fresh));
        }
        let discovered: Vec<(String, String)> = self
            .syphon_servers_cache
            .as_ref()
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        let mut picked_key: Option<String> = None;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Server").small());
            let selected_text = if self.device_id.is_empty() {
                "Select…".to_string()
            } else if let Some((app, name)) = discovered.iter().find(|(a, n)| {
                format!("{}:{}", a, n) == self.device_id
            }) {
                // Truncate for node-width friendliness; dropdown shows full.
                let full = format!("{}: {}", app, name);
                crate::nodes::truncate_chars(&full, 22)
            } else {
                "— (not running)".to_string()
            };
            egui::ComboBox::from_id_salt(egui::Id::new(("vin_syph", ctx.node_id)))
                .selected_text(selected_text)
                .width(190.0)
                .show_ui(ui, |ui| {
                    if discovered.is_empty() {
                        ui.label(
                            egui::RichText::new("No Syphon servers running").small(),
                        );
                    }
                    for (app, name) in &discovered {
                        let key = format!("{}:{}", app, name);
                        let label = format!("{}: {}", app, name);
                        let selected = self.device_id == key;
                        if ui.selectable_label(selected, label).clicked() && !selected {
                            picked_key = Some(key);
                        }
                    }
                });
            if ui.small_button("↻").on_hover_text("Refresh server list").clicked() {
                // Force next frame's cache-stale check to fire.
                self.syphon_servers_cache = None;
            }
        });
        if let Some(key) = picked_key {
            let was_active = self.active;
            self.close_decoder(ctx.node_id);
            self.device_id = key;
            if was_active {
                self.open_syphon_client(ctx);
            }
        }

        // ── Connect / Disconnect toggle ────────────────────────────
        let mut toggle_start = false;
        let mut toggle_stop = false;
        ui.horizontal(|ui| {
            if self.active {
                if ui.button("⏹ Stop").clicked() { toggle_stop = true; }
                ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● RX");
            } else {
                let can_start = !self.device_id.is_empty()
                    && discovered.iter().any(|(a, n)| {
                        format!("{}:{}", a, n) == self.device_id
                    });
                if ui.add_enabled(can_start, egui::Button::new("▶ Connect")).clicked() {
                    toggle_start = true;
                }
            }
        });
        if toggle_start { self.open_syphon_client(ctx); }
        if toggle_stop { self.close_decoder(ctx.node_id); }

        // ── Status / dimensions ────────────────────────────────────
        if !self.status.is_empty() {
            let color = if self.status.contains("Error") || self.status.contains("Failed") {
                egui::Color32::from_rgb(255, 100, 100)
            } else if self.active {
                egui::Color32::from_rgb(80, 200, 80)
            } else {
                egui::Color32::from_rgb(150, 150, 150)
            };
            ui.colored_label(color, egui::RichText::new(&self.status).small());
        }
        if let Some((w, h)) = self.syphon_last_dims {
            ui.colored_label(
                dim,
                egui::RichText::new(format!("{}×{}", w, h)).small(),
            );
        }

        // ── Poll the client each frame ─────────────────────────────
        if self.active {
            self.poll_syphon_client(ctx);
            ui.ctx().request_repaint();
        }

        // Note: no preview thumbnail for Syphon source — the frame is
        // GPU-only (no CPU readback) so we'd have to do a full
        // texture→CPU copy just to preview. Wire a Visual Output
        // downstream if you want to see what's coming in.
        let _ = wgpu_metal::wgpu_device_to_mtl; // silence unused-import warn in non-use branches
    }

    /// Spawn the SyphonClient for the currently-selected server.
    /// Requires the wgpu Metal device, pulled from `RenderContext`.
    #[cfg(target_os = "macos")]
    fn open_syphon_client(&mut self, ctx: &mut RenderContext) {
        use crate::video_io::{syphon, wgpu_metal};
        let Some(render_state) = ctx.wgpu_render_state else {
            self.status = "wgpu render state unavailable".into();
            return;
        };
        let Some(mtl_device) = wgpu_metal::wgpu_device_to_mtl(&render_state.device) else {
            self.status = "Not on Metal backend".into();
            return;
        };
        // Re-query discovered servers — the selected one might have
        // quit between the dropdown render and now.
        let servers_now = syphon::servers();
        let Some(info) = servers_now.iter().find(|i| {
            format!("{}:{}", i.app_name, i.name) == self.device_id
        }) else {
            self.status = "Server not running".into();
            return;
        };
        match syphon::SyphonClient::new(info, &mtl_device) {
            Ok(client) => {
                if let Ok(mut guard) = self.syphon_client.lock() {
                    *guard = Some(client);
                }
                self.active = true;
                self.status = "Receiving".into();
            }
            Err(e) => {
                self.status = format!("Error: {}", e);
                self.active = false;
            }
        }
    }

    /// Per-frame poll: pull the latest MTLTexture from the client,
    /// bridge to wgpu, push into the GPU texture cache so downstream
    /// consumers (Visual Output, Syphon sink, WGSL Viewer) see it.
    #[cfg(target_os = "macos")]
    fn poll_syphon_client(&mut self, ctx: &mut RenderContext) {
        use crate::{gpu_image, video_io::wgpu_metal};
        let Some(render_state) = ctx.wgpu_render_state else { return; };
        let Ok(guard) = self.syphon_client.lock() else { return; };
        let Some(client) = guard.as_ref() else { return; };
        let Some(mtl_tex) = client.take_latest() else { return; };
        drop(guard); // release lock before the wgpu bridge (cheap but nice)

        // Remember dimensions for UI
        use foreign_types_shared::ForeignTypeRef;
        let w = mtl_tex.width() as u32;
        let h = mtl_tex.height() as u32;
        self.syphon_last_dims = Some((w, h));

        // Import the MTLTexture as a wgpu::Texture and push into the
        // per-frame GPU cache so downstream sinks find it via
        // `frame_snapshot_get_texture(node_id, 0)`.
        if let Some(wgpu_tex) = wgpu_metal::mtl_to_wgpu_texture(&render_state.device, mtl_tex) {
            gpu_image::queue_publish_node_output(ctx.node_id, 0, wgpu_tex, w, h);
        }
    }

    /// Phase 3 M5 NDI source UI. Populates the source dropdown from a
    /// 1 Hz-cached `NdiFinder::sources()` query, lets the user pick
    /// one, and spins up a background receiver thread on Start.
    /// Frames land in `current_frame` just like the Camera path, so
    /// the existing `app/mod.rs:2685` GPU-upload arm covers
    /// downstream consumers with zero new integration.
    #[cfg(target_os = "macos")]
    fn render_ndi_ui(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        use crate::video_io::ndi;

        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // ── Gate: runtime must be installed ────────────────────────
        // `VideoSource::Ndi.available()` already blocks selection when
        // the library isn't loaded, but in two cases the user ends up
        // here with NDI selected anyway: (a) they loaded an older
        // project that picked NDI before uninstalling the runtime,
        // and (b) the runtime was uninstalled mid-session. Surface the
        // state inline so they see *why* no sources are listed rather
        // than staring at an empty dropdown. Tooltip carries the
        // install URL for copy/paste.
        if let Err(msg) = ndi::library() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 140, 80),
                egui::RichText::new("⚠ NDI Runtime not installed").strong(),
            )
            .on_hover_text(msg);
            ui.colored_label(
                dim,
                egui::RichText::new(
                    "Install the free NDI Tools package from\n\
                     https://ndi.video/tools/ to enable NDI sources.",
                )
                .small(),
            );
            return;
        }

        // ── Ensure finder exists (lazy) ────────────────────────────
        // Kept alive for the source-picker session; dropped on sink
        // switch / Stop / remove.
        if self
            .ndi_finder
            .lock()
            .map(|g| g.is_none())
            .unwrap_or(true)
        {
            if let Ok(mut g) = self.ndi_finder.lock() {
                match ndi::NdiFinder::new() {
                    Ok(f) => *g = Some(f),
                    Err(e) => {
                        self.status = format!("Finder error: {}", e);
                    }
                }
            }
        }

        // ── Discovered sources dropdown (cached, ~1 Hz refresh) ────
        // Same rationale as `render_syphon_ui` at :604 — calling
        // sources() every render frame is wasteful for a directory
        // that barely changes. 1 Hz TTL + a ↻ button for the impatient.
        let cache_stale = self
            .ndi_sources_cache
            .as_ref()
            .map(|(t, _)| t.elapsed().as_secs_f32() > 1.0)
            .unwrap_or(true);
        if cache_stale {
            if let Ok(g) = self.ndi_finder.lock() {
                if let Some(f) = g.as_ref() {
                    let fresh = f.sources();
                    self.ndi_sources_cache =
                        Some((std::time::Instant::now(), fresh));
                }
            }
        }
        let discovered: Vec<ndi::NdiSourceInfo> = self
            .ndi_sources_cache
            .as_ref()
            .map(|(_, v)| v.clone())
            .unwrap_or_default();

        // ── Dropdown ──────────────────────────────────────────────
        let mut picked: Option<String> = None;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Source").small());
            let selected_text = if self.device_id.is_empty() {
                "Select…".to_string()
            } else if discovered.iter().any(|s| s.name == self.device_id) {
                crate::nodes::truncate_chars(&self.device_id, 24)
            } else {
                "— (offline)".to_string()
            };
            egui::ComboBox::from_id_salt(egui::Id::new(("vin_ndi", ctx.node_id)))
                .selected_text(selected_text)
                .width(220.0)
                .show_ui(ui, |ui| {
                    if discovered.is_empty() {
                        ui.label(
                            egui::RichText::new("No NDI sources on LAN").small(),
                        );
                    }
                    for info in &discovered {
                        let selected = self.device_id == info.name;
                        if ui
                            .selectable_label(selected, &info.name)
                            .on_hover_text(&info.address)
                            .clicked()
                            && !selected
                        {
                            picked = Some(info.name.clone());
                        }
                    }
                });
            if ui
                .small_button("↻")
                .on_hover_text("Refresh NDI source list")
                .clicked()
            {
                self.ndi_sources_cache = None;
            }
        });
        if let Some(key) = picked {
            let was_active = self.active;
            self.close_decoder(ctx.node_id);
            self.device_id = key;
            if was_active {
                self.open_ndi_receiver(ctx);
            }
        }

        // ── Connect / Disconnect toggle ────────────────────────────
        let mut toggle_start = false;
        let mut toggle_stop = false;
        ui.horizontal(|ui| {
            if self.active {
                if ui.button("⏹ Stop").clicked() {
                    toggle_stop = true;
                }
                // Indicator colour reflects liveness, not just
                // "we spawned a worker". Green = frames arriving
                // within the last second; amber = worker's still
                // polling but no frames (sender disappeared, network
                // blip, etc.).
                let fresh = self
                    .ndi_last_frame_at
                    .map(|t| t.elapsed().as_secs_f32() < 1.0)
                    .unwrap_or(false);
                let (indicator, color) = if fresh {
                    ("● RX", egui::Color32::from_rgb(80, 200, 80))
                } else {
                    ("◐ idle", egui::Color32::from_rgb(200, 180, 80))
                };
                ui.colored_label(color, indicator);
            } else {
                let can_start = !self.device_id.is_empty()
                    && discovered.iter().any(|s| s.name == self.device_id);
                if ui
                    .add_enabled(can_start, egui::Button::new("▶ Connect"))
                    .clicked()
                {
                    toggle_start = true;
                }
            }
        });
        if toggle_start {
            self.open_ndi_receiver(ctx);
        }
        if toggle_stop {
            self.close_decoder(ctx.node_id);
        }

        // ── Status / dimensions ────────────────────────────────────
        if !self.status.is_empty() {
            let color = if self.status.contains("Error") || self.status.contains("Failed") {
                egui::Color32::from_rgb(255, 100, 100)
            } else if self.active {
                egui::Color32::from_rgb(80, 200, 80)
            } else {
                egui::Color32::from_rgb(150, 150, 150)
            };
            ui.colored_label(color, egui::RichText::new(&self.status).small());
        }
        if let Some((w, h)) = self.ndi_last_dims {
            ui.colored_label(dim, egui::RichText::new(format!("{}×{}", w, h)).small());
        }

        // ── Poll the worker each frame ─────────────────────────────
        if self.active {
            self.poll_ndi_receiver();
            ui.ctx().request_repaint();
        }

        // Preview — NDI gives us CPU pixels (same shape as Camera), so
        // the shared `render_preview` helper works unmodified.
        self.render_preview(ui, ctx);
    }

    /// Spawn the NDI receiver worker thread for the currently-selected
    /// source. Silently returns with an error status if the source has
    /// disappeared from the LAN between dropdown render and this call.
    #[cfg(target_os = "macos")]
    fn open_ndi_receiver(&mut self, ctx: &mut RenderContext) {
        use crate::video_io::{ndi, pixel_swizzle};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        // Re-query sources now — the one the user picked might have
        // quit between the dropdown render and the Connect click.
        let sources = match self.ndi_finder.lock() {
            Ok(g) => g.as_ref().map(|f| f.sources()).unwrap_or_default(),
            Err(_) => return,
        };
        let Some(info) = sources.into_iter().find(|s| s.name == self.device_id)
        else {
            self.status = "Source not found on LAN".into();
            return;
        };

        let recv_name = format!("PatchWork Video In #{}", ctx.node_id);
        let receiver = match ndi::NdiReceiver::new(&info, &recv_name) {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("Error: {}", e);
                self.active = false;
                return;
            }
        };

        // Spawn the worker thread. It owns the NdiReceiver and runs
        // `capture_video(100ms)` in a tight loop; each Video frame is
        // swizzled BGRA → RGBA and stashed in `latest`. Main-thread
        // `poll_ndi_receiver` drains `latest` on every UI frame —
        // `render_ndi_ui` already calls `ui.ctx().request_repaint()`
        // while `active`, so we don't need to hop contexts from the
        // worker to kick egui into drawing.
        let latest: Arc<Mutex<Option<Arc<ImageData>>>> =
            Arc::new(Mutex::new(None));
        let latest_worker = latest.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_worker = stop.clone();
        let _ = ctx; // ctx only used by caller; worker doesn't need it

        let thread = std::thread::Builder::new()
            .name(format!("ndi-recv-{}", self.device_id))
            .spawn(move || {
                // Capture in 100 ms chunks — balances CPU (vs a tight
                // poll) against stop-flag latency (worst case 100 ms
                // after the user clicks Stop). The worker ONLY exits
                // on the stop flag; `CaptureOutcome::Empty` (timeout,
                // audio/metadata, transient error, status change)
                // just loops. A source that's really gone returns
                // Empty forever — main thread can show that as
                // "no frames" via an idle-timeout check if useful,
                // but doesn't need to tear the worker down.
                while !stop_worker.load(Ordering::Relaxed) {
                    match receiver.capture_video(100) {
                        crate::video_io::ndi::CaptureOutcome::Frame(cf) => {
                            // NDI gave us BGRA; PatchWork's ImageData
                            // is RGBA by convention.
                            let rgba = pixel_swizzle::bgra_to_rgba(&cf.bgra);
                            let img = Arc::new(ImageData {
                                width: cf.width,
                                height: cf.height,
                                pixels: rgba,
                            });
                            if let Ok(mut slot) = latest_worker.lock() {
                                *slot = Some(img);
                            }
                        }
                        crate::video_io::ndi::CaptureOutcome::Empty => {
                            // No frame this iteration. Keep looping.
                        }
                    }
                }
                // Drop of `receiver` runs NDIlib_recv_destroy.
                drop(receiver);
            })
            .ok();

        self.ndi_worker = Some(NdiWorker {
            stop,
            latest,
            thread,
        });
        self.active = true;
        self.status = "Connecting…".into();
    }

    /// Main-thread poll: move any worker-produced frame into
    /// `current_frame` so `app/mod.rs` picks it up via the existing
    /// Camera-path arm.
    ///
    /// The worker thread runs until the stop flag flips (set by
    /// `close_decoder`); transient capture errors / timeouts don't
    /// kill the session. So we don't watch `JoinHandle::is_finished`
    /// here — if the user wants to disconnect, they click Stop.
    /// Staleness — "sender went away, frames stopped flowing" — is
    /// detected in `render_ndi_ui` by comparing
    /// `ndi_last_frame_at.elapsed()` against a threshold, not here.
    #[cfg(target_os = "macos")]
    fn poll_ndi_receiver(&mut self) {
        let Some(worker) = self.ndi_worker.as_ref() else { return; };
        let drained = worker
            .latest
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(img) = drained {
            self.ndi_last_dims = Some((img.width, img.height));
            self.ndi_last_frame_at = Some(std::time::Instant::now());
            self.current_frame = Some(img);
        }
        // Status reflects real liveness: "Receiving" only while frames
        // are actually arriving; otherwise surface the staleness so the
        // user can tell their source died. 1 s grace period covers
        // normal frame-interval jitter at any fps ≥ 1.
        let fresh = self
            .ndi_last_frame_at
            .map(|t| t.elapsed().as_secs_f32() < 1.0)
            .unwrap_or(false);
        let want = if fresh {
            "Receiving"
        } else if self.ndi_last_frame_at.is_some() {
            "Source offline — waiting for frames"
        } else {
            "Connecting…"
        };
        if self.status != want {
            self.status = want.to_string();
        }
    }

    /// Shared preview block used by Camera and Screen UIs. Keeps the
    /// fast_downsample + cached-texture logic in one place.
    fn render_preview(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let Some(frame) = self.current_frame.as_ref() else { return; };

        // Defensive: an upstream that constructs ImageData with
        // mismatched (width, height, pixels) — e.g. a future node
        // with a buggy resize — must not take the whole app down via
        // `ColorImage::from_rgba_unmultiplied`'s assertion. Show a
        // compact status note instead so the user sees what's wrong.
        let expected = (frame.width as usize) * (frame.height as usize) * 4;
        if frame.pixels.len() != expected {
            ui.colored_label(
                egui::Color32::from_rgb(255, 120, 120),
                egui::RichText::new(format!(
                    "⚠ frame size mismatch: {} bytes but {}×{}×4 = {}",
                    frame.pixels.len(),
                    frame.width,
                    frame.height,
                    expected,
                ))
                .small(),
            );
            return;
        }

        let max_w = ui.available_width().min(300.0);
        let aspect = frame.height as f32 / frame.width.max(1) as f32;
        let preview_h = max_w * aspect;
        let frame_ptr = Arc::as_ptr(frame) as usize;
        let cached_same = self
            .preview_tex
            .as_ref()
            .map(|(_, p)| *p == frame_ptr)
            .unwrap_or(false);
        if !cached_same {
            let pw = max_w as u32;
            let ph = preview_h as u32;
            let preview_pixels = fast_downsample(frame, pw, ph);
            // Second-line defence: fast_downsample should always return
            // `pw * ph * 4` bytes, but keep the check in case a future
            // edit reintroduces the size mismatch it used to have.
            let expected_preview = (pw as usize) * (ph as usize) * 4;
            if preview_pixels.len() != expected_preview {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 120, 120),
                    egui::RichText::new("⚠ preview resize returned wrong size").small(),
                );
                return;
            }
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [pw as usize, ph as usize],
                &preview_pixels,
            );
            match self.preview_tex.as_mut() {
                Some((tex, p)) => {
                    tex.set(color_image, egui::TextureOptions::LINEAR);
                    *p = frame_ptr;
                }
                None => {
                    let tex = ui.ctx().load_texture(
                        format!("vin_{}", ctx.node_id),
                        color_image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.preview_tex = Some((tex, frame_ptr));
                }
            }
        }
        if let Some((tex, _)) = self.preview_tex.as_ref() {
            ui.image(egui::load::SizedTexture::new(
                tex.id(),
                egui::vec2(max_w, preview_h),
            ));
        }
    }
}

/// Mirror an RGBA8 frame horizontally and/or vertically. Allocates a
/// new `Arc<ImageData>` — the caller's copy-on-write semantics stay
/// intact (downstream nodes still see the same Arc pointer).
///
/// Either flag alone is cheap (~0.5 ms at 640×480 on Apple silicon);
/// both flags together walk the same row-major loop, so cost is
/// effectively identical.
fn mirror_frame(frame: &Arc<ImageData>, h: bool, v: bool) -> Arc<ImageData> {
    let w = frame.width as usize;
    let height = frame.height as usize;
    let stride = w * 4;
    let src = &frame.pixels;
    let mut out = vec![0u8; src.len()];
    for dy in 0..height {
        let sy = if v { height - 1 - dy } else { dy };
        let src_row_start = sy * stride;
        let dst_row_start = dy * stride;
        if h {
            // Horizontal flip: reverse pixels (4 bytes each) within the row.
            for dx in 0..w {
                let sx = w - 1 - dx;
                let s = src_row_start + sx * 4;
                let d = dst_row_start + dx * 4;
                out[d..d + 4].copy_from_slice(&src[s..s + 4]);
            }
        } else {
            // No H flip — straight row copy.
            out[dst_row_start..dst_row_start + stride]
                .copy_from_slice(&src[src_row_start..src_row_start + stride]);
        }
    }
    Arc::new(ImageData {
        width: frame.width,
        height: frame.height,
        pixels: out,
    })
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("video_in", |state| {
        let mut n = VideoInNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
