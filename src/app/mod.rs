use crate::graph::*;
use crate::node_trait::NodeBehavior;
use crate::http::{HttpAction, HttpManager};
use crate::midi::{MidiAction, MidiManager};
use crate::serial::{SerialAction, SerialManager};
use crate::osc::{OscAction, OscManager};
use crate::ob::ObManager;
use crate::audio::AudioManager;
use crate::nodes;
use eframe::egui;
use eframe::egui_wgpu;
use std::collections::HashMap;


const PORT_RADIUS: f32 = 8.0;    // visual radius of port shapes
const PORT_INTERACT: f32 = 22.0; // clickable/draggable hit area
const PORT_BORDER: f32 = 2.5;    // border stroke
/// Get port fill + border colors based on PortKind.

/// Get port fill + border colors based on PortKind.
/// Fill is a darker shade for inputs, border is a lighter shade — matching the semantic type.
fn port_colors_for_kind(kind: PortKind, is_output: bool) -> (egui::Color32, egui::Color32) {
    let base = kind.base_color();
    let fill = if is_output {
        egui::Color32::from_rgb(base[0], base[1], base[2])
    } else {
        egui::Color32::from_rgb(
            (base[0] as f32 * 0.5) as u8,
            (base[1] as f32 * 0.5) as u8,
            (base[2] as f32 * 0.5) as u8,
        )
    };
    let border = egui::Color32::from_rgb(
        (base[0] as u16 + 80).min(255) as u8,
        (base[1] as u16 + 80).min(255) as u8,
        (base[2] as u16 + 80).min(255) as u8,
    );
    (fill, border)
}

/// Draw a shaped port based on PortKind:
/// - Number/Normalized/Audio/Color/Generic: circle
/// - Text: rounded square
/// - Image: diamond
/// - Trigger: triangle (pointing right)
/// - Gate: half-moon
/// When connected, draws a tiny Phosphor icon inside.
pub(crate) fn draw_shaped_port(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    fill: egui::Color32,
    border: egui::Color32,
    border_width: f32,
    kind: PortKind,
    is_connected: bool,
) {
    match kind.shape_id() {
        1 => {
            // Rounded square (Text)
            let half = radius * 0.85;
            let rect = egui::Rect::from_center_size(center, egui::vec2(half * 2.0, half * 2.0));
            painter.rect_filled(rect, 3.0, fill);
            painter.rect_stroke(rect, 3.0, egui::Stroke::new(border_width, border), egui::StrokeKind::Outside);
        }
        2 => {
            // Triangle (Trigger) — pointing right
            let d = radius * 0.95;
            let points = vec![
                egui::pos2(center.x - d * 0.7, center.y - d),   // top-left
                egui::pos2(center.x + d, center.y),              // right point
                egui::pos2(center.x - d * 0.7, center.y + d),   // bottom-left
            ];
            painter.add(egui::Shape::convex_polygon(points, fill, egui::Stroke::new(border_width, border)));
        }
        3 => {
            // Diamond (Image)
            let d = radius;
            let points = vec![
                egui::pos2(center.x, center.y - d),
                egui::pos2(center.x + d, center.y),
                egui::pos2(center.x, center.y + d),
                egui::pos2(center.x - d, center.y),
            ];
            painter.add(egui::Shape::convex_polygon(points, fill, egui::Stroke::new(border_width, border)));
        }
        4 => {
            // Half-moon (Gate) — left half filled, right half open
            painter.circle_filled(center, radius, fill);
            painter.circle_stroke(center, radius, egui::Stroke::new(border_width, border));
            // Draw a vertical line through center to indicate half
            painter.line_segment(
                [egui::pos2(center.x, center.y - radius + 1.0), egui::pos2(center.x, center.y + radius - 1.0)],
                egui::Stroke::new(1.5, border),
            );
        }
        _ => {
            // Circle (Number, Normalized, Audio, Color, Generic)
            painter.circle_filled(center, radius, fill);
            painter.circle_stroke(center, radius, egui::Stroke::new(border_width, border));

            // Audio: inner dot
            if kind == PortKind::Audio {
                painter.circle_filled(center, radius * 0.35, border);
            }
            // Normalized: thin ring indicator
            if kind == PortKind::Normalized {
                painter.circle_stroke(center, radius * 0.55, egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 80)));
            }
        }
    }

    // Draw tiny icon inside when connected
    if is_connected {
        let glyph = kind.icon_glyph();
        if !glyph.is_empty() {
            let icon_size = (radius * 1.0).max(6.0).min(10.0);
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::new(icon_size, egui::FontFamily::Proportional),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200),
            );
        }
    }
}

mod undo;
mod canvas;
mod interaction;
mod io;
mod menus;

use undo::UndoHistory;

/// Identity of a connection — survives array reindexing, node deletion, undo/redo.
/// Using (from_node, from_port, to_node, to_port) instead of a Vec index
/// prevents stale-index bugs where the index points to the wrong wire.
type ConnectionId = (NodeId, usize, NodeId, usize);

pub struct PatchworkApp {
    graph: Graph,
    port_positions: HashMap<(NodeId, usize, bool), egui::Pos2>,
    node_rects: HashMap<NodeId, egui::Rect>,
    dragging_from: Option<(NodeId, usize, bool)>,
    show_node_menu: bool,
    node_menu_pos: egui::Pos2,
    node_menu_search: String,
    node_menu_selected: usize,
    node_menu_category: String, // "" = All
    /// When set, the node menu was opened via Tab while dragging a wire.
    /// Contains (source_node, source_port, is_output, port_kind) for filtering + auto-connect.
    wire_menu_context: Option<(NodeId, usize, bool, PortKind)>,
    project_path: Option<String>,
    midi: MidiManager,
    serial: SerialManager,
    frame_count: u64,
    console_messages: Vec<String>,
    monitor: nodes::monitor::MonitorState,
    osc: OscManager,
    // Selection & clipboard
    selected_nodes: std::collections::HashSet<NodeId>,
    selected_connection: Option<ConnectionId>,
    wire_menu_conn: Option<ConnectionId>,  // connection identity for wire context menu
    wire_menu_pos: egui::Pos2,
    /// Insert-on-wire picker visible state. When true, the wire menu shows a
    /// filtered node-picker (only nodes whose first input AND first output
    /// are compatible with the wire's PortKind). Selecting a node spawns it
    /// at the wire midpoint and rewires `from → new → to`.
    wire_insert_picker_open: bool,
    wire_insert_search: String,
    // Box selection
    box_select_start: Option<egui::Pos2>,
    box_select_end: Option<egui::Pos2>,
    clipboard: Option<NodeType>,
    context_menu_node: Option<NodeId>,
    show_context_menu: bool,
    context_menu_pos: egui::Pos2,
    context_menu_opened_at: f64, // time when menu was opened (for auto-dismiss)
    // Option+drag duplication
    opt_drag_source: Option<NodeId>,
    opt_drag_created: Option<NodeId>,
    // Canvas pan & zoom
    canvas_offset: egui::Vec2,
    canvas_zoom: f32,
    _prev_zoom: f32,
    panning: bool,
    pinned_nodes: std::collections::HashSet<NodeId>,
    // HTTP & API
    http: HttpManager,
    api_keys: HashMap<String, String>,
    // OB Hardware
    ob: ObManager,
    // Audio
    audio: AudioManager,
    // MCP server
    mcp_rx: Option<std::sync::mpsc::Receiver<crate::mcp::McpRequest>>,
    mcp_log: crate::mcp::McpLog,
    // ML inference
    ml_rx: std::sync::mpsc::Receiver<crate::nodes::ml_model::MlInferenceResult>,
    ml_tx: std::sync::mpsc::Sender<crate::nodes::ml_model::MlInferenceResult>,
    // Background device refresh
    device_refresh_rx: std::sync::mpsc::Receiver<DeviceRefreshResult>,
    // WGPU render state for shader nodes
    wgpu_render_state: Option<egui_wgpu::RenderState>,
    gpu_tex_cache: crate::gpu_image::GpuTextureCache,
    /// Set whenever the graph is replaced wholesale (load, restore,
    /// undo, redo) or a node is deleted. Consumed at the top of the
    /// next `update()` to wipe the GPU texture cache and all egui
    /// per-node temp data so a fresh project's nodes can't read back
    /// the previous project's cached image / GPU handle / fetch state.
    /// We use a deferred flag rather than calling the cleanup inline
    /// because most mutation sites don't have an `egui::Context` in
    /// scope, and `update()` is the natural chokepoint that does.
    caches_dirty: bool,
    // Undo / Redo
    undo_history: UndoHistory,
    drag_undo_pushed: bool,
    /// Coalescing flag for property edits (slider drags, DragValue, text fields, etc.).
    /// Set true when a property-editing widget interaction starts, reset when interaction ends.
    property_undo_pushed: bool,
    // Click-to-connect wiring mode (alternative to drag-and-drop)
    click_wiring: bool,
    // Monotonic clock reference for wall-clock timing
    app_start_instant: std::time::Instant,
    /// Random accent color for sessions without a Theme node.
    /// Generated once on app start and on "New File". Gives each fresh project a unique hue.
    session_accent: [u8; 3],
    /// Target zoom for smooth interpolation. Input sets this, canvas_zoom lerps toward it.
    target_zoom: f32,
    /// Pointer position (screen coords) for zoom anchor during smooth interpolation.
    zoom_anchor_screen: Option<egui::Vec2>,
    /// Last seen EQ curve hash per AudioEq node, used to skip redundant
    /// `UpdateEqBands` commands. Pruned when the node is removed.
    eq_curve_hashes: HashMap<NodeId, u64>,
    /// Edge-triggered "user wants sound" tracker for the DSP auto-enable.
    /// We only auto-flip the AudioDevice node from disabled→enabled on the
    /// frame the intent rises (false→true). After that the user is back in
    /// control of the toggle, so they can turn DSP off mid-playback without
    /// us immediately overriding them.
    prev_play_intent: bool,
}

/// Results from background device enumeration thread
struct DeviceRefreshResult {
    midi_in: Vec<String>,
    midi_out: Vec<String>,
    serial: Vec<String>,
    audio_output: Vec<String>,
    audio_input: Vec<String>,
    /// Channel count per output device name.
    audio_output_channels: std::collections::HashMap<String, usize>,
}

impl PatchworkApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::icons::setup(&cc.egui_ctx);

        // Apply persisted user settings — currently runs LRU eviction
        // on the image cache so it can't grow without bound across
        // sessions. Failures here are non-fatal (defaults are sane).
        crate::settings::Settings::load().apply_image_cache();
        let wgpu_render_state = cc.wgpu_render_state.clone();
        let (ml_tx, ml_rx) = std::sync::mpsc::channel();
        let mut app = Self {
            graph: Graph::new(),
            port_positions: HashMap::new(),
            node_rects: HashMap::new(),
            dragging_from: None,
            show_node_menu: false,
            node_menu_pos: egui::Pos2::ZERO,
            node_menu_search: String::new(),
            node_menu_selected: 0,
            node_menu_category: String::new(),
            wire_menu_context: None,
            project_path: None,
            midi: MidiManager::new(),
            serial: SerialManager::new(),
            frame_count: 0,
            console_messages: Vec::new(),
            monitor: nodes::monitor::MonitorState::default(),
            osc: OscManager::new(),
            selected_nodes: std::collections::HashSet::new(),
            selected_connection: None,
            wire_menu_conn: None,
            wire_menu_pos: egui::Pos2::ZERO,
            wire_insert_picker_open: false,
            wire_insert_search: String::new(),
            box_select_start: None,
            box_select_end: None,
            clipboard: None,
            context_menu_node: None,
            show_context_menu: false,
            context_menu_pos: egui::Pos2::ZERO,
            context_menu_opened_at: 0.0,
            opt_drag_source: None,
            opt_drag_created: None,
            canvas_offset: egui::Vec2::ZERO,
            canvas_zoom: 1.0,
            _prev_zoom: 1.0,
            panning: false,
            pinned_nodes: std::collections::HashSet::new(),
            http: HttpManager::new(),
            api_keys: HashMap::new(),
            ob: ObManager::new(),
            audio: AudioManager::new(),
            app_start_instant: std::time::Instant::now(),
            mcp_log: crate::mcp::new_log(),
            mcp_rx: None,
            ml_rx: ml_rx,
            ml_tx: ml_tx,
            device_refresh_rx: {
                let (tx, rx) = std::sync::mpsc::channel();
                // Background thread enumerates devices every 5 seconds — never blocks UI
                std::thread::spawn(move || {
                    loop {
                        let midi_in = midir::MidiInput::new("patchwork-scan")
                            .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect())
                            .unwrap_or_default();
                        let midi_out = midir::MidiOutput::new("patchwork-scan")
                            .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect())
                            .unwrap_or_default();
                        let serial = serialport::available_ports()
                            .unwrap_or_default().into_iter().map(|p| p.port_name).collect();
                        let (audio_output, audio_input, audio_output_channels) = {
                            use cpal::traits::{HostTrait, DeviceTrait};
                            let host = cpal::default_host();
                            let mut out_names = Vec::new();
                            let mut out_channels = std::collections::HashMap::new();
                            if let Ok(devs) = host.output_devices() {
                                for d in devs {
                                    if let Ok(name) = d.name() {
                                        if let Ok(cfg) = d.default_output_config() {
                                            out_channels.insert(name.clone(), cfg.channels() as usize);
                                        }
                                        out_names.push(name);
                                    }
                                }
                            }
                            let inp = host.input_devices().map(|devs|
                                devs.filter_map(|d| d.name().ok()).collect()).unwrap_or_default();
                            (out_names, inp, out_channels)
                        };
                        let _ = tx.send(DeviceRefreshResult { midi_in, midi_out, serial, audio_output, audio_input, audio_output_channels });
                        std::thread::sleep(std::time::Duration::from_secs(5));
                    }
                });
                rx
            },
            wgpu_render_state,
            gpu_tex_cache: crate::gpu_image::GpuTextureCache::new(),
            caches_dirty: false,
            undo_history: UndoHistory::new(50),
            drag_undo_pushed: false,
            property_undo_pushed: false,
            click_wiring: false,
            session_accent: crate::nodes::theme::random_accent(),
            target_zoom: 1.0,
            zoom_anchor_screen: None,
            eq_curve_hashes: HashMap::new(),
            prev_play_intent: false,
        };
        // Always start MCP server thread — auto-detects if stdin is a pipe (Claude Desktop)
        // vs terminal (normal launch). If stdin is a terminal, the thread exits immediately.
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let log = app.mcp_log.clone();
            std::thread::spawn(move || crate::mcp::run_mcp_thread(tx, log));
            app.mcp_rx = Some(rx);
        }

        // Try restoring previous session; fall back to default nodes
        if !app.restore_session() {
            app.spawn_default_nodes();
        }
        app
    }

    /// Build audio chains from Speaker nodes backward through effects to sources.
    /// Only sources routed through an active Speaker will produce sound.
    /// Sync audio engine state with the graph. Called every frame after node rendering.
    ///
    /// Responsibilities:
    /// 1. Start/stop audio output based on DSP enabled state
    /// 2. Rebuild engine when it's freshly created
    /// 3. Sync all audio node parameters to the engine (lock-free atomic writes)
    /// 4. Auto-register new audio nodes that don't have processors yet
    fn sync_audio_engine(&mut self) {
        // ── Check for audio device error (disconnect, hardware failure) ──
        if self.audio.audio_error.load(std::sync::atomic::Ordering::Relaxed) {
            self.audio.audio_error.store(false, std::sync::atomic::Ordering::Relaxed);
            self.audio.stop_output();
            // Set DSP to off in the AudioDevice node so UI reflects the error
            for node in self.graph.nodes.values_mut() {
                if let NodeType::AudioDevice { enabled, .. } = &mut node.node_type {
                    *enabled = false;
                }
            }
            crate::system_log::error("Audio device disconnected — DSP stopped".to_string());
            return;
        }

        // ── Gate: require an enabled AudioDevice node for any audio to flow ──
        let mut dsp_enabled = self.graph.nodes.values().any(|n| {
            matches!(n.node_type, NodeType::AudioDevice { enabled: true, .. })
        });

        // Auto-enable DSP **on the rising edge** of "user wants sound":
        //   • pressed Play on any Audio Player (file_playing == true)
        //   • activated a Microphone (AudioInput.active == true)
        //
        // We only flip the AudioDevice from disabled→enabled on the frame
        // the intent transitions false→true, NOT every frame the intent
        // remains true. Otherwise the user can never turn DSP off while
        // audio is playing — the gate would re-enable it on the very next
        // frame. With edge-triggering:
        //   - drop MP3 + press Play with DSP off → DSP flips on once   ✓
        //   - DSP on, audio playing, click DSP off → stays off          ✓
        //   - then press Play again (after a stop)  → DSP flips on      ✓
        let has_audio_device = self.graph.nodes.values()
            .any(|n| matches!(n.node_type, NodeType::AudioDevice { .. }));
        let play_intent = has_audio_device && (
            self.audio.file_playing.values().any(|p| *p)
            || self.graph.nodes.values().any(|n| matches!(
                &n.node_type,
                NodeType::AudioInput { active: true, .. }
            ))
        );
        let intent_rising_edge = play_intent && !self.prev_play_intent;
        self.prev_play_intent = play_intent;

        if !dsp_enabled && intent_rising_edge {
            for node in self.graph.nodes.values_mut() {
                if let NodeType::AudioDevice { enabled, .. } = &mut node.node_type {
                    *enabled = true;
                }
            }
            dsp_enabled = true;
        }
        if !dsp_enabled {
            if self.audio.is_running() {
                self.audio.stop_output();
            }
            return;
        }

        // ── Start/rebuild engine ──
        // Start audio output if DSP is enabled but stream isn't running yet.
        // Also rebuild engine if it's running but has no processors
        // (happens when AudioDevice node calls start_output directly during render,
        // before build_audio_chains has a chance to call rebuild_engine_from_graph).
        if !self.audio.is_running() {
            let device_name: Option<String> = self.graph.nodes.values()
                .find_map(|n| {
                    if let NodeType::AudioDevice { selected_output, enabled: true, .. } = &n.node_type {
                        if selected_output.is_empty() { None } else { Some(selected_output.clone()) }
                    } else { None }
                });
            if let Err(e) = self.audio.start_output(device_name.as_deref()) {
                crate::system_log::error(format!("Audio start failed: {}", e));
            } else {
                self.audio.rebuild_engine_from_graph(&self.graph);
            }
        }
        // If engine was just started (by AudioDevice during render or by us above),
        // rebuild to register all processors and connections.
        if self.audio.engine_needs_rebuild {
            self.audio.rebuild_engine_from_graph(&self.graph);
            self.audio.engine_needs_rebuild = false;
        }

        // ── Sync all audio node params to engine ──────────────────────────
        // Write current parameter values for all audio nodes to the engine.
        // This covers effect nodes (delay, reverb, etc.) that don't have direct
        // access to AudioManager in their render functions.
        for (&nid, node) in &self.graph.nodes {
            if !self.audio.has_processor(nid) { continue; }
            match &node.node_type {
                // Synth and Speaker write their own params in render — skip
                NodeType::Synth { .. } | NodeType::Speaker { .. } => {}
                // Effect nodes: sync params from NodeType fields
                NodeType::AudioDelay { time_ms, feedback } => {
                    self.audio.engine_write_param(nid, 0, *time_ms);
                    self.audio.engine_write_param(nid, 1, *feedback);
                }
                NodeType::AudioDistortion { drive } => {
                    self.audio.engine_write_param(nid, 0, *drive);
                }
                NodeType::AudioReverb { room_size, damping, mix } => {
                    self.audio.engine_write_param(nid, 0, *room_size);
                    self.audio.engine_write_param(nid, 1, *damping);
                    self.audio.engine_write_param(nid, 2, *mix);
                }
                NodeType::AudioLowPass { cutoff } => {
                    self.audio.engine_write_param(nid, 0, *cutoff);
                }
                NodeType::AudioHighPass { cutoff } => {
                    self.audio.engine_write_param(nid, 0, *cutoff);
                }
                NodeType::AudioGain { level } => {
                    self.audio.engine_write_param(nid, 0, *level);
                }
                NodeType::AudioMixer { gains, .. } => {
                    for (ch, gain) in gains.iter().enumerate() {
                        self.audio.engine_write_param(nid, ch, *gain);
                    }
                }
                NodeType::AudioPlayer { volume, .. } => {
                    self.audio.engine_write_param(nid, 0, *volume);
                }
                NodeType::AudioSampler { volume, .. } => {
                    self.audio.engine_write_param(nid, 0, *volume);
                }
                NodeType::AudioInput { gain, .. } => {
                    self.audio.engine_write_param(nid, 0, *gain);
                }
                _ => {}
            }
        }

        // ── EQ curve sync ────────────────────────────────────────────────
        // The EQ processor doesn't use the atomic-param channel — its "params"
        // are the curve points themselves, which become biquad coefficients.
        // Hash the points each frame and only ship a new band set when the
        // hash changes (i.e. the user actually moved a control point). This
        // is the missing piece that made the EQ feel like it "didn't do
        // anything" — the curve was being edited, but the audio thread was
        // still running the bands from the moment the processor was created.
        let mut eq_updates: Vec<(NodeId, Vec<[f32; 2]>)> = Vec::new();
        for (&nid, node) in &self.graph.nodes {
            if !self.audio.has_processor(nid) { continue; }
            if let NodeType::AudioEq { points } = &node.node_type {
                let new_hash = crate::audio::eq_curve_hash(points);
                let prev = self.eq_curve_hashes.get(&nid).copied();
                if prev != Some(new_hash) {
                    self.eq_curve_hashes.insert(nid, new_hash);
                    eq_updates.push((nid, points.clone()));
                }
            }
        }
        for (nid, points) in eq_updates {
            self.audio.update_eq_bands(nid, &points);
        }
        // Drop hash entries for nodes that no longer exist (prevents stale
        // entries leaking forever as the user creates/deletes EQ nodes).
        self.eq_curve_hashes.retain(|nid, _| self.graph.nodes.contains_key(nid));

        // Auto-register new audio nodes that don't have processors yet
        // (e.g., nodes added via palette after engine was started)
        if self.audio.engine_tx.is_some() {
            let mut new_nodes: Vec<NodeId> = Vec::new();
            // Drain any nodes whose processors were registered mid-session
            // (e.g., Microphone activated AFTER DSP started). These already
            // have processors, so the loop below would skip them — but their
            // connections were silently dropped during the initial walk and
            // need to be replayed.
            let pending_sync: Vec<NodeId> = self.audio.pending_connection_sync.drain().collect();
            for nid in pending_sync {
                if self.audio.has_processor(nid) {
                    new_nodes.push(nid);
                }
            }
            for (&nid, _node) in &self.graph.nodes {
                if self.audio.has_processor(nid) { continue; }
                // Rebuild just this node
                let graph_snapshot = &self.graph;
                if let Some(node) = graph_snapshot.nodes.get(&nid) {
                    let proc_and_count = self.audio.create_processor_for_node(&node.node_type, nid);
                    if let Some((processor, param_count)) = proc_and_count {
                        self.audio.add_processor(nid, processor, param_count);
                        new_nodes.push(nid);
                        // Set speaker state
                        if let NodeType::Speaker { active, .. } = &node.node_type {
                            self.audio.send_command(crate::audio::engine::AudioCommand::SetSpeaker {
                                node_id: nid, active: *active,
                            });
                        }
                    }
                }
            }

            // Re-sync connections involving newly registered processors.
            // Without this, nodes added mid-session have processors but no wired
            // connections — the connection was silently dropped because has_processor()
            // returned false when the wire was created.
            if !new_nodes.is_empty() {
                for conn in &self.graph.connections {
                    let involves_new = new_nodes.contains(&conn.from_node) || new_nodes.contains(&conn.to_node);
                    if !involves_new { continue; }
                    let from_audio = self.audio.node_params.contains_key(&conn.from_node);
                    let to_audio = self.audio.node_params.contains_key(&conn.to_node);
                    if !from_audio || !to_audio { continue; }
                    // Skip mixer gain ports
                    let is_mixer_gain_port = matches!(
                        self.graph.nodes.get(&conn.to_node).map(|n| &n.node_type),
                        Some(NodeType::AudioMixer { .. })
                    ) && conn.to_port % 2 != 0;
                    if is_mixer_gain_port { continue; }
                    let engine_port = self.mixer_engine_port(conn.to_node, conn.to_port);
                    self.audio.connect_audio(conn.from_node, conn.to_node, engine_port);
                }
            }
        }

        self.graph.audio_topology_dirty = false;
    }

    /// Find a connection's current index from its identity. Returns None if
    /// the connection no longer exists (deleted, undo, etc.).
    fn find_connection_index(&self, id: &ConnectionId) -> Option<usize> {
        self.graph.connections.iter().position(|c| {
            c.from_node == id.0 && c.from_port == id.1 && c.to_node == id.2 && c.to_port == id.3
        })
    }

    /// Map a graph port index to an engine port index.
    /// For mixer nodes, audio ports are at even indices (0, 2, 4) → engine (0, 1, 2).
    /// For all other nodes, port index passes through unchanged.
    fn mixer_engine_port(&self, node_id: NodeId, graph_port: usize) -> usize {
        if matches!(self.graph.nodes.get(&node_id).map(|n| &n.node_type), Some(NodeType::AudioMixer { .. })) {
            graph_port / 2
        } else {
            graph_port
        }
    }

    fn primary_selected(&self) -> Option<NodeId> {
        self.selected_nodes.iter().next().copied()
    }

    // Pinned nodes: pos is screen pixels, computed to egui coords inline in render

    /// Render nodes. If `pinned_only` is true, only render pinned nodes (at zoom 1.0).
    /// If false, only render non-pinned nodes (at canvas zoom).
    fn render_nodes_filtered(&mut self, ctx: &egui::Context, values: &HashMap<(NodeId, usize), PortValue>, pinned_only: bool) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        let connections = self.graph.connections.clone();
        let midi_out_ports = self.midi.cached_output_ports.clone();
        let midi_in_ports = self.midi.cached_input_ports.clone();
        let serial_ports = self.serial.cached_ports.clone();
        let monitor_state = std::mem::take(&mut self.monitor);
        let offset = self.canvas_offset;
        let zoom = self.canvas_zoom;
        // Don't create fresh — extend existing to preserve positions from other passes
        let mut port_positions: HashMap<(NodeId, usize, bool), egui::Pos2> = if pinned_only {
            std::mem::take(&mut self.port_positions)
        } else {
            HashMap::new()
        };
        let mut node_rects: HashMap<NodeId, egui::Rect> = if pinned_only {
            std::mem::take(&mut self.node_rects)
        } else {
            HashMap::new()
        };
        let mut pending_connections: Vec<(NodeId, usize, NodeId, usize)> = Vec::new();
        let mut nodes_to_delete: Vec<NodeId> = Vec::new();
        let mut osc_actions: Vec<OscAction> = Vec::new();
        let mut dragging_from = self.dragging_from;
        let mut palette_spawns: Vec<([f32; 2], NodeType)> = Vec::new();
        let mut midi_actions: Vec<MidiAction> = Vec::new();
        let mut serial_actions: Vec<SerialAction> = Vec::new();
        let mut pending_disconnects: Vec<(NodeId, usize)> = Vec::new();
        let mut ob_manager = std::mem::replace(&mut self.ob, ObManager::new());
        let mut audio_manager = std::mem::replace(&mut self.audio, AudioManager::placeholder());
        let mut http_actions: Vec<HttpAction> = Vec::new();
        let mut multi_drag: Option<(NodeId, egui::Vec2)> = None; // (dragged_node, delta)
        let api_keys = self.api_keys.clone();
        let wgpu_render_state = self.wgpu_render_state.clone();

        // With set_zoom_factor, normal nodes scale automatically via GPU.
        // We only need the original style to restore after pinned nodes' inverse-zoom.
        let _original_style = ctx.style();

        for node_id in node_ids {
            let is_pinned = self.pinned_nodes.contains(&node_id);
            // Skip nodes not in the current pass
            if pinned_only != is_pinned {
                continue;
            }
            let mut node = match self.graph.nodes.remove(&node_id) { Some(n) => n, None => continue };
            let input_defs = node.node_type.inputs();
            let output_defs = node.node_type.outputs();
            let n_inputs = input_defs.len();
            let n_outputs = output_defs.len();
            let [cr, cg, cb] = node.node_type.color_hint();
            let accent = egui::Color32::from_rgb(cr, cg, cb);
            let title = node.node_type.title().to_string();
            let midi_conn_out = self.midi.is_output_connected(node_id);
            let midi_conn_in = self.midi.is_input_connected(node_id);
            let serial_conn = self.serial.is_connected(node_id);
            let osc_listening = self.osc.is_listening(node_id);
            let http_pending = self.http.is_pending(node_id);

            let _open = true;
            let inline = node.node_type.inline_ports();
            // set_zoom_factor(zoom) is active for the whole frame.
            // Normal: egui_pos = canvas_pos + offset/zoom
            // Pinned: egui_pos = screen_pos / zoom (fixed screen position, divided to get logical)
            let offset_egui = offset / zoom;
            let inv = 1.0 / zoom;
            let (egui_x, egui_y) = if is_pinned {
                (node.pos[0] / zoom, node.pos[1] / zoom)
            } else {
                (node.pos[0] + offset_egui.x, node.pos[1] + offset_egui.y)
            };
            // Use trait method for min_width if available, else defaults
            let node_width = if let Some(w) = node.node_type.min_width() {
                w
            } else {
                match &node.node_type {
                    NodeType::Slider { .. } => 50.0,
                    _ if node.node_type.custom_render() => 50.0,
                    _ if is_pinned => 180.0 * inv,
                    _ => 180.0,
                }
            };
            let is_display = node.node_type.title() == "Display";
            let is_monitor = node.node_type.title() == "Monitor";
            let is_audio_player = matches!(node.node_type, NodeType::AudioPlayer { .. });
            let is_math = matches!(node.node_type, NodeType::Math { .. });
            let is_speaker = matches!(node.node_type, NodeType::Speaker { .. });
            let no_title = node.node_type.no_title() || is_display || is_monitor || is_audio_player || is_speaker;
            let port_sz = if is_pinned { PORT_INTERACT * inv } else { PORT_INTERACT };
            let port_r = if is_pinned { PORT_RADIUS * inv } else { PORT_RADIUS };
            let title_size = if is_pinned { 14.0 * inv } else { 14.0 };
            let node_icon = crate::icons::node_icon(node.node_type.title());
            let title_with_icon = format!("{} {}", node_icon, title);
            let title_rt = egui::RichText::new(&title_with_icon).color(accent).strong().size(title_size);
            // Pinned windows use zoom-bucketed ID to reset egui's cached window size on zoom change
            let zoom_bucket = if is_pinned { (zoom * 10.0).round() as i32 } else { 0 };
            // Apply inverse-zoom style for pinned nodes
            // Inverse-zoom style for pinned nodes: cancel out set_zoom_factor so they
            // appear at native screen size regardless of canvas zoom level.
            if is_pinned {
                let mut style = ctx.style().as_ref().clone();
                for (_, font_id) in style.text_styles.iter_mut() {
                    font_id.size *= inv;
                }
                style.spacing.item_spacing *= inv;
                style.spacing.button_padding *= inv;
                style.spacing.interact_size *= inv;
                style.spacing.window_margin *= inv;
                style.spacing.indent *= inv;
                style.spacing.icon_width *= inv;
                style.spacing.icon_width_inner *= inv;
                style.spacing.icon_spacing *= inv;
                style.spacing.slider_width *= inv;
                style.spacing.combo_width *= inv;
                style.spacing.scroll.bar_width *= inv;
                style.spacing.scroll.bar_inner_margin *= inv;
                style.spacing.scroll.bar_outer_margin *= inv;
                let scale_cr = |r: &mut egui::CornerRadius| {
                    r.nw = (r.nw as f32 * inv).round().max(1.0) as u8;
                    r.ne = (r.ne as f32 * inv).round().max(1.0) as u8;
                    r.sw = (r.sw as f32 * inv).round().max(1.0) as u8;
                    r.se = (r.se as f32 * inv).round().max(1.0) as u8;
                };
                let scale_stroke = |s: &mut egui::Stroke| { s.width *= inv; };
                scale_cr(&mut style.visuals.window_corner_radius);
                scale_stroke(&mut style.visuals.window_stroke);
                style.visuals.window_shadow.blur = (style.visuals.window_shadow.blur as f32 * inv).round() as u8;
                style.visuals.window_shadow.spread = (style.visuals.window_shadow.spread as f32 * inv).round() as u8;
                for ws in [&mut style.visuals.widgets.noninteractive, &mut style.visuals.widgets.inactive,
                           &mut style.visuals.widgets.hovered, &mut style.visuals.widgets.active] {
                    scale_cr(&mut ws.corner_radius);
                    scale_stroke(&mut ws.bg_stroke);
                    scale_stroke(&mut ws.fg_stroke);
                }
                style.visuals.resize_corner_size *= inv;
                ctx.set_style(std::sync::Arc::new(style));
            }
            let is_custom_render = node.node_type.custom_render();
            // Custom frame — render_background for trait nodes, legacy match for enum nodes
            let bg_painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Background, egui::Id::new(("node_bg", node_id))));
            let approx_rect = egui::Rect::from_min_size(egui::pos2(egui_x, egui_y), egui::vec2(node_width, 100.0));
            let custom_frame = if let Some(frame) = node.node_type.render_background(&bg_painter, approx_rect) {
                Some(frame)
            } else {
                None
            };
            let is_comment = node.node_type.title() == "Comment";
            let mut win = egui::Window::new(title_rt)
                .id(egui::Id::new(("node", node_id, zoom_bucket)))
                .current_pos(egui::pos2(egui_x, egui_y))
                .default_width(node_width)
                .resizable(is_display || (!is_custom_render && !is_comment && !is_monitor && !is_audio_player && !is_math))
                .constrain(false)
                .collapsible(!is_custom_render && !no_title && !is_pinned)
                .title_bar(!is_custom_render && !no_title);
            if is_pinned {
                win = win.order(egui::Order::Foreground);
            }
            if is_display {
                win = win.default_height(120.0).scroll([false, false]);
            }
            if let Some(frame) = custom_frame {
                win = win.frame(frame);
            }
            let resp = win.show(ctx, |ui| {
                    // Constrain width to prevent expanding nodes
                    ui.set_max_width(node_width.max(180.0));

                    // Top input ports (skip for inline-port nodes)
                    if !inline {
                        for (i, pdef) in input_defs.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let (rect, response) = ui.allocate_exact_size(egui::vec2(port_sz, port_sz), egui::Sense::click_and_drag());
                                let val = Graph::static_input_value(&connections, values, node_id, i);
                                let kind = pdef.kind; // Use semantic kind from port definition
                                let is_connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == i);
                                let (type_fill, type_border) = port_colors_for_kind(kind, false);
                                let (fill, border) = if response.hovered() || response.dragged() {
                                    (egui::Color32::YELLOW, egui::Color32::WHITE)
                                } else { (type_fill, type_border) };
                                // Highlight port if click-wiring and hovering a compatible target
                                let click_wiring_hover = self.click_wiring && dragging_from.is_some() && response.hovered();
                                let (fill, border) = if click_wiring_hover {
                                    (egui::Color32::from_rgb(100, 255, 100), egui::Color32::WHITE)
                                } else { (fill, border) };
                                draw_shaped_port(ui.painter(), rect.center(), port_r, fill, border, PORT_BORDER, kind, is_connected);
                                port_positions.insert((node_id, i, true), rect.center());
                                ui.label(format!("{}: {}", pdef.name, val));
                                if response.drag_started() {
                                    if let Some(existing) = connections.iter().find(|c| c.to_node == node_id && c.to_port == i) {
                                        let src_node = existing.from_node;
                                        let src_port = existing.from_port;
                                        dragging_from = Some((src_node, src_port, true));
                                        pending_disconnects.push((node_id, i));
                                    } else {
                                        dragging_from = Some((node_id, i, false));
                                    }
                                }
                                // Click-to-connect: click on input port starts or completes wiring
                                if response.clicked() {
                                    if let Some((src_node, src_port, is_output)) = dragging_from {
                                        if self.click_wiring && is_output && node_id != src_node {
                                            // Complete: output → this input
                                            pending_connections.push((src_node, src_port, node_id, i));
                                            dragging_from = None;
                                            self.click_wiring = false;
                                        } else if self.click_wiring && !is_output && node_id != src_node {
                                            // Complete: this input ← input (reverse)
                                            pending_connections.push((node_id, i, src_node, src_port));
                                            dragging_from = None;
                                            self.click_wiring = false;
                                        }
                                    } else {
                                        // Start click-wiring from input port
                                        if let Some(existing) = connections.iter().find(|c| c.to_node == node_id && c.to_port == i) {
                                            dragging_from = Some((existing.from_node, existing.from_port, true));
                                            pending_disconnects.push((node_id, i));
                                        } else {
                                            dragging_from = Some((node_id, i, false));
                                        }
                                        self.click_wiring = true;
                                    }
                                }
                            });
                        }
                        if !input_defs.is_empty() { ui.separator(); }
                    }

                    nodes::render_content(ui, &mut node.node_type, node_id, values, &connections,
                        &midi_out_ports, &midi_in_ports, midi_conn_out, midi_conn_in, &mut midi_actions,
                        &serial_ports, serial_conn, &mut serial_actions, &monitor_state,
                        osc_listening, &mut osc_actions, &mut port_positions, &mut dragging_from,
                        &mut http_actions, http_pending, &api_keys, &wgpu_render_state,
                        &mut pending_disconnects, &mut ob_manager, &mut audio_manager,
                        &self.mcp_log, self.mcp_rx.is_some());

                    // Check if a Palette node wants to spawn new nodes
                    if matches!(node.node_type, NodeType::Palette { .. }) {
                        for spawn_nt in nodes::palette_actions(ui) {
                            palette_spawns.push((node.pos, spawn_nt));
                        }
                    }

                    // Bottom output ports (skip for inline-port nodes)
                    if !inline {
                        if !output_defs.is_empty() { ui.separator(); }
                        for (i, pdef) in output_defs.iter().enumerate() {
                            let val = values.get(&(node_id, i)).cloned().unwrap_or(PortValue::None);
                            let label_text = format!("{}: {}", pdef.name, val);
                            let kind = pdef.kind; // Use semantic kind from port definition
                            let is_connected = connections.iter().any(|c| c.from_node == node_id && c.from_port == i);
                            let (type_fill, type_border) = port_colors_for_kind(kind, true);

                            // Use columns: label left, port circle right
                            ui.horizontal(|ui| {
                                ui.label(&label_text);
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let (rect, response) = ui.allocate_exact_size(egui::vec2(port_sz, port_sz), egui::Sense::click_and_drag());
                                    // Highlight port if click-wiring and hovering a compatible target
                                    let click_wiring_hover = self.click_wiring && dragging_from.is_some() && response.hovered();
                                    let (fill, border) = if click_wiring_hover {
                                        (egui::Color32::from_rgb(100, 255, 100), egui::Color32::WHITE)
                                    } else if response.hovered() || response.dragged() {
                                        (egui::Color32::YELLOW, egui::Color32::WHITE)
                                    } else { (type_fill, type_border) };
                                    draw_shaped_port(ui.painter(), rect.center(), port_r, fill, border, PORT_BORDER, kind, is_connected);
                                    port_positions.insert((node_id, i, false), rect.center());
                                    if response.drag_started() { dragging_from = Some((node_id, i, true)); }
                                    // Click-to-connect: click on output port starts or completes wiring
                                    if response.clicked() {
                                        if let Some((src_node, src_port, is_output)) = dragging_from {
                                            if self.click_wiring && !is_output && node_id != src_node {
                                                // Complete: input → this output
                                                pending_connections.push((node_id, i, src_node, src_port));
                                                dragging_from = None;
                                                self.click_wiring = false;
                                            } else if self.click_wiring && is_output && node_id != src_node {
                                                // Complete: output → output (swap direction)
                                                pending_connections.push((src_node, src_port, node_id, i));
                                                dragging_from = None;
                                                self.click_wiring = false;
                                            }
                                        } else {
                                            // Start click-wiring from output port
                                            dragging_from = Some((node_id, i, true));
                                            self.click_wiring = true;
                                        }
                                    }
                                });
                            });
                        }
                    }
                });

            // Restore original style after pinned window's inverse-zoom
            if is_pinned {
                ctx.set_style(_original_style.clone());
            }

            if let Some(r) = &resp {
                if is_pinned {
                    // Pinned: drag delta is in logical (screen/zoom), store as screen pixels
                    if r.response.dragged() {
                        let delta = r.response.drag_delta();
                        node.pos[0] += delta.x * zoom;
                        node.pos[1] += delta.y * zoom;
                    }
                } else {
                    // Normal: convert logical pos back to canvas coords
                    let offset_egui = offset / zoom;
                    node.pos = [
                        r.response.rect.left() - offset_egui.x,
                        r.response.rect.top() - offset_egui.y,
                    ];
                }
                node_rects.insert(node_id, r.response.rect);

                // Pin badge removed — pinned state is shown via the title bar accent color

                // Selection on click (Shift = toggle, no Shift = replace)
                // For custom_render nodes (Slider), only select on drag — click opens their popup
                let select_trigger = if is_custom_render {
                    r.response.drag_started()
                } else {
                    r.response.clicked() || r.response.drag_started()
                };
                if select_trigger {
                    let shift = ctx.input(|i| i.modifiers.shift);
                    if shift {
                        if self.selected_nodes.contains(&node_id) {
                            self.selected_nodes.remove(&node_id);
                        } else {
                            self.selected_nodes.insert(node_id);
                        }
                    } else if !self.selected_nodes.contains(&node_id) {
                        self.selected_nodes.clear();
                        self.selected_nodes.insert(node_id);
                    }
                    self.selected_connection = None;
                }

                // Right-click context menu — skip for custom_render nodes (they handle their own UI)
                let secondary_in_rect = !is_custom_render && ctx.input(|i| {
                    i.pointer.button_clicked(egui::PointerButton::Secondary)
                }) && ctx.pointer_latest_pos().map(|p| r.response.rect.contains(p)).unwrap_or(false);
                if !is_custom_render && (r.response.secondary_clicked() || secondary_in_rect) {
                    if !self.selected_nodes.contains(&node_id) {
                        self.selected_nodes.clear();
                        self.selected_nodes.insert(node_id);
                    }
                    self.context_menu_node = Some(node_id);
                    self.show_context_menu = true;
                    self.context_menu_pos = ctx.pointer_latest_pos().unwrap_or(r.response.rect.center());
                    self.context_menu_opened_at = ctx.input(|i| i.time);
                }

                // Draw selection highlight (skip for custom_render nodes — they handle their own visual feedback)
                if self.selected_nodes.contains(&node_id) && !is_custom_render {
                    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("selection")));
                    let sel_accent = ctx.data_mut(|d| d.get_temp::<[u8; 3]>(egui::Id::new("theme_accent")))
                        .unwrap_or([80, 160, 255]);
                    painter.rect_stroke(r.response.rect.expand(2.0), 4.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(sel_accent[0], sel_accent[1], sel_accent[2])), egui::StrokeKind::Outside);
                }
                // Pin indicator — solid accent circle with white pin icon
                if is_pinned {
                    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pins")));
                    let badge_center = egui::pos2(r.response.rect.left() + 12.0, r.response.rect.top() - 4.0);
                    let theme_accent = ctx.data_mut(|d| d.get_temp::<[u8; 3]>(egui::Id::new("theme_accent")))
                        .unwrap_or([80, 160, 255]);
                    let accent_color = egui::Color32::from_rgb(theme_accent[0], theme_accent[1], theme_accent[2]);
                    // Solid accent background circle (24x24 = radius 12)
                    painter.circle_filled(badge_center, 12.0, accent_color);
                    // White pin icon on top
                    painter.text(
                        badge_center,
                        egui::Align2::CENTER_CENTER,
                        crate::icons::PUSH_PIN,
                        egui::FontId::proportional(13.0),
                        egui::Color32::WHITE,
                    );
                }

                // Error badge — red border + ⚠ corner icon when this node has a
                // recorded evaluation error. Click/hover to read the message.
                if let Some(err) = crate::node_errors::get(node_id) {
                    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("node_errors")));
                    let err_red = egui::Color32::from_rgb(230, 80, 80);
                    // Red border around the whole node
                    painter.rect_stroke(
                        r.response.rect.expand(2.0),
                        4.0,
                        egui::Stroke::new(2.0, err_red),
                        egui::StrokeKind::Outside,
                    );
                    // Warning badge at the top-right corner
                    let badge_center = egui::pos2(r.response.rect.right() - 12.0, r.response.rect.top() - 4.0);
                    painter.circle_filled(badge_center, 10.0, err_red);
                    painter.circle_stroke(badge_center, 10.0, egui::Stroke::new(1.0, egui::Color32::WHITE));
                    painter.text(
                        badge_center,
                        egui::Align2::CENTER_CENTER,
                        "!",
                        egui::FontId::proportional(13.0),
                        egui::Color32::WHITE,
                    );
                    // Hover tooltip with the full error message — shown when
                    // the pointer is over the node window or its badge.
                    let pointer_over = ctx.pointer_latest_pos()
                        .map(|p| r.response.rect.expand(4.0).contains(p))
                        .unwrap_or(false);
                    if pointer_over {
                        egui::show_tooltip_at_pointer(
                            ctx,
                            egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("node_err_tip_layer")),
                            egui::Id::new(("node_err_tip", node_id)),
                            |ui| {
                                ui.colored_label(err_red, "⚠ Node error");
                                ui.label(egui::RichText::new(&err.message).monospace().size(11.0));
                            },
                        );
                    }
                }

                // Collapsed node: populate fallback port positions so wires still draw
                let is_collapsed = r.inner.is_none();
                if is_collapsed {
                    let rect = r.response.rect;
                    let n_in = n_inputs;
                    let n_out = n_outputs;
                    let fg = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("collapsed_ports")));
                    for i in 0..n_in {
                        let y_off = if n_in > 1 { (i as f32 - (n_in - 1) as f32 / 2.0) * 4.0 } else { 0.0 };
                        let pos = egui::pos2(rect.left() - PORT_RADIUS, rect.center().y + y_off);
                        port_positions.insert((node_id, i, true), pos);
                        fg.circle_filled(pos, 3.0, egui::Color32::from_rgb(70, 75, 85));
                    }
                    for i in 0..n_out {
                        let y_off = if n_out > 1 { (i as f32 - (n_out - 1) as f32 / 2.0) * 4.0 } else { 0.0 };
                        let pos = egui::pos2(rect.right() + PORT_RADIUS, rect.center().y + y_off);
                        port_positions.insert((node_id, i, false), pos);
                        fg.circle_filled(pos, 3.0, egui::Color32::from_rgb(60, 140, 255));
                    }
                }

                // Undo snapshot on drag start (coalesced — one per gesture)
                if r.response.drag_started() && !ctx.input(|i| i.modifiers.alt) && !self.drag_undo_pushed {
                    self.push_undo();
                    self.drag_undo_pushed = true;
                }
                if r.response.drag_stopped() {
                    self.drag_undo_pushed = false;
                }

                // Undo snapshot for property edits (slider drags, DragValue, text fields, etc.).
                // render_content() signals via temp data when a widget interaction starts.
                let prop_signal_id = egui::Id::new("_patchwork_prop_edit_signal");
                if ctx.data_mut(|d| d.get_temp::<bool>(prop_signal_id).unwrap_or(false)) {
                    ctx.data_mut(|d| d.remove::<bool>(prop_signal_id));
                    if !self.property_undo_pushed {
                        self.push_undo();
                        self.property_undo_pushed = true;
                    }
                }

                // Track drag delta for multi-node movement
                if r.response.dragged() && self.selected_nodes.contains(&node_id) && self.selected_nodes.len() > 1 {
                    multi_drag = Some((node_id, r.response.drag_delta()));
                }

                // Option+drag to duplicate
                if r.response.drag_started() && ctx.input(|i| i.modifiers.alt) {
                    self.opt_drag_source = Some(node_id);
                }
            }
            // Process slider popup actions (pin/delete)
            // Check all custom node pin actions
            let pin_id = egui::Id::new(("slider_pin_action", node_id));
            let display_pin_id = egui::Id::new(("display_pin_action", node_id));
            let has_pin = ctx.data_mut(|d| d.get_temp::<bool>(pin_id).unwrap_or(false))
                       || ctx.data_mut(|d| d.get_temp::<bool>(display_pin_id).unwrap_or(false));
            ctx.data_mut(|d| { d.remove::<bool>(pin_id); d.remove::<bool>(display_pin_id); });
            if has_pin {
                self.push_undo();
                if self.pinned_nodes.contains(&node_id) {
                    // Unpin: current pos is screen pixels stored by pinned rendering
                    // Convert to canvas coords: canvas = (screen - offset) / zoom
                    let sx = node.pos[0];
                    let sy = node.pos[1];
                    node.pos = [(sx - offset.x) / zoom, (sy - offset.y) / zoom];
                    self.pinned_nodes.remove(&node_id);
                } else {
                    // Pin: current pos is canvas coords
                    // Convert to screen pixels: screen = canvas * zoom + offset
                    let cx = node.pos[0];
                    let cy = node.pos[1];
                    node.pos = [cx * zoom + offset.x, cy * zoom + offset.y];
                    self.pinned_nodes.insert(node_id);
                }
            }
            // Handle delete actions from custom node popups (Slider, Comment, etc.)
            for prefix in &["slider_delete_action", "comment_delete_action", "display_delete_action"] {
                let del_id = egui::Id::new((*prefix, node_id));
                if ctx.data_mut(|d| d.get_temp::<bool>(del_id).unwrap_or(false)) {
                    ctx.data_mut(|d| d.remove::<bool>(del_id));
                    self.push_undo();
                    nodes_to_delete.push(node_id);
                }
            }

            self.graph.nodes.insert(node_id, node);
        }

        // Apply multi-node drag: move all other selected nodes by the same delta
        if let Some((dragged_id, delta)) = multi_drag {
            if delta.length() > 0.0 {
                for &nid in &self.selected_nodes {
                    if nid != dragged_id {
                        if let Some(node) = self.graph.nodes.get_mut(&nid) {
                            if self.pinned_nodes.contains(&nid) {
                                node.pos[0] += delta.x * zoom;
                                node.pos[1] += delta.y * zoom;
                            } else {
                                // delta is in egui logical coords; convert to canvas coords
                                node.pos[0] += delta.x;
                                node.pos[1] += delta.y;
                            }
                        }
                    }
                }
            }
        }

        if let Some((src_node, src_port, is_output)) = dragging_from {
            if self.click_wiring {
                // Click-wiring mode: cancel on Escape or right-click
                let cancel = ctx.input(|i| i.key_pressed(egui::Key::Escape))
                    || ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary));
                if cancel {
                    dragging_from = None;
                    self.click_wiring = false;
                }
                // Connection completion is handled in the port click handlers above
            } else {
                // Normal drag mode: complete on release
                if ctx.input(|i| i.pointer.any_released()) {
                    if let Some(pointer) = ctx.pointer_latest_pos() {
                        // Find the CLOSEST compatible port within the hit radius,
                        // not the first arbitrary one from HashMap iteration.
                        let hit_radius = PORT_INTERACT * 2.0;
                        let mut best: Option<(f32, NodeId, usize, bool)> = None;
                        for (&(nid, pidx, is_input), &pos) in &port_positions {
                            let dist = pos.distance(pointer);
                            if dist < hit_radius && nid != src_node {
                                // Must be opposite direction (output→input or input→output)
                                let valid_dir = (is_output && is_input) || (!is_output && !is_input);
                                if valid_dir {
                                    // Check port type compatibility
                                    let src_kind = self.graph.port_kind(src_node, src_port, is_output);
                                    let tgt_kind = self.graph.port_kind(nid, pidx, !is_input);
                                    let compatible = match (src_kind, tgt_kind) {
                                        (Some(s), Some(t)) => PortKind::compatible(s, t),
                                        _ => true, // Unknown port kind — allow connection
                                    };
                                    if compatible {
                                        if best.as_ref().map(|b| dist < b.0).unwrap_or(true) {
                                            best = Some((dist, nid, pidx, is_input));
                                        }
                                    }
                                }
                            }
                        }
                        if let Some((_, nid, pidx, is_input)) = best {
                            if is_output && is_input {
                                pending_connections.push((src_node, src_port, nid, pidx));
                            } else {
                                pending_connections.push((nid, pidx, src_node, src_port));
                            }
                        }
                    }
                    dragging_from = None;
                }
            }
        }

        // Sync click_wiring state with egui temp data (for inline port nodes)
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("click_wiring_active"), self.click_wiring));
        // Pick up click-wire completions from inline ports
        if let Some((fn_, fp, tn, tp)) = ctx.data_mut(|d| d.get_temp::<(NodeId, usize, NodeId, usize)>(egui::Id::new("click_wire_complete"))) {
            ctx.data_mut(|d| d.remove::<(NodeId, usize, NodeId, usize)>(egui::Id::new("click_wire_complete")));
            pending_connections.push((fn_, fp, tn, tp));
            dragging_from = None;
            self.click_wiring = false;
        }

        self.dragging_from = dragging_from;
        self.port_positions = port_positions;
        self.node_rects = node_rects;
        self.monitor = monitor_state;
        self.ob = ob_manager;
        self.audio = audio_manager;

        // Reset property-edit undo coalescing when no interaction is active.
        // This lets the NEXT widget interaction push a fresh undo snapshot.
        if !ctx.input(|i| i.pointer.any_down()) && ctx.memory(|mem| mem.focused().is_none()) {
            self.property_undo_pushed = false;
        }

        // Audio chains are built after graph evaluation (below) so Select
        // nodes can be resolved using the current frame's port values.

        // Push undo snapshot if any graph mutations are pending
        // (palette_spawns push their own per-node undo snapshots below)
        if !nodes_to_delete.is_empty() || !pending_disconnects.is_empty()
            || !pending_connections.is_empty() {
            self.push_undo();
        }
        let any_deleted = !nodes_to_delete.is_empty();
        for id in nodes_to_delete {
            // Clear opt-drag state if the source or duplicated node is being deleted
            if self.opt_drag_source == Some(id) { self.opt_drag_source = None; }
            if self.opt_drag_created == Some(id) { self.opt_drag_created = None; }
            self.audio.remove_processor(id); // Remove from engine before graph
            self.midi.cleanup_node(id); self.serial.cleanup_node(id); self.osc.cleanup_node(id);
            self.ob.cleanup_node(id); self.audio.cleanup_node(id); crate::nodes::video_player::cleanup_node(id);
            // Drop GPU textures owned by this node immediately so VRAM
            // doesn't balloon when users rapidly spawn/delete image nodes.
            self.gpu_tex_cache.invalidate_node(id, self.wgpu_render_state.as_ref());
            self.graph.remove_node(id);
            // Clean up UI state for deleted node
            self.node_rects.remove(&id);
            self.port_positions.retain(|&(nid, _, _), _| nid != id);
        }
        if any_deleted {
            // Per-node `invalidate_node` clears the GPU cache, but the
            // egui temp data keys (`("dyn_img_cache", id)`, etc.) are
            // still alive. If the user then undoes the delete, the
            // restored node would short-circuit on the stale cache and
            // hand out a `PortValue::GpuImage` whose backing texture
            // was already invalidated, rendering blank until they
            // touched a parameter. Schedule a wholesale wipe.
            self.caches_dirty = true;
        }
        for (nid, port) in &pending_disconnects {
            // Skip mixer gain ports — they're graph-layer, not engine connections
            let is_mixer_gain_port = matches!(
                self.graph.nodes.get(nid).map(|n| &n.node_type),
                Some(NodeType::AudioMixer { .. })
            ) && port % 2 != 0;
            if !is_mixer_gain_port {
                let engine_port = self.mixer_engine_port(*nid, *port);
                self.audio.disconnect_audio(*nid, engine_port);
            }
            self.graph.remove_connections_to_port(*nid, *port);
        }

        // Generic Script stale-connection sweep: prune any connection whose
        // from_port or to_port falls outside the Script node's current valid
        // port range. This catches output-side stale wires (which the
        // to-port-based pending_disconnects mechanism can't handle) and any
        // edge cases from load/rename shuffles.
        {
            let script_info: Vec<(NodeId, usize, usize, bool)> = self.graph.nodes.iter()
                .filter_map(|(id, n)| match &n.node_type {
                    NodeType::Script { input_names, output_names, continuous, .. } =>
                        Some((*id, input_names.len(), output_names.len(), *continuous)),
                    _ => None,
                }).collect();
            if !script_info.is_empty() {
                let before = self.graph.connections.len();
                for (sid, n_in, n_out, cont) in &script_info {
                    let input_base = if *cont { 1 } else { 2 };
                    let max_in = input_base + n_in;
                    self.graph.connections.retain(|c| {
                        // Prune bad from-side (output port out of range)
                        if c.from_node == *sid && c.from_port >= *n_out { return false; }
                        // Prune bad to-side (input port: either control port [0..input_base]
                        // is always valid, or input_base..max_in is valid; anything beyond is stale)
                        if c.to_node == *sid && c.to_port >= max_in { return false; }
                        true
                    });
                }
                if self.graph.connections.len() != before {
                    self.graph.mark_topo_dirty();
                }
            }
        }
        for &(fn_, fp, tn, tp) in &pending_connections {
            let from_name = self.graph.nodes.get(&fn_).map(|n| n.node_type.title()).unwrap_or("?");
            let to_name = self.graph.nodes.get(&tn).map(|n| n.node_type.title()).unwrap_or("?");
            crate::system_log::log(format!("Connected {} (id:{}) → {} (id:{})", from_name, fn_, to_name, tn));
            self.graph.add_connection(fn_, fp, tn, tp);
            // Only send engine connect/disconnect for audio ports
            // For mixer nodes, skip gain control ports (odd: 1, 3, 5)
            let is_mixer_gain_port = matches!(
                self.graph.nodes.get(&tn).map(|n| &n.node_type),
                Some(NodeType::AudioMixer { .. })
            ) && tp % 2 != 0;
            if !is_mixer_gain_port && self.audio.has_processor(fn_) && self.audio.has_processor(tn) {
                let engine_port = self.mixer_engine_port(tn, tp);
                // Disconnect old, connect new
                self.audio.disconnect_audio(tn, engine_port);
                self.audio.connect_audio(fn_, tn, engine_port);
            }
        }
        // Spawn nodes from Palette clicks (place at center of the current viewport)
        // Each node addition gets its own undo snapshot so Ctrl+Z undoes one at a time.
        for (_palette_pos, nt) in palette_spawns {
            self.push_undo();
            let screen_center = ctx.screen_rect().center();
            // egui logical coords → canvas: canvas_pos = egui_pos - offset/zoom
            let off_e = self.canvas_offset / self.canvas_zoom;
            let cx = screen_center.x - off_e.x;
            let cy = screen_center.y - off_e.y;
            self.graph.add_node(nt, [cx, cy]);
        }
        // Clear MCP trigger flags on AI Request nodes
        for (&nid, node) in &mut self.graph.nodes {
            if let NodeType::AiRequest { status, .. } = &mut node.node_type {
                let trigger_id = egui::Id::new(("mcp_ai_triggered", nid));
                if ctx.data_mut(|d| d.get_temp::<bool>(trigger_id).unwrap_or(false)) {
                    ctx.data_mut(|d| d.remove::<bool>(trigger_id));
                    *status = "thinking".into();
                }
            }
        }
        // Handle WGSL "+" button spawn requests
        if let Some(req) = ctx.data_mut(|d| d.get_temp::<crate::nodes::wgsl_viewer::WgslSpawnRequest>(egui::Id::new("wgsl_spawn_request"))) {
            ctx.data_mut(|d| d.remove::<crate::nodes::wgsl_viewer::WgslSpawnRequest>(egui::Id::new("wgsl_spawn_request")));

            // Position the new node to the left of the WGSL node
            let wgsl_pos = self.graph.nodes.get(&req.target_node).map(|n| n.pos).unwrap_or([0.0, 0.0]);
            let spawn_pos = [wgsl_pos[0] - 200.0, wgsl_pos[1] + (req.target_port as f32 * 30.0)];

            let (new_id, from_port) = match req.source_type.as_str() {
                "time" => {
                    let id = self.graph.add_node(
                        NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(crate::nodes::time_node::TimeNode::default()) } },
                        spawn_pos,
                    );
                    (id, 0) // Time node output port 0 = Seconds
                }
                "color" => {
                    let id = self.graph.add_node(
                        NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(crate::nodes::color_node::ColorNode::default()) } },
                        spawn_pos,
                    );
                    (id, 0) // Color node output port 0 = R (will connect R, G, B separately)
                }
                _ => {
                    // Slider with smart defaults
                    let (min, max, step, value) = crate::nodes::wgsl_viewer::slider_defaults_for_uniform(&req.uniform_name);
                    let id = self.graph.add_node(
                        NodeType::Slider { value, min, max, step, slider_color: [80, 160, 255], label: req.uniform_name.clone() },
                        spawn_pos,
                    );
                    (id, 0) // Slider output port 0 = Value
                }
            };

            // Connect the new node to the WGSL uniform port
            if req.source_type == "color" {
                // Connect R, G, B outputs to 3 consecutive ports
                self.graph.add_connection(new_id, 0, req.target_node, req.target_port);     // R
                self.graph.add_connection(new_id, 1, req.target_node, req.target_port + 1); // G
                self.graph.add_connection(new_id, 2, req.target_node, req.target_port + 2); // B
            } else {
                self.graph.add_connection(new_id, from_port, req.target_node, req.target_port);
            }

            crate::system_log::log(format!("Auto-created {} for u.{}", req.source_type, req.uniform_name));
        }

        // Handle WGSL Presets "+" button: spawn a paired WGSL Viewer
        // to the right of the picker and auto-wire Shader -> WGSL.
        if let Some(req) = ctx.data_mut(|d| d.get_temp::<crate::nodes::wgsl_presets::WgslPresetsSpawnRequest>(egui::Id::new("wgsl_presets_spawn_request"))) {
            ctx.data_mut(|d| d.remove::<crate::nodes::wgsl_presets::WgslPresetsSpawnRequest>(egui::Id::new("wgsl_presets_spawn_request")));

            let src_pos = self.graph.nodes.get(&req.source_node).map(|n| n.pos).unwrap_or([0.0, 0.0]);
            let spawn_pos = [src_pos[0] + 320.0, src_pos[1]];

            // Refinement H: if the picker's current preset relies on
            // self-feedback (samples image_a / image_b), pre-set Input A to
            // LastFrame so the paired viewer renders correctly with no extra clicks.
            let is_feedback = self
                .graph
                .nodes
                .get(&req.source_node)
                .and_then(|n| match &n.node_type {
                    NodeType::Dynamic { inner } => {
                        let st = inner.node.save_state();
                        st.get("preset_key")
                            .and_then(|v| v.as_str())
                            .map(|k| crate::nodes::wgsl_presets::is_feedback_preset(k))
                    }
                    _ => None,
                })
                .unwrap_or(false);
            let initial_image_a_mode = if is_feedback {
                crate::graph::ImageInputMode::LastFrame
            } else {
                crate::graph::ImageInputMode::Unused
            };

            let viewer_id = self.graph.add_node(
                NodeType::WgslViewer {
                    wgsl_code: String::new(),
                    uniform_names: vec![],
                    uniform_types: vec![],
                    uniform_values: vec![],
                    uniform_min: vec![],
                    uniform_max: vec![],
                    canvas_w: 400.0,
                    canvas_h: 300.0,
                    resolution: 120,
                    expanded: false,
                    image_a_mode: initial_image_a_mode,
                    image_b_mode: crate::graph::ImageInputMode::Unused,
                    feedback_reset_pending: false,
                    last_compile_ok: false,
                    pending_shader_hash: 0,
                    pending_shader_since_ms: 0,
                    image_a_hint_shown: false,
                    image_b_hint_shown: false,
                    last_auto_reset_ms: 0,
                },
                spawn_pos,
            );
            // Auto-connect picker port 0 (Shader) -> viewer port 0 (WGSL)
            self.graph.add_connection(req.source_node, 0, viewer_id, 0);
            crate::system_log::log("Spawned paired WGSL Viewer for Formula Preset Picker".to_string());
        }

        self.midi.process(midi_actions);
        self.serial.process(serial_actions);
        self.osc.process(osc_actions);
        self.http.process(http_actions);
    }
}

impl eframe::App for PatchworkApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain any pending wholesale-cache wipe queued by load /
        // restore / undo / redo / delete on the previous frame.
        //
        // Must run BEFORE the menu bar — otherwise a fresh File→Open
        // click on this same frame would write `file_action_load`
        // into temp data and the wipe (which clears all temp data)
        // would eat it before `handle_system_node_actions` runs.
        // It also runs before `gpu_tex_cache.begin_frame` further
        // down, so the cache LRU doesn't see the previous project's
        // entries.
        self.clear_caches_if_dirty(ctx);

        // ── OS-level menu bar (always visible) ──
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("\u{2795} New Project").clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("file_action_new"), true));
                        ui.close_menu();
                    }
                    if ui.button("\u{1f4c2} Open...").clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("file_action_load"), true));
                        ui.close_menu();
                    }
                    if ui.button("\u{1f4be} Save").clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("file_action_save"), true));
                        ui.close_menu();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    let can_undo = self.undo_history.can_undo();
                    let can_redo = self.undo_history.can_redo();
                    if ui.add_enabled(can_undo, egui::Button::new("\u{21a9} Undo")).clicked() {
                        self.perform_undo();
                        ui.close_menu();
                    }
                    if ui.add_enabled(can_redo, egui::Button::new("\u{21aa} Redo")).clicked() {
                        self.perform_redo();
                        ui.close_menu();
                    }
                });
            });
        });

        // set_zoom_factor scales everything (nodes, text, chrome) via GPU — zero overhead.
        // Pinned nodes compensate with inverse zoom style + zoom-keyed window IDs.
        ctx.set_zoom_factor(self.canvas_zoom);

        self.apply_theme(ctx);
        self.handle_file_drop(ctx);
        self.update_mouse_trackers(ctx);
        self.update_key_inputs(ctx);
        self.poll_midi_inputs();
        self.poll_serial_inputs();
        self.poll_osc_inputs();
        self.poll_http_responses();
        self.ob.poll_all();
        self.poll_ml_inference(ctx);

        self.frame_count += 1;
        // Receive device lists from background enumeration thread (non-blocking)
        if let Ok(devices) = self.device_refresh_rx.try_recv() {
            self.midi.set_port_lists(devices.midi_in, devices.midi_out);
            self.serial.set_port_list(devices.serial);
            self.audio.set_device_lists(devices.audio_output, devices.audio_input);
            self.audio.device_channel_counts = devices.audio_output_channels;
        }

        let node_count = self.graph.nodes.len();
        let conn_count = self.graph.connections.len();
        self.monitor.tick(node_count, conn_count);

        self.gpu_tex_cache.begin_frame(self.wgpu_render_state.as_ref());
        let now_secs = self.app_start_instant.elapsed().as_secs_f64();
        let mut values = self.graph.evaluate(now_secs);

        // Inject OB hardware values into the graph (before they're used by downstream nodes)
        // These need a second evaluation pass to propagate through Add/Multiply/etc.
        {
            let mut ob_injected = false;
            for (&id, node) in &self.graph.nodes {
                match &node.node_type {
                    NodeType::ObHub { detected_devices, .. } => {
                        let mut port_idx = 0usize;
                        let mut sorted = detected_devices.clone();
                        sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
                        for (dtype, did) in &sorted {
                            if let Some(hub) = self.ob.get_hub(id) {
                                match dtype.as_str() {
                                    "joystick" => {
                                        values.insert((id, port_idx), PortValue::Float(hub.get_value("joystick", *did, "x")));
                                        values.insert((id, port_idx + 1), PortValue::Float(hub.get_value("joystick", *did, "y")));
                                        values.insert((id, port_idx + 2), PortValue::Float(hub.get_value("joystick", *did, "btn")));
                                        port_idx += 3;
                                    }
                                    "encoder" => {
                                        values.insert((id, port_idx), PortValue::Float(hub.get_value("encoder", *did, "turn")));
                                        values.insert((id, port_idx + 1), PortValue::Float(hub.get_value("encoder", *did, "click")));
                                        values.insert((id, port_idx + 2), PortValue::Float(hub.get_value("encoder", *did, "position")));
                                        port_idx += 3;
                                    }
                                    "move" => {
                                        values.insert((id, port_idx), PortValue::Float(hub.get_value("move", *did, "ax")));
                                        values.insert((id, port_idx + 1), PortValue::Float(hub.get_value("move", *did, "ay")));
                                        values.insert((id, port_idx + 2), PortValue::Float(hub.get_value("move", *did, "az")));
                                        values.insert((id, port_idx + 3), PortValue::Float(hub.get_value("move", *did, "gx")));
                                        values.insert((id, port_idx + 4), PortValue::Float(hub.get_value("move", *did, "gy")));
                                        values.insert((id, port_idx + 5), PortValue::Float(hub.get_value("move", *did, "gz")));
                                        port_idx += 6;
                                    }
                                    "bend" => {
                                        values.insert((id, port_idx), PortValue::Float(hub.get_value("bend", *did, "val")));
                                        port_idx += 1;
                                    }
                                    "pressure" => {
                                        values.insert((id, port_idx), PortValue::Float(hub.get_value("pressure", *did, "val")));
                                        port_idx += 1;
                                    }
                                    "distance" => {
                                        values.insert((id, port_idx), PortValue::Float(hub.get_value("distance", *did, "val")));
                                        port_idx += 1;
                                    }
                                    "orb" => {
                                        values.insert((id, port_idx), PortValue::Float(hub.get_value("orb", *did, "ax")));
                                        values.insert((id, port_idx + 1), PortValue::Float(hub.get_value("orb", *did, "ay")));
                                        values.insert((id, port_idx + 2), PortValue::Float(hub.get_value("orb", *did, "az")));
                                        values.insert((id, port_idx + 3), PortValue::Float(hub.get_value("orb", *did, "gx")));
                                        values.insert((id, port_idx + 4), PortValue::Float(hub.get_value("orb", *did, "gy")));
                                        values.insert((id, port_idx + 5), PortValue::Float(hub.get_value("orb", *did, "gz")));
                                        port_idx += 6;
                                    }
                                    _ => { port_idx += 1; }
                                }
                            }
                        }
                        ob_injected = true;
                    }
                    NodeType::ObJoystick { device_id, hub_node_id, .. } => {
                        let find = self.ob.get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("joystick", *device_id))
                            .or_else(|| self.ob.find_device("joystick", *device_id).map(|(_, d)| d));
                        if let Some(dev) = find {
                            let x = dev.values.get("x").copied().unwrap_or(0.0);
                            let y = dev.values.get("y").copied().unwrap_or(0.0);
                            let btn = dev.values.get("btn").copied().unwrap_or(0.0);
                            values.insert((id, 0), PortValue::Float(x));
                            values.insert((id, 1), PortValue::Float(y));
                            values.insert((id, 2), PortValue::Float(btn));
                            // Changed trigger: compare with previous frame
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<[f32; 3]> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let curr = [x, y, btn];
                            let changed = prev.map(|p| (p[0]-curr[0]).abs() > 0.001 || (p[1]-curr[1]).abs() > 0.001 || (p[2]-curr[2]).abs() > 0.5).unwrap_or(false);
                            values.insert((id, 3), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, curr));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObEncoder { device_id, hub_node_id, .. } => {
                        let find = self.ob.get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("encoder", *device_id))
                            .or_else(|| self.ob.find_device("encoder", *device_id).map(|(_, d)| d));
                        if let Some(dev) = find {
                            let turn = dev.values.get("turn").copied().unwrap_or(0.0);
                            let click = dev.values.get("click").copied().unwrap_or(0.0);
                            let pos = dev.values.get("position").copied().unwrap_or(0.0);
                            values.insert((id, 0), PortValue::Float(turn));
                            values.insert((id, 1), PortValue::Float(click));
                            values.insert((id, 2), PortValue::Float(pos));
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<[f32; 3]> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let curr = [turn, click, pos];
                            let changed = prev.map(|p| (p[0]-curr[0]).abs() > 0.001 || (p[1]-curr[1]).abs() > 0.5 || (p[2]-curr[2]).abs() > 0.001).unwrap_or(false);
                            values.insert((id, 3), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, curr));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObMove { device_id, hub_node_id } => {
                        let find = self.ob.get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("move", *device_id))
                            .or_else(|| self.ob.find_device("move", *device_id).map(|(_, d)| d));
                        if let Some(dev) = find {
                            let vals: [f32; 6] = [
                                dev.values.get("ax").copied().unwrap_or(0.0),
                                dev.values.get("ay").copied().unwrap_or(0.0),
                                dev.values.get("az").copied().unwrap_or(0.0),
                                dev.values.get("gx").copied().unwrap_or(0.0),
                                dev.values.get("gy").copied().unwrap_or(0.0),
                                dev.values.get("gz").copied().unwrap_or(0.0),
                            ];
                            for i in 0..6 { values.insert((id, i), PortValue::Float(vals[i])); }
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<[f32; 6]> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let changed = prev.map(|p| (0..6).any(|i| (p[i]-vals[i]).abs() > 0.001)).unwrap_or(false);
                            values.insert((id, 6), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, vals));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObBend { device_id, hub_node_id, .. } => {
                        let find = self.ob.get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("bend", *device_id))
                            .or_else(|| self.ob.find_device("bend", *device_id).map(|(_, d)| d));
                        if let Some(dev) = find {
                            let val = dev.values.get("val").copied().unwrap_or(0.0);
                            values.insert((id, 0), PortValue::Float(val));
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<f32> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let changed = prev.map(|p| (p - val).abs() > 0.001).unwrap_or(false);
                            values.insert((id, 1), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, val));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObPressure { device_id, hub_node_id, .. } => {
                        let find = self.ob.get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("pressure", *device_id))
                            .or_else(|| self.ob.find_device("pressure", *device_id).map(|(_, d)| d));
                        if let Some(dev) = find {
                            let val = dev.values.get("val").copied().unwrap_or(0.0);
                            values.insert((id, 0), PortValue::Float(val));
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<f32> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let changed = prev.map(|p| (p - val).abs() > 0.001).unwrap_or(false);
                            values.insert((id, 1), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, val));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObDistance { device_id, hub_node_id, .. } => {
                        let find = self.ob.get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("distance", *device_id))
                            .or_else(|| self.ob.find_device("distance", *device_id).map(|(_, d)| d));
                        if let Some(dev) = find {
                            let val = dev.values.get("val").copied().unwrap_or(0.0);
                            values.insert((id, 0), PortValue::Float(val));
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<f32> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let changed = prev.map(|p| (p - val).abs() > 0.01).unwrap_or(false);
                            values.insert((id, 1), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, val));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObOrb { device_id, hub_node_id, .. } => {
                        let find = self.ob.get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("orb", *device_id))
                            .or_else(|| self.ob.find_device("orb", *device_id).map(|(_, d)| d));
                        if let Some(dev) = find {
                            let vals: [f32; 6] = [
                                dev.values.get("ax").copied().unwrap_or(0.0),
                                dev.values.get("ay").copied().unwrap_or(0.0),
                                dev.values.get("az").copied().unwrap_or(0.0),
                                dev.values.get("gx").copied().unwrap_or(0.0),
                                dev.values.get("gy").copied().unwrap_or(0.0),
                                dev.values.get("gz").copied().unwrap_or(0.0),
                            ];
                            for i in 0..6 { values.insert((id, i), PortValue::Float(vals[i])); }
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<[f32; 6]> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let changed = prev.map(|p| (0..6).any(|i| (p[i]-vals[i]).abs() > 0.001)).unwrap_or(false);
                            values.insert((id, 6), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, vals));
                        }
                        ob_injected = true;
                    }
                    _ => {}
                }
            }
            // Re-evaluate to propagate OB values through downstream nodes (Add, Multiply, etc.)
            if ob_injected {
                self.graph.evaluate_with_existing(&mut values, now_secs);
            }
        }

        // Profiler output values
        // Profiler removed — Monitor is now trait-based (MonitorNode)

        // Evaluate Rust Plugin nodes
        {
            let plugin_ids: Vec<NodeId> = self.graph.nodes.iter()
                .filter(|(_, n)| matches!(n.node_type, NodeType::RustPlugin { .. }))
                .map(|(&id, _)| id)
                .collect();
            for id in plugin_ids {
                let inputs: Vec<PortValue> = self.graph.collect_inputs(id, &values);
                let node = match self.graph.nodes.get_mut(&id) { Some(n) => n, None => continue };
                let outputs = nodes::rust_plugin::evaluate(id, &mut node.node_type, &inputs, ctx);
                for (port, val) in outputs {
                    values.insert((id, port), val);
                }
            }
        }

        // Make wgpu render state available to image processing nodes via egui temp data
        if let Some(rs) = &self.wgpu_render_state {
            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new("wgpu_render_state"), rs.clone());
                d.insert_temp(egui::Id::new("wgpu_target_format"), rs.target_format);
            });
        }

        // Fast image content hash — samples ~32 pixels spread across the image.
        // Detects real content changes without hashing the full pixel buffer.
        let img_content_hash = |img: &ImageData| -> u64 {
            let mut h: u64 = (img.width as u64).wrapping_mul(31).wrapping_add(img.height as u64);
            let len = img.pixels.len();
            if len > 0 {
                let step = (len / 128).max(1);
                for i in (0..len).step_by(step).take(32) {
                    h = h.wrapping_mul(31).wrapping_add(img.pixels[i] as u64);
                }
            }
            h
        };

        // Hash either a CPU `Image` or a GPU `GpuImage` port value. The GPU
        // variant hashes (producer_node_id, port, frame_stamp) so a producer
        // re-render naturally invalidates the param-cache key downstream.
        let img_or_gpu_hash = |val: &PortValue| -> u64 {
            match val {
                PortValue::Image(img) => img_content_hash(img),
                PortValue::GpuImage(h) => {
                    let mut k = (h.node_id as u64).wrapping_mul(31).wrapping_add(h.port as u64);
                    k = k.wrapping_mul(31).wrapping_add(h.frame_stamp);
                    k
                }
                _ => 0,
            }
        };

        // Populate the per-WgslViewer CPU consumer hint thread_local so that
        // `render_offscreen` (which runs later this frame in the egui paint
        // pass) knows whether to skip its readback. Default-true is safe;
        // a stale hint after a graph edit causes one extra readback and
        // self-recovers next frame.
        crate::gpu_image::clear_cpu_consumer_hints();
        // Iterate in topo order so results are deterministic across
        // runs (HashMap iteration is hash-seed dependent and was a
        // source of post-load flicker).
        let topo = self.graph.ensure_topo_order();
        for &id in &topo {
            if let Some(node) = self.graph.nodes.get(&id) {
                if matches!(node.node_type, NodeType::WgslViewer { .. }) {
                    let needs = self.graph.has_cpu_consumer(id, 0);
                    crate::gpu_image::set_cpu_consumer_hint(id, needs);
                }
            }
        }
        // Inject WGSL Viewer Image output BEFORE image evaluation loop
        // so downstream image nodes (Effects, Style, Blend) can see it.
        // The producer (`render_offscreen`) publishes its texture into
        // `gpu_tex_cache` via the PENDING_CACHE_PUBLISHES queue, drained
        // at the top of this frame's `begin_frame`. If the cache has an
        // entry, we inject `PortValue::GpuImage` (zero-copy GPU handoff);
        // otherwise we fall back to the legacy `PortValue::Image` path
        // (which carries CPU bytes for downstream CPU consumers).
        for &id in &topo {
            let is_viewer = self.graph.nodes.get(&id)
                .map(|n| matches!(n.node_type, NodeType::WgslViewer { .. }))
                .unwrap_or(false);
            if is_viewer {
                if let Some(img) = ctx.data_mut(|d| d.get_temp::<std::sync::Arc<ImageData>>(egui::Id::new(("wgsl_image_output", id)))) {
                    let has_gpu_entry = self.gpu_tex_cache
                        .get_node_output_cloned(id, 0)
                        .is_some();
                    // If any downstream consumer needs CPU pixels (Transform,
                    // Crop, ColorChannel, AI Request, ML Model, save-to-disk,
                    // Theme BG, …) we must readback the GPU texture into a CPU
                    // ImageData here. Otherwise we'd hand them a GpuImage stub
                    // they silently drop.
                    let needs_cpu = self.graph.has_cpu_consumer(id, 0);
                    if has_gpu_entry && img.pixels.is_empty() && needs_cpu {
                        if let Some(rs) = &self.wgpu_render_state {
                            if let Some(cpu_img) = self.gpu_tex_cache.readback_node_output(id, 0, &rs.device, &rs.queue) {
                                values.insert((id, 0), PortValue::Image(cpu_img));
                                continue;
                            }
                        }
                        // Readback unavailable — fall through to GpuImage so
                        // GPU consumers still work; CPU consumers will skip.
                    }
                    if has_gpu_entry && img.pixels.is_empty() {
                        // Fully GPU-resident path: emit a GpuImage handle.
                        let handle = crate::graph::GpuImageHandle {
                            node_id: id,
                            port: 0,
                            width: img.width,
                            height: img.height,
                            frame_stamp: self.gpu_tex_cache.frame_stamp(id, 0),
                        };
                        values.insert((id, 0), PortValue::GpuImage(handle));
                    } else {
                        // CPU bytes are present (or no GPU entry yet) — keep
                        // the legacy CPU path so CPU consumers and the very
                        // first frame work correctly.
                        if let Some(rs) = &self.wgpu_render_state {
                            if !img.pixels.is_empty() {
                                self.gpu_tex_cache.get_or_upload(&rs.device, &rs.queue, &img);
                            }
                        }
                        values.insert((id, 0), PortValue::Image(img));
                    }
                }
            }
        }

        // Inject AudioAnalyzer + AudioPlayer values BEFORE image eval loop,
        // so downstream nodes (Map Range → Image Style) see fresh values.
        {
            let connections = self.graph.connections.clone();
            let analyzer_ids: Vec<NodeId> = self.graph.nodes.iter()
                .filter(|(_, n)| matches!(n.node_type, NodeType::AudioAnalyzer))
                .map(|(&id, _)| id).collect();
            for id in analyzer_ids {
                let (amp, peak, bass, mid, treble) = self.audio.analyzer_results.get(&id)
                    .and_then(|a| a.try_lock().ok())
                    .map(|a| (a.amplitude, a.peak, a.bass, a.mid, a.treble))
                    .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
                let source_name = connections.iter()
                    .find(|c| c.to_node == id && c.to_port == 0)
                    .and_then(|c| self.graph.nodes.get(&c.from_node))
                    .map(|n| n.node_type.title().to_string())
                    .unwrap_or_default();
                ctx.data_mut(|d| {
                    d.insert_temp(egui::Id::new(("audio_analysis", id)), [amp, peak, bass, mid, treble]);
                    d.insert_temp(egui::Id::new(("audio_analysis_source", id)), source_name);
                });
                values.insert((id, 1), PortValue::Float(amp));
                values.insert((id, 2), PortValue::Float(peak));
                values.insert((id, 3), PortValue::Float(bass));
                values.insert((id, 4), PortValue::Float(mid));
                values.insert((id, 5), PortValue::Float(treble));
            }

            // ── Spectrum Analyzer injection ────────────────────────────
            let spectrum_ids: Vec<NodeId> = self.graph.nodes.iter()
                .filter(|(_, n)| matches!(n.node_type, NodeType::SpectrumAnalyzer))
                .map(|(&id, _)| id).collect();
            for id in spectrum_ids {
                let sample_rate = self.audio.engine_sample_rate.max(1.0);
                let (bins, peak_hz, centroid_hz, energy) = self.audio.spectrum_results.get(&id)
                    .and_then(|s| s.try_lock().ok())
                    .map(|s| {
                        let bins = s.bins.clone();
                        let n = bins.len().max(1);
                        // Map bin index → frequency using same log-spaced 30 Hz → Nyquist scheme
                        // as the analysis pass.
                        let min_f = 30.0f32;
                        let max_f = (sample_rate * 0.5).max(min_f + 1.0);
                        let lmin = min_f.ln();
                        let lmax = max_f.ln();
                        let bin_to_hz = |b: usize| -> f32 {
                            let t = (b as f32 + 0.5) / n as f32;
                            (lmin + (lmax - lmin) * t).exp()
                        };
                        // Peak bin
                        let mut peak_idx = 0usize;
                        let mut peak_val = 0.0f32;
                        let mut sum_w = 0.0f32;
                        let mut sum_wf = 0.0f32;
                        let mut sum = 0.0f32;
                        for (i, &v) in bins.iter().enumerate() {
                            if v > peak_val { peak_val = v; peak_idx = i; }
                            let f = bin_to_hz(i);
                            sum_w += v;
                            sum_wf += v * f;
                            sum += v;
                        }
                        let peak_hz = bin_to_hz(peak_idx);
                        let centroid_hz = if sum_w > 1e-6 { sum_wf / sum_w } else { 0.0 };
                        let energy = (sum / n as f32).clamp(0.0, 1.0);
                        (bins, peak_hz, centroid_hz, energy)
                    })
                    .unwrap_or_else(|| (vec![0.0f32; crate::audio::analysis::SPECTRUM_BINS], 0.0, 0.0, 0.0));

                let source_name = connections.iter()
                    .find(|c| c.to_node == id && c.to_port == 0)
                    .and_then(|c| self.graph.nodes.get(&c.from_node))
                    .map(|n| n.node_type.title().to_string())
                    .unwrap_or_default();
                ctx.data_mut(|d| {
                    d.insert_temp(egui::Id::new(("spectrum_bins", id)), bins.clone());
                    d.insert_temp(egui::Id::new(("spectrum_scalars", id)), [peak_hz, centroid_hz, energy]);
                    d.insert_temp(egui::Id::new(("spectrum_source", id)), source_name);
                });
                // Image output (port 1) — rasterize bars off the audio thread.
                let img = crate::nodes::spectrum_analyzer::rasterize_bins(&bins, 256, 96);
                values.insert((id, 1), PortValue::Image(std::sync::Arc::new(img)));
                values.insert((id, 2), PortValue::Float(peak_hz));
                values.insert((id, 3), PortValue::Float(centroid_hz));
                values.insert((id, 4), PortValue::Float(energy));
            }
        }
        for (&id, node) in &self.graph.nodes {
            if matches!(node.node_type, NodeType::AudioPlayer { .. }) {
                if let Some(progress) = ctx.data_mut(|d| d.get_temp::<f32>(egui::Id::new(("audio_player_progress", id)))) {
                    values.insert((id, 1), PortValue::Float(progress));
                }
            }
        }

        // Re-evaluate Dynamic nodes that depend on freshly injected values
        // (e.g., Map Range reading from Audio Analyzer → feeds into Image Style).
        // Only re-evaluate nodes whose inputs changed since graph.evaluate().
        // Iterate in topo order so a chain of trait nodes propagates in
        // a single pass (HashMap order would leave deeper nodes one
        // frame behind).
        {
            let node_ids = self.graph.ensure_topo_order();
            for nid in node_ids {
                let is_dynamic = matches!(self.graph.nodes.get(&nid).map(|n| &n.node_type), Some(NodeType::Dynamic { .. }));
                if !is_dynamic { continue; }
                let inputs: Vec<PortValue> = self.graph.collect_inputs(nid, &values);
                // Skip nodes with image inputs (handled in the image loop below).
                // Must include GpuImage too — otherwise a Dynamic node with a
                // GpuImage input is re-evaluated here via the generic
                // `inner.node.evaluate(&inputs)` path which only matches
                // PortValue::Image and stamps `None` into `values`, racing
                // with the image loop's correct evaluation below.
                if inputs.iter().any(|v| matches!(v, PortValue::Image(_) | PortValue::GpuImage(_))) { continue; }
                // Only re-evaluate if any input is non-None (connected)
                if inputs.iter().all(|v| matches!(v, PortValue::None)) { continue; }
                if let Some(mut node) = self.graph.nodes.remove(&nid) {
                    if let NodeType::Dynamic { ref mut inner } = node.node_type {
                        let results = inner.node.evaluate(&inputs);
                        for (port, val) in results {
                            values.insert((nid, port), val);
                        }
                    }
                    self.graph.nodes.insert(nid, node);
                }
            }
        }

        // Evaluate image nodes (with caching — only reprocess when inputs change).
        //
        // Iterate in topological order so a chain of arbitrary depth
        // (Source → Blend → Effects → Blend → Effects → Display) finishes
        // in a SINGLE pass. The previous code iterated `nodes.keys()`
        // (HashMap order, hash-seed dependent) and ran two fixed passes,
        // which only saturated depth-2 chains and left deeper graphs
        // permanently one frame behind per extra hop, plus visible
        // flicker on reload depending on iteration order.
        let image_ids = self.graph.ensure_topo_order();
        {
            for id in image_ids {
                let inputs: Vec<PortValue> = self.graph.collect_inputs(id, &values);
                // Identity (producer NodeId, port) of each connected input. Lets GPU
                // node implementations look up the upstream texture in `gpu_tex_cache`
                // and skip the redundant CPU→GPU upload.
                let input_sources = self.graph.collect_input_sources(id);

                // Trait-based image nodes (Transform, ImageStyle, ColorChannel, Crop, etc.)
                // need re-evaluation here because image sources weren't populated during graph.evaluate().
                // Cached: only reprocess when inputs (image content + param state) change.
                let is_dynamic = matches!(self.graph.nodes.get(&id).map(|n| &n.node_type), Some(NodeType::Dynamic { .. }));
                if is_dynamic && inputs.iter().any(|v| matches!(v, PortValue::Image(_) | PortValue::GpuImage(_))) {
                    // Build cache key from image content + params + node state
                    let mut cache_key: u64 = 0;
                    for inp in &inputs {
                        match inp {
                            PortValue::Image(_) | PortValue::GpuImage(_) => {
                                cache_key = cache_key.wrapping_mul(31).wrapping_add(img_or_gpu_hash(inp))
                            }
                            PortValue::Float(f) => cache_key = cache_key.wrapping_mul(31).wrapping_add(f.to_bits() as u64),
                            _ => {}
                        }
                    }
                    // Include node state (save_state hash) for slider/param changes
                    if let Some(node) = self.graph.nodes.get(&id) {
                        if let NodeType::Dynamic { inner } = &node.node_type {
                            let state = inner.node.save_state();
                            let state_str = state.to_string();
                            for b in state_str.bytes() {
                                cache_key = cache_key.wrapping_mul(31).wrapping_add(b as u64);
                            }
                        }
                    }
                    let cache_id = egui::Id::new(("dyn_img_cache", id));
                    let cached: Option<(u64, Vec<(usize, PortValue)>)> = ctx.data_mut(|d| d.get_temp(cache_id));
                    if let Some((prev_key, prev_results)) = cached {
                        if prev_key == cache_key {
                            // Cache hit: keep any cached GPU output texture alive past
                            // the 2-frame LRU and re-publish to the display callback's
                            // prerendered map (which is cleared every frame).
                            let has_gpu_output = prev_results.iter().any(|(_, v)| matches!(v, PortValue::GpuImage(_)));
                            if has_gpu_output {
                                self.gpu_tex_cache.touch(id, 0);
                                if let Some(rs) = &self.wgpu_render_state {
                                    self.gpu_tex_cache.store_for_display(id, rs);
                                }
                            }
                            for (port, val) in prev_results {
                                values.insert((id, port), val);
                            }
                            continue;
                        }
                    }
                    // Cache miss — reprocess (GPU path for ImageStyleNode, CPU for others)
                    let needs_readback = self.graph.has_cpu_consumer(id, 0);

                    // For trait nodes that take the CPU `evaluate()` fallback
                    // (Transform, Crop, ColorChannel, etc. — anything that
                    // isn't `image_style`), `evaluate()` only matches
                    // `PortValue::Image` and silently drops `GpuImage`. So we
                    // do an on-demand readback here, transforming any
                    // `GpuImage` input into a CPU `PortValue::Image` *before*
                    // calling evaluate. The fast `image_style` GPU path keeps
                    // the original inputs (it consumes GpuImage stubs via
                    // gpu_source lookups).
                    let peek_tag = self.graph.nodes.get(&id).and_then(|n| match &n.node_type {
                        NodeType::Dynamic { inner } => Some(inner.node.type_tag().to_string()),
                        _ => None,
                    }).unwrap_or_default();
                    let cpu_inputs: Vec<PortValue> = if peek_tag == "image_style" {
                        inputs.clone()
                    } else {
                        inputs.iter().map(|v| match v {
                            PortValue::GpuImage(h) => {
                                if let Some(rs) = &self.wgpu_render_state {
                                    self.gpu_tex_cache.readback_node_output(h.node_id, h.port, &rs.device, &rs.queue)
                                        .map(PortValue::Image)
                                        .unwrap_or(PortValue::None)
                                } else { PortValue::None }
                            }
                            other => other.clone(),
                        }).collect()
                    };

                    if let Some(mut node_mut) = self.graph.nodes.remove(&id) {
                        if let NodeType::Dynamic { ref mut inner } = node_mut.node_type {
                            let tag = inner.node.type_tag().to_string();
                            let rs = self.wgpu_render_state.clone();

                            // Apply port inputs to node state BEFORE GPU processing.
                            // This ensures connected values (e.g., Amount from Map Range)
                            // override the slider value in the node's internal state.
                            // Use `cpu_inputs` so trait nodes that need CPU pixels
                            // see readback Images instead of GpuImage stubs.
                            inner.node.evaluate(&cpu_inputs);

                            // Try GPU path for supported nodes
                            let gpu_results: Option<Vec<(usize, PortValue)>> = match tag.as_str() {
                                "image_style" => {
                                    // Accept both CPU `Image` and GPU `GpuImage` upstream.
                                    // For GpuImage, build a dimensions-only stub; the
                                    // gpu_source lookup inside process_gpu_cached MUST
                                    // cache-hit (else process_gpu_cached returns None).
                                    let stub_owned: Option<ImageData> = match inputs.first() {
                                        Some(PortValue::GpuImage(h)) => Some(ImageData { width: h.width, height: h.height, pixels: Vec::new() }),
                                        _ => None,
                                    };
                                    let img_ref: Option<&ImageData> = match inputs.first() {
                                        Some(PortValue::Image(img)) => Some(img.as_ref()),
                                        Some(PortValue::GpuImage(_)) => stub_owned.as_ref(),
                                        _ => None,
                                    };
                                    if let (Some(rs), Some(img_for_call)) = (&rs, img_ref) {
                                        let state = inner.node.save_state();
                                        let gpu_src = input_sources.first().copied().flatten();
                                        let result = serde_json::from_value::<nodes::image_style_node::ImageStyleNode>(state).ok()
                                            .and_then(|sn| sn.process_gpu_cached(img_for_call, id, rs, &mut self.gpu_tex_cache, gpu_src, needs_readback));
                                        result.map(|val| vec![(0, val)])
                                    } else { None }
                                }
                                // color_channel: CPU path is faster (simple per-pixel multiply,
                                // GPU readback ×4 outputs is slower than CPU)
                                _ => None,
                            };

                            let results = gpu_results.unwrap_or_else(|| inner.node.evaluate(&cpu_inputs));
                            for &(port, ref val) in &results {
                                values.insert((id, port), val.clone());
                            }
                            ctx.data_mut(|d| d.insert_temp(cache_id, (cache_key, results)));
                        }
                        self.graph.nodes.insert(id, node_mut);
                    }
                    continue;
                }

                let node = match self.graph.nodes.get(&id) { Some(n) => n, None => continue };
                match &node.node_type {
                    NodeType::ImageNode { image_data, .. } => {
                        // Pass-through any image variant; downstream may be CPU
                        // or GPU. ImageNode itself returns false from
                        // `needs_cpu_image_input`, so its upstream may emit a
                        // GpuImage stub. If a downstream of THIS node needs
                        // CPU pixels (AI Request, ML Model, Theme BG, save-
                        // to-disk, …) we readback the GpuImage here so the
                        // forwarded value is a real `Image`. Otherwise the
                        // CPU consumer would silently see no image.
                        let needs_cpu_downstream = self.graph.has_cpu_consumer(id, 0);
                        let out = match inputs.first() {
                            Some(PortValue::Image(img)) => PortValue::Image(img.clone()),
                            Some(PortValue::GpuImage(h)) => {
                                if needs_cpu_downstream {
                                    if let Some(rs) = &self.wgpu_render_state {
                                        if let Some(cpu_img) = self.gpu_tex_cache.readback_node_output(h.node_id, h.port, &rs.device, &rs.queue) {
                                            PortValue::Image(cpu_img)
                                        } else {
                                            PortValue::GpuImage(*h)
                                        }
                                    } else {
                                        PortValue::GpuImage(*h)
                                    }
                                } else {
                                    PortValue::GpuImage(*h)
                                }
                            }
                            _ => image_data.as_ref().map(|img| PortValue::Image(img.clone())).unwrap_or(PortValue::None),
                        };
                        values.insert((id, 0), out);
                    }
                    NodeType::ImageEffects { brightness, contrast, saturation, hue, exposure, gamma } => {
                        // Accept either CPU `Image` or GPU `GpuImage` upstream.
                        let stub_owned: Option<ImageData> = match inputs.first() {
                            Some(PortValue::GpuImage(h)) => Some(ImageData { width: h.width, height: h.height, pixels: Vec::new() }),
                            _ => None,
                        };
                        let img_ref: Option<&ImageData> = match inputs.first() {
                            Some(PortValue::Image(img)) => Some(img.as_ref()),
                            Some(PortValue::GpuImage(_)) => stub_owned.as_ref(),
                            _ => None,
                        };
                        if let Some(img) = img_ref {
                            let param_hash = ((*brightness * 1000.0) as u64) ^ ((*contrast * 1000.0) as u64) << 8
                                ^ ((*saturation * 1000.0) as u64) << 16 ^ ((*hue * 10.0) as u64) << 24
                                ^ ((*exposure * 1000.0) as u64) << 32 ^ ((*gamma * 1000.0) as u64) << 40;
                            let cache_key = param_hash ^ img_or_gpu_hash(inputs.first().unwrap());
                            let cache_id = egui::Id::new(("img_fx_cache", id));
                            let cached: Option<(u64, PortValue)> = ctx.data_mut(|d| d.get_temp(cache_id));
                            if let Some((prev_key, prev_val)) = cached {
                                if prev_key == cache_key {
                                    if matches!(&prev_val, PortValue::GpuImage(_)) {
                                        self.gpu_tex_cache.touch(id, 0);
                                        if let Some(rs) = &self.wgpu_render_state {
                                            self.gpu_tex_cache.store_for_display(id, rs);
                                        }
                                    }
                                    values.insert((id, 0), prev_val);
                                    continue;
                                }
                            }
                            let needs_readback = self.graph.has_cpu_consumer(id, 0);
                            let result_val: PortValue = if let Some(rs) = &self.wgpu_render_state {
                                nodes::image_effects::process_gpu_cached(img, *brightness, *contrast, *saturation, *hue, *exposure, *gamma, id, rs, &mut self.gpu_tex_cache, input_sources.first().copied().flatten(), needs_readback)
                                    .unwrap_or_else(|| {
                                        // CPU fallback only meaningful when we actually have pixels
                                        if !img.pixels.is_empty() {
                                            PortValue::Image(nodes::image_effects::process(img, *brightness, *contrast, *saturation, *hue, *exposure, *gamma))
                                        } else {
                                            PortValue::None
                                        }
                                    })
                            } else if !img.pixels.is_empty() {
                                PortValue::Image(nodes::image_effects::process(img, *brightness, *contrast, *saturation, *hue, *exposure, *gamma))
                            } else {
                                PortValue::None
                            };
                            ctx.data_mut(|d| d.insert_temp(cache_id, (cache_key, result_val.clone())));
                            values.insert((id, 0), result_val);
                        }
                    }
                    // Crop migrated to trait-based CropNode (evaluated in graph.evaluate)
                    NodeType::Blend { mode, mix } => {
                        let stub_a: Option<ImageData> = match inputs.first() {
                            Some(PortValue::GpuImage(h)) => Some(ImageData { width: h.width, height: h.height, pixels: Vec::new() }),
                            _ => None,
                        };
                        let stub_b: Option<ImageData> = match inputs.get(1) {
                            Some(PortValue::GpuImage(h)) => Some(ImageData { width: h.width, height: h.height, pixels: Vec::new() }),
                            _ => None,
                        };
                        let a: Option<&ImageData> = match inputs.first() {
                            Some(PortValue::Image(img)) => Some(img.as_ref()),
                            Some(PortValue::GpuImage(_)) => stub_a.as_ref(),
                            _ => None,
                        };
                        let b: Option<&ImageData> = match inputs.get(1) {
                            Some(PortValue::Image(img)) => Some(img.as_ref()),
                            Some(PortValue::GpuImage(_)) => stub_b.as_ref(),
                            _ => None,
                        };
                        if let (Some(a), Some(b)) = (a, b) {
                            let h_a = inputs.first().map(|v| img_or_gpu_hash(v)).unwrap_or(0);
                            let h_b = inputs.get(1).map(|v| img_or_gpu_hash(v)).unwrap_or(0);
                            let cache_key = h_a ^ h_b.wrapping_mul(7)
                                ^ (*mode as u64) ^ ((*mix * 1000.0) as u64) << 8;
                            let cache_id = egui::Id::new(("blend_cache", id));
                            let cached: Option<(u64, PortValue)> = ctx.data_mut(|d| d.get_temp(cache_id));
                            if let Some((prev_key, prev_val)) = cached {
                                if prev_key == cache_key {
                                    if matches!(&prev_val, PortValue::GpuImage(_)) {
                                        self.gpu_tex_cache.touch(id, 0);
                                        if let Some(rs) = &self.wgpu_render_state {
                                            self.gpu_tex_cache.store_for_display(id, rs);
                                        }
                                    }
                                    values.insert((id, 0), prev_val);
                                    continue;
                                }
                            }
                            let needs_readback = self.graph.has_cpu_consumer(id, 0);
                            let result_val: PortValue = if let Some(rs) = &self.wgpu_render_state {
                                nodes::blend::process_gpu_cached(a, b, *mode, *mix, id, rs, &mut self.gpu_tex_cache, input_sources.first().copied().flatten(), input_sources.get(1).copied().flatten(), needs_readback)
                                    .unwrap_or_else(|| {
                                        if !a.pixels.is_empty() && !b.pixels.is_empty() {
                                            PortValue::Image(nodes::blend::process(a, b, *mode, *mix))
                                        } else {
                                            PortValue::None
                                        }
                                    })
                            } else if !a.pixels.is_empty() && !b.pixels.is_empty() {
                                PortValue::Image(nodes::blend::process(a, b, *mode, *mix))
                            } else {
                                PortValue::None
                            };
                            ctx.data_mut(|d| d.insert_temp(cache_id, (cache_key, result_val.clone())));
                            values.insert((id, 0), result_val);
                        }
                    }
                    NodeType::Curve { points, .. } => {
                        // Y/Phase/End are computed in graph.evaluate().
                        // Here we generate the Image LUT output (port 3).
                        // Order-sensitive `*31 + bits` (the same chain
                        // every other cache key uses). The previous XOR-
                        // then-sum collapsed under point reorder and
                        // had a high collision rate, so two distinct
                        // curve edits could share a hash and yield a
                        // stale LUT.
                        let mut pts_hash: u64 = points.len() as u64;
                        for p in points.iter() {
                            pts_hash = pts_hash.wrapping_mul(31).wrapping_add(p[0].to_bits() as u64);
                            pts_hash = pts_hash.wrapping_mul(31).wrapping_add(p[1].to_bits() as u64);
                        }
                        let cache_id = egui::Id::new(("curve_lut_cache", id));
                        let cached: Option<(u64, std::sync::Arc<ImageData>)> = ctx.data_mut(|d| d.get_temp(cache_id));
                        let need_regen = cached.as_ref().map(|(h, _)| *h != pts_hash).unwrap_or(true);
                        if need_regen {
                            // Generate 256×1 LUT image
                            let w = 256u32;
                            let h = 1u32;
                            let mut pixels = Vec::with_capacity((w * h * 4) as usize);
                            for i in 0..w {
                                let t = i as f32 / (w - 1) as f32;
                                let v = nodes::curve::evaluate_curve(points, t);
                                let byte = (v.clamp(0.0, 1.0) * 255.0) as u8;
                                pixels.extend_from_slice(&[byte, byte, byte, 255]);
                            }
                            let img = std::sync::Arc::new(ImageData::new(w, h, pixels));
                            ctx.data_mut(|d| d.insert_temp(cache_id, (pts_hash, img.clone())));
                            values.insert((id, 3), PortValue::Image(img));
                        } else if let Some((_, img)) = cached {
                            values.insert((id, 3), PortValue::Image(img));
                        }
                    }
                    // Draw migrated to trait-based DrawNode
                    // Noise migrated to trait-based NoiseNode (evaluated in graph.evaluate)
                    NodeType::ColorCurves { master, red, green, blue, .. } => {
                        let stub_owned: Option<ImageData> = match inputs.first() {
                            Some(PortValue::GpuImage(h)) => Some(ImageData { width: h.width, height: h.height, pixels: Vec::new() }),
                            _ => None,
                        };
                        let img_ref: Option<&ImageData> = match inputs.first() {
                            Some(PortValue::Image(img)) => Some(img.as_ref()),
                            Some(PortValue::GpuImage(_)) => stub_owned.as_ref(),
                            _ => None,
                        };
                        if let Some(img) = img_ref {
                            let mut curve_hash: u64 = 0;
                            for pts in [master.as_slice(), red.as_slice(), green.as_slice(), blue.as_slice()] {
                                for p in pts {
                                    curve_hash = curve_hash.wrapping_mul(31).wrapping_add(p[0].to_bits() as u64);
                                    curve_hash = curve_hash.wrapping_mul(31).wrapping_add(p[1].to_bits() as u64);
                                }
                            }
                            let cache_key = img_or_gpu_hash(inputs.first().unwrap()) ^ curve_hash;
                            let cache_id = egui::Id::new(("cc_cache", id));
                            let cached: Option<(u64, PortValue)> = ctx.data_mut(|d| d.get_temp(cache_id));
                            if let Some((prev_key, prev_val)) = cached {
                                if prev_key == cache_key {
                                    if matches!(&prev_val, PortValue::GpuImage(_)) {
                                        self.gpu_tex_cache.touch(id, 0);
                                        if let Some(rs) = &self.wgpu_render_state {
                                            self.gpu_tex_cache.store_for_display(id, rs);
                                        }
                                    }
                                    values.insert((id, 0), prev_val);
                                    continue;
                                }
                            }
                            let needs_readback = self.graph.has_cpu_consumer(id, 0);
                            let result_val: PortValue = if let Some(rs) = &self.wgpu_render_state {
                                nodes::color_curves::process_gpu_cached(
                                    img, master, red, green, blue, id, rs, &mut self.gpu_tex_cache, input_sources.first().copied().flatten(), needs_readback)
                                    .unwrap_or_else(|| {
                                        if !img.pixels.is_empty() {
                                            PortValue::Image(nodes::color_curves::process(img, master, red, green, blue))
                                        } else {
                                            PortValue::None
                                        }
                                    })
                            } else if !img.pixels.is_empty() {
                                PortValue::Image(nodes::color_curves::process(img, master, red, green, blue))
                            } else {
                                PortValue::None
                            };
                            ctx.data_mut(|d| d.insert_temp(cache_id, (cache_key, result_val.clone())));
                            values.insert((id, 0), result_val);
                        }
                    }
                    NodeType::VideoPlayer { current_frame, duration, .. } => {
                        if let Some(frame) = current_frame {
                            values.insert((id, 0), PortValue::Image(frame.clone()));
                            if let Some(rs) = &self.wgpu_render_state {
                                self.gpu_tex_cache.get_or_upload(&rs.device, &rs.queue, frame);
                                self.gpu_tex_cache.mark_node_source(frame, id, 0);
                            }
                            if *duration > 0.0 {
                                // Progress output would need frame counting — skip for now
                                values.insert((id, 1), PortValue::Float(0.0));
                            }
                        }
                    }
                    NodeType::Camera { current_frame, .. } => {
                        if let Some(frame) = current_frame {
                            values.insert((id, 0), PortValue::Image(frame.clone()));
                            // Pre-upload to GPU cache so downstream GPU nodes skip upload.
                            // Tag the entry with (node_id, 0) so the per-frame snapshot
                            // exports it for WGSL Viewer External image inputs to resolve.
                            if let Some(rs) = &self.wgpu_render_state {
                                self.gpu_tex_cache.get_or_upload(&rs.device, &rs.queue, frame);
                                self.gpu_tex_cache.mark_node_source(frame, id, 0);
                            }
                        }
                    }
                    NodeType::MlModel { annotated_frame, result_text, result_json, .. } => {
                        if let Some(frame) = annotated_frame {
                            values.insert((id, 0), PortValue::Image(frame.clone()));
                        }
                        if !result_text.is_empty() {
                            values.insert((id, 1), PortValue::Text(result_text.clone()));
                        }
                        if !result_json.is_empty() {
                            values.insert((id, 2), PortValue::Text(result_json.clone()));
                        }
                    }
                    // Trait-based Dynamic nodes with image inputs are handled above (before the match)
                    _ => {}
                }
            }
        } // end image-pass topo iteration

        // AudioAnalyzer + AudioPlayer values already injected before the image evaluation loop above.

        // Sync OB Hub detected_devices from ObManager + auto-connect saved ports
        {
            let hub_nodes: Vec<(NodeId, String)> = self.graph.nodes.iter()
                .filter_map(|(&id, n)| {
                    if let NodeType::ObHub { port_name, .. } = &n.node_type {
                        Some((id, port_name.clone()))
                    } else {
                        None
                    }
                })
                .collect();

            for (hub_id, port_name) in &hub_nodes {
                // Auto-connect: if port_name is set but hub not connected, try to reconnect
                if !port_name.is_empty() && self.ob.get_hub(*hub_id).is_none() {
                    let _ = self.ob.connect_hub(*hub_id, port_name);
                }

                // Sync detected_devices into NodeType
                let detected: Vec<(String, u8)> = self.ob.get_hub(*hub_id)
                    .map(|h| h.devices.keys().cloned().collect())
                    .unwrap_or_default();

                // Build a label list for a (sorted) device set mirroring
                // the output-port layout used in Graph::outputs() for ObHub.
                // We compare old vs new so that when a device drops off
                // (unplugged / timed out) or a new one takes its slot,
                // any wire whose semantic meaning changed is disconnected
                // instead of silently re-binding to the wrong device.
                let build_labels = |devs: &[(String, u8)]| -> Vec<String> {
                    let mut sorted = devs.to_vec();
                    sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
                    let mut labels: Vec<String> = Vec::new();
                    for (dtype, id) in &sorted {
                        match dtype.as_str() {
                            "joystick" => {
                                labels.push(format!("j{}_x", id));
                                labels.push(format!("j{}_y", id));
                                labels.push(format!("j{}_btn", id));
                            }
                            "encoder" => {
                                labels.push(format!("e{}_turn", id));
                                labels.push(format!("e{}_click", id));
                                labels.push(format!("e{}_pos", id));
                            }
                            "move" => {
                                labels.push(format!("m{}_ax", id));
                                labels.push(format!("m{}_ay", id));
                                labels.push(format!("m{}_az", id));
                                labels.push(format!("m{}_gx", id));
                                labels.push(format!("m{}_gy", id));
                                labels.push(format!("m{}_gz", id));
                            }
                            "bend" => labels.push(format!("b{}_val", id)),
                            "pressure" => labels.push(format!("p{}_val", id)),
                            "distance" => labels.push(format!("d{}_val", id)),
                            "orb" => {
                                labels.push(format!("orb{}_ax", id));
                                labels.push(format!("orb{}_ay", id));
                                labels.push(format!("orb{}_az", id));
                                labels.push(format!("orb{}_gx", id));
                                labels.push(format!("orb{}_gy", id));
                                labels.push(format!("orb{}_gz", id));
                            }
                            other => {
                                labels.push(format!("{}{}_val",
                                    other.chars().next().map(|c| c.to_string()).unwrap_or_default(),
                                    id));
                            }
                        }
                    }
                    labels
                };

                let mut stale_ports: Vec<usize> = Vec::new();
                if let Some(node) = self.graph.nodes.get(hub_id) {
                    if let NodeType::ObHub { detected_devices, .. } = &node.node_type {
                        let old_labels = build_labels(detected_devices);
                        let new_labels = build_labels(&detected);
                        if old_labels != new_labels {
                            let max_len = old_labels.len().max(new_labels.len());
                            for i in 0..max_len {
                                if old_labels.get(i) != new_labels.get(i) {
                                    stale_ports.push(i);
                                }
                            }
                        }
                    }
                }

                if let Some(node) = self.graph.nodes.get_mut(hub_id) {
                    if let NodeType::ObHub { detected_devices, .. } = &mut node.node_type {
                        *detected_devices = detected;
                    }
                }

                // Prune stale output-side connections for this hub.
                if !stale_ports.is_empty() {
                    let before = self.graph.connections.len();
                    self.graph.connections.retain(|c| {
                        !(c.from_node == *hub_id && stale_ports.contains(&c.from_port))
                    });
                    if self.graph.connections.len() != before {
                        self.graph.mark_topo_dirty();
                    }
                }
            }
        }

        // Inject Monitor + Audio node values
        for (&id, node) in &self.graph.nodes {
            match &node.node_type {
                // Synth and AudioPlayer output their NodeId so FX nodes can reference them
                NodeType::Synth { .. } | NodeType::AudioPlayer { .. } => {
                    values.insert((id, 0), PortValue::Float(id as f32));
                }
                _ => {}
            }
        }

        // Process MCP commands (if MCP server is active)
        self.process_mcp_commands(&values);

        // Store BG image/shader for canvas drawing (from Theme node's BG Image port 21)
        {
            let mut bg_img: Option<std::sync::Arc<ImageData>> = None;
            let mut bg_wgsl_node: Option<NodeId> = None;

            if let Some((&theme_id, _)) = self.graph.nodes.iter().find(|(_, n)| matches!(n.node_type, NodeType::Theme { .. })) {
                if let Some(conn) = self.graph.connections.iter().find(|c| c.to_node == theme_id && c.to_port == 21) {
                    let source_node = conn.from_node;
                    // Check if the source is a WGSL Viewer — render shader directly as BG
                    if self.graph.nodes.get(&source_node).map(|n| matches!(n.node_type, NodeType::WgslViewer { .. })).unwrap_or(false) {
                        bg_wgsl_node = Some(source_node);
                    } else {
                        // Regular image source (Image node, ImageEffects, Blend, etc.)
                        // Read from the source node's output via the values HashMap.
                        // GpuImage handles get on-demand readback from the GPU
                        // texture cache so chains that ran fully on-GPU still
                        // produce a CPU-side BG image for the canvas painter.
                        if let Some(val) = values.get(&(conn.from_node, conn.from_port)) {
                            match val {
                                PortValue::Image(img) => { bg_img = Some(img.clone()); }
                                PortValue::GpuImage(h) => {
                                    if let Some(rs) = &self.wgpu_render_state {
                                        if let Some(img) = self.gpu_tex_cache.readback_node_output(
                                            h.node_id, h.port, &rs.device, &rs.queue,
                                        ) {
                                            bg_img = Some(img);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new("canvas_bg_image"), bg_img);
                d.insert_temp(egui::Id::new("canvas_bg_wgsl"), bg_wgsl_node);
            });
        }

        self.canvas(ctx);
        self.render_connections(ctx, &values);
        self.render_nodes_filtered(ctx, &values, false);
        self.render_nodes_filtered(ctx, &values, true);

        // Build audio chains AFTER node rendering — nodes populate s.sources
        // Sync audio engine state with the graph (start/stop, rebuild, param sync)
        self.sync_audio_engine();

        self.sync_console_messages();
        self.handle_system_node_actions(ctx);

        // Handle OSC In spawn requests (create new OscIn pre-configured for a discovered address)
        {
            let osc_ids: Vec<NodeId> = self.graph.nodes.keys().copied()
                .filter(|id| matches!(self.graph.nodes.get(id).map(|n| &n.node_type), Some(NodeType::OscIn { .. })))
                .collect();
            for osc_id in osc_ids {
                let spawn_id = egui::Id::new(("osc_spawn", osc_id));
                if let Some((addr, argc, port)) = ctx.data_mut(|d| d.get_temp::<(String, usize, u16)>(spawn_id)) {
                    ctx.data_mut(|d| d.remove::<(String, usize, u16)>(spawn_id));
                    let src_pos = self.graph.nodes.get(&osc_id).map(|n| n.pos).unwrap_or([200.0, 200.0]);
                    let nt = NodeType::OscIn {
                        port,
                        address_filter: addr,
                        arg_count: argc,
                        last_args: vec![0.0; argc],
                        last_args_text: Vec::new(),
                        log: Vec::new(),
                        listening: true,
                        discovered: Vec::new(),
                    };
                    self.push_undo();
                    let new_id = self.graph.add_node(nt, [src_pos[0] + 280.0, src_pos[1]]);
                    self.selected_nodes.clear();
                    self.selected_nodes.insert(new_id);
                    // Auto-start listening on the spawned node
                    self.osc.process(vec![crate::osc::OscAction::StartListening { node_id: new_id, port }]);
                }
            }
        }

        // Handle OB Hub spawn requests (create device node auto-connected to hub)
        {
            let hub_ids: Vec<NodeId> = self.graph.nodes.keys().copied()
                .filter(|id| matches!(self.graph.nodes.get(id).map(|n| &n.node_type), Some(NodeType::ObHub { .. })))
                .collect();
            for hub_id in hub_ids {
                let spawn_id = egui::Id::new(("ob_spawn", hub_id));
                if let Some((dtype, did)) = ctx.data_mut(|d| d.get_temp::<(String, u8)>(spawn_id)) {
                    ctx.data_mut(|d| d.remove::<(String, u8)>(spawn_id));
                    let hub_pos = self.graph.nodes.get(&hub_id).map(|n| n.pos).unwrap_or([200.0, 200.0]);
                    let nt = match dtype.as_str() {
                        "joystick" => NodeType::ObJoystick { device_id: did, hub_node_id: hub_id, label_color: [255, 255, 255] },
                        "encoder" => NodeType::ObEncoder { device_id: did, hub_node_id: hub_id, label_color: [255, 255, 255] },
                        "move" => NodeType::ObMove { device_id: did, hub_node_id: hub_id },
                        "bend" => NodeType::ObBend { device_id: did, hub_node_id: hub_id, label_color: [255, 255, 255] },
                        "pressure" => NodeType::ObPressure { device_id: did, hub_node_id: hub_id, label_color: [255, 255, 255] },
                        "distance" => NodeType::ObDistance { device_id: did, hub_node_id: hub_id, label_color: [255, 255, 255] },
                        "orb" => NodeType::ObOrb { device_id: did, hub_node_id: hub_id, mode: 0, color: [255, 255, 255], param1: 0.0, param2: 0.0, speed: 1.0, brightness: 1.0 },
                        _ => continue,
                    };
                    self.push_undo();
                    let new_id = self.graph.add_node(nt, [hub_pos[0] + 250.0, hub_pos[1]]);
                    self.selected_nodes.clear();
                    self.selected_nodes.insert(new_id);
                }
            }
        }
        self.node_menu(ctx);
        self.context_menu(ctx);
        self.handle_pan_zoom(ctx);
        self.handle_canvas_interaction(ctx);

        ctx.request_repaint();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_session();
    }
}
