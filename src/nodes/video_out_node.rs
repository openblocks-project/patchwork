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
    fn available(self) -> bool {
        #[cfg(target_os = "macos")]
        {
            matches!(self, Self::Window | Self::Syphon)
        }
        #[cfg(not(target_os = "macos"))]
        {
            matches!(self, Self::Window)
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
        }
    }
}

impl Clone for VideoOutNode {
    /// Cloned copy starts cold: fresh sink state, no running SyphonServer
    /// (it'll be re-created on first publish tick if `enabled` is on).
    /// Matches `DynNode::clone`'s serialize→deserialize round-trip shape.
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
    /// flip to `true` once a CPU-requiring sink (NDI, vcam IOSurface,
    /// file record) is wired up.
    fn needs_cpu_image_input(&self, _port: usize) -> bool {
        matches!(self.sink, VideoSink::Ndi | VideoSink::VirtualCamera | VideoSink::FileRecord)
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
            // `enabled` / `status` intentionally not restored.
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
        #[cfg(target_os = "macos")]
        {
            if let Ok(mut guard) = self.syphon_server.lock() {
                *guard = None;
            }
        }
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
                        let text = if s.available() {
                            s.label().to_string()
                        } else {
                            format!("{}  ({})", s.label(), s.phase_hint())
                        };
                        ui.add_enabled_ui(s.available(), |ui| {
                            ui.selectable_value(&mut self.sink, s, text);
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
                }
                // Carry forward a Syphon publish-name default if coming
                // into Syphon for the first time so the text field isn't
                // blank (and the server isn't anonymous).
                if self.sink == VideoSink::Syphon && self.destination.is_empty() {
                    self.destination = "PatchWork".into();
                }
            }
        });

        // ── Sink‑specific UI ───────────────────────────────────────
        match self.sink {
            VideoSink::Window => self.render_window_sink(ui, ctx, &input_val),
            #[cfg(target_os = "macos")]
            VideoSink::Syphon => self.render_syphon_sink(ui, ctx, &input_val),
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
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("video_out", |state| {
        let mut n = VideoOutNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
