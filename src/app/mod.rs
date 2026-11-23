use crate::app::undo::UndoSnapshot;
use crate::audio::AudioManager;
use crate::graph::*;
use crate::http::{HttpAction, HttpManager};
use crate::midi::{MidiAction, MidiManager};
use crate::network::{NetworkAction, NetworkManager};
use crate::node_trait::NodeBehavior;
use crate::nodes;
use crate::ob::ObManager;
use crate::osc::{OscAction, OscManager};
use crate::serial::{SerialAction, SerialManager};
use eframe::egui;
use eframe::egui_wgpu;
use std::collections::HashMap;
const PORT_RADIUS: f32 = 8.0; // visual radius of port shapes
const PORT_INTERACT: f32 = 22.0; // clickable/draggable hit area
const PORT_BORDER: f32 = 2.5; // border stroke
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
            painter.rect_stroke(
                rect,
                3.0,
                egui::Stroke::new(border_width, border),
                egui::StrokeKind::Outside,
            );
        }
        2 => {
            // Triangle (Trigger) — pointing right
            let d = radius * 0.95;
            let points = vec![
                egui::pos2(center.x - d * 0.7, center.y - d), // top-left
                egui::pos2(center.x + d, center.y),           // right point
                egui::pos2(center.x - d * 0.7, center.y + d), // bottom-left
            ];
            painter.add(egui::Shape::convex_polygon(
                points,
                fill,
                egui::Stroke::new(border_width, border),
            ));
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
            painter.add(egui::Shape::convex_polygon(
                points,
                fill,
                egui::Stroke::new(border_width, border),
            ));
        }
        5 => {
            // Square (Gate)
            let half = radius * 0.8;
            let rect = egui::Rect::from_center_size(center, egui::vec2(half * 2.0, half * 2.0));
            painter.rect_filled(rect, 0.0, fill);
            painter.rect_stroke(
                rect,
                0.0,
                egui::Stroke::new(border_width, border),
                egui::StrokeKind::Outside,
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
            // Normalized: inner dot (like Audio but blue)
            if kind == PortKind::Normalized {
                painter.circle_filled(center, radius * 0.35, border);
            }
        }
    } // Draw tiny icon inside when connected
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

mod canvas;
// P6 Stage 2.2: Windows portable export.
pub(crate) mod export;
// P6 Stage 2.3: File → Export as App → Windows Portable…
mod export_dialog;
mod group;
mod interaction;
mod io;
// P6 Stage 2.3: program start, shared by the editor and the exported apps'
// runtime.
pub(crate) mod launch;
// P4.3.2B: MCP loads projects with the same normalization.
pub(crate) use io::load_project_graph;
mod library;
mod marketplace;
mod menus;
mod permissions;
mod permissions_manage;
pub(crate) mod player;
mod provenance;
#[cfg(test)]
mod provenance_tests;
#[cfg(test)]
mod revocation_tests;
mod share;
mod toast;
mod trust;
pub(crate) mod undo;
mod version;
use undo::UndoHistory;
/// Identity of a connection — survives array reindexing, node deletion, undo/redo.
/// Using (from_node, from_port, to_node, to_port) instead of a Vec index
/// prevents stale-index bugs where the index points to the wrong wire.
type ConnectionId = (NodeId, usize, NodeId, usize);

pub struct PatchworkApp {
    documents: crate::document::DocumentManager,
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
    /// DMX-over-USB transport. Owns serial-port handles + per-node sender
    /// threads. Mirrors the OSC/MIDI manager pattern.
    dmx: crate::dmx::DmxManager,
    /// Art-Net (DMX-over-UDP) transport. Owns the shared send socket and
    /// per-node listener threads.
    artnet: crate::artnet::ArtNetManager,
    // Selection & clipboard
    selected_nodes: std::collections::HashSet<NodeId>,
    /// Nodes whose click-popup (Slider, Comment, …) should auto-open on the
    /// next render. Populated by `add_node_selected`, drained at the top of
    /// `update()`. Lets newly created nodes show their click-options without
    /// requiring an extra click after the user drops them from the palette.
    pending_popup_open: std::collections::HashSet<NodeId>,
    selected_connection: Option<ConnectionId>,
    wire_menu_conn: Option<ConnectionId>, // connection identity for wire context menu
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
    /// P4.3.2E1: whether the clipboard came from a document allowed to run.
    clipboard_runnable: bool,
    /// P4.3.2E2: the rights of the document the clipboard node came from.
    clipboard_rights: crate::capability::CapSet,
    /// P4.3.2E2: capabilities the active graph's nodes use, cached by a
    /// fingerprint of its node types.
    capability_uses_cache: Option<(u64, crate::capability::CapSet)>,
    /// P4.3.2E2: the capability context of each open document (by id).
    document_scopes: crate::capability::DocumentScopes,
    /// P4.3.2E2: the trust generation the open documents' runtimes were
    /// last brought in line with.
    trust_generation_applied: u64,
    context_menu_node: Option<NodeId>,
    show_context_menu: bool,
    context_menu_pos: egui::Pos2,
    context_menu_opened_at: f64, // time when menu was opened (for auto-dismiss)
    // Option+drag duplication
    opt_drag_source: Option<NodeId>,
    opt_drag_created: Option<NodeId>,
    opt_drag_anchor_pos: [f32; 2],
    opt_drag_anchor_ptr: [f32; 2],
    // Canvas pan & zoom
    canvas_offset: egui::Vec2,
    canvas_zoom: f32,
    _prev_zoom: f32,
    panning: bool,
    pinned_nodes: std::collections::HashSet<NodeId>,
    // HTTP & API
    http: HttpManager,
    // Network (iroh P2P)
    network: NetworkManager,
    api_keys: HashMap<String, String>,
    // OB Hardware
    ob: ObManager,
    // P5: USB hardware discovery
    hardware_devices: Vec<crate::hardware::HardwareDevice>,
    hardware_discovery_error: Option<String>,
    hardware_scan_requested: bool,
    // P5: Selected firmware file for flashing
    hardware_firmware_path: Option<std::path::PathBuf>,
    hardware_validation_result: Option<Result<(), String>>,
    hardware_flash_confirmation_open: bool,
    hardware_flash_result: Option<Result<(), String>>,
    hardware_flash_in_progress: bool,
    hardware_flash_receiver: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    // P5: Selected hardware target
    hardware_selected_serial_port: Option<String>,
    // P5: ESP32 firmware flashing configuration
    hardware_esp32_chip: String,
    hardware_esp32_flash_address: String,

    #[cfg(target_os = "windows")]
    hardware_selected_pico_drive: Option<std::path::PathBuf>,
    // P5: Detected Raspberry Pi Pico BOOTSEL devices
    #[cfg(target_os = "windows")]
    hardware_pico_devices: Vec<crate::hardware::PicoBootloaderDevice>,
    // Audio
    audio: AudioManager,
    // MCP server
    mcp_rx: Option<std::sync::mpsc::Receiver<crate::mcp::McpRequest>>,
    mcp_log: crate::mcp::McpLog,
    // ML inference
    ml_rx: std::sync::mpsc::Receiver<crate::nodes::ml_model::MlInferenceResult>,
    ml_tx: std::sync::mpsc::Sender<crate::nodes::ml_model::MlInferenceResult>,
    ml_running: std::collections::HashSet<NodeId>,
    // TTS synthesis
    tts_rx: std::sync::mpsc::Receiver<crate::nodes::tts_node::TtsSynthResult>,
    tts_tx: std::sync::mpsc::Sender<crate::nodes::tts_node::TtsSynthResult>,
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
    pending_gesture_snapshot: Option<UndoSnapshot>,
    pending_close_tab: Option<u64>,
    /// P3: .pwnode file queued to open for editing (double-click / context menu).
    /// Processed after node rendering to avoid borrowing the graph mid-frame.
    pending_edit_custom_node: Option<String>,
    /// P3: Set when this frame's primary double-click landed on a node, so the
    /// empty-canvas handler (which runs later, possibly after a tab switch has
    /// cleared `node_rects`) does not also open the Add Node palette.
    node_double_click_consumed: bool,
    /// P2: "Group into Custom Node" dialog state.
    group_dialog_open: bool,
    group_name: String,
    /// Selection captured when the group dialog opened.
    group_selection: std::collections::HashSet<NodeId>,
    /// P2: .pwnode files created by grouping this session (Project Nodes).
    project_custom_nodes: Vec<std::path::PathBuf>,
    /// P4.0: cached Library listing (rescanned on demand, not per frame).
    library_cache: crate::library::LibraryCache,
    /// P4.1: "Publish to Library" dialog state.
    publish_dialog: Option<library::PublishDialog>,
    /// P4.3.3: Share to Marketplace window.
    share: Option<share::ShareWindow>,
    /// P4.2 Stage 5: a version switch awaiting confirmation.
    pending_version_switch: Option<version::PendingVersionSwitch>,
    /// P4.3.1A: read-only Marketplace catalogue browser.
    marketplace: marketplace::MarketplaceWindow,
    /// P4.3.2C: Package Trust Review (Library migration).
    trust_review: trust::TrustReviewWindow,
    /// P4.3.2D: package execution permissions (banner and review window).
    permissions: permissions::PermissionsWindow,
    /// P6 Stage 2.3: File → Export as App → Windows Portable…
    export_dialog: Option<export_dialog::ExportDialog>,
    // P3: Save As dialog state
    save_as_open: bool,
    save_as_name: String,
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
    /// True when the in-memory project differs from what's on disk. Drives
    /// the `*` in the title bar and the save-on-quit confirmation.
    is_dirty: bool,
    /// Last title sent via ViewportCommand — cached so we only push a new
    /// command when the displayed string actually changes.
    last_title: String,
    /// Showing the "save changes before quitting?" modal.
    pending_quit_dialog: bool,
    /// User chose Don't Save / saved successfully — next close request
    /// is allowed through without re-prompting.
    force_quit: bool,
    /// B1: the user chose "Discard All & Quit"; the session must not keep
    /// any unsaved graph state.
    discard_on_exit: bool,
    /// MRU list of project directories (most recent first, capped at 10).
    /// Persisted via SessionState; rendered as the File → Open Recent submenu.
    recent_projects: Vec<String>,
    /// Active toast notifications (top-center, auto-dismiss or sticky).
    /// Pushed via `toast_warn` / `toast_info` / etc; rendered every frame.
    toasts: Vec<toast::Toast>,
    /// P6: set when running as the Player (`--player`); the editor is off.
    player: Option<player::PlayerState>,
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
        Self::build(cc, None)
    }

    /// P6: the Player for `launch` (no editor, no session, no MCP).
    pub fn new_player(cc: &eframe::CreationContext<'_>, start: player::PlayerStart) -> Self {
        Self::build(cc, Some(start))
    }

    fn build(cc: &eframe::CreationContext<'_>, player_launch: Option<player::PlayerStart>) -> Self {
        crate::icons::setup(&cc.egui_ctx);

        // Apply persisted user settings — currently runs LRU eviction
        // on the image cache so it can't grow without bound across
        // sessions. Failures here are non-fatal (defaults are sane).
        crate::settings::Settings::load().apply_image_cache();
        let wgpu_render_state = cc.wgpu_render_state.clone();
        let (ml_tx, ml_rx) = std::sync::mpsc::channel();
        let (tts_tx, tts_rx) = std::sync::mpsc::channel();
        let mut app = Self {
            documents: crate::document::DocumentManager::new(),
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
            dmx: crate::dmx::DmxManager::new(),
            artnet: crate::artnet::ArtNetManager::new(),
            selected_nodes: std::collections::HashSet::new(),
            pending_popup_open: std::collections::HashSet::new(),
            pending_close_tab: None,
            pending_edit_custom_node: None,
            node_double_click_consumed: false,
            group_dialog_open: false,
            group_name: String::new(),
            group_selection: std::collections::HashSet::new(),
            project_custom_nodes: Vec::new(),
            library_cache: crate::library::LibraryCache::new(),
            publish_dialog: None,
            share: None,
            pending_version_switch: None,
            marketplace: Default::default(),
            trust_review: Default::default(),
            permissions: Default::default(),
            export_dialog: None,
            save_as_open: false,
            save_as_name: String::new(),
            selected_connection: None,
            wire_menu_conn: None,
            wire_menu_pos: egui::Pos2::ZERO,
            wire_insert_picker_open: false,
            wire_insert_search: String::new(),
            box_select_start: None,
            box_select_end: None,
            clipboard: None,
            clipboard_runnable: false,
            clipboard_rights: crate::capability::CapSet::EMPTY,
            capability_uses_cache: None,
            document_scopes: Default::default(),
            trust_generation_applied: crate::library::trust::generation(),
            context_menu_node: None,
            show_context_menu: false,
            context_menu_pos: egui::Pos2::ZERO,
            context_menu_opened_at: 0.0,
            opt_drag_source: None,
            opt_drag_created: None,
            opt_drag_anchor_pos: [0.0; 2],
            opt_drag_anchor_ptr: [0.0; 2],
            canvas_offset: egui::Vec2::ZERO,
            canvas_zoom: 1.0,
            _prev_zoom: 1.0,
            panning: false,
            pinned_nodes: std::collections::HashSet::new(),
            http: HttpManager::new(),
            network: NetworkManager::new(),
            api_keys: HashMap::new(),
            ob: ObManager::new(),
            // P5: USB hardware discovery
            hardware_devices: Vec::new(),
            hardware_discovery_error: None,
            hardware_scan_requested: false,
            // P5: No firmware selected initially
            hardware_firmware_path: None,
            hardware_validation_result: None,
            hardware_flash_confirmation_open: false,
            hardware_flash_result: None,
            hardware_flash_in_progress: false,
            hardware_flash_receiver: None,
            // P5: No hardware target selected initially
            hardware_selected_serial_port: None,
            // P5: Default ESP32 application configuration
            hardware_esp32_chip: "esp32".to_string(),
            hardware_esp32_flash_address: "0x10000".to_string(),

            #[cfg(target_os = "windows")]
            hardware_selected_pico_drive: None,

            // P5: Pico BOOTSEL device storage
            #[cfg(target_os = "windows")]
            hardware_pico_devices: Vec::new(),
            audio: AudioManager::new(),
            app_start_instant: std::time::Instant::now(),
            mcp_log: crate::mcp::new_log(),
            mcp_rx: None,
            ml_rx: ml_rx,
            ml_tx: ml_tx,
            ml_running: std::collections::HashSet::new(),
            tts_rx,
            tts_tx,
            device_refresh_rx: {
                let (tx, rx) = std::sync::mpsc::channel();
                // Background thread enumerates devices every 5 seconds — never blocks UI
                std::thread::spawn(move || {
                    loop {
                        let midi_in = midir::MidiInput::new("patchwork-scan")
                            .map(|m| {
                                m.ports()
                                    .iter()
                                    .filter_map(|p| m.port_name(p).ok())
                                    .collect()
                            })
                            .unwrap_or_default();
                        let midi_out = midir::MidiOutput::new("patchwork-scan")
                            .map(|m| {
                                m.ports()
                                    .iter()
                                    .filter_map(|p| m.port_name(p).ok())
                                    .collect()
                            })
                            .unwrap_or_default();
                        let serial = serialport::available_ports()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|p| p.port_name)
                            .collect();
                        let (audio_output, audio_input, audio_output_channels) = {
                            use cpal::traits::{DeviceTrait, HostTrait};
                            let host = cpal::default_host();
                            let mut out_names = Vec::new();
                            let mut out_channels = std::collections::HashMap::new();
                            if let Ok(devs) = host.output_devices() {
                                for d in devs {
                                    if let Ok(name) = d.name() {
                                        if let Ok(cfg) = d.default_output_config() {
                                            out_channels
                                                .insert(name.clone(), cfg.channels() as usize);
                                        }
                                        out_names.push(name);
                                    }
                                }
                            }
                            let inp = host
                                .input_devices()
                                .map(|devs| devs.filter_map(|d| d.name().ok()).collect())
                                .unwrap_or_default();
                            (out_names, inp, out_channels)
                        };
                        let _ = tx.send(DeviceRefreshResult {
                            midi_in,
                            midi_out,
                            serial,
                            audio_output,
                            audio_input,
                            audio_output_channels,
                        });
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
            pending_gesture_snapshot: None,
            click_wiring: false,
            session_accent: crate::nodes::theme::random_accent(),
            target_zoom: 1.0,
            zoom_anchor_screen: None,
            eq_curve_hashes: HashMap::new(),
            prev_play_intent: false,
            is_dirty: false,
            last_title: String::new(),
            pending_quit_dialog: false,
            force_quit: false,
            discard_on_exit: false,
            recent_projects: Vec::new(),
            toasts: Vec::new(),
            player: None,
        };
        // Always start MCP server thread — auto-detects if stdin is a pipe (Claude Desktop)
        // vs terminal (normal launch). If stdin is a terminal, the thread exits immediately.
        // P6: never in the Player (MCP commands edit the graph).
        if player_launch.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            let log = app.mcp_log.clone();
            std::thread::spawn(move || crate::mcp::run_mcp_thread(tx, log));
            app.mcp_rx = Some(rx);
        }

        // Kick off the process-wide HID manager. OnceLock guarantees the
        // enumeration thread is spawned exactly once. We eager-initialize
        // here so HID devices are detected even before the user places an
        // OB Knob node.
        let _ = crate::hid::global();

        // P6: the Player loads its project read-only and never restores or
        // saves the editor session.
        if let Some(start) = player_launch {
            app.start_player(start);
            return app;
        }

        // Try restoring previous session; fall back to default nodes
        // Try restoring previous session; fall back to default nodes
        if !app.restore_session() {
            app.spawn_default_nodes();
        }

        // Synchronize the initial document with the loaded project.
        app.sync_active_document();

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
        if self
            .audio
            .audio_error
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.audio
                .audio_error
                .store(false, std::sync::atomic::Ordering::Relaxed);
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
        let mut dsp_enabled = self
            .graph
            .nodes
            .values()
            .any(|n| matches!(n.node_type, NodeType::AudioDevice { enabled: true, .. }));

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
        let has_audio_device = self
            .graph
            .nodes
            .values()
            .any(|n| matches!(n.node_type, NodeType::AudioDevice { .. }));
        let play_intent =
            has_audio_device
                && (self.audio.file_playing.values().any(|p| *p)
                    || self.graph.nodes.values().any(|n| {
                        matches!(&n.node_type, NodeType::AudioInput { active: true, .. })
                    }));
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
            let device_name: Option<String> = self.graph.nodes.values().find_map(|n| {
                if let NodeType::AudioDevice {
                    selected_output,
                    enabled: true,
                    ..
                } = &n.node_type
                {
                    if selected_output.is_empty() {
                        None
                    } else {
                        Some(selected_output.clone())
                    }
                } else {
                    None
                }
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
            if !self.audio.has_processor(nid) {
                continue;
            }
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
                NodeType::AudioPitchShift { semitones } => {
                    self.audio.engine_write_param(nid, 0, *semitones);
                }
                NodeType::AudioReverb {
                    room_size,
                    damping,
                    mix,
                } => {
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
                NodeType::AudioPlaylist { volume, .. } => {
                    self.audio.engine_write_param(nid, 0, *volume);
                }
                NodeType::AudioSampler { volume, .. } => {
                    self.audio.engine_write_param(nid, 0, *volume);
                }
                NodeType::AudioInput { gain, .. } => {
                    self.audio.engine_write_param(nid, 0, *gain);
                }
                // Dynamic nodes with audio params (e.g., Spectral Synth)
                NodeType::Dynamic { inner } => {
                    let params = inner.node.audio_params();
                    if !params.is_empty() {
                        for (i, &v) in params.iter().enumerate() {
                            self.audio.engine_write_param(nid, i, v);
                        }
                    }
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
            if !self.audio.has_processor(nid) {
                continue;
            }
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
        self.eq_curve_hashes
            .retain(|nid, _| self.graph.nodes.contains_key(nid));

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
                if self.audio.has_processor(nid) {
                    continue;
                }
                // Rebuild just this node
                let graph_snapshot = &self.graph;
                if let Some(node) = graph_snapshot.nodes.get(&nid) {
                    let proc_and_count = self.audio.create_processor_for_node(&node.node_type, nid);
                    if let Some((processor, param_count)) = proc_and_count {
                        self.audio.add_processor(nid, processor, param_count);
                        new_nodes.push(nid);
                        // Set speaker state
                        if let NodeType::Speaker { active, .. } = &node.node_type {
                            self.audio.send_command(
                                crate::audio::engine::AudioCommand::SetSpeaker {
                                    node_id: nid,
                                    active: *active,
                                },
                            );
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
                    let involves_new =
                        new_nodes.contains(&conn.from_node) || new_nodes.contains(&conn.to_node);
                    if !involves_new {
                        continue;
                    }
                    let from_audio = self.audio.node_params.contains_key(&conn.from_node);
                    let to_audio = self.audio.node_params.contains_key(&conn.to_node);
                    if !from_audio || !to_audio {
                        continue;
                    }
                    // Skip mixer gain ports
                    let is_mixer_gain_port = matches!(
                        self.graph.nodes.get(&conn.to_node).map(|n| &n.node_type),
                        Some(NodeType::AudioMixer { .. })
                    ) && conn.to_port % 2 != 0;
                    if is_mixer_gain_port {
                        continue;
                    }
                    let engine_port = self.mixer_engine_port(conn.to_node, conn.to_port);
                    self.audio
                        .connect_audio(conn.from_node, conn.to_node, engine_port);
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
        if matches!(
            self.graph.nodes.get(&node_id).map(|n| &n.node_type),
            Some(NodeType::AudioMixer { .. })
        ) {
            graph_port / 2
        } else {
            graph_port
        }
    }

    fn primary_selected(&self) -> Option<NodeId> {
        self.selected_nodes.iter().next().copied()
    }

    /// Insert a node and immediately make it the sole selection. Use this for
    /// every user-driven creation (palette, RMB, duplicate, wire-splice,
    /// file-drop) so the new node is highlighted and ready for keyboard /
    /// click actions. Programmatic creations (WGSL aux nodes, MCP, undo
    /// internals) should keep using `self.graph.add_node` to avoid hijacking
    /// the user's current selection.
    pub(super) fn add_node_selected(
        &mut self,
        ctx: &egui::Context,
        mut nt: NodeType,
        pos: [f32; 2],
    ) -> NodeId {
        Self::apply_theme_defaults_to_new(ctx, &mut nt);
        let id = self.graph.add_node(nt, pos);
        self.selected_nodes.clear();
        self.selected_nodes.insert(id);
        // For nodes that surface their settings via a click-popup (Slider,
        // Comment, …), auto-open it. The actual ctx writes happen in
        // `update()` at the top of the next frame — see the drain block.
        self.pending_popup_open.insert(id);
        id
    }

    /// Replace per-node default colors with the current theme's accent /
    /// text color. Only fires when the field still holds the catalog's
    /// hardcoded sentinel value, so users who explicitly picked a color
    /// (and re-spawned a clone) keep their pick.
    ///
    /// Called inside `add_node_selected` and any programmatic spawn that
    /// should follow the user's theme.
    pub(super) fn apply_theme_defaults_to_new(ctx: &egui::Context, nt: &mut NodeType) {
        let accent = crate::nodes::theme::current_accent_arr(ctx);
        let text = crate::nodes::theme::current_text_arr(ctx);
        match nt {
            // Slider's catalog default is `[80, 160, 255]`.
            NodeType::Slider { slider_color, .. } if *slider_color == [80, 160, 255] => {
                *slider_color = accent;
            }
            // OB controllers all default to white labels.
            NodeType::ObJoystick { label_color, .. }
            | NodeType::ObEncoder { label_color, .. }
            | NodeType::ObBend { label_color, .. }
            | NodeType::ObPressure { label_color, .. }
            | NodeType::ObDistance { label_color, .. }
            | NodeType::ObKnob { label_color, .. }
                if *label_color == [255, 255, 255] =>
            {
                *label_color = text;
            }
            NodeType::ObOrb { color, .. } if *color == [255, 255, 255] => {
                *color = text;
            }
            // Dynamic nodes (Comment, Button, …) opt in via the
            // `apply_theme_defaults` trait method. Default is a no-op,
            // so existing node types don't need touching.
            NodeType::Dynamic { inner } => {
                inner.node.apply_theme_defaults(ctx);
            }
            _ => {}
        }
    }

    // Pinned nodes: pos is screen pixels, computed to egui coords inline in render

    /// The runtime part of a frame, shared by the editor and the Player
    /// (P6): device polling, graph evaluation, packages, GPU and image
    /// processing. A document that may not run is parked for its duration
    /// (an empty graph stands in, so every subsystem closes what it
    /// opened) and put back before returning. Callers resolve the content
    /// state and enter the document's rights first.
    /// Where the active graph's package references resolve, and under
    /// which trust policy. No-op when unchanged.
    fn set_graph_package_environment(&mut self) {
        // P4.2 Stage 2: package references resolve against this project's
        // folder (projects only) and the Library.
        let package_project_dir = match self.documents.active().kind {
            crate::document::DocumentKind::Project => {
                self.project_path.as_ref().map(std::path::PathBuf::from)
            }
            crate::document::DocumentKind::CustomNode => None,
        };
        // P6 Stage 2.1: an exported Player uses only what it bundles.
        let package_library_root = if self.player_isolated() {
            None
        } else {
            self.library_cache.root().ok()
        };
        self.graph
            .set_package_environment(package_project_dir, package_library_root);
        // P4.3.2D: package runtimes run only when the trust store allows
        // them. Set on whatever graph is about to run (a loaded project, a
        // restored session, a custom-node tab, an MCP LoadProject result).
        self.graph
            .set_trust_policy(permissions::editor_trust_policy());
    }

    fn run_graph_frame(&mut self, ctx: &egui::Context) -> HashMap<(NodeId, usize), PortValue> {
        let parked_graph = (!self.graph.content.runnable())
            .then(|| std::mem::replace(&mut self.graph, Graph::new()));
        self.update_mouse_trackers(ctx);
        self.update_key_inputs(ctx);
        self.poll_midi_inputs();
        self.poll_serial_inputs();
        self.poll_osc_inputs();
        self.poll_dmx_inputs();
        self.poll_artnet_inputs();
        self.poll_network_events();
        self.poll_http_responses();
        self.process_ai_gear_click(ctx);
        self.ob.poll_all();
        self.poll_ml_inference(ctx);
        self.poll_tts_synthesis(ctx);
        self.frame_count += 1;
        // Receive device lists from background enumeration thread (non-blocking)
        if let Ok(devices) = self.device_refresh_rx.try_recv() {
            self.midi.set_port_lists(devices.midi_in, devices.midi_out);
            self.serial.set_port_list(devices.serial);
            self.audio
                .set_device_lists(devices.audio_output, devices.audio_input);
            self.audio.device_channel_counts = devices.audio_output_channels;
        }
        let node_count = self.graph.nodes.len();
        let conn_count = self.graph.connections.len();
        self.monitor.tick(node_count, conn_count);
        self.gpu_tex_cache
            .begin_frame(self.wgpu_render_state.as_ref());
        let now_secs = self.app_start_instant.elapsed().as_secs_f64();
        self.set_graph_package_environment();
        let mut values = self.graph.evaluate(now_secs);
        // P3: Report interface changes picked up while reloading custom nodes
        // (e.g. a .pwnode modified outside this editor).
        let port_events = self.graph.take_custom_port_events();
        if port_events.changed {
            self.is_dirty = true;
            let title = self.documents.active().title.clone();
            self.report_removed_custom_connections(&title, &port_events.removed);
        }
        // Inject OB hardware values into the graph (before they're used by downstream nodes)
        // These need a second evaluation pass to propagate through Add/Multiply/etc.
        {
            let mut ob_injected = false;
            for (&id, node) in &self.graph.nodes {
                match &node.node_type {
                    NodeType::ObHub {
                        detected_devices, ..
                    } => {
                        let mut port_idx = 0usize;
                        let mut sorted = detected_devices.clone();
                        sorted.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
                        for (dtype, did) in &sorted {
                            if let Some(hub) = self.ob.get_hub(id) {
                                match dtype.as_str() {
                                    "joystick" => {
                                        values.insert(
                                            (id, port_idx),
                                            PortValue::Float(hub.get_value("joystick", *did, "x")),
                                        );
                                        values.insert(
                                            (id, port_idx + 1),
                                            PortValue::Float(hub.get_value("joystick", *did, "y")),
                                        );
                                        values.insert(
                                            (id, port_idx + 2),
                                            PortValue::Float(
                                                hub.get_value("joystick", *did, "btn"),
                                            ),
                                        );
                                        port_idx += 3;
                                    }
                                    "encoder" => {
                                        values.insert(
                                            (id, port_idx),
                                            PortValue::Float(
                                                hub.get_value("encoder", *did, "turn"),
                                            ),
                                        );
                                        values.insert(
                                            (id, port_idx + 1),
                                            PortValue::Float(
                                                hub.get_value("encoder", *did, "click"),
                                            ),
                                        );
                                        values.insert(
                                            (id, port_idx + 2),
                                            PortValue::Float(
                                                hub.get_value("encoder", *did, "position"),
                                            ),
                                        );
                                        port_idx += 3;
                                    }
                                    "move" => {
                                        values.insert(
                                            (id, port_idx),
                                            PortValue::Float(hub.get_value("move", *did, "ax")),
                                        );
                                        values.insert(
                                            (id, port_idx + 1),
                                            PortValue::Float(hub.get_value("move", *did, "ay")),
                                        );
                                        values.insert(
                                            (id, port_idx + 2),
                                            PortValue::Float(hub.get_value("move", *did, "az")),
                                        );
                                        values.insert(
                                            (id, port_idx + 3),
                                            PortValue::Float(hub.get_value("move", *did, "gx")),
                                        );
                                        values.insert(
                                            (id, port_idx + 4),
                                            PortValue::Float(hub.get_value("move", *did, "gy")),
                                        );
                                        values.insert(
                                            (id, port_idx + 5),
                                            PortValue::Float(hub.get_value("move", *did, "gz")),
                                        );
                                        port_idx += 6;
                                    }
                                    "bend" => {
                                        values.insert(
                                            (id, port_idx),
                                            PortValue::Float(hub.get_value("bend", *did, "val")),
                                        );
                                        port_idx += 1;
                                    }
                                    "pressure" => {
                                        values.insert(
                                            (id, port_idx),
                                            PortValue::Float(
                                                hub.get_value("pressure", *did, "val"),
                                            ),
                                        );
                                        port_idx += 1;
                                    }
                                    "distance" => {
                                        values.insert(
                                            (id, port_idx),
                                            PortValue::Float(
                                                hub.get_value("distance", *did, "val"),
                                            ),
                                        );
                                        port_idx += 1;
                                    }
                                    "knob" => {
                                        // Knob stores its reading under "value", not "val"
                                        // (see ob.rs apply_data arm for ("knob", "value")).
                                        values.insert(
                                            (id, port_idx),
                                            PortValue::Float(hub.get_value("knob", *did, "value")),
                                        );
                                        port_idx += 1;
                                    }
                                    "orb" => {
                                        values.insert(
                                            (id, port_idx),
                                            PortValue::Float(hub.get_value("orb", *did, "ax")),
                                        );
                                        values.insert(
                                            (id, port_idx + 1),
                                            PortValue::Float(hub.get_value("orb", *did, "ay")),
                                        );
                                        values.insert(
                                            (id, port_idx + 2),
                                            PortValue::Float(hub.get_value("orb", *did, "az")),
                                        );
                                        values.insert(
                                            (id, port_idx + 3),
                                            PortValue::Float(hub.get_value("orb", *did, "gx")),
                                        );
                                        values.insert(
                                            (id, port_idx + 4),
                                            PortValue::Float(hub.get_value("orb", *did, "gy")),
                                        );
                                        values.insert(
                                            (id, port_idx + 5),
                                            PortValue::Float(hub.get_value("orb", *did, "gz")),
                                        );
                                        port_idx += 6;
                                    }
                                    _ => {
                                        port_idx += 1;
                                    }
                                }
                            }
                        }
                        ob_injected = true;
                    }
                    NodeType::ObJoystick {
                        device_id,
                        hub_node_id,
                        ..
                    } => {
                        let find = self
                            .ob
                            .get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("joystick", *device_id))
                            .or_else(|| {
                                self.ob.find_device("joystick", *device_id).map(|(_, d)| d)
                            });
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
                            let changed = prev
                                .map(|p| {
                                    (p[0] - curr[0]).abs() > 0.001
                                        || (p[1] - curr[1]).abs() > 0.001
                                        || (p[2] - curr[2]).abs() > 0.5
                                })
                                .unwrap_or(false);
                            values
                                .insert((id, 3), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, curr));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObEncoder {
                        device_id,
                        hub_node_id,
                        ..
                    } => {
                        // Three lookup paths, same priority as other OB nodes:
                        //   1. Bound hub → 2. Any serial hub → 3. HID auto-discovery
                        //
                        // HID path (OB Wheel): parse_encoder_report accumulates
                        // position and writes turn/click/position keys directly
                        // into the device map, so the readout here is identical
                        // to the serial-hub path.
                        //
                        // Turn-pulse caveat: the HID parser writes the delta on
                        // each report and leaves it stale between reports. We
                        // clear it to 0 here AFTER reading so a single tick
                        // produces a single frame of non-zero Turn output,
                        // which matches the serial-hub behavior.
                        let hub_lookup = self
                            .ob
                            .get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("encoder", *device_id))
                            .or_else(|| self.ob.find_device("encoder", *device_id).map(|(_, d)| d))
                            .map(|d| {
                                (
                                    d.values.get("turn").copied().unwrap_or(0.0),
                                    d.values.get("click").copied().unwrap_or(0.0),
                                    d.values.get("position").copied().unwrap_or(0.0),
                                    true, // was_hub
                                )
                            });
                        let (turn, click, pos, was_hub) = hub_lookup
                            .or_else(|| {
                                crate::hid::global()
                                    .find_device("encoder", *device_id)
                                    .map(|d| {
                                        (
                                            d.values.get("turn").copied().unwrap_or(0.0),
                                            d.values.get("click").copied().unwrap_or(0.0),
                                            d.values.get("position").copied().unwrap_or(0.0),
                                            false,
                                        )
                                    })
                            })
                            .unwrap_or((0.0, 0.0, 0.0, false));

                        // Clear the one-shot "turn" field so a single detent
                        // produces a single frame of non-zero Turn output.
                        // Only do this for HID — the serial hub handles its
                        // own turn lifecycle.
                        if !was_hub && turn.abs() > 0.001 {
                            crate::hid::global().clear_device_value("encoder", *device_id, "turn");
                        }

                        values.insert((id, 0), PortValue::Float(turn));
                        values.insert((id, 1), PortValue::Float(click));
                        values.insert((id, 2), PortValue::Float(pos));
                        let prev_key = egui::Id::new(("ob_prev", id));
                        let prev: Option<[f32; 3]> = ctx.data_mut(|d| d.get_temp(prev_key));
                        let curr = [turn, click, pos];
                        let changed = prev
                            .map(|p| {
                                (p[0] - curr[0]).abs() > 0.001
                                    || (p[1] - curr[1]).abs() > 0.5
                                    || (p[2] - curr[2]).abs() > 0.001
                            })
                            .unwrap_or(false);
                        values.insert((id, 3), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                        ctx.data_mut(|d| d.insert_temp(prev_key, curr));
                        ob_injected = true;
                    }
                    NodeType::ObMove {
                        device_id,
                        hub_node_id,
                    } => {
                        let find = self
                            .ob
                            .get_hub(*hub_node_id)
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
                            for i in 0..6 {
                                values.insert((id, i), PortValue::Float(vals[i]));
                            }
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<[f32; 6]> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let changed = prev
                                .map(|p| (0..6).any(|i| (p[i] - vals[i]).abs() > 0.001))
                                .unwrap_or(false);
                            values
                                .insert((id, 6), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, vals));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObBend {
                        device_id,
                        hub_node_id,
                        ..
                    } => {
                        let find = self
                            .ob
                            .get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("bend", *device_id))
                            .or_else(|| self.ob.find_device("bend", *device_id).map(|(_, d)| d));
                        if let Some(dev) = find {
                            let val = dev.values.get("val").copied().unwrap_or(0.0);
                            values.insert((id, 0), PortValue::Float(val));
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<f32> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let changed = prev.map(|p| (p - val).abs() > 0.001).unwrap_or(false);
                            values
                                .insert((id, 1), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, val));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObPressure {
                        device_id,
                        hub_node_id,
                        ..
                    } => {
                        // Three lookup paths, same priority as ObKnob/ObDistance:
                        //   1. Bound hub → 2. Any serial hub → 3. HID auto-discovery
                        // HID path uses parse_knob_report, which writes both
                        // "val" (legacy key, used here) and "value" (knob key).
                        let hub_lookup = self
                            .ob
                            .get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("pressure", *device_id))
                            .or_else(|| self.ob.find_device("pressure", *device_id).map(|(_, d)| d))
                            .map(|d| (d.values.get("val").copied().unwrap_or(0.0), d.is_active));
                        let (val, _is_active) = hub_lookup
                            .or_else(|| {
                                crate::hid::global()
                                    .find_device("pressure", *device_id)
                                    .map(|d| {
                                        (d.values.get("val").copied().unwrap_or(0.0), d.is_active)
                                    })
                            })
                            .unwrap_or((0.0, false));

                        values.insert((id, 0), PortValue::Float(val));
                        let prev_key = egui::Id::new(("ob_prev", id));
                        let prev: Option<f32> = ctx.data_mut(|d| d.get_temp(prev_key));
                        let changed = prev.map(|p| (p - val).abs() > 0.001).unwrap_or(false);
                        values.insert((id, 1), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                        ctx.data_mut(|d| d.insert_temp(prev_key, val));
                        ob_injected = true;
                    }
                    NodeType::ObDistance {
                        device_id,
                        hub_node_id,
                        ..
                    } => {
                        // Three lookup paths, same priority as ObKnob:
                        //   1. Bound hub → 2. Any serial hub → 3. HID auto-discovery
                        // Ports: 0 = Value (0..1 normalized), 1 = Distance (mm),
                        //        2 = Changed (trigger).
                        // Firmware maps 0..300 mm → 0..1 for the normalized output;
                        // mm port is the raw reading (useful for absolute distances
                        // or when chaining a MapRange node for a custom range).
                        // Serial-hub devices predate the `mm` key, so we fall back
                        // to 0 on that path — downstream can use Value if absolute
                        // mm isn't critical.
                        let hub_lookup = self
                            .ob
                            .get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("distance", *device_id))
                            .or_else(|| self.ob.find_device("distance", *device_id).map(|(_, d)| d))
                            .map(|d| {
                                (
                                    d.values.get("val").copied().unwrap_or(0.0),
                                    d.values.get("mm").copied().unwrap_or(0.0),
                                    d.is_active,
                                )
                            });
                        let (val, mm, _is_active) = hub_lookup
                            .or_else(|| {
                                crate::hid::global()
                                    .find_device("distance", *device_id)
                                    .map(|d| {
                                        (
                                            d.values.get("val").copied().unwrap_or(0.0),
                                            d.values.get("mm").copied().unwrap_or(0.0),
                                            d.is_active,
                                        )
                                    })
                            })
                            .unwrap_or((0.0, 0.0, false));

                        values.insert((id, 0), PortValue::Float(val));
                        values.insert((id, 1), PortValue::Float(mm));
                        let prev_key = egui::Id::new(("ob_prev", id));
                        let prev: Option<f32> = ctx.data_mut(|d| d.get_temp(prev_key));
                        let changed = prev.map(|p| (p - val).abs() > 0.01).unwrap_or(false);
                        values.insert((id, 2), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                        ctx.data_mut(|d| d.insert_temp(prev_key, val));

                        ob_injected = true;
                    }
                    NodeType::ObKnob {
                        device_id,
                        hub_node_id,
                        ..
                    } => {
                        // Knob reading is stored under "value" (see ob.rs apply_data
                        // for ("knob", "value"), and hid.rs parse_knob_report).
                        // Three lookup sources tried in order:
                        //   1. The node's bound hub (if any)
                        //   2. Any connected serial hub
                        //   3. HID auto-discovery (no hub required) — via the
                        //      process-wide singleton in `crate::hid::global()`.
                        // HID is the expected path for USB-direct knobs.
                        let hub_lookup = self
                            .ob
                            .get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("knob", *device_id))
                            .or_else(|| self.ob.find_device("knob", *device_id).map(|(_, d)| d))
                            .map(|d| (d.values.get("value").copied().unwrap_or(0.0), d.is_active));
                        let (val, _is_active) = hub_lookup
                            .or_else(|| {
                                crate::hid::global()
                                    .find_device("knob", *device_id)
                                    .map(|d| {
                                        (d.values.get("value").copied().unwrap_or(0.0), d.is_active)
                                    })
                            })
                            .unwrap_or((0.0, false));

                        values.insert((id, 0), PortValue::Float(val));
                        let prev_key = egui::Id::new(("ob_prev", id));
                        let prev: Option<f32> = ctx.data_mut(|d| d.get_temp(prev_key));
                        let changed = prev.map(|p| (p - val).abs() > 0.001).unwrap_or(false);
                        values.insert((id, 1), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                        ctx.data_mut(|d| d.insert_temp(prev_key, val));
                        ob_injected = true;
                    }
                    NodeType::ObOrb {
                        device_id,
                        hub_node_id,
                        ..
                    } => {
                        let find = self
                            .ob
                            .get_hub(*hub_node_id)
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
                            for i in 0..6 {
                                values.insert((id, i), PortValue::Float(vals[i]));
                            }
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<[f32; 6]> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let changed = prev
                                .map(|p| (0..6).any(|i| (p[i] - vals[i]).abs() > 0.001))
                                .unwrap_or(false);
                            values
                                .insert((id, 6), PortValue::Float(if changed { 1.0 } else { 0.0 }));
                            ctx.data_mut(|d| d.insert_temp(prev_key, vals));
                        }
                        ob_injected = true;
                    }
                    NodeType::ObMotor {
                        device_id,
                        hub_node_id,
                        ..
                    } => {
                        // DAT carries (moving, position_deg, target_deg).
                        // Done port pulses on a moving→idle transition.
                        let find = self
                            .ob
                            .get_hub(*hub_node_id)
                            .and_then(|h| h.get_device("motor", *device_id))
                            .or_else(|| self.ob.find_device("motor", *device_id).map(|(_, d)| d));
                        if let Some(dev) = find {
                            let pos = dev.values.get("position").copied().unwrap_or(0.0);
                            let mov = dev.values.get("moving").copied().unwrap_or(0.0);
                            values.insert((id, 0), PortValue::Float(pos));
                            values.insert(
                                (id, 1),
                                PortValue::Float(if mov > 0.5 { 1.0 } else { 0.0 }),
                            );
                            let prev_key = egui::Id::new(("ob_prev", id));
                            let prev: Option<f32> = ctx.data_mut(|d| d.get_temp(prev_key));
                            let done_pulse = prev.map(|p| p > 0.5 && mov <= 0.5).unwrap_or(false);
                            values.insert(
                                (id, 2),
                                PortValue::Float(if done_pulse { 1.0 } else { 0.0 }),
                            );
                            ctx.data_mut(|d| d.insert_temp(prev_key, mov));
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
            let plugin_ids: Vec<NodeId> = self
                .graph
                .nodes
                .iter()
                .filter(|(_, n)| matches!(n.node_type, NodeType::RustPlugin { .. }))
                .map(|(&id, _)| id)
                .collect();
            for id in plugin_ids {
                let inputs: Vec<PortValue> = self.graph.collect_inputs(id, &values);
                let node = match self.graph.nodes.get_mut(&id) {
                    Some(n) => n,
                    None => continue,
                };
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
        let img_content_hash = |img: &ImageData| -> u64 {
            let mut h: u64 = (img.width as u64)
                .wrapping_mul(31)
                .wrapping_add(img.height as u64);
            let len = img.pixels.len();
            // Read 64 bits at a time and cap the sample count, so cost is
            // bounded (~8k iterations) regardless of resolution. A sparse
            // sample misses small moving features against a static
            // background — bars on black — and silently freezes every
            // downstream node's param cache.
            const MAX_WORDS: usize = 8192;
            let n_words = len / 8;
            if n_words > 0 {
                // Odd stride so successive reads don't lock onto one column.
                let stride = ((n_words / MAX_WORDS).max(1)) | 1;
                let mut i = 0usize;
                while i < n_words {
                    let o = i * 8;
                    let w = u64::from_le_bytes(img.pixels[o..o + 8].try_into().unwrap());
                    h = (h ^ w).wrapping_mul(0x0000_0100_0000_01b3);
                    i += stride;
                }
            }
            // Tail bytes that the word loop can't cover.
            for &b in &img.pixels[n_words * 8..] {
                h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        };
        // Hash either a CPU `Image` or a GPU `GpuImage` port value. The GPU
        // variant hashes (producer_node_id, port, frame_stamp) so a producer
        // re-render naturally invalidates the param-cache key downstream.
        // Also covers Mesh / Text so 3D-pipeline nodes (3D Render) participate
        // correctly — Material rides on a Text port now, so its content hash
        // here is what flips the cache when the user edits the material.
        let img_or_gpu_hash = |val: &PortValue| -> u64 {
            match val {
                PortValue::Image(img) => img_content_hash(img),
                PortValue::GpuImage(h) => {
                    let mut k = (h.node_id as u64)
                        .wrapping_mul(31)
                        .wrapping_add(h.port as u64);
                    k = k.wrapping_mul(31).wrapping_add(h.frame_stamp);
                    k
                }
                PortValue::Mesh(m) => std::sync::Arc::as_ptr(m) as usize as u64,
                PortValue::GpuMesh(h) => {
                    let mut k = (h.node_id as u64)
                        .wrapping_mul(31)
                        .wrapping_add(h.port as u64);
                    k = k.wrapping_mul(31).wrapping_add(h.frame_stamp);
                    k
                }
                PortValue::Text(s) => {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut h = DefaultHasher::new();
                    s.hash(&mut h);
                    h.finish()
                }
                _ => 0,
            }
        };
        // Build a per-frame set of `(producer_node, producer_port)` pairs that
        // have at least one downstream consumer requiring CPU pixel bytes.
        // The image-evaluation loop (and the WGSL Viewer injection below)
        // previously called `Graph::has_cpu_consumer(id, 0)` at 7 points,
        // each walking `connections` from scratch — O(N·E) per frame for N
        // image nodes and E edges. Building the set once reduces every
        // query to an O(1) HashSet lookup.
        let cpu_consumer_producers: std::collections::HashSet<(NodeId, usize)> = {
            let mut set = std::collections::HashSet::new();
            for conn in &self.graph.connections {
                if let Some(consumer) = self.graph.nodes.get(&conn.to_node) {
                    if consumer.node_type.needs_cpu_image_input(conn.to_port) {
                        set.insert((conn.from_node, conn.from_port));
                    }
                }
            }
            set
        };
        let has_cpu_consumer_cached =
            |nid: NodeId, port: usize| -> bool { cpu_consumer_producers.contains(&(nid, port)) };
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
                    let needs = has_cpu_consumer_cached(id, 0);
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
            let is_viewer = self
                .graph
                .nodes
                .get(&id)
                .map(|n| matches!(n.node_type, NodeType::WgslViewer { .. }))
                .unwrap_or(false);
            if is_viewer {
                if let Some(img) = ctx.data_mut(|d| {
                    d.get_temp::<std::sync::Arc<ImageData>>(egui::Id::new((
                        "wgsl_image_output",
                        id,
                    )))
                }) {
                    let has_gpu_entry = self.gpu_tex_cache.get_node_output_cloned(id, 0).is_some();
                    // If any downstream consumer needs CPU pixels (Transform,
                    // Crop, ColorChannel, AI Request, ML Model, save-to-disk,
                    // Theme BG, …) we must readback the GPU texture into a CPU
                    // ImageData here. Otherwise we'd hand them a GpuImage stub
                    // they silently drop.
                    let needs_cpu = has_cpu_consumer_cached(id, 0);
                    if has_gpu_entry && img.pixels.is_empty() && needs_cpu {
                        if let Some(rs) = &self.wgpu_render_state {
                            if let Some(cpu_img) = self
                                .gpu_tex_cache
                                .readback_node_output(id, 0, &rs.device, &rs.queue)
                            {
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
                                self.gpu_tex_cache
                                    .get_or_upload(&rs.device, &rs.queue, &img);
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
            let analyzer_ids: Vec<NodeId> = self
                .graph
                .nodes
                .iter()
                .filter(|(_, n)| match &n.node_type {
                    NodeType::AudioAnalyzer => true,
                    NodeType::Dynamic { inner } => inner.node.type_tag() == "audio_analyzer",
                    _ => false,
                })
                .map(|(&id, _)| id)
                .collect();
            for id in analyzer_ids {
                let (amp, peak, bass, mid, treble) = self
                    .audio
                    .analyzer_results
                    .get(&id)
                    .and_then(|a| a.try_lock().ok())
                    .map(|a| (a.amplitude, a.peak, a.bass, a.mid, a.treble))
                    .unwrap_or((0.0, 0.0, 0.0, 0.0, 0.0));
                let source_name = connections
                    .iter()
                    .find(|c| c.to_node == id && c.to_port == 0)
                    .and_then(|c| self.graph.nodes.get(&c.from_node))
                    .map(|n| n.node_type.title().to_string())
                    .unwrap_or_default();
                ctx.data_mut(|d| {
                    d.insert_temp(
                        egui::Id::new(("audio_analysis", id)),
                        [amp, peak, bass, mid, treble],
                    );
                    d.insert_temp(egui::Id::new(("audio_analysis_source", id)), source_name);
                });
                values.insert((id, 0), PortValue::Float(amp));
                values.insert((id, 1), PortValue::Float(peak));
                values.insert((id, 2), PortValue::Float(bass));
                values.insert((id, 3), PortValue::Float(mid));
                values.insert((id, 4), PortValue::Float(treble));
            }

            // ── Spectrum Analyzer injection ────────────────────────────
            let spectrum_ids: Vec<NodeId> = self
                .graph
                .nodes
                .iter()
                .filter(|(_, n)| match &n.node_type {
                    NodeType::SpectrumAnalyzer => true,
                    NodeType::Dynamic { inner } => inner.node.type_tag() == "music_visualizer",
                    _ => false,
                })
                .map(|(&id, _)| id)
                .collect();
            for id in spectrum_ids {
                let sample_rate = self.audio.engine_sample_rate.max(1.0);
                let (bins, phases, peak_hz, centroid_hz, energy) = self
                    .audio
                    .spectrum_results
                    .get(&id)
                    .and_then(|s| s.try_lock().ok())
                    .map(|s| {
                        let bins = s.bins.clone();
                        let phases = s.phases.clone();
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
                            if v > peak_val {
                                peak_val = v;
                                peak_idx = i;
                            }
                            let f = bin_to_hz(i);
                            sum_w += v;
                            sum_wf += v * f;
                            sum += v;
                        }
                        let peak_hz = bin_to_hz(peak_idx);
                        let centroid_hz = if sum_w > 1e-6 { sum_wf / sum_w } else { 0.0 };
                        let energy = (sum / n as f32).clamp(0.0, 1.0);
                        (bins, phases, peak_hz, centroid_hz, energy)
                    })
                    .unwrap_or_else(|| {
                        (
                            vec![0.0f32; crate::audio::analysis::SPECTRUM_BINS],
                            vec![0.5f32; crate::audio::analysis::SPECTRUM_BINS],
                            0.0,
                            0.0,
                            0.0,
                        )
                    });

                let source_name = connections
                    .iter()
                    .find(|c| c.to_node == id && c.to_port == 0)
                    .and_then(|c| self.graph.nodes.get(&c.from_node))
                    .map(|n| n.node_type.title().to_string())
                    .unwrap_or_default();
                ctx.data_mut(|d| {
                    d.insert_temp(egui::Id::new(("spectrum_bins", id)), bins.clone());
                    d.insert_temp(egui::Id::new(("spectrum_phases", id)), phases.clone());
                    d.insert_temp(
                        egui::Id::new(("spectrum_scalars", id)),
                        [peak_hz, centroid_hz, energy],
                    );
                    d.insert_temp(egui::Id::new(("spectrum_source", id)), source_name);
                });
                // Rasterize bars for SpectrumAnalyzer only (port 0 = Image).
                // Music Visualizer renders its own image in evaluate(), don't overwrite.
                if matches!(
                    self.graph.nodes.get(&id).map(|n| &n.node_type),
                    Some(NodeType::SpectrumAnalyzer)
                ) {
                    let img = crate::nodes::spectrum_analyzer::rasterize_bins(&bins, 256, 96);
                    values.insert((id, 0), PortValue::Image(std::sync::Arc::new(img)));
                    values.insert((id, 1), PortValue::Float(peak_hz));
                    values.insert((id, 2), PortValue::Float(centroid_hz));
                    values.insert((id, 3), PortValue::Float(energy));
                }
            }
        }
        for (&id, node) in &self.graph.nodes {
            if matches!(node.node_type, NodeType::AudioPlayer { .. }) {
                if let Some(progress) = ctx
                    .data_mut(|d| d.get_temp::<f32>(egui::Id::new(("audio_player_progress", id))))
                {
                    values.insert((id, 1), PortValue::Float(progress));
                }
            }
            if matches!(node.node_type, NodeType::AudioPlaylist { .. }) {
                if let Some(p) =
                    ctx.data_mut(|d| d.get_temp::<f32>(egui::Id::new(("pl_progress", id))))
                {
                    values.insert((id, 1), PortValue::Float(p));
                }
                if let Some(idx) =
                    ctx.data_mut(|d| d.get_temp::<f32>(egui::Id::new(("pl_index", id))))
                {
                    values.insert((id, 2), PortValue::Float(idx));
                }
                if let Some(name) =
                    ctx.data_mut(|d| d.get_temp::<String>(egui::Id::new(("pl_name", id))))
                {
                    values.insert((id, 3), PortValue::Text(name));
                }
            }
            // Keypoint Extract: parsed in render_with_context (after ML image loop),
            // inject X/Y/Conf back into values map so downstream nodes see them.
            if let NodeType::Dynamic { ref inner } = node.node_type {
                if inner.node.type_tag() == "keypoint_extract" {
                    if let Some(x) =
                        ctx.data_mut(|d| d.get_temp::<f32>(egui::Id::new(("kp_x", id))))
                    {
                        values.insert((id, 0), PortValue::Float(x));
                    }
                    if let Some(y) =
                        ctx.data_mut(|d| d.get_temp::<f32>(egui::Id::new(("kp_y", id))))
                    {
                        values.insert((id, 1), PortValue::Float(y));
                    }
                    if let Some(c) =
                        ctx.data_mut(|d| d.get_temp::<f32>(egui::Id::new(("kp_conf", id))))
                    {
                        values.insert((id, 2), PortValue::Float(c));
                    }
                }
                if inner.node.type_tag() == "pose_match" {
                    if let Some(l) =
                        ctx.data_mut(|d| d.get_temp::<String>(egui::Id::new(("pm_label", id))))
                    {
                        values.insert((id, 0), PortValue::Text(l));
                    }
                    if let Some(c) =
                        ctx.data_mut(|d| d.get_temp::<f32>(egui::Id::new(("pm_conf", id))))
                    {
                        values.insert((id, 1), PortValue::Float(c));
                    }
                    if let Some(p) =
                        ctx.data_mut(|d| d.get_temp::<String>(egui::Id::new(("pm_probs", id))))
                    {
                        values.insert((id, 2), PortValue::Text(p));
                    }
                    if let Some(m) =
                        ctx.data_mut(|d| d.get_temp::<f32>(egui::Id::new(("pm_matched", id))))
                    {
                        values.insert((id, 3), PortValue::Float(m));
                    }
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
                let is_dynamic = matches!(
                    self.graph.nodes.get(&nid).map(|n| &n.node_type),
                    Some(NodeType::Dynamic { .. })
                );
                if !is_dynamic {
                    continue;
                }
                let inputs: Vec<PortValue> = self.graph.collect_inputs(nid, &values);
                // Skip nodes with image inputs (handled in the image loop below).
                // Must include GpuImage too — otherwise a Dynamic node with a
                // GpuImage input is re-evaluated here via the generic
                // `inner.node.evaluate(&inputs)` path which only matches
                // PortValue::Image and stamps `None` into `values`, racing
                // with the image loop's correct evaluation below.
                if inputs
                    .iter()
                    .any(|v| matches!(v, PortValue::Image(_) | PortValue::GpuImage(_)))
                {
                    continue;
                }
                // Only re-evaluate if any input is non-None (connected)
                if inputs.iter().all(|v| matches!(v, PortValue::None)) {
                    continue;
                }
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
                let is_dynamic = matches!(
                    self.graph.nodes.get(&id).map(|n| &n.node_type),
                    Some(NodeType::Dynamic { .. })
                );
                // HandDetectionNode/FaceDetection/PoseDetection: image input is consumed by async
                // inference, not synchronous evaluate(). Their annotated output comes from egui
                // temp data (set during render). Exclude them so their match arms below run.
                let is_async_ml_node = is_dynamic
                    && self
                        .graph
                        .nodes
                        .get(&id)
                        .map(|n| match &n.node_type {
                            NodeType::Dynamic { inner } => {
                                let tag = inner.node.type_tag();
                                tag == "hand_detection"
                                    || tag == "face_detection"
                                    || tag == "pose_detection"
                            }
                            _ => false,
                        })
                        .unwrap_or(false);
                if is_dynamic
                    && !is_async_ml_node
                    && inputs.iter().any(|v| {
                        matches!(
                            v,
                            PortValue::Image(_)
                                | PortValue::GpuImage(_)
                                | PortValue::Mesh(_)
                                | PortValue::GpuMesh(_)
                        )
                    })
                {
                    // Build cache key from image content + params + node state.
                    // Mesh / Material / Camera all flow through `img_or_gpu_hash`
                    // so 3D Render reacts to upstream changes.
                    let mut cache_key: u64 = 0;
                    for inp in &inputs {
                        match inp {
                            PortValue::Image(_)
                            | PortValue::GpuImage(_)
                            | PortValue::Mesh(_)
                            | PortValue::GpuMesh(_)
                            | PortValue::Text(_) => {
                                cache_key = cache_key
                                    .wrapping_mul(31)
                                    .wrapping_add(img_or_gpu_hash(inp))
                            }
                            PortValue::Float(f) => {
                                cache_key =
                                    cache_key.wrapping_mul(31).wrapping_add(f.to_bits() as u64)
                            }
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
                    let cached: Option<(u64, Vec<(usize, PortValue)>)> =
                        ctx.data_mut(|d| d.get_temp(cache_id));
                    if let Some((prev_key, prev_results)) = cached {
                        if prev_key == cache_key {
                            // Cache hit: keep any cached GPU output texture alive past
                            // the 2-frame LRU and re-publish to the display callback's
                            // prerendered map (which is cleared every frame).
                            let has_gpu_output = prev_results
                                .iter()
                                .any(|(_, v)| matches!(v, PortValue::GpuImage(_)));
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
                    // Cache miss — reprocess via evaluate_with_ctx.
                    //
                    // Nodes that override evaluate_with_ctx (ImageStyleNode,
                    // BlendNode) handle their own GPU processing. CPU-only
                    // nodes (CropNode, TransformNode, ColorChannelNode, etc.)
                    // use the default delegation to evaluate(), which works
                    // because the caller readbacks GpuImage→Image for any
                    // node with needs_cpu_image_input() == true (the default).
                    let needs_readback = has_cpu_consumer_cached(id, 0);
                    // Build cpu_inputs: readback GpuImage→Image for nodes
                    // that need CPU pixels on each port. GPU-native nodes
                    // (needs_cpu_image_input → false) receive the raw
                    // GpuImage and handle it inside evaluate_with_ctx.
                    let cpu_inputs: Vec<PortValue> = {
                        // Query per-port CPU needs from the node
                        let needs_cpu_per_port: Vec<bool> = self
                            .graph
                            .nodes
                            .get(&id)
                            .map(|n| {
                                (0..inputs.len())
                                    .map(|p| n.node_type.needs_cpu_image_input(p))
                                    .collect()
                            })
                            .unwrap_or_else(|| vec![true; inputs.len()]);
                        inputs
                            .iter()
                            .enumerate()
                            .map(|(port_idx, v)| match v {
                                PortValue::GpuImage(h) => {
                                    if *needs_cpu_per_port.get(port_idx).unwrap_or(&true) {
                                        if let Some(rs) = &self.wgpu_render_state {
                                            self.gpu_tex_cache
                                                .readback_node_output(
                                                    h.node_id, h.port, &rs.device, &rs.queue,
                                                )
                                                .map(PortValue::Image)
                                                .unwrap_or(PortValue::None)
                                        } else {
                                            PortValue::None
                                        }
                                    } else {
                                        v.clone() // pass GpuImage through for GPU-native nodes
                                    }
                                }
                                other => other.clone(),
                            })
                            .collect()
                    };
                    if let Some(mut node_mut) = self.graph.nodes.remove(&id) {
                        if let NodeType::Dynamic { ref mut inner } = node_mut.node_type {
                            // Single evaluate_with_ctx call replaces the old
                            // three-step sequence of: (1) evaluate to apply port
                            // inputs, (2) type-tag dispatch to GPU path, (3) fallback
                            // evaluate. Nodes that override evaluate_with_ctx handle
                            // GPU processing themselves; all others delegate to
                            // evaluate() via the default trait method.
                            let results = {
                                let mut eval_ctx = crate::node_trait::EvalCtx {
                                    node_id: id,
                                    render_state: self.wgpu_render_state.as_ref(),
                                    gpu_tex_cache: &mut self.gpu_tex_cache,
                                    input_sources: input_sources.clone(),
                                    needs_readback,
                                };
                                inner.node.evaluate_with_ctx(&cpu_inputs, &mut eval_ctx)
                            }; // eval_ctx dropped — gpu_tex_cache borrow released
                            // NOTE: Do NOT call store_for_display here on cache miss.
                            // GPU-producing nodes (ImageStyleNode, BlendNode) call
                            // `tex_cache.publish(...)` inside their `process_gpu_cached`,
                            // which already invokes `store_for_display` internally. An
                            // extra call here would allocate a redundant bind group +
                            // texture view every frame (10x slowdown on dynamic inputs
                            // like Camera, where every frame is a cache miss).
                            // `touch` is still safe & cheap — just updates LRU stamp —
                            // but the publish path already refreshes it, so omit too.
                            for &(port, ref val) in &results {
                                values.insert((id, port), val.clone());
                            }
                            ctx.data_mut(|d| d.insert_temp(cache_id, (cache_key, results)));
                        }
                        self.graph.nodes.insert(id, node_mut);
                    }
                    continue;
                }
                let node = match self.graph.nodes.get(&id) {
                    Some(n) => n,
                    None => continue,
                };
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
                        let needs_cpu_downstream = has_cpu_consumer_cached(id, 0);
                        let out = match inputs.first() {
                            Some(PortValue::Image(img)) => PortValue::Image(img.clone()),
                            Some(PortValue::GpuImage(h)) => {
                                if needs_cpu_downstream {
                                    if let Some(rs) = &self.wgpu_render_state {
                                        if let Some(cpu_img) =
                                            self.gpu_tex_cache.readback_node_output(
                                                h.node_id, h.port, &rs.device, &rs.queue,
                                            )
                                        {
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
                            _ => image_data
                                .as_ref()
                                .map(|img| PortValue::Image(img.clone()))
                                .unwrap_or(PortValue::None),
                        };
                        values.insert((id, 0), out);
                    }
                    NodeType::ImageEffects {
                        brightness,
                        contrast,
                        saturation,
                        hue,
                        exposure,
                        gamma,
                    } => {
                        // Accept either CPU `Image` or GPU `GpuImage` upstream.
                        let stub_owned: Option<ImageData> = match inputs.first() {
                            Some(PortValue::GpuImage(h)) => Some(ImageData {
                                width: h.width,
                                height: h.height,
                                pixels: Vec::new(),
                            }),
                            _ => None,
                        };
                        let img_ref: Option<&ImageData> = match inputs.first() {
                            Some(PortValue::Image(img)) => Some(img.as_ref()),
                            Some(PortValue::GpuImage(_)) => stub_owned.as_ref(),
                            _ => None,
                        };
                        if let Some(img) = img_ref {
                            let param_hash = ((*brightness * 1000.0) as u64)
                                ^ ((*contrast * 1000.0) as u64) << 8
                                ^ ((*saturation * 1000.0) as u64) << 16
                                ^ ((*hue * 10.0) as u64) << 24
                                ^ ((*exposure * 1000.0) as u64) << 32
                                ^ ((*gamma * 1000.0) as u64) << 40;
                            // Defensive: `if let Some(img) = img_ref` above ensures
                            // `inputs.first()` is Some here, but guard anyway so a
                            // future refactor (e.g. default-texture fallback path) can't
                            // panic the UI thread on a disconnected input.
                            let cache_key = param_hash
                                ^ inputs.first().map(|v| img_or_gpu_hash(v)).unwrap_or(0);
                            let cache_id = egui::Id::new(("img_fx_cache", id));
                            let cached: Option<(u64, PortValue)> =
                                ctx.data_mut(|d| d.get_temp(cache_id));
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
                            let needs_readback = has_cpu_consumer_cached(id, 0);
                            let result_val: PortValue = if let Some(rs) = &self.wgpu_render_state {
                                nodes::image_effects::process_gpu_cached(
                                    img,
                                    *brightness,
                                    *contrast,
                                    *saturation,
                                    *hue,
                                    *exposure,
                                    *gamma,
                                    id,
                                    rs,
                                    &mut self.gpu_tex_cache,
                                    input_sources.first().copied().flatten(),
                                    needs_readback,
                                )
                                .unwrap_or_else(|| {
                                    // CPU fallback only meaningful when we actually have pixels
                                    if !img.pixels.is_empty() {
                                        PortValue::Image(nodes::image_effects::process(
                                            img,
                                            *brightness,
                                            *contrast,
                                            *saturation,
                                            *hue,
                                            *exposure,
                                            *gamma,
                                        ))
                                    } else {
                                        PortValue::None
                                    }
                                })
                            } else if !img.pixels.is_empty() {
                                PortValue::Image(nodes::image_effects::process(
                                    img,
                                    *brightness,
                                    *contrast,
                                    *saturation,
                                    *hue,
                                    *exposure,
                                    *gamma,
                                ))
                            } else {
                                PortValue::None
                            };
                            ctx.data_mut(|d| {
                                d.insert_temp(cache_id, (cache_key, result_val.clone()))
                            });
                            values.insert((id, 0), result_val);
                        }
                    }
                    // Crop migrated to trait-based CropNode (evaluated in graph.evaluate)
                    // Blend migrated to trait-based BlendNode — now handled by
                    // evaluate_with_ctx in the generic Dynamic-image block above.
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
                            pts_hash = pts_hash
                                .wrapping_mul(31)
                                .wrapping_add(p[0].to_bits() as u64);
                            pts_hash = pts_hash
                                .wrapping_mul(31)
                                .wrapping_add(p[1].to_bits() as u64);
                        }
                        let cache_id = egui::Id::new(("curve_lut_cache", id));
                        let cached: Option<(u64, std::sync::Arc<ImageData>)> =
                            ctx.data_mut(|d| d.get_temp(cache_id));
                        let need_regen =
                            cached.as_ref().map(|(h, _)| *h != pts_hash).unwrap_or(true);
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
                    NodeType::ColorCurves {
                        master,
                        red,
                        green,
                        blue,
                        ..
                    } => {
                        let stub_owned: Option<ImageData> = match inputs.first() {
                            Some(PortValue::GpuImage(h)) => Some(ImageData {
                                width: h.width,
                                height: h.height,
                                pixels: Vec::new(),
                            }),
                            _ => None,
                        };
                        let img_ref: Option<&ImageData> = match inputs.first() {
                            Some(PortValue::Image(img)) => Some(img.as_ref()),
                            Some(PortValue::GpuImage(_)) => stub_owned.as_ref(),
                            _ => None,
                        };
                        if let Some(img) = img_ref {
                            let mut curve_hash: u64 = 0;
                            for pts in [
                                master.as_slice(),
                                red.as_slice(),
                                green.as_slice(),
                                blue.as_slice(),
                            ] {
                                for p in pts {
                                    curve_hash = curve_hash
                                        .wrapping_mul(31)
                                        .wrapping_add(p[0].to_bits() as u64);
                                    curve_hash = curve_hash
                                        .wrapping_mul(31)
                                        .wrapping_add(p[1].to_bits() as u64);
                                }
                            }
                            // Defensive: same as the img_fx_cache site above — guarded
                            // by the surrounding `if let Some(img) = img_ref` but the
                            // .unwrap() is a refactor hazard, so fall back to 0 instead.
                            let cache_key = inputs.first().map(|v| img_or_gpu_hash(v)).unwrap_or(0)
                                ^ curve_hash;
                            let cache_id = egui::Id::new(("cc_cache", id));
                            let cached: Option<(u64, PortValue)> =
                                ctx.data_mut(|d| d.get_temp(cache_id));
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
                            let needs_readback = has_cpu_consumer_cached(id, 0);
                            let result_val: PortValue = if let Some(rs) = &self.wgpu_render_state {
                                nodes::color_curves::process_gpu_cached(
                                    img,
                                    master,
                                    red,
                                    green,
                                    blue,
                                    id,
                                    rs,
                                    &mut self.gpu_tex_cache,
                                    input_sources.first().copied().flatten(),
                                    needs_readback,
                                )
                                .unwrap_or_else(|| {
                                    if !img.pixels.is_empty() {
                                        PortValue::Image(nodes::color_curves::process(
                                            img, master, red, green, blue,
                                        ))
                                    } else {
                                        PortValue::None
                                    }
                                })
                            } else if !img.pixels.is_empty() {
                                PortValue::Image(nodes::color_curves::process(
                                    img, master, red, green, blue,
                                ))
                            } else {
                                PortValue::None
                            };
                            ctx.data_mut(|d| {
                                d.insert_temp(cache_id, (cache_key, result_val.clone()))
                            });
                            values.insert((id, 0), result_val);
                        }
                    }
                    NodeType::VideoPlayer {
                        current_frame,
                        duration,
                        ..
                    } => {
                        if let Some(frame) = current_frame {
                            values.insert((id, 0), PortValue::Image(frame.clone()));
                            if let Some(rs) = &self.wgpu_render_state {
                                self.gpu_tex_cache
                                    .get_or_upload(&rs.device, &rs.queue, frame);
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
                                self.gpu_tex_cache
                                    .get_or_upload(&rs.device, &rs.queue, frame);
                                self.gpu_tex_cache.mark_node_source(frame, id, 0);
                            }
                        }
                    }
                    // New trait-based Video In node — same GPU-cache pattern as
                    // the legacy Camera enum variant above. Without this, its
                    // frames never reach `FRAME_TEX_SNAPSHOT`, so downstream
                    // GPU consumers (notably the Phase 2 Syphon sink) can't
                    // resolve an upstream texture.
                    NodeType::Dynamic { inner } if inner.node.type_tag() == "video_in" => {
                        let any = &*inner.node as &dyn std::any::Any;
                        if let Some(vin) =
                            any.downcast_ref::<crate::nodes::video_in_node::VideoInNode>()
                        {
                            // CPU path (Camera / Screen): upload frame to GPU
                            // cache and emit PortValue::Image so CPU consumers
                            // (Transform / Crop / Fill / AI Request / ML) still
                            // work, while the cache makes GPU consumers fast.
                            if let Some(frame) = vin.current_frame.as_ref() {
                                values.insert((id, 0), PortValue::Image(frame.clone()));
                                if let Some(rs) = &self.wgpu_render_state {
                                    self.gpu_tex_cache
                                        .get_or_upload(&rs.device, &rs.queue, frame);
                                    self.gpu_tex_cache.mark_node_source(frame, id, 0);
                                }
                            }
                            // GPU path (Syphon source): `poll_syphon_client`
                            // already pushed a fresh wgpu::Texture into the
                            // cache via `queue_publish_node_output`. Emit a
                            // `GpuImage` handle so downstream consumers like
                            // Visual Output / Syphon sink can resolve it via
                            // `frame_snapshot_get_texture(node_id, 0)`. CPU
                            // consumers will see nothing from this path —
                            // they need a readback upgrade (tracked in the
                            // plan's "Deferred follow-ups" section).
                            #[cfg(target_os = "macos")]
                            {
                                use crate::nodes::video_in_node::VideoSource;
                                if vin.source == VideoSource::Syphon {
                                    if let Some((w, h)) = vin.syphon_last_dims {
                                        let handle = crate::graph::GpuImageHandle {
                                            node_id: id,
                                            port: 0,
                                            width: w,
                                            height: h,
                                            frame_stamp: self.gpu_tex_cache.frame_stamp(id, 0),
                                        };
                                        values.insert((id, 0), PortValue::GpuImage(handle));
                                    }
                                }
                            }
                        }
                    }
                    NodeType::MlModel {
                        annotated_frame,
                        result_text,
                        result_json,
                        ..
                    } => {
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
                    NodeType::Dynamic { inner } if inner.node.type_tag() == "hand_detection" => {
                        // The HandDetectionNode stores its annotated frame in egui temp data
                        // (set in render_with_context when a new inference result arrives).
                        // Read it here and inject as port 0. Ports 1+2 come from evaluate().
                        let annotated_id = egui::Id::new(("hand_annotated", id));
                        if let Some(frame) = ctx.data(|d| {
                            d.get_temp::<std::sync::Arc<crate::graph::ImageData>>(annotated_id)
                        }) {
                            values.insert((id, 0), PortValue::Image(frame));
                        }
                    }
                    NodeType::Dynamic { inner } if inner.node.type_tag() == "face_detection" => {
                        let annotated_id = egui::Id::new(("face_annotated", id));
                        if let Some(frame) = ctx.data(|d| {
                            d.get_temp::<std::sync::Arc<crate::graph::ImageData>>(annotated_id)
                        }) {
                            values.insert((id, 0), PortValue::Image(frame));
                        }
                    }
                    NodeType::Dynamic { inner } if inner.node.type_tag() == "pose_detection" => {
                        let annotated_id = egui::Id::new(("pose_annotated", id));
                        if let Some(frame) = ctx.data(|d| {
                            d.get_temp::<std::sync::Arc<crate::graph::ImageData>>(annotated_id)
                        }) {
                            values.insert((id, 0), PortValue::Image(frame));
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
            let hub_nodes: Vec<(NodeId, String)> = self
                .graph
                .nodes
                .iter()
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
                let detected: Vec<(String, u8)> = self
                    .ob
                    .get_hub(*hub_id)
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
                            "knob" => labels.push(format!("k{}_val", id)),
                            "orb" => {
                                labels.push(format!("orb{}_ax", id));
                                labels.push(format!("orb{}_ay", id));
                                labels.push(format!("orb{}_az", id));
                                labels.push(format!("orb{}_gx", id));
                                labels.push(format!("orb{}_gy", id));
                                labels.push(format!("orb{}_gz", id));
                            }
                            other => {
                                labels.push(format!(
                                    "{}{}_val",
                                    other
                                        .chars()
                                        .next()
                                        .map(|c| c.to_string())
                                        .unwrap_or_default(),
                                    id
                                ));
                            }
                        }
                    }
                    labels
                };
                let mut stale_ports: Vec<usize> = Vec::new();
                if let Some(node) = self.graph.nodes.get(hub_id) {
                    if let NodeType::ObHub {
                        detected_devices, ..
                    } = &node.node_type
                    {
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
                    if let NodeType::ObHub {
                        detected_devices, ..
                    } = &mut node.node_type
                    {
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
                NodeType::Synth { .. }
                | NodeType::AudioPlayer { .. }
                | NodeType::AudioPlaylist { .. } => {
                    values.insert((id, 0), PortValue::Float(id as f32));
                }
                _ => {}
            }
        }
        if let Some(graph) = parked_graph {
            self.graph = graph;
        }
        values
    }

    /// Render nodes. If `pinned_only` is true, only render pinned nodes (at zoom 1.0).
    /// If false, only render non-pinned nodes (at canvas zoom).
    fn render_nodes_filtered(
        &mut self,
        ctx: &egui::Context,
        values: &HashMap<(NodeId, usize), PortValue>,
        pinned_only: bool,
    ) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        // P4.3.2E1: node bodies of a graph that may not run are not drawn.
        let inert = !self.graph.content.runnable();
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
        let mut network_actions: Vec<NetworkAction> = Vec::new();
        let mut dragging_from = self.dragging_from;
        let mut palette_spawns: Vec<([f32; 2], NodeType)> = Vec::new();
        let mut midi_actions: Vec<MidiAction> = Vec::new();
        let mut serial_actions: Vec<SerialAction> = Vec::new();
        let mut dmx_actions: Vec<crate::dmx::DmxAction> = Vec::new();
        let mut artnet_actions: Vec<crate::artnet::ArtNetAction> = Vec::new();
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
        // Capture a baseline when any pointer gesture begins. If the gesture
        // changed the graph (slider drag, field edit), it's committed as one
        // undo step on release. push()'s hash dedup drops no-op gestures.
        if ctx.input(|i| i.pointer.any_pressed()) && self.pending_gesture_snapshot.is_none() {
            self.pending_gesture_snapshot = Some(UndoSnapshot::new(
                self.graph.clone(),
                self.pinned_nodes.clone(),
            ));
        }

        for node_id in node_ids {
            let is_pinned = self.pinned_nodes.contains(&node_id);
            // Skip nodes not in the current pass
            if pinned_only != is_pinned {
                continue;
            }
            let mut node = match self.graph.nodes.remove(&node_id) {
                Some(n) => n,
                None => continue,
            };
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
            let dmx_output_open = self.dmx.is_output_open(node_id);
            let dmx_listening = self.dmx.is_listening(node_id);
            let artnet_listening = self.artnet.is_listening(node_id);
            let http_pending = self.http.is_pending(node_id);

            let _open = true;
            let inline = node.node_type.inline_ports() && !inert;
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
                if is_pinned { w * inv } else { w }
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
            let is_audio_player = matches!(
                node.node_type,
                NodeType::AudioPlayer { .. } | NodeType::AudioPlaylist { .. }
            );
            let is_speaker = matches!(node.node_type, NodeType::Speaker { .. });
            let no_title = node.node_type.no_title()
                || is_display
                || is_monitor
                || is_audio_player
                || is_speaker;
            let port_sz = if is_pinned {
                PORT_INTERACT * inv
            } else {
                PORT_INTERACT
            };
            let port_r = if is_pinned {
                PORT_RADIUS * inv
            } else {
                PORT_RADIUS
            };
            let title_size = if is_pinned { 14.0 * inv } else { 14.0 };
            let node_icon = crate::icons::node_icon(node.node_type.title());
            let title_with_icon = format!("{} {}", node_icon, title);
            let title_rt = egui::RichText::new(&title_with_icon)
                .color(accent)
                .strong()
                .size(title_size);
            // Pinned windows use zoom-bucketed ID to reset egui's cached window size on zoom change
            let zoom_bucket = if is_pinned {
                (zoom * 10.0).round() as i32
            } else {
                0
            };
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
                let scale_stroke = |s: &mut egui::Stroke| {
                    s.width *= inv;
                };
                scale_cr(&mut style.visuals.window_corner_radius);
                scale_stroke(&mut style.visuals.window_stroke);
                style.visuals.window_shadow.blur =
                    (style.visuals.window_shadow.blur as f32 * inv).round() as u8;
                style.visuals.window_shadow.spread =
                    (style.visuals.window_shadow.spread as f32 * inv).round() as u8;
                for ws in [
                    &mut style.visuals.widgets.noninteractive,
                    &mut style.visuals.widgets.inactive,
                    &mut style.visuals.widgets.hovered,
                    &mut style.visuals.widgets.active,
                ] {
                    scale_cr(&mut ws.corner_radius);
                    scale_stroke(&mut ws.bg_stroke);
                    scale_stroke(&mut ws.fg_stroke);
                }
                style.visuals.resize_corner_size *= inv;
                ctx.set_style(std::sync::Arc::new(style));
            }
            let is_custom_render = node.node_type.custom_render();
            // Custom frame — render_background for trait nodes, legacy match for enum nodes
            let bg_painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new(("node_bg", node_id)),
            ));
            let approx_rect = egui::Rect::from_min_size(
                egui::pos2(egui_x, egui_y),
                egui::vec2(node_width, 100.0),
            );
            let custom_frame =
                if let Some(frame) = node.node_type.render_background(&bg_painter, approx_rect) {
                    Some(frame)
                } else {
                    None
                };
            let is_comment = node.node_type.title() == "Comment";
            // Read-only tab, or editing paused while quitting (R3).
            let read_only_doc = !self.active_is_editable();
            // P4.2 Stage 2: a packaged instance opens its resolved entry.
            let custom_node_file = match &node.node_type {
                NodeType::CustomNodeInstance { .. } => self.graph.custom_instance_source(node_id),
                _ => None,
            };
            let mut win = egui::Window::new(title_rt)
                .id(egui::Id::new((
                    "node",
                    self.documents.active().id,
                    node_id,
                    zoom_bucket,
                )))
                .current_pos(egui::pos2(egui_x, egui_y))
                .default_width(node_width)
                .resizable(is_display && !read_only_doc)
                .movable(!read_only_doc)
                .constrain(false)
                .collapsible(
                    // Custom-node instances use double-click to open their
                    // .pwnode, which would otherwise also toggle collapse.
                    !is_custom_render && !no_title && !is_pinned && custom_node_file.is_none(),
                )
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
                // P4.1: nothing inside a read-only package tab is editable.
                if read_only_doc {
                    ui.disable();
                }
                // Constrain width to prevent expanding nodes — but Comment
                // sizes itself tightly to its text (see comment_node.rs
                // body_w computation), so the 180-floor here was leaving
                // empty padding to the right of short notes. Skip the
                // clamp for Comment so its window fits the visible card.
                if !is_comment {
                    ui.set_max_width(node_width.max(180.0));
                }

                // Top input ports (skip for inline-port nodes)
                if !inline {
                    for (i, pdef) in input_defs.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(port_sz, port_sz),
                                egui::Sense::click_and_drag(),
                            );
                            let val = Graph::static_input_value(&connections, values, node_id, i);
                            let kind = pdef.kind; // Use semantic kind from port definition
                            let is_connected = connections
                                .iter()
                                .any(|c| c.to_node == node_id && c.to_port == i);
                            let (type_fill, type_border) = port_colors_for_kind(kind, false);
                            let (fill, border) = if response.hovered() || response.dragged() {
                                (egui::Color32::YELLOW, egui::Color32::WHITE)
                            } else {
                                (type_fill, type_border)
                            };
                            // Highlight port if click-wiring and hovering a compatible target
                            let click_wiring_hover =
                                self.click_wiring && dragging_from.is_some() && response.hovered();
                            let (fill, border) = if click_wiring_hover {
                                (egui::Color32::from_rgb(100, 255, 100), egui::Color32::WHITE)
                            } else {
                                (fill, border)
                            };
                            draw_shaped_port(
                                ui.painter(),
                                rect.center(),
                                port_r,
                                fill,
                                border,
                                PORT_BORDER,
                                kind,
                                is_connected,
                            );
                            port_positions.insert((node_id, i, true), rect.center());
                            ui.label(format!("{}: {}", pdef.name, val));
                            if !self.save_as_open && response.drag_started() {
                                if let Some(existing) = connections
                                    .iter()
                                    .find(|c| c.to_node == node_id && c.to_port == i)
                                {
                                    let src_node = existing.from_node;
                                    let src_port = existing.from_port;
                                    dragging_from = Some((src_node, src_port, true));
                                    pending_disconnects.push((node_id, i));
                                } else {
                                    dragging_from = Some((node_id, i, false));
                                }
                            }
                            // Click-to-connect: click on input port starts or completes wiring
                            // Click-to-connect: click on input port starts or completes wiring
                            if !self.save_as_open && response.clicked() {
                                if let Some((src_node, src_port, is_output)) = dragging_from {
                                    if self.click_wiring && is_output && node_id != src_node {
                                        // Complete: output → this input
                                        pending_connections.push((src_node, src_port, node_id, i));
                                        dragging_from = None;
                                        self.click_wiring = false;
                                    } else if self.click_wiring && !is_output && node_id != src_node
                                    {
                                        // Complete: this input ← input (reverse)
                                        pending_connections.push((node_id, i, src_node, src_port));
                                        dragging_from = None;
                                        self.click_wiring = false;
                                    }
                                } else {
                                    // Start click-wiring from input port
                                    if let Some(existing) = connections
                                        .iter()
                                        .find(|c| c.to_node == node_id && c.to_port == i)
                                    {
                                        dragging_from =
                                            Some((existing.from_node, existing.from_port, true));
                                        pending_disconnects.push((node_id, i));
                                    } else {
                                        dragging_from = Some((node_id, i, false));
                                    }
                                    self.click_wiring = true;
                                }
                            }
                        });
                    }
                    if !input_defs.is_empty() {
                        ui.separator();
                    }
                }

                let project_dir_path = self.project_path.as_deref().map(std::path::Path::new);
                // P4.3.2E1: a graph that may not run is drawn inert: its
                // nodes, ports and wires stay editable, but node code (and
                // the device, network and audio actions it can request) does
                // not run. Editor chrome (File, Zoom, Palette) stays usable.
                if inert && !crate::graph::group::is_editor_chrome(&node.node_type) {
                    ui.weak("Not running: this document needs permission.");
                } else {
                    nodes::render_content(
                        ui,
                        &mut node.node_type,
                        node_id,
                        values,
                        &connections,
                        &midi_out_ports,
                        &midi_in_ports,
                        midi_conn_out,
                        midi_conn_in,
                        &mut midi_actions,
                        &serial_ports,
                        serial_conn,
                        &mut serial_actions,
                        &monitor_state,
                        osc_listening,
                        &mut osc_actions,
                        &mut network_actions,
                        &mut port_positions,
                        &mut dragging_from,
                        &mut http_actions,
                        http_pending,
                        &api_keys,
                        &wgpu_render_state,
                        &mut pending_disconnects,
                        &mut ob_manager,
                        &mut audio_manager,
                        &self.mcp_log,
                        self.mcp_rx.is_some(),
                        &mut dmx_actions,
                        &mut artnet_actions,
                        dmx_output_open,
                        dmx_listening,
                        artnet_listening,
                        project_dir_path,
                    );
                }

                // Check if a Palette node wants to spawn new nodes
                if matches!(node.node_type, NodeType::Palette { .. }) {
                    for spawn_nt in nodes::palette_actions(ui) {
                        palette_spawns.push((node.pos, spawn_nt));
                    }
                }

                // Bottom output ports (skip for inline-port nodes)
                if !inline {
                    if !output_defs.is_empty() {
                        ui.separator();
                    }
                    for (i, pdef) in output_defs.iter().enumerate() {
                        let val = values
                            .get(&(node_id, i))
                            .cloned()
                            .unwrap_or(PortValue::None);
                        let label_text = format!("{}: {}", pdef.name, val);
                        let kind = pdef.kind; // Use semantic kind from port definition
                        let is_connected = connections
                            .iter()
                            .any(|c| c.from_node == node_id && c.from_port == i);
                        let (type_fill, type_border) = port_colors_for_kind(kind, true);

                        // Use columns: label left, port circle right
                        ui.horizontal(|ui| {
                            ui.label(&label_text);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let (rect, response) = ui.allocate_exact_size(
                                        egui::vec2(port_sz, port_sz),
                                        egui::Sense::click_and_drag(),
                                    );
                                    // Highlight port if click-wiring and hovering a compatible target
                                    let click_wiring_hover = self.click_wiring
                                        && dragging_from.is_some()
                                        && response.hovered();
                                    let (fill, border) = if click_wiring_hover {
                                        (
                                            egui::Color32::from_rgb(100, 255, 100),
                                            egui::Color32::WHITE,
                                        )
                                    } else if response.hovered() || response.dragged() {
                                        (egui::Color32::YELLOW, egui::Color32::WHITE)
                                    } else {
                                        (type_fill, type_border)
                                    };
                                    draw_shaped_port(
                                        ui.painter(),
                                        rect.center(),
                                        port_r,
                                        fill,
                                        border,
                                        PORT_BORDER,
                                        kind,
                                        is_connected,
                                    );
                                    port_positions.insert((node_id, i, false), rect.center());
                                    if !self.save_as_open && response.drag_started() {
                                        dragging_from = Some((node_id, i, true));
                                    }
                                    // Click-to-connect: click on output port starts or completes wiring
                                    // Click-to-connect: click on output port starts or completes wiring
                                    if !self.save_as_open && response.clicked() {
                                        if let Some((src_node, src_port, is_output)) = dragging_from
                                        {
                                            if self.click_wiring
                                                && !is_output
                                                && node_id != src_node
                                            {
                                                // Complete: input → this output
                                                pending_connections
                                                    .push((node_id, i, src_node, src_port));
                                                dragging_from = None;
                                                self.click_wiring = false;
                                            } else if self.click_wiring
                                                && is_output
                                                && node_id != src_node
                                            {
                                                // Complete: output → output (swap direction)
                                                pending_connections
                                                    .push((src_node, src_port, node_id, i));
                                                dragging_from = None;
                                                self.click_wiring = false;
                                            }
                                        } else {
                                            // Start click-wiring from output port
                                            dragging_from = Some((node_id, i, true));
                                            self.click_wiring = true;
                                        }
                                    }
                                },
                            );
                        });
                    }
                }
            });

            // Restore original style after pinned window's inverse-zoom
            if is_pinned {
                ctx.set_style(_original_style.clone());
            }

            if let Some(r) = &resp {
                if !self.save_as_open {
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
                // Stage 5: no selection changes while a version switch is
                // being confirmed.
                if !self.save_as_open
                    && self.pending_version_switch.is_none()
                    && select_trigger
                    && !ctx.input(|i| i.modifiers.alt)
                {
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
                    crate::system_log::log(&format!(
                        "select block: node {:?} alt={} selected_nodes now = {:?}",
                        node_id,
                        ctx.input(|i| i.modifiers.alt),
                        self.selected_nodes
                    ));
                }

                // P3: A double-click on any node belongs to that node, never
                // to the empty canvas behind it.
                let double_clicked_here = r.response.double_clicked()
                    || (ctx.input(|i| {
                        i.pointer
                            .button_double_clicked(egui::PointerButton::Primary)
                    }) && ctx
                        .pointer_latest_pos()
                        .map(|p| r.response.rect.contains(p))
                        .unwrap_or(false));
                if double_clicked_here {
                    self.node_double_click_consumed = true;
                }

                // P3: Double-click a custom-node instance to edit its .pwnode.
                if let Some(file) = &custom_node_file {
                    if !self.save_as_open
                        && self.pending_close_tab.is_none()
                        && !self.documents.is_locked()
                        && double_clicked_here
                    {
                        self.pending_edit_custom_node = Some(file.clone());
                    }
                }

                // Right-click context menu — skip for custom_render nodes (they handle their own UI)
                let secondary_in_rect = !is_custom_render
                    && ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary))
                    && ctx
                        .pointer_latest_pos()
                        .map(|p| r.response.rect.contains(p))
                        .unwrap_or(false);
                if !self.save_as_open
                    && !read_only_doc
                    && !is_custom_render
                    && (r.response.secondary_clicked() || secondary_in_rect)
                {
                    if !self.selected_nodes.contains(&node_id) {
                        self.selected_nodes.clear();
                        self.selected_nodes.insert(node_id);
                    }
                    self.context_menu_node = Some(node_id);
                    self.show_context_menu = true;
                    self.context_menu_pos =
                        ctx.pointer_latest_pos().unwrap_or(r.response.rect.center());
                    self.context_menu_opened_at = ctx.input(|i| i.time);
                }

                // Draw selection highlight — consistent for every node type.
                // (Custom-render nodes like Slider don't draw their own ring,
                // so they previously appeared unselected even when active.)
                if self.selected_nodes.contains(&node_id) {
                    // let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("selection")));
                    let painter = ctx.layer_painter(r.response.layer_id);
                    let sel_accent = ctx
                        .data_mut(|d| d.get_temp::<[u8; 3]>(egui::Id::new("theme_accent")))
                        .unwrap_or([80, 160, 255]);
                    painter.rect_stroke(
                        r.response.rect.expand(2.0),
                        4.0,
                        egui::Stroke::new(
                            2.0,
                            egui::Color32::from_rgb(sel_accent[0], sel_accent[1], sel_accent[2]),
                        ),
                        egui::StrokeKind::Outside,
                    );
                }
                // Pin indicator — solid accent circle with white pin icon
                if is_pinned {
                    // let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("pins")));
                    let painter = ctx.layer_painter(r.response.layer_id);
                    let badge_center =
                        egui::pos2(r.response.rect.left() + 12.0, r.response.rect.top() - 4.0);
                    let theme_accent = ctx
                        .data_mut(|d| d.get_temp::<[u8; 3]>(egui::Id::new("theme_accent")))
                        .unwrap_or([80, 160, 255]);
                    let accent_color =
                        egui::Color32::from_rgb(theme_accent[0], theme_accent[1], theme_accent[2]);
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

                // P4.3.2D: a package runtime refused by authorization keeps
                // its node and wires; label it so it is not mistaken for a
                // working node. Details are in Library → Permissions.
                if let Some(blocked) = self.graph.custom_blocked(node_id) {
                    let text = if blocked.needs_review() {
                        "🔒 Needs permission"
                    } else {
                        "🔒 Blocked"
                    };
                    let painter = ctx.layer_painter(r.response.layer_id);
                    painter.text(
                        egui::pos2(r.response.rect.left(), r.response.rect.top() - 6.0),
                        egui::Align2::LEFT_BOTTOM,
                        text,
                        egui::FontId::proportional(12.0),
                        egui::Color32::from_rgb(255, 190, 80),
                    );
                }
                // Error badge — red border + ⚠ corner icon when this node has a
                // recorded evaluation error. Click/hover to read the message.
                if let Some(err) = crate::node_errors::get(node_id) {
                    // let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("node_errors")));
                    let painter = ctx.layer_painter(r.response.layer_id);
                    let err_red = egui::Color32::from_rgb(230, 80, 80);
                    // Red border around the whole node
                    painter.rect_stroke(
                        r.response.rect.expand(2.0),
                        4.0,
                        egui::Stroke::new(2.0, err_red),
                        egui::StrokeKind::Outside,
                    );
                    // Warning badge at the top-right corner
                    let badge_center =
                        egui::pos2(r.response.rect.right() - 12.0, r.response.rect.top() - 4.0);
                    painter.circle_filled(badge_center, 10.0, err_red);
                    painter.circle_stroke(
                        badge_center,
                        10.0,
                        egui::Stroke::new(1.0, egui::Color32::WHITE),
                    );
                    painter.text(
                        badge_center,
                        egui::Align2::CENTER_CENTER,
                        "!",
                        egui::FontId::proportional(13.0),
                        egui::Color32::WHITE,
                    );
                    // Hover tooltip with the full error message — shown when
                    // the pointer is over the node window or its badge.
                    let pointer_over = ctx
                        .pointer_latest_pos()
                        .map(|p| r.response.rect.expand(4.0).contains(p))
                        .unwrap_or(false);
                    if pointer_over {
                        egui::show_tooltip_at_pointer(
                            ctx,
                            egui::LayerId::new(
                                egui::Order::Tooltip,
                                egui::Id::new("node_err_tip_layer"),
                            ),
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
                    let fg = ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("collapsed_ports"),
                    ));
                    // Input dots: dim version of theme text color (subtle hint).
                    // Output dots: theme accent so they read as the node's
                    // outgoing identity, matching the selected-wire highlight.
                    let theme_text = crate::nodes::theme::current_text_arr(ctx);
                    let in_dot = egui::Color32::from_rgba_unmultiplied(
                        theme_text[0],
                        theme_text[1],
                        theme_text[2],
                        90,
                    );
                    let out_dot = crate::nodes::theme::current_accent(ctx);
                    for i in 0..n_in {
                        let y_off = if n_in > 1 {
                            (i as f32 - (n_in - 1) as f32 / 2.0) * 4.0
                        } else {
                            0.0
                        };
                        let pos = egui::pos2(rect.left() - PORT_RADIUS, rect.center().y + y_off);
                        port_positions.insert((node_id, i, true), pos);
                        fg.circle_filled(pos, 3.0, in_dot);
                    }
                    for i in 0..n_out {
                        let y_off = if n_out > 1 {
                            (i as f32 - (n_out - 1) as f32 / 2.0) * 4.0
                        } else {
                            0.0
                        };
                        let pos = egui::pos2(rect.right() + PORT_RADIUS, rect.center().y + y_off);
                        port_positions.insert((node_id, i, false), pos);
                        fg.circle_filled(pos, 3.0, out_dot);
                    }

                    // For WGSL nodes: run the offscreen render pass even when collapsed
                    // so downstream multi-pass chains keep receiving image data.
                    if let NodeType::WgslViewer {
                        wgsl_code,
                        uniform_names,
                        uniform_types,
                        uniform_values,
                        canvas_w,
                        canvas_h,
                        image_a_mode,
                        image_b_mode,
                        feedback_reset_pending,
                        ..
                    } = &mut node.node_type
                    {
                        crate::nodes::wgsl_viewer::render_offscreen_pass(
                            ctx,
                            node_id,
                            wgsl_code,
                            uniform_names,
                            uniform_types,
                            uniform_values,
                            *canvas_w,
                            *canvas_h,
                            *image_a_mode,
                            *image_b_mode,
                            feedback_reset_pending,
                            &connections,
                            values,
                            &wgpu_render_state,
                        );
                    }
                }

                // Undo snapshot on drag start (coalesced — one per gesture)
                if r.response.drag_started()
                    && !ctx.input(|i| i.modifiers.alt)
                    && !self.drag_undo_pushed
                {
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
                    crate::system_log::log("PROP SIGNAL READ".to_string());
                    if !self.property_undo_pushed {
                        self.push_undo();
                        self.property_undo_pushed = true;
                    }
                }

                // Track drag delta for multi-node movement
                if r.response.dragged()
                    && self.selected_nodes.contains(&node_id)
                    && self.selected_nodes.len() > 1
                {
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
            ctx.data_mut(|d| {
                d.remove::<bool>(pin_id);
                d.remove::<bool>(display_pin_id);
            });
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
            for prefix in &[
                "slider_delete_action",
                "comment_delete_action",
                "display_delete_action",
                "button_delete_action",
            ] {
                let del_id = egui::Id::new((*prefix, node_id));
                if ctx.data_mut(|d| d.get_temp::<bool>(del_id).unwrap_or(false)) {
                    ctx.data_mut(|d| d.remove::<bool>(del_id));
                    self.push_undo();
                    nodes_to_delete.push(node_id);
                }
            }

            self.graph.nodes.insert(node_id, node);
        }
        if ctx.input(|i| i.pointer.any_released()) {
            if let Some(base) = self.pending_gesture_snapshot.take() {
                let current = UndoSnapshot::new(self.graph.clone(), self.pinned_nodes.clone());
                if !base.has_same_content(&current) {
                    self.undo_history.push(base);
                    self.is_dirty = true;
                }
            }
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
                        // Track the closest *incompatible* port too — if the user
                        // released right on top of an incompatible port we want
                        // to surface a toast instead of silently doing nothing.
                        let hit_radius = PORT_INTERACT * 2.0;
                        let mut best: Option<(f32, NodeId, usize, bool)> = None;
                        let mut best_incompat: Option<(f32, PortKind, PortKind)> = None;

                        crate::system_log::log(&format!(
                            "drop: src=({},{}) is_output={} ptr=({:.0},{:.0}) radius={:.0} ports={}",
                            src_node,
                            src_port,
                            is_output,
                            pointer.x,
                            pointer.y,
                            hit_radius,
                            port_positions.len()
                        ));

                        for (&(n, p, i), &q) in &port_positions {
                            crate::system_log::log(&format!(
                                "    reg nid={} pidx={} is_input={} at=({:.0},{:.0})",
                                n, p, i, q.x, q.y
                            ));
                        }

                        for (&(nid, pidx, is_input), &pos) in &port_positions {
                            let dist = pos.distance(pointer);

                            if dist < 120.0 {
                                crate::system_log::log(&format!(
                                    "  cand nid={} pidx={} is_input={} dist={:.0} same_node={}",
                                    nid,
                                    pidx,
                                    is_input,
                                    dist,
                                    nid == src_node
                                ));
                            }
                            if dist < hit_radius && nid != src_node {
                                // Must be opposite direction (output→input or input→output)
                                let valid_dir =
                                    (is_output && is_input) || (!is_output && !is_input);
                                if valid_dir {
                                    // Check port type compatibility
                                    let src_kind =
                                        self.graph.port_kind(src_node, src_port, is_output);
                                    let tgt_kind = self.graph.port_kind(nid, pidx, !is_input);
                                    let compatible = match (src_kind, tgt_kind) {
                                        (Some(s), Some(t)) => PortKind::compatible(s, t),
                                        _ => true, // Unknown port kind — allow connection
                                    };
                                    if compatible {
                                        if best.as_ref().map(|b| dist < b.0).unwrap_or(true) {
                                            best = Some((dist, nid, pidx, is_input));
                                        }
                                    } else if let (Some(s), Some(t)) = (src_kind, tgt_kind) {
                                        if best_incompat
                                            .as_ref()
                                            .map(|b| dist < b.0)
                                            .unwrap_or(true)
                                        {
                                            best_incompat = Some((dist, s, t));
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
                        } else if let Some((_, src_kind, tgt_kind)) = best_incompat {
                            // User dropped on a port but the kinds don't match —
                            // tell them why nothing happened.
                            let (from_k, to_k) = if is_output {
                                (src_kind, tgt_kind)
                            } else {
                                (tgt_kind, src_kind)
                            };
                            self.toast_warn(format!("Can't connect {:?} → {:?}", from_k, to_k));
                        }
                    }
                    dragging_from = None;
                }
            }
        }

        // Sync click_wiring state with egui temp data (for inline port nodes)
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("click_wiring_active"), self.click_wiring));
        // Pick up click-wire completions from inline ports
        if let Some((fn_, fp, tn, tp)) = ctx.data_mut(|d| {
            d.get_temp::<(NodeId, usize, NodeId, usize)>(egui::Id::new("click_wire_complete"))
        }) {
            ctx.data_mut(|d| {
                d.remove::<(NodeId, usize, NodeId, usize)>(egui::Id::new("click_wire_complete"))
            });
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
        if !nodes_to_delete.is_empty()
            || !pending_disconnects.is_empty()
            || !pending_connections.is_empty()
        {
            self.push_undo();
        }
        let any_deleted = !nodes_to_delete.is_empty();
        for id in nodes_to_delete {
            // Clear opt-drag state if the source or duplicated node is being deleted
            if self.opt_drag_source == Some(id) {
                self.opt_drag_source = None;
            }
            if self.opt_drag_created == Some(id) {
                self.opt_drag_created = None;
            }
            self.audio.remove_processor(id); // Remove from engine before graph
            self.midi.cleanup_node(id);
            self.serial.cleanup_node(id);
            self.osc.cleanup_node(id);
            self.network.cleanup_node(id);
            self.ob.cleanup_node(id);
            self.audio.cleanup_node(id);
            crate::nodes::video_player::cleanup_node(id);
            // Drop GPU textures owned by this node immediately so VRAM
            // doesn't balloon when users rapidly spawn/delete image nodes.
            self.gpu_tex_cache
                .invalidate_node(id, self.wgpu_render_state.as_ref());
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
            let script_info: Vec<(NodeId, usize, usize, bool)> = self
                .graph
                .nodes
                .iter()
                .filter_map(|(id, n)| match &n.node_type {
                    NodeType::Script {
                        input_names,
                        output_names,
                        continuous,
                        ..
                    } => Some((*id, input_names.len(), output_names.len(), *continuous)),
                    _ => None,
                })
                .collect();
            if !script_info.is_empty() {
                let before = self.graph.connections.len();
                for (sid, n_in, n_out, cont) in &script_info {
                    let input_base = if *cont { 1 } else { 2 };
                    let max_in = input_base + n_in;
                    self.graph.connections.retain(|c| {
                        // Prune bad from-side (output port out of range)
                        if c.from_node == *sid && c.from_port >= *n_out {
                            return false;
                        }
                        // Prune bad to-side (input port: either control port [0..input_base]
                        // is always valid, or input_base..max_in is valid; anything beyond is stale)
                        if c.to_node == *sid && c.to_port >= max_in {
                            return false;
                        }
                        true
                    });
                }
                if self.graph.connections.len() != before {
                    self.graph.mark_topo_dirty();
                }
            }
        }
        // Route / Switch stale-connection sweep: prune output-side connections
        // whose from_port falls outside the node's current valid output count,
        // and input-side connections beyond the Switch's valid input count.
        // Mirrors the Script sweep above.
        {
            let route_info: Vec<(NodeId, usize, &'static str)> = self
                .graph
                .nodes
                .iter()
                .filter_map(|(id, n)| match &n.node_type {
                    NodeType::Dynamic { inner } => {
                        let tag = inner.node.type_tag();
                        if tag == "route" || tag == "switch" {
                            let n_out = inner.node.outputs().len();
                            Some((*id, n_out, if tag == "route" { "route" } else { "switch" }))
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
                .collect();

            // Rebuild with in-count for switch (inputs().len() includes selector port)
            let switch_in_info: Vec<(NodeId, usize)> = self
                .graph
                .nodes
                .iter()
                .filter_map(|(id, n)| match &n.node_type {
                    NodeType::Dynamic { inner } if inner.node.type_tag() == "switch" => {
                        Some((*id, inner.node.inputs().len()))
                    }
                    _ => None,
                })
                .collect();

            if !route_info.is_empty() {
                let before = self.graph.connections.len();
                for (rid, n_out, _) in &route_info {
                    self.graph.connections.retain(|c| {
                        if c.from_node == *rid && c.from_port >= *n_out {
                            return false;
                        }
                        true
                    });
                }
                for (sid, max_in) in &switch_in_info {
                    self.graph.connections.retain(|c| {
                        if c.to_node == *sid && c.to_port >= *max_in {
                            return false;
                        }
                        true
                    });
                }
                if self.graph.connections.len() != before {
                    self.graph.mark_topo_dirty();
                }
            }
        }

        for &(fn_, fp, tn, tp) in &pending_connections {
            let from_name = self
                .graph
                .nodes
                .get(&fn_)
                .map(|n| n.node_type.title())
                .unwrap_or("?");
            let from_port = self
                .graph
                .nodes
                .get(&fn_)
                .and_then(|n| n.node_type.outputs().get(fp).map(|p| p.name.clone()))
                .unwrap_or_else(|| fp.to_string().into());
            let to_name = self
                .graph
                .nodes
                .get(&tn)
                .map(|n| n.node_type.title())
                .unwrap_or("?");
            let to_port = self
                .graph
                .nodes
                .get(&tn)
                .and_then(|n| n.node_type.inputs().get(tp).map(|p| p.name.clone()))
                .unwrap_or_else(|| tp.to_string().into());
            crate::system_log::log(format!(
                "Connected {}.{} → {}.{}",
                from_name, from_port, to_name, to_port
            ));
            self.graph.add_connection(fn_, fp, tn, tp);
            // Only send engine connect/disconnect for audio ports
            // For mixer nodes, skip gain control ports (odd: 1, 3, 5)
            let is_mixer_gain_port = matches!(
                self.graph.nodes.get(&tn).map(|n| &n.node_type),
                Some(NodeType::AudioMixer { .. })
            ) && tp % 2 != 0;
            if !is_mixer_gain_port && self.audio.has_processor(fn_) && self.audio.has_processor(tn)
            {
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
        if let Some(req) = ctx.data_mut(|d| {
            d.get_temp::<crate::nodes::wgsl_viewer::WgslSpawnRequest>(egui::Id::new(
                "wgsl_spawn_request",
            ))
        }) {
            ctx.data_mut(|d| {
                d.remove::<crate::nodes::wgsl_viewer::WgslSpawnRequest>(egui::Id::new(
                    "wgsl_spawn_request",
                ))
            });

            // Position the new node to the left of the WGSL node
            let wgsl_pos = self
                .graph
                .nodes
                .get(&req.target_node)
                .map(|n| n.pos)
                .unwrap_or([0.0, 0.0]);
            let spawn_pos = [
                wgsl_pos[0] - 200.0,
                wgsl_pos[1] + (req.target_port as f32 * 30.0),
            ];

            let (new_id, from_port) = match req.source_type.as_str() {
                "time" => {
                    let id = self.graph.add_node(
                        NodeType::Dynamic {
                            inner: crate::graph::DynNode {
                                node: Box::new(crate::nodes::time_node::TimeNode::default()),
                            },
                        },
                        spawn_pos,
                    );
                    (id, 0) // Time node output port 0 = Seconds
                }
                "color" => {
                    let id = self.graph.add_node(
                        NodeType::Dynamic {
                            inner: crate::graph::DynNode {
                                node: Box::new(crate::nodes::color_node::ColorNode::default()),
                            },
                        },
                        spawn_pos,
                    );
                    (id, 0) // Color node output port 0 = R (will connect R, G, B separately)
                }
                _ => {
                    // Slider with smart defaults
                    let (min, max, step, value) =
                        crate::nodes::wgsl_viewer::slider_defaults_for_uniform(&req.uniform_name);
                    let mut nt = NodeType::Slider {
                        value,
                        min,
                        max,
                        step,
                        slider_color: [80, 160, 255],
                        label: req.uniform_name.clone(),
                    };
                    Self::apply_theme_defaults_to_new(ctx, &mut nt);
                    let id = self.graph.add_node(nt, spawn_pos);
                    (id, 0) // Slider output port 0 = Value
                }
            };
            // Connect the new node to the WGSL uniform port
            if req.source_type == "color" {
                // Connect R, G, B outputs to 3 consecutive ports
                self.graph
                    .add_connection(new_id, 0, req.target_node, req.target_port); // R
                self.graph
                    .add_connection(new_id, 1, req.target_node, req.target_port + 1); // G
                self.graph
                    .add_connection(new_id, 2, req.target_node, req.target_port + 2); // B
            } else {
                self.graph
                    .add_connection(new_id, from_port, req.target_node, req.target_port);
            }

            crate::system_log::log(format!(
                "Auto-created {} for u.{}",
                req.source_type, req.uniform_name
            ));
        }
        // Handle WGSL Presets "+" button: spawn a paired WGSL Viewer
        // to the right of the picker and auto-wire Shader -> WGSL.
        if let Some(req) = ctx.data_mut(|d| {
            d.get_temp::<crate::nodes::wgsl_presets::WgslPresetsSpawnRequest>(egui::Id::new(
                "wgsl_presets_spawn_request",
            ))
        }) {
            ctx.data_mut(|d| {
                d.remove::<crate::nodes::wgsl_presets::WgslPresetsSpawnRequest>(egui::Id::new(
                    "wgsl_presets_spawn_request",
                ))
            });
            let src_pos = self
                .graph
                .nodes
                .get(&req.source_node)
                .map(|n| n.pos)
                .unwrap_or([0.0, 0.0]);
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
            crate::system_log::log(
                "Spawned paired WGSL Viewer for Formula Preset Picker".to_string(),
            );
        }
        // Reverse pairing: an empty Visuals (WGSL) viewer asked for a
        // Visual Presets picker. Spawn one to the LEFT with the default
        // (top-of-list) preset already selected and auto-wire it.
        if let Some(req) = ctx.data_mut(|d| {
            d.get_temp::<crate::nodes::wgsl_presets::WgslViewerLoadPresetRequest>(egui::Id::new(
                "wgsl_viewer_load_preset_request",
            ))
        }) {
            ctx.data_mut(|d| {
                d.remove::<crate::nodes::wgsl_presets::WgslViewerLoadPresetRequest>(egui::Id::new(
                    "wgsl_viewer_load_preset_request",
                ))
            });
            let viewer_pos = self
                .graph
                .nodes
                .get(&req.viewer_node)
                .map(|n| n.pos)
                .unwrap_or([0.0, 0.0]);
            let spawn_pos = [viewer_pos[0] - 280.0, viewer_pos[1]];
            // Build a fresh Visual Presets node defaulted to "gradient" —
            // the Default impl picks whatever the first priority-ordered
            // preset is, but we force "gradient" explicitly so that key
            // is used regardless of future reordering.
            let mut picker = crate::nodes::wgsl_presets::WgslPresetsNode::default();
            picker.preset_key = "gradient".to_string();
            let picker_id = self.graph.add_node(
                NodeType::Dynamic {
                    inner: crate::graph::DynNode {
                        node: Box::new(picker),
                    },
                },
                spawn_pos,
            );
            self.graph.add_connection(picker_id, 0, req.viewer_node, 0);
            crate::system_log::log(
                "Spawned Visual Presets picker for empty Visuals (WGSL) viewer".to_string(),
            );
        }
        self.midi.process(midi_actions);
        self.serial.process(serial_actions);
        self.osc.process(osc_actions);
        self.network.process(network_actions);
        self.http.process(http_actions);
        self.dmx.process(dmx_actions);
        self.artnet.process(artnet_actions);
    }
}
impl eframe::App for PatchworkApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // P4.3.2E2: everything the open document's nodes do this frame (in
        // node evaluation, drawing, device managers and the audio engine)
        // is checked against its rights. Entered again below once its
        // content state is resolved.
        let _document_rights = self.document_rights().enter();
        if let Some(receiver) = &self.hardware_flash_receiver {
            match receiver.try_recv() {
                Ok(result) => {
                    self.hardware_flash_result = Some(result);
                    self.hardware_flash_in_progress = false;
                    self.hardware_flash_receiver = None;
                }

                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.hardware_flash_result = Some(Err(
                        "Firmware flashing worker disconnected unexpectedly".to_string(),
                    ));
                    self.hardware_flash_in_progress = false;
                    self.hardware_flash_receiver = None;
                }

                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Background operation is still running.
                }
            }
        }
        // Share the active document type with the node palette.
        let is_custom_node = !self.documents.documents.is_empty()
            && self.documents.active().kind == crate::document::DocumentKind::CustomNode;

        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("active_is_custom_node"), is_custom_node);
        });
        // Drop the previous frame's ScrollArea inner-rect registrations
        // before any node draws this frame and re-pushes its own. The pan
        // handler at the end of update() reads what's in the registry
        // *after* nodes have drawn, so it sees this frame's rects.
        crate::nodes::clear_scroll_rects();

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
        // P6: the Player has its own frame (no editor chrome, canvas input,
        // shortcuts, dialogs or MCP) around the same runtime.
        if self.player.is_some() {
            self.player_frame(ctx);
            return;
        }
        // ── Auto-open click-popups for freshly created Slider / Comment ─────
        // (Other node types in the set are silently ignored — only those
        // listed below get a popup-open hint. Set populated by
        // `add_node_selected`.)
        if !self.pending_popup_open.is_empty() {
            let now = ctx.input(|i| i.time);
            let ids: Vec<NodeId> = self.pending_popup_open.drain().collect();
            for id in ids {
                let Some(node) = self.graph.nodes.get(&id) else {
                    continue;
                };
                match &node.node_type {
                    NodeType::Slider { .. } => {
                        ctx.data_mut(|d| {
                            d.insert_temp(egui::Id::new(("slider_popup", id)), true);
                            d.insert_temp(egui::Id::new(("slider_popup_time", id)), now);
                        });
                    }
                    NodeType::Dynamic { inner } if inner.node.type_tag() == "comment" => {
                        ctx.data_mut(|d| {
                            d.insert_temp(egui::Id::new(("comment_popup_d", id)), true);
                            d.insert_temp(egui::Id::new(("comment_popup_time_d", id)), now);
                        });
                    }
                    _ => {}
                }
            }
        }
        // Keep the OS title bar in sync with project + dirty state, and
        // intercept close requests when there are unsaved changes.
        self.update_window_title(ctx);
        self.handle_close_request(ctx);
        // ── OS-level menu bar (always visible) ──
        // Snapshot recent_projects to avoid borrowing `self` inside the
        // menu closure (which already mutates ctx.data).
        let recent_projects: Vec<String> = self.recent_projects.clone();
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("\u{2795} New Project").clicked() {
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(egui::Id::new("file_action_new"), true));
                        ui.close_menu();
                    }
                    if ui.button("\u{1f4c2} Open...").clicked() {
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(egui::Id::new("file_action_load"), true));
                        ui.close_menu();
                    }
                    if ui.button("Open Custom Node...").clicked() {
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(egui::Id::new("file_action_open_custom_node"), true);
                        });
                        ui.close_menu();
                    }
                    if ui.button("Insert Custom Node...").clicked() {
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(egui::Id::new("file_action_insert_custom_node"), true);
                        });
                        ui.close_menu();
                    }
                    ui.add_enabled_ui(!recent_projects.is_empty(), |ui| {
                        ui.menu_button("Open Recent", |ui| {
                            for path in &recent_projects {
                                let label = std::path::Path::new(path)
                                    .file_name()
                                    .map(|f| f.to_string_lossy().to_string())
                                    .unwrap_or_else(|| path.clone());
                                if ui.button(label).on_hover_text(path).clicked() {
                                    ui.ctx().data_mut(|d| {
                                        d.insert_temp(
                                            egui::Id::new("file_action_load_recent"),
                                            path.clone(),
                                        );
                                    });
                                    ui.close_menu();
                                }
                            }
                            if !recent_projects.is_empty() {
                                ui.separator();
                                if ui.button("Clear Recent").clicked() {
                                    ui.ctx().data_mut(|d| {
                                        d.insert_temp(
                                            egui::Id::new("file_action_clear_recent"),
                                            true,
                                        );
                                    });
                                    ui.close_menu();
                                }
                            }
                        });
                    });
                    if ui.button("\u{1f4be} Save").clicked() {
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(egui::Id::new("file_action_save"), true));
                        ui.close_menu();
                    }
                    if ui.button("Save As...").clicked() {
                        // Projects and custom nodes have separate Save As flows.
                        self.begin_save_as();
                        ui.close_menu();
                    }
                    // P6 Stage 2.3: export the saved project as an app.
                    let export_source = self.export_source();
                    ui.add_enabled_ui(export_source.is_some(), |ui| {
                        ui.menu_button("Export as App", |ui| {
                            if ui.button("Windows Portable\u{2026}").clicked() {
                                if let Some(source) = &export_source {
                                    self.open_export_dialog(source);
                                }
                                ui.close_menu();
                            }
                        });
                    })
                    .response
                    .on_disabled_hover_text(if cfg!(windows) {
                        "Save the project to export it as an app"
                    } else {
                        "Exporting apps is available on Windows"
                    });
                    ui.separator();

                    let can_publish = self.documents.active().kind
                        == crate::document::DocumentKind::CustomNode
                        && self.documents.active().path.is_some()
                        && !self.documents.active().read_only
                        && !self.is_dirty
                        && !self.documents.active().is_dirty;

                    if ui
                        .add_enabled(can_publish, egui::Button::new("Publish to Library"))
                        .on_hover_text(
                            "Save the custom node before publishing it to the shared Library",
                        )
                        .clicked()
                    {
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(egui::Id::new("file_action_publish_library"), true);
                        });
                        ui.close_menu();
                    }
                    // P4.1: editable copy of a read-only package version.
                    let read_only_path = self
                        .documents
                        .active()
                        .path
                        .clone()
                        .filter(|_| self.documents.active().read_only);
                    if ui
                        .add_enabled(
                            read_only_path.is_some(),
                            egui::Button::new("Duplicate for Editing…"),
                        )
                        .on_hover_text("Copy this published Library version to an editable file")
                        .clicked()
                    {
                        if let Some(path) = read_only_path {
                            self.duplicate_for_editing(path);
                        }
                        ui.close_menu();
                    }
                });
                ui.menu_button("Hardware", |ui| {
                    if ui.button("Devices...").clicked() {
                        self.hardware_scan_requested = true;

                        ui.ctx().data_mut(|d| {
                            d.insert_temp(egui::Id::new("hardware_devices_open"), true);
                        });

                        ui.close_menu();
                    }
                });
                // P4.3.1A: read-only catalogue browser.
                ui.menu_button("Library", |ui| {
                    if ui.button("Marketplace…").clicked() {
                        self.open_marketplace();
                        ui.close_menu();
                    }
                    if ui.button("Package Trust Review…").clicked() {
                        self.open_trust_review();
                        ui.close_menu();
                    }
                    if ui.button("Permissions…").clicked() {
                        self.open_permissions();
                        ui.close_menu();
                    }
                });
                ui.menu_button("Edit", |ui| {
                    let can_undo = self.undo_history.can_undo();
                    let can_redo = self.undo_history.can_redo();
                    if ui
                        .add_enabled(can_undo, egui::Button::new("\u{21a9} Undo"))
                        .clicked()
                    {
                        self.perform_undo();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(can_redo, egui::Button::new("\u{21aa} Redo"))
                        .clicked()
                    {
                        self.perform_redo();
                        ui.close_menu();
                    }
                });
            });
        });
        // P3: Document Tabs
        egui::TopBottomPanel::top("document_tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let active_id = self.documents.active().id;
                // P3: How often each open custom node is used in open documents.
                let usage = self.documents.custom_node_usage(&self.graph);

                let tabs: Vec<(u64, String, bool)> = self
                    .documents
                    .documents
                    .iter()
                    .map(|doc| {
                        let dirty = if doc.id == active_id {
                            self.is_dirty
                        } else {
                            doc.is_dirty
                        };
                        // P4.1: published package versions are read-only.
                        let title = if doc.read_only {
                            format!("🔒 {}", doc.title)
                        } else {
                            doc.title.clone()
                        };

                        (doc.id, title, dirty)
                    })
                    .collect();

                // Tabs are locked while the quit dialog is open.
                let tabs_locked = self.documents.is_locked();
                for (id, title, dirty) in tabs {
                    let title = match usage.get(&id) {
                        Some(count) => format!("{title} (used ×{count})"),
                        None => title,
                    };
                    let label = if dirty { format!("{} *", title) } else { title };
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !tabs_locked,
                                egui::SelectableLabel::new(id == active_id, label),
                            )
                            .clicked()
                        {
                            if self.pending_close_tab.is_none() && !self.save_as_open {
                                self.switch_document(id);
                            }
                        }
                        if ui
                            .add_enabled(!tabs_locked, egui::Button::new("×").small())
                            .on_hover_text("Close Tab")
                            .clicked()
                            && !self.save_as_open
                        {
                            // Do not close the last remaining tab.
                            if self.documents.documents.len() > 1 {
                                let is_dirty = if self.documents.active().id == id {
                                    self.is_dirty
                                } else {
                                    self.documents
                                        .documents
                                        .iter()
                                        .find(|doc| doc.id == id)
                                        .map(|doc| doc.is_dirty)
                                        .unwrap_or(false)
                                };
                                if is_dirty {
                                    // Ask for confirmation before closing.
                                    self.pending_close_tab = Some(id);
                                } else {
                                    // Clean tabs can close immediately.
                                    self.close_tab(id);
                                }
                            }
                        }
                    });
                }
                if ui
                    .add_enabled(!tabs_locked, egui::Button::new("+"))
                    .on_hover_text("New Tab")
                    .clicked()
                {
                    self.create_new_tab();
                }
            });
        });
        // P4.1: banner + Duplicate for Editing for read-only package tabs.
        self.render_read_only_banner(ctx);

        // P5: Hardware Devices window
        let hardware_window_id = egui::Id::new("hardware_devices_open");

        let mut hardware_open =
            ctx.data_mut(|d| d.get_temp::<bool>(hardware_window_id).unwrap_or(false));

        if hardware_open {
            if self.hardware_scan_requested {
                // P5: Refreshing hardware requires fresh target validation.
                self.hardware_validation_result = None;
                self.hardware_flash_confirmation_open = false;
                self.hardware_flash_result = None;
                match crate::hardware::discover_devices() {
                    Ok(devices) => {
                        self.hardware_devices = devices;
                        self.hardware_discovery_error = None;
                    }
                    Err(error) => {
                        self.hardware_devices.clear();
                        self.hardware_discovery_error = Some(error);
                    }
                }
                // P5: Discover Raspberry Pi Pico BOOTSEL drives.
                #[cfg(target_os = "windows")]
                {
                    self.hardware_pico_devices = crate::hardware::discover_identified_pico_drives();
                }
                // P5: Clear USB serial selection if the device disappeared.
                if let Some(selected_port) = &self.hardware_selected_serial_port {
                    let still_connected = self
                        .hardware_devices
                        .iter()
                        .any(|device| &device.port_name == selected_port);

                    if !still_connected {
                        self.hardware_selected_serial_port = None;
                    }
                }

                // P5: Clear Pico BOOTSEL selection if the drive disappeared.
                #[cfg(target_os = "windows")]
                {
                    if let Some(selected_drive) = &self.hardware_selected_pico_drive {
                        let still_connected = self
                            .hardware_pico_devices
                            .iter()
                            .any(|device| &device.drive_path == selected_drive);

                        if !still_connected {
                            self.hardware_selected_pico_drive = None;
                        }
                    }
                }
                self.hardware_scan_requested = false;
            }
            egui::Window::new("Hardware Devices")
                .open(&mut hardware_open)
                .default_width(440.0)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.label("Connected USB serial devices");
                    ui.separator();

                    if ui.button("Refresh").clicked() {
                        self.hardware_scan_requested = true;
                    }
                    // P5: Firmware file selection
                    ui.add_space(8.0);
                    ui.separator();
                    ui.heading("Firmware Flashing");
                    ui.add_space(6.0);
                    // P5: Display selected hardware target.
                    ui.label("Selected hardware target:");

                    if let Some(port) = &self.hardware_selected_serial_port {
                        ui.label(format!("USB serial port: {port}"));
                    } else {
                        #[cfg(target_os = "windows")]
                        {
                            if let Some(drive) = &self.hardware_selected_pico_drive {
                                ui.label(format!("Pico BOOTSEL drive: {}", drive.display()));
                            } else {
                                ui.label("No hardware device selected.");
                            }
                        }

                        #[cfg(not(target_os = "windows"))]
                        {
                            ui.label("No hardware device selected.");
                        }
                    }
                    // P5: Validate firmware against the selected hardware target.
                    if ui.button("Validate Target").clicked() {
                        if let Some(path) = &self.hardware_firmware_path {
                            let target = match path
                                .extension()
                                .and_then(|ext| ext.to_str())
                                .map(|ext| ext.to_ascii_lowercase())
                                .as_deref()
                            {
                                Some("bin") => Some(crate::hardware::FirmwareTarget::Esp32),
                                Some("uf2") => {
                                    Some(crate::hardware::FirmwareTarget::RaspberryPiPico)
                                }
                                _ => None,
                            };

                            if let Some(target) = target {
                                let request = crate::hardware::FlashRequest {
                                    target,
                                    firmware_path: path.clone(),
                                    port_name: self.hardware_selected_serial_port.clone(),
                                };

                                #[cfg(target_os = "windows")]
                                let pico_drive = self.hardware_selected_pico_drive.as_deref();

                                #[cfg(not(target_os = "windows"))]
                                let pico_drive: Option<
                                    &std::path::Path,
                                > = None;
                                let validation_result =
                                    crate::hardware::validate_flash_target(&request, pico_drive)
                                        .and_then(|_| {
                                            if matches!(
                                                target,
                                                crate::hardware::FirmwareTarget::Esp32
                                            ) {
                                                let address_text =
                                                    self.hardware_esp32_flash_address.trim();

                                                let address_hex = address_text
                                                    .strip_prefix("0x")
                                                    .or_else(|| address_text.strip_prefix("0X"))
                                                    .unwrap_or(address_text);

                                                let flash_address =
                u32::from_str_radix(address_hex, 16)
                    .map_err(|_| {
                        "Invalid ESP32 flash address. Use hexadecimal, e.g. 0x10000."
                            .to_string()
                    })?;

                                                let config = crate::hardware::Esp32FlashConfig {
                layout: crate::hardware::Esp32FirmwareLayout::Application,
                flash_address,
                chip: self.hardware_esp32_chip.clone(),
            };

                                                crate::hardware::validate_esp32_flash_config(
                                                    &config,
                                                )
                                                .and_then(|_| {
                                                    crate::hardware::validate_esp32_image(
                                                        &request.firmware_path,
                                                    )
                                                })
                                                .and_then(|_| {
                                                    crate::hardware::validate_esp32_firmware_chip(
                                                        &request.firmware_path,
                                                        &config.chip,
                                                    )
                                                })
                                            } else {
                                                Ok(())
                                            }
                                        });

                                self.hardware_validation_result = Some(validation_result);
                            } else {
                                eprintln!("P5: Unsupported firmware file type.");
                            }
                        } else {
                            eprintln!("P5: Select a firmware file first.");
                        }
                    }

                    if self.hardware_flash_confirmation_open {
                        egui::Window::new("Confirm Firmware Flash")
                            .collapsible(false)
                            .resizable(false)
                            .show(ctx, |ui| {
                                ui.heading("Flash firmware to hardware?");
                                ui.label("This operation will overwrite the device firmware.");

                                ui.add_space(8.0);

                                if let Some(path) = &self.hardware_firmware_path {
                                    ui.label(format!("Firmware: {}", path.display()));
                                }

                                ui.add_space(12.0);

                                ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
    self.hardware_flash_confirmation_open = false;
    self.hardware_flash_result = None;
}

if ui.button("Confirm Flash").clicked() {
    self.hardware_flash_confirmation_open = false;
    let is_pico_firmware = self.hardware_firmware_path
    .as_ref()
    .and_then(|path| path.extension())
    .and_then(|ext| ext.to_str())
    .map(|ext| ext.eq_ignore_ascii_case("uf2"))
    .unwrap_or(false);

if !is_pico_firmware {
    let is_esp32_firmware = self.hardware_firmware_path
        .as_ref()
        .and_then(|path| path.extension())
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("bin"))
        .unwrap_or(false);

    if is_esp32_firmware {
    let firmware_path = self.hardware_firmware_path.clone();
    let port_name = self.hardware_selected_serial_port.clone();

    let flash_address = self
        .hardware_esp32_flash_address
        .trim()
        .trim_start_matches("0x");

    let parsed_address = u32::from_str_radix(flash_address, 16);

    let preparation_result = match (
        firmware_path,
        port_name,
        parsed_address,
    ) {
        (Some(path), Some(port), Ok(address)) => {
            let config = crate::hardware::Esp32FlashConfig {
                layout: crate::hardware::Esp32FirmwareLayout::Application,
                flash_address: address,
                chip: self.hardware_esp32_chip.clone(),
            };

            crate::hardware::validate_esp32_flash_config(&config)
                .map(|_| (path, port, config))
        }
        _ => Err(
            "ESP32 firmware, serial port, or flash address is invalid."
                .to_string(),
        ),
    };

    match preparation_result {
    Ok((path, port, config)) => {
        if self.hardware_flash_in_progress {
            self.hardware_flash_result = Some(Err(
                "Flashing is already in progress".to_string(),
            ));
        } else {
            self.hardware_flash_result = None;
            self.hardware_flash_in_progress = true;

            self.hardware_flash_receiver = Some(
                crate::hardware::spawn_esp32_flash_worker(
                    path,
                    port,
                    config,
                ),
            );
        }
    }

    Err(error) => {
        self.hardware_flash_result = Some(Err(error));
    }
}
} else {
        self.hardware_flash_result = Some(Err(
            "Unsupported firmware type. Select a .uf2 or .bin file."
                .to_string(),
        ));
    }
} else {

#[cfg(target_os = "windows")]
{
    if self.hardware_flash_in_progress {
        self.hardware_flash_result =
            Some(Err("Flashing is already in progress".to_string()));
    } else if let (
        Some(firmware_path),
        Some(drive_path),
    ) = (
        self.hardware_firmware_path.clone(),
        self.hardware_selected_pico_drive.clone(),
    ) {
    let request = crate::hardware::FlashRequest {
        target: crate::hardware::FirmwareTarget::RaspberryPiPico,
        firmware_path: firmware_path.clone(),
        port_name: None,
    };

    let discovered_drive = crate::hardware::discover_identified_pico_drives()
    .into_iter()
    .any(|device| {
        device.drive_path
            .canonicalize()
            .ok()
            == drive_path.canonicalize().ok()
            && drive_path.canonicalize().is_ok()
    });

let validation_result = if discovered_drive {
    crate::hardware::validate_flash_target(
        &request,
        Some(&drive_path),
    )
} else {
    Err("Selected Pico BOOTSEL drive was not found in the latest scan".to_string())
};

match validation_result {
        Ok(()) => {
            self.hardware_flash_result = None;
            self.hardware_flash_in_progress = true;

            self.hardware_flash_receiver =
                Some(crate::hardware::spawn_pico_flash_worker(
                    firmware_path,
                    drive_path,
                ));
        }

        Err(error) => {
            self.hardware_flash_result = Some(Err(error));
            self.hardware_flash_in_progress = false;
        }
    }
} else {
    self.hardware_flash_result = Some(Err(
        "Select a Pico firmware file and BOOTSEL drive".to_string(),
    ));
}
}
}
}
            });
                            });
                        if let Some(result) = &self.hardware_flash_result {
                            ui.add_space(8.0);

                            match result {
                                Ok(()) => {
                                    ui.colored_label(
                                        egui::Color32::GREEN,
                                        "Firmware flashing completed successfully.",
                                    );
                                }
                                Err(error) => {
                                    ui.colored_label(
                                        egui::Color32::RED,
                                        format!("Firmware flashing failed: {error}"),
                                    );
                                }
                            }
                        }
                    }

                    ui.add_space(8.0);

                    let target_validated = matches!(self.hardware_validation_result, Some(Ok(())));

                    let firmware_target = self
                        .hardware_firmware_path
                        .as_ref()
                        .and_then(|path| path.extension())
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.to_ascii_lowercase());

                    // Pico BOOTSEL drives are only discovered on Windows; the
                    // field does not exist elsewhere (Stage 4.0: this check
                    // read it unconditionally, which broke non-Windows builds).
                    #[cfg(target_os = "windows")]
                    let pico_drive_selected = self.hardware_selected_pico_drive.is_some();
                    #[cfg(not(target_os = "windows"))]
                    let pico_drive_selected = false;

                    let device_ready = match firmware_target.as_deref() {
                        Some("uf2") => {
                            pico_drive_selected && self.hardware_selected_serial_port.is_none()
                        }

                        // ESP32 remains disabled until its flash
                        // configuration and UI are implemented.
                        // ESP32 requires a selected serial port and no Pico drive.
                        Some("bin") => {
                            self.hardware_selected_serial_port.is_some() && !pico_drive_selected
                        }

                        _ => false,
                    };

                    let can_flash =
                        target_validated && device_ready && !self.hardware_flash_in_progress;

                    if ui
                        .add_enabled(can_flash, egui::Button::new("Flash Firmware"))
                        .clicked()
                    {
                        self.hardware_flash_confirmation_open = true;
                    }

                    if self.hardware_flash_in_progress {
                        ui.label("Firmware flashing in progress...");
                    }
                    if let Some(result) = &self.hardware_validation_result {
                        match result {
                            Ok(()) => {
                                ui.colored_label(egui::Color32::GREEN, "Target validation passed.");
                            }
                            Err(error) => {
                                ui.colored_label(
                                    egui::Color32::RED,
                                    format!("Target validation failed: {error}"),
                                );
                            }
                        }
                    }
                    if ui.button("Browse Firmware...").clicked() {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Select ESP32 or Raspberry Pi Pico Firmware")
                            .add_filter("Supported Firmware", &["bin", "uf2"])
                            .add_filter("ESP32 Firmware", &["bin"])
                            .add_filter("Raspberry Pi Pico Firmware", &["uf2"])
                            .pick_file()
                        {
                            self.hardware_firmware_path = Some(path);
                            // P5: A newly selected firmware file requires fresh validation.
                            self.hardware_validation_result = None;
                            self.hardware_flash_confirmation_open = false;
                            self.hardware_flash_result = None;
                        }
                    }

                    if let Some(path) = &self.hardware_firmware_path {
                        ui.label(format!("Selected: {}", path.display()));

                        let extension = path
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();

                        let target = match extension.as_str() {
                            "bin" => Some(crate::hardware::FirmwareTarget::Esp32),
                            "uf2" => Some(crate::hardware::FirmwareTarget::RaspberryPiPico),
                            _ => None,
                        };

                        if let Some(target) = target {
                            let target_name = match target {
                                crate::hardware::FirmwareTarget::Esp32 => "ESP32",
                                crate::hardware::FirmwareTarget::RaspberryPiPico => {
                                    "Raspberry Pi Pico"
                                }
                            };

                            ui.label(format!("Firmware type: {target_name}"));
                            // P5: ESP32 firmware configuration controls
                            if matches!(target, crate::hardware::FirmwareTarget::Esp32) {
                                let previous_chip = self.hardware_esp32_chip.clone();
                                let previous_address = self.hardware_esp32_flash_address.clone();
                                ui.separator();
                                ui.heading("ESP32 Flash Configuration");

                                ui.horizontal(|ui| {
                                    ui.label("Chip model:");

                                    egui::ComboBox::from_id_salt("esp32_chip_selector")
                                        .selected_text(&self.hardware_esp32_chip)
                                        .show_ui(ui, |ui| {
                                            for chip in [
                                                "esp32", "esp32s2", "esp32s3", "esp32c2",
                                                "esp32c3", "esp32c5", "esp32c6", "esp32c61",
                                                "esp32h2", "esp32h4", "esp32p4", "esp32s31",
                                            ] {
                                                ui.selectable_value(
                                                    &mut self.hardware_esp32_chip,
                                                    chip.to_string(),
                                                    chip,
                                                );
                                            }
                                        });
                                });

                                ui.horizontal(|ui| {
                                    ui.label("Flash address:");

                                    ui.text_edit_singleline(&mut self.hardware_esp32_flash_address);
                                });
                                if self.hardware_esp32_chip != previous_chip
                                    || self.hardware_esp32_flash_address != previous_address
                                {
                                    self.hardware_validation_result = None;
                                    self.hardware_flash_confirmation_open = false;
                                    self.hardware_flash_result = None;
                                }

                                ui.label("Firmware layout: Application");

                                ui.colored_label(
        egui::Color32::YELLOW,
        "ESP32 flashing is disabled until configuration and device checks are complete.",
    );

                                ui.separator();
                            }
                            // P5: Warn when the firmware type does not match the selected device.
                            #[cfg(target_os = "windows")]
                            {
                                let pico_selected = self.hardware_selected_pico_drive.is_some();
                                let serial_selected = self.hardware_selected_serial_port.is_some();

                                match target {
                                    crate::hardware::FirmwareTarget::Esp32 if pico_selected => {
                                        ui.colored_label(
                egui::Color32::RED,
                "Device mismatch: ESP32 firmware cannot be flashed to a Pico BOOTSEL drive.",
            );
                                    }

                                    crate::hardware::FirmwareTarget::RaspberryPiPico
                                        if serial_selected =>
                                    {
                                        ui.colored_label(
                egui::Color32::RED,
                "Device mismatch: Select a Pico BOOTSEL drive for UF2 firmware.",
            );
                                    }

                                    _ => {}
                                }
                            }
                            let request = crate::hardware::FlashRequest {
                                target,
                                firmware_path: path.clone(),
                                port_name: None,
                            };

                            match match target {
                                crate::hardware::FirmwareTarget::Esp32 => {
                                    crate::hardware::validate_esp32_image(&path).and_then(|_| {
                                        crate::hardware::validate_esp32_firmware_chip(
                                            &path,
                                            &self.hardware_esp32_chip,
                                        )
                                    })
                                }
                                _ => crate::hardware::validate_firmware(&request),
                            } {
                                Ok(()) => {
                                    ui.colored_label(
                                        egui::Color32::GREEN,
                                        "Firmware file validation passed.",
                                    );

                                    // P5: Check Pico firmware against the selected BOOTSEL drive.
                                    #[cfg(target_os = "windows")]
                                    if matches!(
                                        target,
                                        crate::hardware::FirmwareTarget::RaspberryPiPico
                                    ) {
                                        if let Some(drive) = &self.hardware_selected_pico_drive {
                                            let info_path = drive.join("INFO_UF2.TXT");

                                            match crate::hardware::validate_pico_flash_target(
                                                path.as_path(),
                                                info_path.as_path(),
                                            ) {
                                                Ok(board) => {
                                                    ui.colored_label(
                                                        egui::Color32::GREEN,
                                                        format!(
                                                            "Firmware matches selected Pico: {:?}",
                                                            board
                                                        ),
                                                    );
                                                }
                                                Err(error) => {
                                                    ui.colored_label(
                                                        egui::Color32::RED,
                                                        format!(
                                                            "Pico compatibility failed: {error}"
                                                        ),
                                                    );
                                                }
                                            }
                                        } else {
                                            ui.label(
                    "Select a Pico BOOTSEL device to check compatibility.",
                );
                                        }
                                    }
                                }
                                Err(error) => {
                                    ui.colored_label(
                                        egui::Color32::RED,
                                        format!("Firmware validation failed: {error}"),
                                    );
                                }
                            }
                        } else {
                            ui.colored_label(
                                egui::Color32::RED,
                                "Unsupported firmware format. Select a .bin or .uf2 file.",
                            );
                        }
                    } else {
                        ui.label("No firmware selected.");
                    }
                    ui.add_space(8.0);

                    // match crate::hardware::discover_devices() {
                    //     Ok(devices) if devices.is_empty() => {
                    //         ui.label("No USB serial devices detected.");
                    //     }
                    //     Ok(devices) => {
                    //         for device in devices {
                    //             ui.group(|ui| {
                    //                 ui.heading(&device.port_name);
                    //                 ui.label(&device.board_family);

                    //                 ui.label(format!(
                    //                     "VID: {:04X} | PID: {:04X}",
                    //                     device.vid, device.pid
                    //                 ));

                    //                 if let Some(product) = &device.product {
                    //                     ui.label(format!("Product: {product}"));
                    //                 }

                    //                 if let Some(manufacturer) = &device.manufacturer {
                    //                     ui.label(format!("Manufacturer: {manufacturer}"));
                    //                 }
                    //             });

                    //             ui.add_space(5.0);
                    //         }
                    //     }
                    //     Err(error) => {
                    //         ui.colored_label(
                    //             egui::Color32::RED,
                    //             format!("Device discovery failed: {error}"),
                    //         );
                    //     }
                    // }

                    if let Some(error) = &self.hardware_discovery_error {
                        ui.colored_label(
                            egui::Color32::RED,
                            format!("Device discovery failed: {error}"),
                        );
                    } else if self.hardware_devices.is_empty() {
                        ui.label("No USB serial devices detected.");
                    } else {
                        for device in &self.hardware_devices {
                            ui.group(|ui| {
                                ui.heading(&device.port_name);
                                // P5: Select USB serial device for firmware flashing.
                                let is_selected = self.hardware_selected_serial_port.as_deref()
                                    == Some(device.port_name.as_str());
                                if ui.radio(is_selected, "Select for flashing").clicked() {
                                    self.hardware_selected_serial_port =
                                        Some(device.port_name.clone());

                                    self.hardware_validation_result = None;
                                    self.hardware_flash_confirmation_open = false;
                                    self.hardware_flash_result = None;

                                    // Only one hardware target may be selected.
                                    #[cfg(target_os = "windows")]
                                    {
                                        self.hardware_selected_pico_drive = None;
                                    }
                                }
                                ui.label(&device.board_family);

                                ui.label(format!(
                                    "VID: {:04X} | PID: {:04X}",
                                    device.vid, device.pid
                                ));

                                if let Some(product) = &device.product {
                                    ui.label(format!("Product: {product}"));
                                }

                                if let Some(manufacturer) = &device.manufacturer {
                                    ui.label(format!("Manufacturer: {manufacturer}"));
                                }
                            });

                            ui.add_space(5.0);
                        }
                    }
                    // P5: Raspberry Pi Pico BOOTSEL devices
                    #[cfg(target_os = "windows")]
                    {
                        ui.add_space(12.0);
                        ui.separator();
                        ui.heading("Raspberry Pi Pico BOOTSEL Devices");
                        ui.add_space(6.0);

                        if self.hardware_pico_devices.is_empty() {
                            ui.label("No Pico BOOTSEL devices detected.");
                        } else {
                            for device in &self.hardware_pico_devices {
                                ui.group(|ui| {
                                    let selected = self.hardware_selected_pico_drive.as_ref()
                                        == Some(&device.drive_path);

                                    let drive_label =
                                        format!("Pico BOOTSEL — {}", device.drive_path.display());

                                    if ui.radio(selected, drive_label).clicked() {
                                        if selected {
                                            self.hardware_selected_pico_drive = None;
                                        } else {
                                            self.hardware_selected_pico_drive =
                                                Some(device.drive_path.clone());

                                            // Only one hardware target may be selected.
                                            self.hardware_selected_serial_port = None;
                                        }
                                        self.hardware_validation_result = None;
                                        self.hardware_flash_confirmation_open = false;
                                        self.hardware_flash_result = None;
                                    }

                                    ui.label(format!("Drive: {}", device.drive_path.display()));

                                    let board_name = match device.board {
                                        crate::hardware::PicoBoard::Rp2040 => "RP2040",
                                        crate::hardware::PicoBoard::Rp2350 => "RP2350",
                                    };

                                    ui.label(format!("Board: {board_name}"));
                                    ui.label("Status: BOOTSEL mode");
                                });

                                ui.add_space(5.0);
                            }
                        }
                    }
                });
        }

        ctx.data_mut(|d| {
            d.insert_temp(hardware_window_id, hardware_open);
        });
        // P3: Confirm before closing a modified document tab.
        if let Some(document_id) = self.pending_close_tab {
            let document_title = self
                .documents
                .documents
                .iter()
                .find(|doc| doc.id == document_id)
                .map(|doc| doc.title.clone())
                .unwrap_or_else(|| "Untitled".to_string());

            egui::Window::new("Unsaved Changes")
                .collapsible(false)
                .resizable(false)
                .frame(
                    egui::Frame::window(&ctx.style())
                        .fill(egui::Color32::from_rgb(30, 30, 35))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(90, 90, 100))),
                )
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Save changes to '{}' before closing?",
                        document_title
                    ));
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            // Save the document being closed, not another active tab.
                            if self.documents.active().id != document_id {
                                self.switch_document(document_id);
                            }
                            if self.save_project_quick() {
                                self.sync_active_document();
                                self.pending_close_tab = None;
                                self.close_tab(document_id);
                            }
                        }
                        if ui.button("Discard").clicked() {
                            // Activate the target document so its state is current.
                            if self.documents.active().id != document_id {
                                self.switch_document(document_id);
                            }
                            // Explicitly discard its unsaved state.
                            self.is_dirty = false;
                            self.documents.active_mut().is_dirty = false;
                            self.pending_close_tab = None;
                            self.close_tab(document_id);
                        }
                        if ui.button("Cancel").clicked() {
                            self.pending_close_tab = None;
                        }
                    });
                });
        }
        // P6 Stage 2.3: the Export as App dialog.
        if let Some(mut dialog) = self.export_dialog.take() {
            let unsaved = self.is_dirty || self.documents.active().is_dirty;
            if dialog.show(ctx, unsaved) {
                self.export_dialog = Some(dialog);
            }
        }
        // P3: Save As project naming dialog
        // P3: Save As project naming dialog
        if self.save_as_open {
            let mut open = self.save_as_open;
            let mut save_requested = false;
            // P3: Keyboard shortcuts for Save As modal
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                open = false;
            }

            if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                save_requested = true;
            }
            egui::Modal::new(egui::Id::new("save_as_modal")).show(ctx, |ui| {
                ui.label("Project name:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.save_as_name)
                        .desired_width(280.0)
                        .hint_text("My Project"),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save_requested = true;
                    }

                    if ui.button("Cancel").clicked() {
                        open = false;
                    }
                });
            });
            if save_requested
                && self.documents.active().kind != crate::document::DocumentKind::Project
            {
                // Defensive: project Save As never converts a custom node.
                save_requested = false;
                open = false;
                self.toast_warn("Project Save As is not available in a custom-node tab.");
            }
            if save_requested {
                let name = self.save_as_name.trim().to_string();
                // Reject empty names, special directory names and path separators.
                let invalid_name = name.is_empty()
                    || name == "."
                    || name == ".."
                    || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
                    || name.ends_with('.')
                    || name.ends_with(' ');
                if invalid_name {
                    rfd::MessageDialog::new()
                        .set_title("Invalid Project Name")
                        .set_description("Enter a valid project name without special characters.")
                        .set_level(rfd::MessageLevel::Error)
                        .set_buttons(rfd::MessageButtons::Ok)
                        .show();
                } else if let Some(parent_dir) = rfd::FileDialog::new()
                    .set_title("Choose Where to Save Project")
                    .pick_folder()
                {
                    // If the selected folder already has the project name,
                    // use it directly instead of creating another subfolder.
                    let selected_name = parent_dir
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let project_dir = if selected_name.eq_ignore_ascii_case(&name) {
                        parent_dir
                    } else {
                        parent_dir.join(&name)
                    };
                    if self.save_to_dir(project_dir.display().to_string()) {
                        open = false;
                    }
                }
            }

            self.save_as_open = open;
        }
        // P2: Group-into-custom-node naming dialog.
        self.render_group_dialog(ctx);
        // P4.1: Publish to Library dialog.
        self.render_publish_dialog(ctx);
        // P4.3.3: Share to Marketplace.
        self.render_share_dialog(ctx);
        self.render_version_switch_dialog(ctx);
        // P4.3.1A: Marketplace browser (read-only; never edits the graph).
        self.render_marketplace(ctx);
        // P4.3.2C: Package Trust Review.
        self.render_trust_review(ctx);
        // P4.3.2D: package permission banner and review.
        self.render_permissions(ctx);
        // Floating modal for "save before quit?" — drawn after the menu
        // so it visually layers on top of canvas chrome.
        self.render_quit_dialog(ctx);
        // Top-center toast notifications. Drawn last so they overlay the
        // entire UI; auto-dismiss handled inside.
        self.render_toasts(ctx);
        // set_zoom_factor scales everything (nodes, text, chrome) via GPU — zero overhead.
        // Pinned nodes compensate with inverse zoom style + zoom-keyed window IDs.
        // ctx.set_zoom_factor(self.canvas_zoom);
        self.apply_theme(ctx);
        self.handle_file_drop(ctx);
        // P4.3.2E1: a document whose content is not authored here or
        // allowed by the user does not run. For the runtime part of the
        // frame (device polling, evaluation, hardware, plugins, image and
        // audio processing) the live graph is parked and an empty graph
        // stands in, so every subsystem behaves as if the document had no
        // nodes and closes what it opened. The graph is put back before MCP
        // and drawing, so it stays visible and editable.
        provenance::resolve_graph(&mut self.graph);
        let _document_rights = self.document_rights().enter();
        let values = self.run_graph_frame(ctx);
        // Process MCP commands (if MCP server is active)
        self.process_mcp_commands(&values);
        // Store BG image/shader for canvas drawing (from Theme node's BG Image port 21)
        {
            let mut bg_img: Option<std::sync::Arc<ImageData>> = None;
            let mut bg_wgsl_node: Option<NodeId> = None;
            if let Some((&theme_id, _)) = self
                .graph
                .nodes
                .iter()
                .find(|(_, n)| matches!(n.node_type, NodeType::Theme { .. }))
            {
                if let Some(conn) = self
                    .graph
                    .connections
                    .iter()
                    .find(|c| c.to_node == theme_id && c.to_port == 21)
                {
                    let source_node = conn.from_node;
                    // Check if the source is a WGSL Viewer — render shader directly as BG
                    if self
                        .graph
                        .nodes
                        .get(&source_node)
                        .map(|n| matches!(n.node_type, NodeType::WgslViewer { .. }))
                        .unwrap_or(false)
                    {
                        bg_wgsl_node = Some(source_node);
                    } else {
                        // Regular image source (Image node, ImageEffects, Blend, etc.)
                        // Read from the source node's output via the values HashMap.
                        // GpuImage handles get on-demand readback from the GPU
                        // texture cache so chains that ran fully on-GPU still
                        // produce a CPU-side BG image for the canvas painter.
                        if let Some(val) = values.get(&(conn.from_node, conn.from_port)) {
                            match val {
                                PortValue::Image(img) => {
                                    bg_img = Some(img.clone());
                                }
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
        // Reset per frame; node rendering sets it when a node is double-clicked.
        self.node_double_click_consumed = false;
        self.render_nodes_filtered(ctx, &values, false);
        self.render_nodes_filtered(ctx, &values, true);
        // Build audio chains AFTER node rendering — nodes populate s.sources
        // Sync audio engine state with the graph (start/stop, rebuild, param sync)
        if self.graph.content.runnable() {
            self.sync_audio_engine();
        } else {
            // The audio engine sees no nodes: chains stop, nothing starts.
            let graph = std::mem::replace(&mut self.graph, Graph::new());
            self.sync_audio_engine();
            self.graph = graph;
        }
        self.sync_console_messages();
        self.handle_system_node_actions(ctx);
        // P3: Open a custom node queued by double-click or the context menu.
        if let Some(file) = self.pending_edit_custom_node.take() {
            if !self.save_as_open && self.pending_close_tab.is_none() && !self.documents.is_locked()
            {
                self.open_custom_node_for_edit(std::path::PathBuf::from(file));
            }
        }
        // Handle OSC In spawn requests (create new OscIn pre-configured for a discovered address)
        {
            let osc_ids: Vec<NodeId> = self
                .graph
                .nodes
                .keys()
                .copied()
                .filter(|id| {
                    matches!(
                        self.graph.nodes.get(id).map(|n| &n.node_type),
                        Some(NodeType::OscIn { .. })
                    )
                })
                .collect();
            for osc_id in osc_ids {
                let spawn_id = egui::Id::new(("osc_spawn", osc_id));
                if let Some((addr, argc, port)) =
                    ctx.data_mut(|d| d.get_temp::<(String, usize, u16)>(spawn_id))
                {
                    ctx.data_mut(|d| d.remove::<(String, usize, u16)>(spawn_id));
                    // Stage 0: no automatic insertion into read-only tabs.
                    if !self.require_editable("Adding an OSC node") {
                        continue;
                    }
                    let src_pos = self
                        .graph
                        .nodes
                        .get(&osc_id)
                        .map(|n| n.pos)
                        .unwrap_or([200.0, 200.0]);
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
                    let new_id = self.add_node_selected(ctx, nt, [src_pos[0] + 280.0, src_pos[1]]);
                    // Auto-start listening on the spawned node
                    self.osc
                        .process(vec![crate::osc::OscAction::StartListening {
                            node_id: new_id,
                            port,
                        }]);
                }
            }
        }
        // Handle OB Hub spawn requests (create device node auto-connected to hub)
        {
            let hub_ids: Vec<NodeId> = self
                .graph
                .nodes
                .keys()
                .copied()
                .filter(|id| {
                    matches!(
                        self.graph.nodes.get(id).map(|n| &n.node_type),
                        Some(NodeType::ObHub { .. })
                    )
                })
                .collect();
            for hub_id in hub_ids {
                let spawn_id = egui::Id::new(("ob_spawn", hub_id));
                if let Some((dtype, did)) = ctx.data_mut(|d| d.get_temp::<(String, u8)>(spawn_id)) {
                    ctx.data_mut(|d| d.remove::<(String, u8)>(spawn_id));
                    // Stage 0: no automatic insertion into read-only tabs.
                    if !self.require_editable("Adding a device node") {
                        continue;
                    }
                    let hub_pos = self
                        .graph
                        .nodes
                        .get(&hub_id)
                        .map(|n| n.pos)
                        .unwrap_or([200.0, 200.0]);
                    let nt = match dtype.as_str() {
                        "joystick" => NodeType::ObJoystick {
                            device_id: did,
                            hub_node_id: hub_id,
                            label_color: [255, 255, 255],
                        },
                        "encoder" => NodeType::ObEncoder {
                            device_id: did,
                            hub_node_id: hub_id,
                            label_color: [255, 255, 255],
                        },
                        "move" => NodeType::ObMove {
                            device_id: did,
                            hub_node_id: hub_id,
                        },
                        "bend" => NodeType::ObBend {
                            device_id: did,
                            hub_node_id: hub_id,
                            label_color: [255, 255, 255],
                        },
                        "pressure" => NodeType::ObPressure {
                            device_id: did,
                            hub_node_id: hub_id,
                            label_color: [255, 255, 255],
                        },
                        "distance" => NodeType::ObDistance {
                            device_id: did,
                            hub_node_id: hub_id,
                            label_color: [255, 255, 255],
                        },
                        "knob" => NodeType::ObKnob {
                            device_id: did,
                            hub_node_id: hub_id,
                            label_color: [255, 255, 255],
                        },
                        "orb" => NodeType::ObOrb {
                            device_id: did,
                            hub_node_id: hub_id,
                            mode: 0,
                            color: [255, 255, 255],
                            param1: 0.0,
                            param2: 0.0,
                            speed: 1.0,
                            brightness: 1.0,
                        },
                        "motor" => NodeType::ObMotor {
                            device_id: did,
                            hub_node_id: hub_id,
                            mode: 0,
                            dir: 1,
                            speed: 400.0,
                            amount: 90.0,
                        },
                        "light" => NodeType::ObLight {
                            device_id: did,
                            hub_node_id: hub_id,
                            mode: 0,
                            color: [255, 180, 100],
                            brightness: 0.20,
                            position: 0.5,
                            speed: 0.5,
                            color_mode: 0,
                            send_mode: 0,
                        },
                        _ => continue,
                    };
                    self.push_undo();
                    let _ = self.add_node_selected(ctx, nt, [hub_pos[0] + 250.0, hub_pos[1]]);
                }
            }
        }
        // P4.1: read-only package tabs never become dirty and accept no
        // editing input (pan/zoom and non-mutating shortcuts still work).
        let read_only = self.documents.active().read_only;
        if read_only {
            self.is_dirty = false;
            self.documents.active_mut().is_dirty = false;
            self.show_node_menu = false;
            self.show_context_menu = false;
            self.wire_menu_conn = None;
        }
        // Canvas input (incl. keyboard shortcuts) pauses while a modal is open.
        // R3: while the quit dialog is open the canvas still pans and
        // renders, but nothing can be edited.
        // Stage 5: the version-switch confirmation pauses canvas input too.
        let quitting = self.modal_lock();
        if quitting {
            self.show_node_menu = false;
            self.show_context_menu = false;
            self.wire_menu_conn = None;
        }
        if !self.save_as_open && !self.group_dialog_open && self.publish_dialog.is_none() {
            if !read_only && !quitting {
                self.node_menu(ctx);
                self.context_menu(ctx);
            }
            self.handle_pan_zoom(ctx);
            if !read_only && !quitting {
                self.handle_canvas_interaction(ctx);
            }
        }
        ctx.request_repaint();
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // P6: the Player never replaces the editor's session.
        if self.player.is_none() {
            self.save_session();
        }
        // P6 Stage 2.9: GPU objects in thread-locals go now, with the
        // device, not during process exit (see the function).
        crate::gpu_image::release_thread_gpu_objects();
    }
}
impl PatchworkApp {
    pub(super) fn switch_document(&mut self, document_id: u64) -> bool {
        self.switch_document_inner(document_id, false)
    }

    /// Quit dialog only: move to the next unsaved document while documents
    /// are locked.
    pub(super) fn switch_document_for_quit(&mut self, document_id: u64) -> bool {
        self.switch_document_inner(document_id, true)
    }

    fn switch_document_inner(&mut self, document_id: u64, during_quit: bool) -> bool {
        // Tabs cannot be switched while the quit dialog is open, except by
        // Save & Continue itself.
        if self.documents.is_locked() && !during_quit {
            return false;
        }
        // Already active: a read-only tab is still restored from its stored
        // (authoritative) graph, in case the live graph was changed.
        if self.documents.active().id == document_id {
            self.restore_read_only_active();
            return true;
        }
        // Check the destination before changing any state.
        if !self
            .documents
            .documents
            .iter()
            .any(|doc| doc.id == document_id)
        {
            return false;
        }
        // Save the current document's state. Read-only documents keep the
        // graph they were loaded with (Stage 0 model invariant).
        {
            let current = self.documents.active_mut();
            current.store_graph(&self.graph);
            current.selected_nodes = self.selected_nodes.clone();
            current.pinned_nodes = self.pinned_nodes.clone();
            current.canvas_offset = self.canvas_offset;
            current.canvas_zoom = self.canvas_zoom;
            current.target_zoom = self.target_zoom;
            current.is_dirty = self.is_dirty && current.is_editable();
            current.undo_history = self.undo_history.clone();
            let path = self.project_path.as_ref().map(std::path::PathBuf::from);
            if let Err(error) = current.set_path(path) {
                crate::system_log::error(format!("Document location not changed: {error}"));
            }
        }
        // Switch the active document.
        if during_quit {
            self.documents.activate_for_quit(document_id);
        } else {
            self.documents.activate(document_id);
        }
        // Restore the destination document's state.
        {
            let next = self.documents.active();
            self.graph = next.graph.clone();
            self.selected_nodes = next.selected_nodes.clone();
            self.pinned_nodes = next.pinned_nodes.clone();
            self.canvas_offset = next.canvas_offset;
            self.canvas_zoom = next.canvas_zoom;
            self.target_zoom = next.target_zoom;
            self.is_dirty = next.is_dirty;
            self.undo_history = next.undo_history.clone();
            self.project_path = next
                .path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned());
        }
        // Clear temporary canvas state.
        self.port_positions.clear();
        self.node_rects.clear();
        self.selected_connection = None;
        self.dragging_from = None;
        self.box_select_start = None;
        self.box_select_end = None;
        // Clear any pending gesture from the previous tab
        self.pending_gesture_snapshot = None;
        self.caches_dirty = true;
        true
    }
    fn sync_active_document(&mut self) {
        let doc = self.documents.active_mut();
        // Graph and project information (read-only documents keep theirs)
        doc.store_graph(&self.graph);
        let path = self.project_path.as_ref().map(std::path::PathBuf::from);
        if let Err(error) = doc.set_path(path) {
            crate::system_log::error(format!("Document location not changed: {error}"));
        }
        // Editor state
        doc.selected_nodes = self.selected_nodes.clone();
        doc.pinned_nodes = self.pinned_nodes.clone();
        doc.canvas_offset = self.canvas_offset;
        doc.canvas_zoom = self.canvas_zoom;
        doc.target_zoom = self.target_zoom;
        // Undo history and dirty state
        doc.undo_history = self.undo_history.clone();
        doc.is_dirty = self.is_dirty;
        // Update the document title
        doc.title = doc
            .path
            .as_ref()
            .and_then(|path| path.file_stem())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string());
    }
    /// Open a new Untitled tab. Returns false (and creates nothing) while
    /// the quit dialog is open, so callers never load into the current tab.
    fn create_new_tab(&mut self) -> bool {
        if self.documents.is_locked() {
            return false;
        }
        let id = self.documents.new_document("Untitled");
        self.switch_document(id)
    }

    /// Open or close the quit dialog. While open, documents are locked:
    /// no tab switching, closing, opening or creating (Save & Continue
    /// excepted). Pending tab-close and open-custom-node requests are
    /// cancelled, which changes nothing.
    pub(super) fn set_quit_dialog_open(&mut self, open: bool) {
        self.pending_quit_dialog = open;
        if open {
            self.pending_version_switch = None;
        }
        self.documents.set_locked(open);
        if open {
            self.pending_close_tab = None;
            self.pending_edit_custom_node = None;
        }
    }

    /// Restore the live graph of an active read-only tab from its stored
    /// graph, and keep it clean. No-op for editable tabs.
    pub(super) fn restore_read_only_active(&mut self) {
        let document = self.documents.active();
        if !document.restore_into(&mut self.graph) {
            return;
        }
        self.is_dirty = false;
        self.documents.active_mut().is_dirty = false;
        let graph = &self.graph;
        self.selected_nodes
            .retain(|id| graph.nodes.contains_key(id));
        self.selected_connection = None;
        self.dragging_from = None;
        self.caches_dirty = true;
    }

    /// File → Save As / Ctrl+Shift+S, by document kind:
    /// * project: name + folder dialog (writes a project folder);
    /// * custom node: "Save Custom Node As" (writes a new .pwnode);
    /// * read-only package version: Duplicate for Editing.
    /// P6 Stage 2.3: the saved project an export would use (the active
    /// project document's folder), on Windows.
    fn export_source(&self) -> Option<std::path::PathBuf> {
        if !cfg!(windows) || self.documents.active().kind != crate::document::DocumentKind::Project
        {
            return None;
        }
        let dir = std::path::PathBuf::from(self.project_path.as_ref()?);
        dir.join("project.json").is_file().then_some(dir)
    }

    fn open_export_dialog(&mut self, source: &std::path::Path) {
        if self.export_dialog.as_ref().is_some_and(|d| d.running()) {
            return;
        }
        self.export_dialog = Some(export_dialog::ExportDialog::open(source, &self.graph));
    }

    pub(super) fn begin_save_as(&mut self) {
        if self.documents.is_locked() {
            return;
        }
        let document = self.documents.active();
        match document.kind {
            crate::document::DocumentKind::Project => {
                self.save_as_name = self
                    .project_path
                    .as_ref()
                    .and_then(|p| std::path::Path::new(p).file_name())
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| document.title.clone());
                self.save_as_open = true;
            }
            crate::document::DocumentKind::CustomNode => {
                if document.read_only {
                    if let Some(path) = document.path.clone() {
                        self.duplicate_for_editing(path);
                    }
                } else {
                    self.save_custom_node_as();
                }
            }
        }
    }

    /// Stage 0: whether the active document accepts graph changes.
    /// R3: editing is also paused while the quit dialog is open.
    pub(super) fn active_is_editable(&self) -> bool {
        // R3 / P4.2 Stage 5: editing pauses while a modal confirmation
        // (quit dialog or version switch) is open.
        self.documents.active().is_editable() && !self.modal_lock()
    }

    /// Stage 0: gate for every graph-mutating entry point. Shows a warning
    /// and returns false in a read-only (published package) tab.
    pub(super) fn require_editable(&mut self, action: &str) -> bool {
        if self.active_is_editable() {
            return true;
        }
        if self.pending_quit_dialog {
            self.toast_warn(format!(
                "{action} is paused while the quit dialog is open. Choose Cancel to keep editing."
            ));
            return false;
        }
        if self.pending_version_switch.is_some() {
            self.toast_warn(format!(
                "{action} is paused while the version switch is being confirmed."
            ));
            return false;
        }
        self.toast_warn(format!(
            "{action} is not available: this published Library version is read-only. \
             Use File → Duplicate for Editing to make an editable copy."
        ));
        false
    }
    /// P3: Open a .pwnode in an editable tab, reusing its tab if already open.
    pub(super) fn open_custom_node_for_edit(&mut self, path: std::path::PathBuf) {
        // P4.3.2D: a package version opened in a tab is authorized first.
        self.documents
            .set_trust_policy(permissions::editor_trust_policy());
        match self.documents.open_custom_node(path) {
            Ok(id) => {
                self.switch_document(id);
            }
            Err(error)
                if self.documents.take_blocked().is_some_and(|blocked| {
                    self.permissions.add_refused(blocked);
                    true
                }) =>
            {
                crate::system_log::warn(format!("Not opened: {error}"));
                self.toast_warn(error);
                self.permissions.open = true;
            }
            Err(error) => {
                crate::system_log::error(format!("Failed to open custom node: {error}"));
                self.toast_error(format!("Failed to open custom node: {error}"));
            }
        }
    }
    /// P3: After a .pwnode was saved, resynchronise every instance that
    /// references it in all open documents (active graph included) and drop
    /// their cached runtimes so the saved version is used immediately.
    pub(super) fn refresh_custom_node_instances(
        &mut self,
        saved_path: &std::path::Path,
        inputs: &[crate::document::CustomNodePort],
        outputs: &[crate::document::CustomNodePort],
    ) {
        let Ok(target) = std::fs::canonicalize(saved_path) else {
            return;
        };
        self.documents.clear_resolved_paths();

        let active_id = self.documents.active().id;
        let mut reports: Vec<(String, Vec<String>)> = Vec::new();

        // The active document's live graph is `self.graph`.
        let report = self
            .graph
            .refresh_custom_instances(&target, inputs, outputs);
        if report.changed {
            self.is_dirty = true;
            self.caches_dirty = true;
        }
        if !report.removed.is_empty() {
            reports.push((
                self.documents.active().title.clone(),
                describe_removed_custom_connections(&self.graph, &report.removed),
            ));
        }

        // Background documents keep their own graphs. Read-only documents
        // are never modified.
        for doc in self
            .documents
            .documents
            .iter_mut()
            .filter(|doc| doc.id != active_id && doc.is_editable())
        {
            let report = doc.graph.refresh_custom_instances(&target, inputs, outputs);
            if report.changed {
                doc.is_dirty = true;
            }
            if !report.removed.is_empty() {
                reports.push((
                    doc.title.clone(),
                    describe_removed_custom_connections(&doc.graph, &report.removed),
                ));
            }
        }

        for (title, lines) in reports {
            self.log_removed_custom_connections(&title, lines);
        }
    }
    /// P3: Tell the user about connections removed by a custom-node interface change.
    pub(super) fn report_removed_custom_connections(
        &mut self,
        document_title: &str,
        removed: &[crate::graph::RemovedCustomConnection],
    ) {
        if removed.is_empty() {
            return;
        }
        let lines = describe_removed_custom_connections(&self.graph, removed);
        self.log_removed_custom_connections(document_title, lines);
    }
    fn log_removed_custom_connections(&mut self, document_title: &str, lines: Vec<String>) {
        if lines.is_empty() {
            return;
        }
        for line in &lines {
            crate::system_log::warn(format!("[{document_title}] {line}"));
        }
        self.toast_warn(format!(
            "{} connection(s) removed in '{}' because a custom node's ports changed. \
             See the system log for details.",
            lines.len(),
            document_title
        ));
    }
    fn close_tab(&mut self, document_id: u64) -> bool {
        // Do not close the last remaining tab.
        if self.documents.documents.len() <= 1 {
            return false;
        }
        // Check whether the tab has unsaved changes.
        let is_active = self.documents.active().id == document_id;
        let Some(document) = self
            .documents
            .documents
            .iter()
            .find(|doc| doc.id == document_id)
        else {
            return false;
        };

        let is_dirty = if is_active {
            self.is_dirty
        } else {
            document.is_dirty
        };

        // Never silently discard unsaved work.
        if is_dirty {
            return false;
        }
        if is_active {
            // Find another tab to activate before removing this one.
            let Some(next_id) = self
                .documents
                .documents
                .iter()
                .find(|doc| doc.id != document_id)
                .map(|doc| doc.id)
            else {
                return false;
            };
            // Saves the outgoing document and loads the next document.
            self.switch_document(next_id);
        }
        let closed = self.documents.close_document(document_id);
        if closed {
            // P4.3.2E2: the closed document's rights end now, and with
            // them its workers' privileged operations.
            self.revoke_closed_documents();
        }
        closed
    }
}

/// P3: Human-readable descriptions of connections removed by a custom-node
/// interface change, resolved against the graph they were removed from.
fn describe_removed_custom_connections(
    graph: &Graph,
    removed: &[crate::graph::RemovedCustomConnection],
) -> Vec<String> {
    let node_label = |id: NodeId| {
        graph
            .nodes
            .get(&id)
            .map(|node| format!("{} #{}", node.node_type.title(), id))
            .unwrap_or_else(|| format!("#{id}"))
    };
    removed
        .iter()
        .map(|r| {
            let custom_name = std::path::Path::new(&r.file)
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| r.file.clone());
            let direction = if r.is_input { "input" } else { "output" };
            format!(
                "Removed connection {} port {} → {} port {}: custom node '{}' (#{}) no longer has {} port '{}'.",
                node_label(r.from_node),
                r.from_port,
                node_label(r.to_node),
                r.to_port,
                custom_name,
                r.instance_id,
                direction,
                r.port_name
            )
        })
        .collect()
}
