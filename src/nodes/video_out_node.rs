//! Video Out — unified video sink node.
//!
//! Destination picked via a dropdown. Phase 1 shipped the **Window**
//! sink (display picker + pop-out viewport). Phase 2 ships the
//! **Syphon** sink on macOS — publish the incoming GPU texture to any
//! listening app (TouchDesigner, Resolume, OBS, …) with zero CPU
//! readback. NDI / Spout / Virtual Camera / File Record still appear
//! in the dropdown with their phase badge and are disabled until
//! shipped — see `docs/video_io_spec.md` §3.3 and §7.
//!
//! Multi‑destination fan‑out happens in the graph (add more Video Out
//! nodes sharing the same upstream) — not via per‑node toggles.

use crate::graph::{Graph, ImageData, NodeId, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Destination dropdown for `VideoOutNode`. Only `Window` is wired up
/// in Phase 1; the rest show their landing phase as a chip.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum VideoSink {
    #[default]
    Window,
    Syphon,
    Ndi,
    Spout,
    VirtualCamera,
    FileRecord,
}

impl VideoSink {
    fn label(self) -> &'static str {
        match self {
            Self::Window => "Window / Display",
            Self::Syphon => "Syphon (mac)",
            Self::Ndi => "NDI",
            Self::Spout => "Spout (win)",
            Self::VirtualCamera => "Virtual Camera",
            Self::FileRecord => "File Recording",
        }
    }
    fn phase_hint(self) -> &'static str {
        match self {
            Self::Window => "Phase 1",
            Self::Syphon => "Phase 2",
            Self::Ndi => "Phase 3",
            Self::Spout => "Phase 4",
            Self::VirtualCamera => "Phase 5",
            Self::FileRecord => "Phase 6",
        }
    }
    /// Whether this sink is wired up in the current build + environment.
    ///
    /// `Ndi` is the odd one out: cross-platform in principle, but the
    /// NDI Runtime is a user-installed EULA'd dylib, not something we
    /// can bundle. So `Ndi.available()` checks `ndi::library()` at
    /// runtime — the UI disables NDI and shows an install tooltip when
    /// libndi is missing.
    fn available(self) -> bool {
        match self {
            Self::Window => true,
            #[cfg(target_os = "macos")]
            Self::Syphon => true,
            #[cfg(target_os = "macos")]
            Self::Ndi => crate::video_io::ndi::library().is_ok(),
            // Phase 5 v1: Virtual Camera enabled on macOS as a modal-only
            // shim that redirects to Syphon (see render_vcam_sink). Real
            // CMIO Camera Extension lands in v2.
            #[cfg(target_os = "macos")]
            Self::VirtualCamera => true,
            // Phase 6: file recorder pipes RGBA into ffmpeg. ffmpeg
            // availability is checked at record-start (lazy), not here —
            // matches how the legacy Camera node handles its dep.
            Self::FileRecord => true,
            _ => false,
        }
    }
    fn all() -> [Self; 6] {
        [
            Self::Window, Self::Syphon, Self::Ndi, Self::Spout,
            Self::VirtualCamera, Self::FileRecord,
        ]
    }
}

#[derive(Serialize, Deserialize)]
pub struct VideoOutNode {
    #[serde(default)] pub sink: VideoSink,

    /// Display id for `Window` sinks (matches `Display::id` from
    /// `crate::display`); publish name for Syphon / NDI / Spout
    /// (Phase 2–4); path for FileRecord (Phase 6). Empty string means
    /// "primary display" for Window sinks.
    #[serde(default)] pub destination: String,

    /// Fullscreen flag for the Window sink. Persisted so a project that
    /// ships with Video Out meant to go fullscreen on a second monitor
    /// opens that way on reload.
    #[serde(default)] pub fullscreen: bool,

    /// Optional fps cap (NDI sender uses this in Phase 3).
    #[serde(default)] pub fps_cap: f32,

    /// Whether the sink is currently publishing. Deliberately not
    /// serialised — user toggles on after loading a project so stray
    /// viewports don't pop on load.
    #[serde(skip)] pub enabled: bool,

    #[serde(skip)] pub status: String,

    /// Syphon server, lazily created on first enabled tick when the
    /// sink is `Syphon`. `Arc<Mutex<…>>` so the whole node stays
    /// `Send + Sync` (required by `NodeBehavior`); the SyphonServer
    /// itself is !Sync.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    syphon_server: Arc<Mutex<Option<crate::video_io::syphon::SyphonServer>>>,

    /// NDI sender, lazily created on first enabled tick when the sink
    /// is `Ndi`. Same `Arc<Mutex<Option<...>>>` shape as the Syphon
    /// server so the whole node stays `Send + Sync`. Dropping the
    /// `NdiSender` (sink change / disable / node removal) runs
    /// `NDIlib_send_destroy` → receivers drop our entry within ~2–3 s.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    ndi_sender: Arc<Mutex<Option<crate::video_io::ndi::NdiSender>>>,

    /// Reusable BGRA scratch buffer for the NDI send path. We swizzle
    /// the upstream `Arc<ImageData>` (RGBA) into this buffer once per
    /// frame and hand it to `NdiSender::publish_bgra`. Allocated once
    /// on first publish, grown if resolution increases, never shrunk.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    ndi_scratch: Vec<u8>,

    /// Instant of the last `publish_bgra` call. Used to enforce
    /// `fps_cap` (skip the publish if the cap budget hasn't elapsed)
    /// and to power the UI "last send: Nms ago" heartbeat. `None`
    /// until the first send of a session — resets on Stop / node
    /// removal.
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    ndi_last_send: Option<std::time::Instant>,

    /// Phase 5 v1: `true` while the OBS-bridge setup modal is open.
    /// Resets on sink change so a quick dropdown switch can't leave the
    /// modal stranded. The "Don't show again" preference itself lives
    /// in `Settings::vcam_modal_dismissed` — once dismissed, the modal
    /// never opens again and the sink redirects to Syphon directly.
    #[serde(skip)]
    vcam_modal_open: bool,

    /// Phase 6 file recorder. Codec selector index — 0=H.264, 1=HEVC,
    /// 2=ProRes (`crate::video_io::file_recorder::codec_from_byte`
    /// parses; unknown values fall back to H.264). Persists so a saved
    /// project re-loads with the user's last codec choice.
    #[serde(default)] pub recorder_codec: u8,

    /// Path of the next file to write. Persists across save/load so
    /// users don't re-pick the path each time they reopen a project.
    /// Empty means "use the auto-generated default" (filled in on
    /// Start).
    #[serde(default)] pub recorder_path: String,

    /// Active recorder. `Arc<Mutex<Option<...>>>` matches the
    /// Send + Sync shape used for syphon/ndi handles. Lazy-created on
    /// the first frame after Start; dropped on Stop / sink change /
    /// node removal. `Drop` SIGKILLs ffmpeg; for a clean file the
    /// caller must hand the recorder to `stop_graceful` on a worker
    /// thread (handled in `render_filerec_sink`).
    #[serde(skip)]
    file_recorder: Arc<Mutex<Option<crate::video_io::file_recorder::FileRecorder>>>,

    /// Result channel from a graceful-stop worker thread. Polled by
    /// the UI tick to surface the outcome (clean exit / timeout) into
    /// `self.status`. `None` = no stop in flight.
    /// `crossbeam_channel::Receiver` is Send + Sync (std `mpsc::Receiver`
    /// is Send-only) so the whole node stays `NodeBehavior`-compliant
    /// without an extra Arc<Mutex<>> wrap.
    #[serde(skip)]
    stop_result_rx: Option<crossbeam_channel::Receiver<Result<(), String>>>,
}

impl Default for VideoOutNode {
    fn default() -> Self {
        Self {
            sink: VideoSink::Window,
            destination: String::new(),
            fullscreen: false,
            fps_cap: 0.0,
            enabled: false,
            status: String::new(),
            #[cfg(target_os = "macos")]
            syphon_server: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            ndi_sender: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            ndi_scratch: Vec::new(),
            #[cfg(target_os = "macos")]
            ndi_last_send: None,
            vcam_modal_open: false,
            recorder_codec: 0,
            recorder_path: String::new(),
            file_recorder: Arc::new(Mutex::new(None)),
            stop_result_rx: None,
        }
    }
}

impl Clone for VideoOutNode {
    /// Cloned copy starts cold: fresh sink state, no running server /
    /// sender (they're re-created on first publish tick if `enabled`
    /// comes back on). Matches `DynNode::clone`'s serialize→deserialize
    /// round-trip shape.
    fn clone(&self) -> Self {
        Self {
            sink: self.sink,
            destination: self.destination.clone(),
            fullscreen: self.fullscreen,
            fps_cap: self.fps_cap,
            enabled: false,
            status: String::new(),
            #[cfg(target_os = "macos")]
            syphon_server: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            ndi_sender: Arc::new(Mutex::new(None)),
            #[cfg(target_os = "macos")]
            ndi_scratch: Vec::new(),
            #[cfg(target_os = "macos")]
            ndi_last_send: None,
            vcam_modal_open: false,
            recorder_codec: self.recorder_codec,
            recorder_path: self.recorder_path.clone(),
            file_recorder: Arc::new(Mutex::new(None)),
            stop_result_rx: None,
        }
    }
}

impl NodeBehavior for VideoOutNode {
    fn title(&self) -> &str { "Video Out" }
    fn inputs(&self) -> Vec<PortDef> { vec![PortDef::new("Image", PortKind::Image)] }
    fn outputs(&self) -> Vec<PortDef> { vec![] }
    fn color_hint(&self) -> [u8; 3] { [200, 100, 255] }
    fn type_tag(&self) -> &str { "video_out" }

    /// We draw the Image input port ourselves inside `render_with_context`
    /// via `inline_port_circle` — tell the node framework to skip the
    /// default auto-rendered port header so we don't get two "Image"
    /// rows stacked on top of each other.
    fn inline_ports(&self) -> bool { true }

    /// Window sink is GPU-resident (draws via `show_image_gpu`). Only
    /// flip to `true` once a CPU-requiring sink (virtual camera
    /// IOSurface, file record) is wired up.
    ///
    /// `Ndi` used to return `true` here — which forced the upstream
    /// to run `readback_texture` and normalise BGRA→RGBA, only for
    /// NDI Out to swizzle RGBA→BGRA again before `send_video_v2`.
    /// Two full passes over multi-MB buffers per frame. Now `Ndi`
    /// returns `false` and `render_ndi_sink` pulls the texture out
    /// of the GPU cache itself via the fused `readback_texture_bgra`
    /// helper — single pass, zero work when source is already BGRA.
    /// The CPU fallback (Camera → NDI with no intermediate GPU node)
    /// still works because Camera emits `PortValue::Image` directly.
    fn needs_cpu_image_input(&self, _port: usize) -> bool {
        matches!(self.sink, VideoSink::VirtualCamera | VideoSink::FileRecord)
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> { vec![] }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<VideoOutNode>(state.clone()) {
            self.sink = l.sink;
            self.destination = l.destination;
            self.fullscreen = l.fullscreen;
            self.fps_cap = l.fps_cap;
            self.recorder_codec = l.recorder_codec;
            self.recorder_path = l.recorder_path;
            // `enabled` / `status` / `file_recorder` intentionally not
            // restored — a saved project never auto-resumes recording.
        }
    }

    fn on_removed(&mut self, _node_id: NodeId) {
        // Window sink: disabling `enabled` is enough — the viewport is
        // an egui child of this render pass and goes away once we stop
        // calling `show_viewport_immediate`.
        self.enabled = false;
        // Syphon sink: Drop on SyphonServer runs -stop, which
        // unregisters the server from the directory so TD / OBS drop
        // our name from their pickers within one announce cycle.
        //
        // NDI sink: Drop on NdiSender runs NDIlib_send_destroy, so
        // Studio Monitor / OBS NDI In drop our entry within one NDI
        // announce cycle (~2–3 s on LAN).
        #[cfg(target_os = "macos")]
        {
            if let Ok(mut guard) = self.syphon_server.lock() {
                *guard = None;
            }
            if let Ok(mut guard) = self.ndi_sender.lock() {
                *guard = None;
            }
            self.ndi_last_send = None;
        }
        // File recorder: Drop SIGKILLs ffmpeg. The output file may be
        // truncated; `+frag_keyframe+empty_moov` keeps it playable to
        // the last keyframe. For a clean file the user should hit Stop
        // before deleting the node.
        if let Ok(mut guard) = self.file_recorder.lock() {
            *guard = None;
        }
        self.stop_result_rx = None;
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // ── Input port row ─────────────────────────────────────────
        let wired = ctx
            .connections
            .iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let input_val = if wired {
            Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0)
        } else {
            PortValue::None
        };

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, ctx.node_id, 0, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from,
                ctx.pending_disconnects, PortKind::Image,
            );
            ui.label(egui::RichText::new("Image").small());
            if wired {
                let dims = match &input_val {
                    PortValue::Image(img) => Some((img.width, img.height)),
                    PortValue::GpuImage(h) => Some((h.width, h.height)),
                    _ => None,
                };
                if let Some((w, h)) = dims {
                    ui.label(egui::RichText::new(format!("{}×{}", w, h)).small().color(dim));
                }
            }
        });

        ui.separator();

        // ── Destination dropdown ───────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Destination").small());
            let prev = self.sink;
            egui::ComboBox::from_id_salt(egui::Id::new(("vout_sink", ctx.node_id)))
                .selected_text(self.sink.label())
                .width(170.0)
                .show_ui(ui, |ui| {
                    for s in VideoSink::all() {
                        // NDI when disabled means the Runtime is missing
                        // (shippable, just needs the user to install it).
                        // Other disabled sinks are "future phase" — show
                        // the phase tag so it's clear what's pending.
                        // Type annotation on `tooltip` because on
                        // non-macOS the Some(...) arm below is cfg'd
                        // out, leaving None with no inference target.
                        let (text, tooltip): (String, Option<&'static str>) = if s.available() {
                            (s.label().to_string(), None)
                        } else {
                            #[cfg(target_os = "macos")]
                            {
                                if matches!(s, VideoSink::Ndi) {
                                    (
                                        format!("{}  (install NDI Runtime)", s.label()),
                                        Some(
                                            "NDI Runtime not installed.\n\
                                             Install the free NDI Tools package\n\
                                             from https://ndi.video/tools/.",
                                        ),
                                    )
                                } else {
                                    (
                                        format!("{}  ({})", s.label(), s.phase_hint()),
                                        None,
                                    )
                                }
                            }
                            #[cfg(not(target_os = "macos"))]
                            {
                                (format!("{}  ({})", s.label(), s.phase_hint()), None)
                            }
                        };
                        ui.add_enabled_ui(s.available(), |ui| {
                            let resp = ui.selectable_value(&mut self.sink, s, text);
                            if let Some(t) = tooltip {
                                resp.on_hover_text(t);
                            }
                        });
                    }
                });
            if self.sink != prev {
                // Switching sinks: close any open viewport / stop any
                // running publisher.
                self.enabled = false;
                self.status = String::new();
                #[cfg(target_os = "macos")]
                {
                    if let Ok(mut guard) = self.syphon_server.lock() {
                        *guard = None;
                    }
                    if let Ok(mut guard) = self.ndi_sender.lock() {
                        *guard = None;
                    }
                    self.ndi_last_send = None;
                }
                // Re-entrancy guard: a fast sequence like Window →
                // VirtualCamera → Syphon should never leave the modal
                // hovering over an unrelated sink.
                self.vcam_modal_open = false;
                // File recorder: tear down (Drop SIGKILLs ffmpeg). A
                // sink switch mid-record loses the file's tail but
                // `+frag_keyframe` keeps the partial playable.
                if let Ok(mut guard) = self.file_recorder.lock() {
                    *guard = None;
                }
                self.stop_result_rx = None;
                // Carry forward publish-name + fps defaults when
                // switching INTO Syphon / NDI so the user isn't greeted
                // with a blank text field + 0 fps cap.
                if matches!(self.sink, VideoSink::Syphon | VideoSink::Ndi)
                    && self.destination.is_empty()
                {
                    self.destination = "PatchWork".into();
                }
                if self.sink == VideoSink::Ndi && self.fps_cap <= 0.0 {
                    self.fps_cap = 60.0;
                }
            }
        });

        // ── Sink‑specific UI ───────────────────────────────────────
        match self.sink {
            VideoSink::Window => self.render_window_sink(ui, ctx, &input_val),
            #[cfg(target_os = "macos")]
            VideoSink::Syphon => self.render_syphon_sink(ui, ctx, &input_val),
            #[cfg(target_os = "macos")]
            VideoSink::Ndi => self.render_ndi_sink(ui, ctx, &input_val),
            #[cfg(target_os = "macos")]
            VideoSink::VirtualCamera => self.render_vcam_sink(ui, ctx),
            VideoSink::FileRecord => self.render_filerec_sink(ui, ctx, &input_val),
            _ => {
                ui.add_space(4.0);
                ui.colored_label(
                    dim,
                    egui::RichText::new(format!(
                        "{} — shipping in {}.\nSee docs/video_io_spec.md",
                        self.sink.label(),
                        self.sink.phase_hint(),
                    )).small(),
                );
                ui.add_space(4.0);
            }
        }
    }
}

impl VideoOutNode {
    /// Phase 2 Syphon sink (macOS only). Publishes the upstream GPU
    /// texture to a `SyphonMetalServer` so any subscriber app
    /// (TouchDesigner, Resolume, OBS, …) can consume it with zero
    /// CPU readback.
    ///
    /// Lifecycle: server is created lazily on the first frame where
    /// `enabled = true` *and* an upstream texture exists. Rename /
    /// disable / sink-switch / node-removal all tear it down so TD's
    /// picker stays in sync.
    #[cfg(target_os = "macos")]
    fn render_syphon_sink(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &mut RenderContext,
        input_val: &PortValue,
    ) {
        use crate::gpu_image;
        use crate::video_io::{syphon::SyphonServer, wgpu_metal};

        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // Clear transient status each frame. Only genuine errors (set
        // below on the `Err` paths) survive to the next render tick,
        // because they bail with `return` before we'd overwrite.
        // Without this, a first-frame "No input connected" latches
        // forever even after the wire commits.
        self.status.clear();

        // ── Publish name text field ────────────────────────────────
        let mut name_changed = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Publish as").small())
                .on_hover_text(
                    "Name shown in TouchDesigner / Resolume / OBS pickers.\n\n\
                     Note: when you quit and reopen PatchWork, Syphon gives \
                     the server a new internal id. Consumer apps that were \
                     subscribed will keep the name displayed but stop \
                     receiving frames — re-pick the sender in their dropdown \
                     to reconnect. Standard Syphon ecosystem behaviour.",
                );
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.destination)
                    .desired_width(180.0)
                    .hint_text("PatchWork"),
            );
            // Only react on edit commit (Enter / focus loss) so we
            // don't tear down the server on every keystroke.
            if resp.lost_focus() || resp.ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                name_changed = true;
            }
        });
        if name_changed {
            if let Ok(mut guard) = self.syphon_server.lock() {
                // Drop the old server so the next tick re-creates with
                // the new name.
                *guard = None;
            }
        }

        // ── Broadcast toggle + state indicator ─────────────────────
        // Indicator reflects real state, not just `enabled`:
        //   server not yet created → dim "ready"
        //   server up, 0 clients   → amber "waiting for subscribers"
        //   server up, ≥1 client   → green "● live"
        let mut toggle_clicked = false;
        ui.horizontal(|ui| {
            let label = if self.enabled { "● Stop" } else { "▶ Broadcast" };
            if ui.button(label).clicked() { toggle_clicked = true; }
            if self.enabled {
                let (has_server, has_clients) = self
                    .syphon_server
                    .lock()
                    .ok()
                    .map(|g| match g.as_ref() {
                        Some(s) => (true, s.has_clients()),
                        None => (false, false),
                    })
                    .unwrap_or((false, false));
                let (text, color) = if !has_server {
                    ("ready", dim)
                } else if has_clients {
                    ("● live", egui::Color32::from_rgb(80, 200, 80))
                } else {
                    ("waiting for subscribers", egui::Color32::from_rgb(200, 180, 80))
                };
                ui.colored_label(color, egui::RichText::new(text).small());
            }
        });
        if toggle_clicked {
            self.enabled = !self.enabled;
            if !self.enabled {
                if let Ok(mut guard) = self.syphon_server.lock() {
                    *guard = None;
                }
            }
        }

        if !self.status.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 100, 100),
                egui::RichText::new(&self.status).small(),
            );
        }

        if !self.enabled {
            return;
        }

        // ── Resolve upstream GPU texture ───────────────────────────
        // Use the Image input wire to find the producer's (node, port),
        // then look it up in the per-frame snapshot populated by
        // GpuTextureCache::begin_frame. The upstream must be a
        // GPU-producing node (WGSL Viewer, Blend, ImageEffects, Camera
        // with GPU upload) or a CPU-only path that's been uploaded
        // into the cache.
        let src = ctx
            .connections
            .iter()
            .find(|c| c.to_node == ctx.node_id && c.to_port == 0)
            .map(|c| (c.from_node, c.from_port));
        let Some((src_node, src_port)) = src else {
            // Transient — can happen for ~1 frame after the user drags
            // a new wire (pending_connections takes a tick to flush).
            // Don't latch an error; just wait.
            ui.ctx().request_repaint();
            return;
        };
        let Some((tex, w, h)) = gpu_image::frame_snapshot_get_texture(src_node, src_port) else {
            // 1-frame latency is normal here on the very first
            // publishable tick. Fall through silently.
            let _ = input_val;
            ui.ctx().request_repaint();
            return;
        };

        // ── Lazy-init SyphonServer ─────────────────────────────────
        let Some(render_state) = ctx.wgpu_render_state else {
            self.status = "wgpu render state unavailable".into();
            return;
        };
        let Ok(mut guard) = self.syphon_server.lock() else { return; };
        if guard.is_none() {
            let Some(mtl_device) = wgpu_metal::wgpu_device_to_mtl(&render_state.device) else {
                self.status = "Not on Metal backend".into();
                self.enabled = false;
                return;
            };
            let name = if self.destination.trim().is_empty() {
                "PatchWork"
            } else {
                self.destination.trim()
            };
            match SyphonServer::new(name, &mtl_device) {
                Ok(s) => *guard = Some(s),
                Err(e) => {
                    self.status = format!("Error: {}", e);
                    self.enabled = false;
                    return;
                }
            }
        }
        let Some(server) = guard.as_ref() else { return; };

        // ── Bridge wgpu::Texture → MTLTexture and publish ──────────
        let Some(mtl_tex) = wgpu_metal::wgpu_texture_to_mtl(&tex) else {
            self.status = "as_hal returned None (non-Metal?)".into();
            return;
        };
        // `metal::Texture: Deref<Target = TextureRef>` so `&*` yields
        // the borrowed &TextureRef that SyphonServer::publish wants.
        // `flipped: true` tells Syphon our texture is top-left origin
        // (Metal/egui convention) — Syphon then flips it for OpenGL-
        // style consumers (TouchDesigner's Syphon Spout In TOP, most
        // VJ apps). Without this, TD shows the image upside-down.
        server.publish(&*mtl_tex, w, h, true);

        // Keep egui repainting so the publish loop keeps ticking at
        // monitor refresh. Without this the node would go idle the
        // moment the user stopped moving the mouse.
        ui.ctx().request_repaint();

        ui.add_space(2.0);
        ui.colored_label(dim, egui::RichText::new(format!("{}×{}", w, h)).small());
    }

    /// Phase 3 NDI sink (macOS only for now — libloading FFI is ready
    /// for Windows / Linux once we un-gate `src/video_io/ndi.rs`).
    /// Publishes the upstream `PortValue::Image` (BGRA-swizzled from
    /// RGBA) to an `NdiSender` so any NDI consumer on the LAN
    /// (NDI Studio Monitor, OBS NDI In, Resolume, Zoom) can subscribe.
    ///
    /// `needs_cpu_image_input(0) == true` for `Ndi` (set at
    /// `video_out_node.rs` :needs_cpu_image_input), so the upstream
    /// image arrives here as `PortValue::Image(Arc<ImageData>)` with
    /// CPU pixels ready. No GPU readback logic lives in this function;
    /// it's handled by the shared `has_cpu_consumer` plumbing in
    /// `app/mod.rs`.
    ///
    /// Lifecycle: sender lazy-created on the first enabled frame with
    /// a valid upstream. Rename / disable / sink-switch /
    /// `on_removed` all drop it so Studio Monitor's picker stays in sync.
    #[cfg(target_os = "macos")]
    fn render_ndi_sink(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &mut RenderContext,
        input_val: &PortValue,
    ) {
        use crate::gpu_image;
        use crate::video_io::{ndi, pixel_swizzle};

        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // Clear transient status each frame — only genuine error paths
        // below set it. Mirrors the M4.c fix on `render_syphon_sink`.
        self.status.clear();

        // ── Gate: runtime must be installed ────────────────────────
        // Same reasoning as `render_ndi_ui` — user may land here via
        // a loaded project or after uninstalling the runtime mid-session.
        // Skip the entire publish UI and surface an install hint so
        // nobody's left wondering why "Broadcast" does nothing.
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
                     https://ndi.video/tools/ to enable NDI publishing.",
                )
                .small(),
            );
            return;
        }

        // ── Publish name text field ────────────────────────────────
        let mut name_changed = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Publish as").small())
                .on_hover_text(
                    "Appears in NDI Studio Monitor / OBS NDI Source / \
                     Resolume pickers as `HOSTNAME (Publish Name)`.\n\n\
                     When PatchWork restarts, receivers that were \
                     subscribed will keep the name visible but stop \
                     receiving frames — re-pick the sender in their \
                     dropdown to reconnect. Standard NDI behaviour.",
                );
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.destination)
                    .desired_width(180.0)
                    .hint_text("PatchWork"),
            );
            if resp.lost_focus() || resp.ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                name_changed = true;
            }
        });
        if name_changed {
            // Drop the old sender so the next tick re-creates with the
            // new publish name. Matches render_syphon_sink behaviour.
            if let Ok(mut guard) = self.ndi_sender.lock() {
                *guard = None;
            }
            self.ndi_last_send = None;
        }

        // ── FPS cap DragValue ──────────────────────────────────────
        // NDI consumers expect reasonable frame pacing — allow 5–120 Hz
        // with 60 as the sensible default. Legacy projects with
        // `fps_cap == 0.0` get bumped to 60 when switching to NDI
        // (handled in the sink-change branch above).
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("fps cap").small())
                .on_hover_text(
                    "Max frames-per-second to publish. NDI senders \
                     that flood faster than receivers can decode cause \
                     visible jitter — 60 is a safe default, drop lower \
                     if the upstream is slower than that.",
                );
            ui.add(
                egui::DragValue::new(&mut self.fps_cap)
                    .range(5.0..=120.0)
                    .speed(1.0)
                    .suffix(" Hz"),
            );
        });

        // ── Broadcast toggle + state indicator ─────────────────────
        // Mirrors render_syphon_sink's three-state machine:
        //   sender not yet created → dim "ready"
        //   sender up, 0 conns     → amber "waiting for receivers"
        //   sender up, ≥1 conn     → green "● live (N)"
        let mut toggle_clicked = false;
        ui.horizontal(|ui| {
            let label = if self.enabled { "● Stop" } else { "▶ Broadcast" };
            if ui.button(label).clicked() {
                toggle_clicked = true;
            }
            if self.enabled {
                let (has_sender, conns) = self
                    .ndi_sender
                    .lock()
                    .ok()
                    .map(|g| match g.as_ref() {
                        Some(s) => (true, s.connection_count(0)),
                        None => (false, 0),
                    })
                    .unwrap_or((false, 0));
                let (text, color) = if !has_sender {
                    ("ready".to_string(), dim)
                } else if conns > 0 {
                    (
                        format!("● live ({})", conns),
                        egui::Color32::from_rgb(80, 200, 80),
                    )
                } else {
                    (
                        "waiting for receivers".into(),
                        egui::Color32::from_rgb(200, 180, 80),
                    )
                };
                ui.colored_label(color, egui::RichText::new(text).small());
            }
        });
        if toggle_clicked {
            self.enabled = !self.enabled;
            if !self.enabled {
                if let Ok(mut guard) = self.ndi_sender.lock() {
                    *guard = None;
                }
                self.ndi_last_send = None;
            }
        }

        // Heartbeat: "last send: 12ms ago" — 1 Hz visible feedback that
        // the publish path is actually running. Important because
        // `NDIlib_send_send_video_v2` is `void` (no return code) and
        // silent failure otherwise looks identical to "waiting for
        // receivers". Pre-planned per the Phase 2 M4.c lesson.
        if self.enabled {
            if let Some(t) = self.ndi_last_send {
                let since = t.elapsed();
                ui.colored_label(
                    dim,
                    egui::RichText::new(format!(
                        "last send: {} ms ago",
                        since.as_millis()
                    ))
                    .small(),
                );
            }
        }

        if !self.status.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 100, 100),
                egui::RichText::new(&self.status).small(),
            );
        }

        if !self.enabled {
            return;
        }

        // ── Resolve upstream pixels (prefer GPU path) ──────────────
        //
        // Two source shapes are possible with `needs_cpu_image_input
        // = false` for Ndi:
        //   - `PortValue::Image` — upstream is CPU-native (Camera /
        //     Screen / Video File / NDI In). We swizzle RGBA→BGRA
        //     into `ndi_scratch`.
        //   - `PortValue::GpuImage` — upstream is GPU-resident
        //     (Transform, WGSL Viewer, Blend, …). We do the readback
        //     ourselves via `readback_texture_bgra`, which fuses the
        //     copy + (conditional) swizzle into one pass. If the
        //     texture is already `Bgra8Unorm` (Transform's Phase 2.5
        //     default) we ship those bytes untouched — zero swizzle.
        //   - `PortValue::None` — 1-frame transient after wiring, or
        //     upstream hasn't produced yet. Wait.
        let publish_dims: (u32, u32) = match input_val {
            PortValue::Image(arc) => {
                if arc.width == 0 || arc.height == 0 || arc.pixels.is_empty() {
                    ui.ctx().request_repaint();
                    return;
                }
                let expected = (arc.width as usize) * (arc.height as usize) * 4;
                if arc.pixels.len() < expected {
                    self.status = "upstream frame truncated".into();
                    ui.ctx().request_repaint();
                    return;
                }
                // CPU fast-path: one copy + one swizzle, same as
                // before the optimisation.
                if self.ndi_scratch.len() != expected {
                    self.ndi_scratch.resize(expected, 0);
                }
                self.ndi_scratch.copy_from_slice(&arc.pixels[..expected]);
                pixel_swizzle::rgba_to_bgra_in_place(&mut self.ndi_scratch);
                (arc.width, arc.height)
            }
            PortValue::GpuImage(handle) => {
                let Some(render_state) = ctx.wgpu_render_state else {
                    self.status = "wgpu render state unavailable".into();
                    return;
                };
                // Resolve the actual texture in the per-frame snapshot.
                // Missing = 1-frame transient (upstream just wired, or
                // publish queue hasn't drained yet). Wait.
                let Some((tex, w, h)) =
                    gpu_image::frame_snapshot_get_texture(handle.node_id, handle.port)
                else {
                    ui.ctx().request_repaint();
                    return;
                };
                // Fused readback into scratch. If `tex.format()` is
                // already BGRA, this is effectively just a wgpu
                // copy_texture_to_buffer + one row-wise memcpy per
                // row. If it's RGBA, it additionally swaps R↔B
                // inline during the extraction — still a single pass.
                let bgra = gpu_image::readback_texture_bgra(
                    &render_state.device, &render_state.queue, &tex, w, h,
                );
                let expected = (w as usize) * (h as usize) * 4;
                if bgra.len() != expected {
                    self.status = "GPU readback size mismatch".into();
                    ui.ctx().request_repaint();
                    return;
                }
                self.ndi_scratch = bgra;
                (w, h)
            }
            _ => {
                ui.ctx().request_repaint();
                return;
            }
        };
        let (w, h) = publish_dims;

        // ── fps cap ────────────────────────────────────────────────
        let fps_cap = self.fps_cap.max(5.0).min(120.0);
        let frame_budget = std::time::Duration::from_secs_f32(1.0 / fps_cap);
        if let Some(t) = self.ndi_last_send {
            if t.elapsed() < frame_budget {
                ui.ctx().request_repaint();
                return;
            }
        }

        // ── Lazy-init NdiSender ────────────────────────────────────
        let Ok(mut guard) = self.ndi_sender.lock() else { return; };
        if guard.is_none() {
            let name = if self.destination.trim().is_empty() {
                "PatchWork"
            } else {
                self.destination.trim()
            };
            match ndi::NdiSender::new(name) {
                Ok(s) => *guard = Some(s),
                Err(e) => {
                    self.status = format!("Error: {}", e);
                    self.enabled = false;
                    return;
                }
            }
        }
        let Some(sender) = guard.as_ref() else { return; };

        // ── Publish ────────────────────────────────────────────────
        let (rn, rd) = rational_fps(fps_cap);
        let publish = sender.publish_bgra(w, h, &self.ndi_scratch, rn, rd);
        if let Err(e) = publish {
            self.status = format!("publish error: {}", e);
        } else {
            self.ndi_last_send = Some(std::time::Instant::now());
        }
        drop(guard);

        ui.ctx().request_repaint();

        ui.add_space(2.0);
        ui.colored_label(
            dim,
            egui::RichText::new(format!("{}×{}", w, h)).small(),
        );
    }

    /// Phase 1 Window sink: display picker + fullscreen checkbox +
    /// Enable toggle. The viewport is an egui child spawned via
    /// `show_viewport_immediate`; it lives exactly as long as we keep
    /// calling that function each frame, so toggling `enabled = false`
    /// or removing the node closes it automatically.
    fn render_window_sink(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &mut RenderContext,
        input_val: &PortValue,
    ) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let displays = crate::display::enumerate_displays();

        // Resolve the currently-selected display (by id), defaulting to
        // the primary if the id is empty or stale.
        let chosen = displays
            .iter()
            .find(|d| d.id == self.destination)
            .or_else(|| displays.iter().find(|d| d.is_primary))
            .or_else(|| displays.first());

        // ── Display picker ─────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Display").small());
            // Collapsed button text keeps just the name + primary star
            // (truncated) — the full `pretty_label` with resolution
            // lives inside the dropdown so the node itself stays narrow.
            let collapsed = chosen
                .map(|d| {
                    let star = if d.is_primary { " ★" } else { "" };
                    format!("{}{}", crate::nodes::truncate_chars(&d.name, 18), star)
                })
                .unwrap_or_else(|| "None".into());
            egui::ComboBox::from_id_salt(egui::Id::new(("vout_display", ctx.node_id)))
                .selected_text(collapsed)
                .width(180.0)
                .show_ui(ui, |ui| {
                    if displays.is_empty() {
                        ui.label(egui::RichText::new("No displays detected").small());
                    }
                    for d in &displays {
                        let selected = d.id == self.destination
                            || (self.destination.is_empty() && d.is_primary);
                        if ui.selectable_label(selected, d.pretty_label()).clicked() {
                            self.destination = d.id.clone();
                        }
                    }
                });
        });

        // ── Fullscreen + Enable row ────────────────────────────────
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.fullscreen, "Fullscreen");
            let button_label = if self.enabled { "● Close" } else { "▶ Open" };
            if ui.button(button_label).clicked() {
                self.enabled = !self.enabled;
                if self.enabled {
                    self.status = match self.sink {
                        VideoSink::Window => "Open".into(),
                        _ => String::new(),
                    };
                } else {
                    self.status = String::new();
                }
            }
        });

        // ── Status ─────────────────────────────────────────────────
        if !self.status.is_empty() {
            let color = if self.status.contains("Error") {
                egui::Color32::from_rgb(255, 100, 100)
            } else if self.enabled {
                egui::Color32::from_rgb(80, 200, 80)
            } else {
                egui::Color32::from_rgb(150, 150, 150)
            };
            ui.colored_label(color, egui::RichText::new(&self.status).small());
        }

        if !self.enabled {
            return;
        }

        // ── Spawn viewport ─────────────────────────────────────────
        // Keep it alive by calling show_viewport_immediate every frame
        // while `enabled = true`. Stopping the call (enabled = false,
        // sink change, node removed) tears the viewport down.
        let Some(display) = chosen else {
            self.enabled = false;
            self.status = "No display".into();
            return;
        };

        // Normalise to an `Arc<ImageData>` + optional GPU source, same
        // shape `show_image_gpu` expects.
        let img_and_src: Option<(Arc<ImageData>, Option<(NodeId, usize)>)> = match input_val {
            PortValue::Image(img) => {
                let gpu_source = ctx.connections.iter()
                    .find(|c| c.to_node == ctx.node_id && c.to_port == 0)
                    .map(|c| (c.from_node, c.from_port));
                Some((img.clone(), gpu_source))
            }
            PortValue::GpuImage(handle) => {
                let stub = Arc::new(ImageData {
                    width: handle.width, height: handle.height, pixels: Vec::new(),
                });
                Some((stub, Some((handle.node_id, handle.port))))
            }
            _ => None,
        };

        // If nothing's wired, leave the viewport alive but showing
        // black — easier to debug than silently disabling on the
        // producer side being momentarily empty (e.g. first frame).
        let (img_for_viewport, src_for_viewport) = match img_and_src {
            Some(x) => (Some(x.0), x.1),
            None => (None, None),
        };

        let mut builder = egui::ViewportBuilder::default()
            .with_title(format!("Video Out #{}", ctx.node_id))
            .with_position([display.origin.0 as f32, display.origin.1 as f32])
            .with_inner_size([display.size.0 as f32, display.size.1 as f32]);
        if self.fullscreen {
            builder = builder.with_fullscreen(true).with_decorations(false);
        }

        let viewport_id = egui::ViewportId::from_hash_of(("vout_vp", ctx.node_id));
        let nid = ctx.node_id;

        // `show_viewport_immediate` re-runs this closure every frame.
        // Inside it we need to know whether the user closed or hit
        // Escape, and propagate back to `self.enabled`. Use an egui
        // temp flag as the bridge (the closure can only &mut borrow
        // things with `'static`).
        let close_flag_id = egui::Id::new(("vout_close", nid));
        ui.ctx().show_viewport_immediate(
            viewport_id,
            builder,
            move |ctx, _class| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                    .show(ctx, |ui| {
                        let avail = ui.available_size();
                        if let Some(img) = img_for_viewport.as_ref() {
                            let img_aspect = img.width as f32 / img.height.max(1) as f32;
                            let screen_aspect = avail.x / avail.y.max(1.0);
                            let (draw_w, draw_h) = if img_aspect > screen_aspect {
                                (avail.x, avail.x / img_aspect)
                            } else {
                                (avail.y * img_aspect, avail.y)
                            };
                            let pad_x = (avail.x - draw_w) / 2.0;
                            let pad_y = (avail.y - draw_h) / 2.0;
                            if pad_y > 0.0 { ui.add_space(pad_y); }
                            ui.horizontal(|ui| {
                                if pad_x > 0.0 { ui.add_space(pad_x); }
                                crate::nodes::image_node::show_image_gpu(
                                    ui, nid, img, draw_w, src_for_viewport,
                                );
                            });
                        }
                        // Otherwise leave it black — upstream is empty.
                    });
                if ctx.input(|i| i.viewport().close_requested())
                    || ctx.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    ctx.data_mut(|d| d.insert_temp(close_flag_id, true));
                }
                ctx.request_repaint();
            },
        );

        // Drain the close flag from egui data so the next frame
        // observes the teardown request.
        let closed = ui
            .ctx()
            .data_mut(|d| d.remove_temp::<bool>(close_flag_id))
            .unwrap_or(false);
        if closed {
            self.enabled = false;
            self.status = String::new();
        }
    }

    /// Phase 5 v1: Virtual Camera shim. Real CMIO Camera Extension
    /// (`.systemextension`) is out of scope (see `docs/video_io_spec.md`
    /// §7.4 — needs Apple Developer ID + entitlement, ≥5 weeks of work).
    /// This UI redirects users to the OBS Virtual Camera + Syphon
    /// Client bridge: a one-time modal explains the 3 setup steps; on
    /// Confirm the node's sink is auto-flipped to `Syphon` with publish
    /// name "PatchWork" so OBS Syphon Client picks it up.
    ///
    /// "Don't show again" persists in `Settings::vcam_modal_dismissed`
    /// — once set, picking Virtual Camera redirects without UI.
    #[cfg(target_os = "macos")]
    fn render_vcam_sink(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        // Fast path: if the user has dismissed the modal in a prior
        // session, redirect the sink to Syphon immediately. The
        // dropdown already drew "Virtual Camera" this frame so there's
        // a single-frame flash; the next frame's dispatch lands in
        // `render_syphon_sink` with publish name "PatchWork".
        let dismissed = crate::settings::Settings::load().vcam_modal_dismissed;
        if dismissed {
            self.sink = VideoSink::Syphon;
            self.destination = "PatchWork".into();
            // Drop any prior Syphon server so the next tick re-creates
            // with the canonical name (the existing sink-change branch
            // already ran when the user picked Virtual Camera, but be
            // defensive in case a future caller jumps in via load_state).
            if let Ok(mut g) = self.syphon_server.lock() {
                *g = None;
            }
            ui.ctx().request_repaint();
            return;
        }

        // Modal stays open across frames once armed. `vcam_modal_open`
        // is a `#[serde(skip)]` bool — re-opens automatically on a
        // fresh render of an undismissed modal.
        self.vcam_modal_open = true;

        // egui has no native modal primitive in 0.31; we get close with
        // a centered foreground Window plus a click-outside drainer
        // baked into Confirm/Cancel button handling. The "Don't show
        // again" checkbox state lives in egui temp data so it persists
        // across frames without polluting the serialized node.
        let dont_show_id = egui::Id::new(("vcam_modal_dont_show", ctx.node_id));
        let mut dont_show = ui
            .ctx()
            .data(|d| d.get_temp::<bool>(dont_show_id))
            .unwrap_or(false);

        let mut confirm_clicked = false;
        let mut cancel_clicked = false;

        egui::Window::new("Set up Virtual Camera")
            .id(egui::Id::new(("vcam_modal", ctx.node_id)))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.set_min_width(420.0);
                ui.set_max_width(460.0);

                ui.label(
                    "To appear as a webcam in Zoom / Meet / etc., PatchWork \
                     needs a system bridge. We recommend OBS:",
                );
                ui.add_space(6.0);
                ui.label("1. Install OBS Studio.");
                ui.label("2. Add Source → \"Syphon Client\" → pick \"PatchWork\".");
                ui.label("3. Tools → \"Start Virtual Camera\".");
                ui.add_space(6.0);
                ui.label("Zoom / Meet will now see your PatchWork output.");
                ui.add_space(10.0);

                ui.checkbox(&mut dont_show, "Don't show this again");
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel_clicked = true;
                    }
                    ui.add_space(6.0);
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new("Confirm").strong(),
                        ))
                        .clicked()
                    {
                        confirm_clicked = true;
                    }
                });

                // Esc dismisses too — common modal expectation.
                if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel_clicked = true;
                }
            });

        ui.ctx().data_mut(|d| d.insert_temp(dont_show_id, dont_show));

        if confirm_clicked {
            // Persist the "don't show again" preference. Load-mutate-
            // save runs on the UI thread so it can't race with the
            // Settings node's own writes.
            if dont_show {
                let mut s = crate::settings::Settings::load();
                s.vcam_modal_dismissed = true;
                let _ = s.save();
            }
            self.sink = VideoSink::Syphon;
            self.destination = "PatchWork".into();
            self.vcam_modal_open = false;
            // Drop the egui temp checkbox state — next time we open
            // the modal (if user un-dismisses via settings), it starts
            // unchecked.
            ui.ctx().data_mut(|d| d.remove_temp::<bool>(dont_show_id));
        } else if cancel_clicked {
            // Revert to Window — never leave the user staring at a
            // disabled modal-only sink.
            self.sink = VideoSink::Window;
            self.vcam_modal_open = false;
            ui.ctx().data_mut(|d| d.remove_temp::<bool>(dont_show_id));
        }
    }

    /// Phase 6 file recorder sink. Pipes the upstream `Image` (RGBA)
    /// into an ffmpeg subprocess. See `crate::video_io::file_recorder`
    /// for the codec / container rules and graceful-vs-hard stop
    /// semantics.
    ///
    /// Lifecycle:
    /// - **Start** click: marks `enabled = true`. The recorder is
    ///   lazy-created on the first frame so we can lock encoder dims to
    ///   the actual upstream resolution.
    /// - **Pause / Resume**: flips `paused` on the recorder. Frames are
    ///   dropped during pause; the output file is a single contiguous
    ///   stream (no segment splits).
    /// - **Stop** click: hands the recorder to a worker thread that
    ///   runs `stop_graceful` (close stdin → wait 5 s → SIGKILL on
    ///   timeout). The result lands back in `stop_result_rx` so the UI
    ///   never blocks on the moov flush.
    /// - **Sink change / node removal**: `Drop` SIGKILLs ffmpeg. Output
    ///   stays playable to the last keyframe via `+frag_keyframe`.
    fn render_filerec_sink(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &mut RenderContext,
        input_val: &PortValue,
    ) {
        use crate::video_io::file_recorder::{self as fr, Codec, FileRecorder};

        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // Phase 3 lesson: clear transient status each frame; only
        // genuine errors below set it. Without this, a first-frame
        // "Set a path" message latches forever after the user types
        // a path.
        self.status.clear();

        // Drain any pending graceful-stop result. If we don't, the
        // user's "Saved." or timeout message never reaches the UI.
        if let Some(rx) = self.stop_result_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    self.status = "Saved.".into();
                    self.stop_result_rx = None;
                }
                Ok(Err(e)) => {
                    self.status = format!("Stop: {}", e);
                    self.stop_result_rx = None;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    // Reaper still working. Repaint so we drain ASAP.
                    ui.ctx().request_repaint();
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.stop_result_rx = None;
                }
            }
        }

        let recorder_active = self
            .file_recorder
            .lock()
            .ok()
            .map(|g| g.is_some())
            .unwrap_or(false);
        let codec = fr::codec_from_byte(self.recorder_codec);

        // ── Codec dropdown ─────────────────────────────────────────
        // Locked while recording — switching codec mid-record would
        // require a brand-new ffmpeg process (different container,
        // different encoder). User must Stop first.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Codec").small())
                .on_hover_text(
                    "H.264 = max compatibility (any player).\n\
                     HEVC = ~half the file size, mac HW encoder.\n\
                     ProRes 422 = editor intermediate (Final Cut, DaVinci).",
                );
            ui.add_enabled_ui(!recorder_active, |ui| {
                let prev = self.recorder_codec;
                egui::ComboBox::from_id_salt(egui::Id::new(("vout_filerec_codec", ctx.node_id)))
                    .selected_text(codec.label())
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (b, c) in [
                            (0u8, Codec::H264Sw),
                            (1u8, Codec::HevcVt),
                            (2u8, Codec::ProResVt),
                        ] {
                            if ui
                                .selectable_label(self.recorder_codec == b, c.label())
                                .clicked()
                            {
                                self.recorder_codec = b;
                            }
                        }
                    });
                if self.recorder_codec != prev && !self.recorder_path.is_empty() {
                    // Codec switched in idle — keep the file stem but
                    // swap the extension so the user doesn't end up
                    // with a .mp4 ProRes (rejected) or .mov H.264.
                    let new_codec = fr::codec_from_byte(self.recorder_codec);
                    rewrite_extension(&mut self.recorder_path, new_codec.extension());
                }
            });
        });

        // ── Path field + Browse ────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Path").small());
            ui.add_enabled_ui(!recorder_active, |ui| {
                let hint = format!("PatchWork-recording.{}", codec.extension());
                ui.add(
                    egui::TextEdit::singleline(&mut self.recorder_path)
                        .desired_width(220.0)
                        .hint_text(hint),
                );
                if ui.button("…").on_hover_text("Pick output file").clicked() {
                    let initial = if self.recorder_path.trim().is_empty() {
                        default_recorder_path(codec, ctx.node_id)
                    } else {
                        std::path::PathBuf::from(self.recorder_path.trim())
                    };
                    let mut dlg = rfd::FileDialog::new()
                        .add_filter(codec.extension(), &[codec.extension()])
                        .set_file_name(
                            initial
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("PatchWork-recording")
                                .to_string(),
                        );
                    if let Some(parent) = initial.parent() {
                        if parent.is_dir() {
                            dlg = dlg.set_directory(parent);
                        }
                    }
                    if let Some(p) = dlg.save_file() {
                        let mut s = p.to_string_lossy().to_string();
                        rewrite_extension(&mut s, codec.extension());
                        self.recorder_path = s;
                    }
                }
            });
        });

        // ── Start / Pause / Resume / Stop row ──────────────────────
        // Show file size on disk (encoded bytes), not bytes_written
        // (raw RGBA pushed to ffmpeg, which is ~500× larger). Users
        // expect the chip to match what they see in Finder.
        let (paused, frames_w, file_bytes, secs) = self
            .file_recorder
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|r| (r.is_paused(), r.frames_written(), r.file_size_on_disk(), r.elapsed().as_secs()))
            })
            .unwrap_or((false, 0, 0, 0));

        ui.horizontal(|ui| {
            if recorder_active {
                if paused {
                    if ui.button("▶ Resume").clicked() {
                        if let Ok(mut g) = self.file_recorder.lock() {
                            if let Some(r) = g.as_mut() {
                                r.resume();
                            }
                        }
                    }
                } else {
                    if ui.button("⏸ Pause").clicked() {
                        if let Ok(mut g) = self.file_recorder.lock() {
                            if let Some(r) = g.as_mut() {
                                r.pause();
                            }
                        }
                    }
                }
                if ui.button("◼ Stop").clicked() {
                    // Hand the recorder to a worker thread for the
                    // graceful flush — UI thread can't afford the 5 s
                    // wait. Result comes back via `stop_result_rx`.
                    let opt = self
                        .file_recorder
                        .lock()
                        .ok()
                        .and_then(|mut g| g.take());
                    if let Some(rec) = opt {
                        let (tx, rx) = crossbeam_channel::bounded::<Result<(), String>>(1);
                        std::thread::spawn(move || {
                            let r = rec.stop_graceful();
                            let _ = tx.send(r);
                        });
                        self.stop_result_rx = Some(rx);
                    }
                    self.enabled = false;
                }
                let size_str = format_file_size(file_bytes);
                let m = secs / 60;
                let s = secs % 60;
                let (text, color) = if paused {
                    (
                        format!("⏸ paused ({} frames, {}, {:02}:{:02})", frames_w, size_str, m, s),
                        egui::Color32::from_rgb(200, 180, 80),
                    )
                } else {
                    (
                        format!("● recording ({} frames, {}, {:02}:{:02})", frames_w, size_str, m, s),
                        egui::Color32::from_rgb(80, 200, 80),
                    )
                };
                ui.colored_label(color, egui::RichText::new(text).small());
            } else {
                let saving = self.stop_result_rx.is_some();
                let start_enabled = !saving;
                ui.add_enabled_ui(start_enabled, |ui| {
                    if ui.button("● Start").on_hover_text("Begin recording").clicked() {
                        // Auto-fill path from default if user left it
                        // blank — single-click "just record" UX.
                        if self.recorder_path.trim().is_empty() {
                            let p = default_recorder_path(codec, ctx.node_id);
                            self.recorder_path = p.to_string_lossy().to_string();
                        }
                        self.enabled = true;
                    }
                });
                let (text, color) = if saving {
                    ("saving…", egui::Color32::from_rgb(200, 180, 80))
                } else {
                    ("ready", dim)
                };
                ui.colored_label(color, egui::RichText::new(text).small());
            }
        });

        if !self.status.is_empty() {
            let red = self.status.starts_with("Error");
            let color = if red {
                egui::Color32::from_rgb(255, 100, 100)
            } else {
                dim
            };
            ui.colored_label(color, egui::RichText::new(&self.status).small());
        }

        // ── Latest stderr line (amber, only while recording) ───────
        if recorder_active {
            if let Some(line) = self
                .file_recorder
                .lock()
                .ok()
                .and_then(|mut g| {
                    if let Some(r) = g.as_mut() {
                        r.pump_stderr();
                        let log = r.stderr_log();
                        if log.is_empty() {
                            None
                        } else {
                            Some(log.lines().last().unwrap_or("").to_string())
                        }
                    } else {
                        None
                    }
                })
            {
                if !line.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 180, 80),
                        egui::RichText::new(line).small(),
                    );
                }
            }
        }

        if !self.enabled {
            return;
        }

        // ── Frame pump ─────────────────────────────────────────────
        // Resolve the upstream pixels. With `needs_cpu_image_input(0)
        // == true` for FileRecord the framework's eval loop usually
        // delivers `PortValue::Image` already. But `render_with_context`
        // resolves the value via `Graph::static_input_value` (raw
        // upstream), which can still be `GpuImage` if the upstream is
        // a GPU node — in that case we readback ourselves into RGBA.
        let arc: std::sync::Arc<ImageData> = match input_val {
            PortValue::Image(arc) => {
                if arc.width == 0 || arc.height == 0 || arc.pixels.is_empty() {
                    ui.ctx().request_repaint();
                    return;
                }
                arc.clone()
            }
            PortValue::GpuImage(handle) => {
                let Some(rs) = ctx.wgpu_render_state else {
                    self.status = "Error: wgpu render state unavailable".into();
                    self.enabled = false;
                    return;
                };
                let Some((tex, w, h)) =
                    crate::gpu_image::frame_snapshot_get_texture(handle.node_id, handle.port)
                else {
                    // 1-frame transient on the first publishable tick.
                    ui.ctx().request_repaint();
                    return;
                };
                crate::gpu_image::readback_texture(&rs.device, &rs.queue, &tex, w, h)
            }
            _ => {
                ui.ctx().request_repaint();
                return;
            }
        };

        // ── Lazy-init recorder on first frame ──────────────────────
        let mut guard = match self.file_recorder.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.is_none() {
            let path = std::path::PathBuf::from(self.recorder_path.trim());
            if path.as_os_str().is_empty() {
                self.status = "Error: pick a recording path first".into();
                self.enabled = false;
                return;
            }
            match FileRecorder::new(&path, codec, arc.width, arc.height) {
                Ok(r) => {
                    *guard = Some(r);
                }
                Err(e) => {
                    self.status = format!("Error: {}", e);
                    self.enabled = false;
                    return;
                }
            }
        }
        if let Some(rec) = guard.as_mut() {
            if let Err(e) = rec.write_frame(&arc.pixels, arc.width, arc.height) {
                self.status = format!("Error: {}", e);
                self.enabled = false;
                *guard = None;
            }
        }
        drop(guard);

        // Heartbeat — keep frame counts ticking on the UI.
        ui.ctx().request_repaint();
    }
}

/// Default `~/Movies/PatchWork-{node_id}-{epoch_secs}.{ext}` for a
/// blank recorder path. Epoch seconds keep names monotonic without
/// pulling in a date crate; node id disambiguates if the user has
/// multiple Video Out nodes recording at once.
fn default_recorder_path(
    codec: crate::video_io::file_recorder::Codec,
    node_id: NodeId,
) -> std::path::PathBuf {
    let movies = dirs::home_dir()
        .map(|h| h.join("Movies"))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    movies.join(format!(
        "PatchWork-{}-{}.{}",
        node_id,
        secs,
        codec.extension(),
    ))
}

/// Format a byte count for the recording chip. Uses KB / MB / GB based
/// on magnitude — matches Finder's "About This File" feel.
fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// Force a path's extension to `new_ext` while preserving the parent
/// directory and file stem. Used when the codec dropdown changes —
/// a `.mp4` H.264 path becomes `.mov` ProRes (and vice versa).
fn rewrite_extension(path: &mut String, new_ext: &str) {
    let p = std::path::PathBuf::from(&*path);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = p.parent().map(|x| x.to_path_buf());
    let new_name = format!("{}.{}", stem, new_ext);
    let new_path = match parent {
        Some(d) if !d.as_os_str().is_empty() => d.join(new_name),
        _ => std::path::PathBuf::from(new_name),
    };
    *path = new_path.to_string_lossy().into_owned();
}

/// Convert an `f32` Hz value into an NDI rational frame-rate (N / D).
///
/// NDI transmits frame rate as two `i32` fields (`frame_rate_N`,
/// `frame_rate_D`) so consumers can reconstruct exact values for
/// standard rates like 29.97 / 59.94. For integer caps (60, 30, 25)
/// we use D=1; fractional values get D=100.
#[cfg(target_os = "macos")]
fn rational_fps(fps: f32) -> (i32, i32) {
    let rounded = fps.round();
    if (fps - rounded).abs() < 0.01 {
        (rounded as i32, 1)
    } else {
        ((fps * 100.0).round() as i32, 100)
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("video_out", |state| {
        let mut n = VideoOutNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
