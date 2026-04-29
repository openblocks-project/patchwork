pub mod slider;
pub mod math_formula;
pub mod wgsl_viewer;
pub mod midi_out;
pub mod midi_in;
pub mod serial;
pub mod theme;
pub mod script;
pub mod console;
pub mod monitor;
pub mod osc_out;
pub mod osc_in;
pub mod dmx_out;
pub mod dmx_in;
pub mod artnet_out;
pub mod artnet_in;
pub mod palette;
pub mod http_request;
pub mod ai_request;
pub mod ai_shared;
pub mod ai_config_node;
pub mod text_gen_node;
pub mod code_gen_node;
pub mod image_gen_node;
pub mod file_menu;
pub mod zoom_control;
pub mod ob_hub;
pub mod ob_joystick;
pub mod ob_encoder;
pub mod ob_orb;
pub mod ob_distance;
pub mod ob_pressure;
pub mod ob_bend;
pub mod ob_knob;
pub mod ob_move;
pub mod html_viewer;
pub mod mcp_server;
pub mod rust_plugin;
pub mod synth;
pub mod audio_player;
pub mod audio_playlist;
pub mod audio_device;
pub mod audio_delay;
pub mod audio_distortion;
pub mod audio_pitchshift;
pub mod audio_filter;
pub mod audio_gain;
pub mod audio_eq;
pub mod speaker;
pub mod audio_mixer;
pub mod audio_input;
pub mod spectrum_analyzer;
pub mod audio_reverb;
pub mod clap_plugin;
pub mod audio_sampler;
pub mod folder_browser;
// Trait-based nodes
pub mod add_node;
pub mod multiply_node;
pub mod comment_node;
pub mod json_extract_node;
pub mod color_node;
pub mod gate_node;
pub mod display_node;
pub mod map_range_node;
pub mod string_format_node;
pub mod text_editor_node;
pub mod mouse_tracker_node;
pub mod point_2d_node;
pub mod fill_node;
pub mod image_scanner_node;
pub mod frame_recorder_node;
pub mod noise_removal_node;
pub mod voice_effects_node;
pub mod audio_analyzer_node;
pub mod music_visualizer_node;
pub mod spectral_synth_node;
pub mod neural_audio_node;
pub mod web_app_node;
pub mod websocket_node;
pub mod clock_node;
pub mod key_input_node;
pub mod button_node;
pub mod pack_node;
pub mod time_node;
pub mod file_node;
pub mod visual_output_node;
pub mod video_in_node;
pub mod video_out_node;
pub mod crop_node;
pub mod noise_node;
pub mod monitor_node;
pub mod sample_hold_node;
pub mod folder_browser_node;
pub mod file_menu_node;
pub mod draw_node;
pub mod zoom_control_node;
pub mod html_viewer_node;
pub mod console_node;
pub mod midi_note_node;
pub mod timer_node;
pub mod image_node;
pub mod image_effects;
pub mod blend;
pub mod curve;
pub mod draw;
pub mod color_curves;
pub mod ml_model;
pub mod transform_node;
pub mod image_style_node;
pub mod kaleidoscope_node;
pub mod color_channel_node;
pub mod video_player;
pub mod timer;
pub mod sample_hold;
pub mod select;
pub mod network_send;
pub mod network_receive;
pub mod settings_node;
pub mod wgsl_presets;
pub mod route_node;
pub mod tts_node;
pub mod terminal_node;
pub mod text_ops_node;
pub mod printer_node;
pub mod keypoint_extract;
pub mod smoother;
pub mod hand_detection;
pub mod face_detection;
pub mod pose_detection;
pub mod json_fields;

use crate::graph::*;
use crate::midi::MidiAction;
use crate::serial::SerialAction;
use crate::osc::OscAction;
use crate::http::HttpAction;
use crate::network::NetworkAction;
use crate::ob::ObManager;
use crate::audio::AudioManager;
use eframe::egui;
use std::collections::HashMap;

pub struct NodeCatalogEntry {
    pub label: &'static str,
    pub category: &'static str,
    pub factory: fn() -> NodeType,
    pub wip: bool,
    pub singleton: bool,
}

/// Category pill groups for the Add Node menu.
pub const CATEGORY_GROUPS: &[(&str, &[&str])] = &[
    ("All", &[]),
    ("Input", &["Input", "Signal"]),
    ("Math", &["Math"]),
    ("Visual", &["Image", "Video", "Shader"]),
    ("AI / ML", &["AI", "ML"]),
    ("Audio", &["Audio", "MIDI"]),
    ("I/O", &["IO", "Network", "Serial", "OSC", "Hardware", "Output", "Lighting"]),
    ("Utility", &["Utility", "Custom"]),
];

/// UTF-8-safe truncation for UI previews. `&str[..n]` panics when `n`
/// lands mid-character (em-dash, emoji, CJK, …); every node preview that
/// shows upstream / AI-generated text must route through this helper
/// instead of byte-index slicing. Returns `"prefix…"` if truncated,
/// the original string otherwise.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars { return s.to_string(); }
    let head: String = s.chars().take(max_chars).collect();
    format!("{}…", head)
}

/// Draw a small inline port using the PortKind visual system.
/// Uses `draw_shaped_port` from `app/mod.rs` for consistent rendering across all ports.
pub fn inline_port_circle(
    ui: &mut egui::Ui, node_id: NodeId, port: usize, is_input: bool,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    kind: PortKind,
) {
    let is_wired = if is_input {
        connections.iter().any(|c| c.to_node == node_id && c.to_port == port)
    } else {
        connections.iter().any(|c| c.from_node == node_id && c.from_port == port)
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click_and_drag());

    // Colors based on PortKind
    let base = kind.base_color();
    let (fill, border) = if response.hovered() || response.dragged() {
        (egui::Color32::YELLOW, egui::Color32::WHITE)
    } else if is_wired {
        (
            egui::Color32::from_rgb(base[0], base[1], base[2]),
            egui::Color32::from_rgb(
                (base[0] as u16 + 60).min(255) as u8,
                (base[1] as u16 + 60).min(255) as u8,
                (base[2] as u16 + 60).min(255) as u8,
            ),
        )
    } else {
        (
            egui::Color32::from_rgb(
                (base[0] as f32 * 0.35) as u8,
                (base[1] as f32 * 0.35) as u8,
                (base[2] as f32 * 0.35) as u8,
            ),
            egui::Color32::from_rgb(
                (base[0] as f32 * 0.6) as u8,
                (base[1] as f32 * 0.6) as u8,
                (base[2] as f32 * 0.6) as u8,
            ),
        )
    };

    // Use the shared draw_shaped_port for consistent visuals
    crate::app::draw_shaped_port(
        ui.painter(), rect.center(), 7.0, fill, border, 2.0, kind, is_wired,
    );

    port_positions.insert((node_id, port, is_input), rect.center());
    if response.drag_started() {
        if is_input {
            if let Some(existing) = connections.iter().find(|c| c.to_node == node_id && c.to_port == port) {
                *dragging_from = Some((existing.from_node, existing.from_port, true));
                pending_disconnects.push((node_id, port));
            } else {
                *dragging_from = Some((node_id, port, false));
            }
        } else {
            *dragging_from = Some((node_id, port, true));
        }
    }
    // Click-to-connect support for inline ports
    let click_wiring_id = egui::Id::new("click_wiring_active");
    let is_click_wiring = ui.ctx().data_mut(|d| d.get_temp::<bool>(click_wiring_id).unwrap_or(false));
    if response.clicked() {
        if let Some((src_node, src_port, src_is_output)) = *dragging_from {
            if is_click_wiring && node_id != src_node {
                // Complete click-wiring connection
                if src_is_output && is_input {
                    ui.ctx().data_mut(|d| d.insert_temp(
                        egui::Id::new("click_wire_complete"),
                        (src_node, src_port, node_id, port),
                    ));
                } else if !src_is_output && !is_input {
                    ui.ctx().data_mut(|d| d.insert_temp(
                        egui::Id::new("click_wire_complete"),
                        (node_id, port, src_node, src_port),
                    ));
                }
                *dragging_from = None;
                ui.ctx().data_mut(|d| d.insert_temp(click_wiring_id, false));
            }
        } else {
            // Start click-wiring from this inline port
            if is_input {
                if let Some(existing) = connections.iter().find(|c| c.to_node == node_id && c.to_port == port) {
                    *dragging_from = Some((existing.from_node, existing.from_port, true));
                    pending_disconnects.push((node_id, port));
                } else {
                    *dragging_from = Some((node_id, port, false));
                }
            } else {
                *dragging_from = Some((node_id, port, true));
            }
            ui.ctx().data_mut(|d| d.insert_temp(click_wiring_id, true));
        }
    }
}

/// Draw a labeled audio port row (input or output)
pub fn audio_port_row(
    ui: &mut egui::Ui, label: &str, node_id: NodeId, port: usize, is_input: bool,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    connections: &[Connection],
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    kind: PortKind,
) {
    ui.horizontal(|ui| {
        if is_input {
            inline_port_circle(ui, node_id, port, true, connections, port_positions, dragging_from, pending_disconnects, kind);
            ui.label(egui::RichText::new(label).small());
        } else {
            let remaining = ui.available_width() - 30.0;
            if remaining > 0.0 { ui.add_space(remaining); }
            ui.label(egui::RichText::new(label).small());
            inline_port_circle(ui, node_id, port, false, connections, port_positions, dragging_from, pending_disconnects, kind);
        }
    });
}

/// Draw a right-aligned output port row: label + value right-aligned, port flush at edge.
/// Uses fixed-width monospace for values to prevent jitter when values change.
pub fn output_port_row(
    ui: &mut egui::Ui, label: &str, value: &str, node_id: NodeId, port: usize,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    connections: &[Connection],
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    kind: PortKind,
) {
    ui.horizontal(|ui| {
        // Use right-to-left layout for clean right-alignment
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Port circle (rightmost)
            inline_port_circle(ui, node_id, port, false, connections, port_positions, dragging_from, pending_disconnects, kind);
            // Value in monospace (fixed width, no jitter)
            ui.label(egui::RichText::new(value).small().monospace());
            // Label
            ui.label(egui::RichText::new(label).small().color(egui::Color32::from_rgb(140, 140, 155)));
        });
    });
}

/// Whether a node's render surface contains a scrollable widget
/// (ScrollArea, multiline TextEdit, log panel, code editor, etc).
///
/// Used by `handle_pan_zoom` to suppress canvas scroll / pan / zoom when
/// the pointer hovers such a node — so scrolling inside a Console log or
/// Text Editor never bleeds through to move the canvas.
///
/// Nodes NOT listed here (Slider, Knob, Timer, Add, Multiply, Synth,
/// etc.) allow scroll-through: the user can freely pan the canvas with
/// the pointer hovering these nodes. That matches expectation — there's
/// nothing in a Slider node that can meaningfully consume scroll.
///
/// Kept in one place so new scrollable node types get added here when
/// their render gains a ScrollArea.
pub fn node_type_has_scroll(nt: &NodeType) -> bool {
    match nt {
        // ── Legacy enum variants with known ScrollArea / TextEdit ──
        NodeType::Console { .. }
        | NodeType::Serial { .. }
        | NodeType::MidiIn { .. }
        | NodeType::OscIn { .. }
        | NodeType::Script { .. }
        | NodeType::HttpRequest { .. }
        | NodeType::AiRequest { .. }
        | NodeType::AudioPlaylist { .. }
        | NodeType::RustPlugin { .. }
        | NodeType::McpServer
        | NodeType::HtmlViewer
        | NodeType::ObHub { .. }
        | NodeType::ClapPlugin { .. }
        | NodeType::Palette { .. }
        | NodeType::FolderBrowser { .. } => true,

        // ── Dynamic (trait-based) nodes — match by type_tag ──
        // type_tag is the stable identifier registered in NODE_REGISTRY;
        // saved projects use it for deserialization.
        NodeType::Dynamic { inner } => matches!(
            inner.node.type_tag(),
            "comment"
            | "fill"
            | "json_fields"
            | "json_extract"
            | "folder_browser"
            | "web_app"
            | "terminal"
            | "text_editor"
            | "string_format"
            | "file"
            | "html_viewer"
            | "console"
            | "mcp_server"
        ),
        _ => false,
    }
}

pub fn catalog() -> Vec<NodeCatalogEntry> {
    vec![
        // ── Input ────────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Slider", category: "Input",
            factory: || NodeType::Slider { value: 0.5, min: 0.0, max: 1.0, step: 0.01, slider_color: [80, 160, 255], label: String::new() } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Button", category: "Input",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(button_node::ButtonNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Time", category: "Input",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(time_node::TimeNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Color", category: "Input",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(color_node::ColorNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Mouse Tracker", category: "Input",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(mouse_tracker_node::MouseTrackerNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Point 2D", category: "Math",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(point_2d_node::Point2dNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Keyboard Input", category: "Input",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(key_input_node::KeyInputNode::default()) } } },

        // ── Math ─────────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Add", category: "Math", factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(add_node::AddNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Multiply", category: "Math", factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(multiply_node::MultiplyNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Math", category: "Math",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(math_formula::MathNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Gate", category: "Math",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(gate_node::GateNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Clock", category: "Input",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(clock_node::ClockNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Timer", category: "Input",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(timer_node::TimerNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Map/Range", category: "Math",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(map_range_node::MapRangeNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "String Format", category: "IO",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(string_format_node::StringFormatNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Sample & Hold", category: "Math",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(sample_hold_node::SampleHoldNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Select", category: "Math",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(select::SelectNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Route", category: "Math",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(route_node::RouteNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Switch", category: "Math",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(route_node::SwitchNode::default()) } } },

        // ── IO ───────────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "File", category: "IO",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(file_node::FileNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Folder", category: "IO",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(folder_browser_node::FolderBrowserNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Text Editor", category: "IO",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(text_editor_node::TextEditorNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Terminal", category: "IO",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(terminal_node::TerminalNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Text Ops", category: "IO",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(text_ops_node::TextOpsNode::default()) } } },

        // ── Output ───────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Scope", category: "Output",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(display_node::DisplayNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Visual Output", category: "Output",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(visual_output_node::VisualOutputNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "HTML Viewer", category: "Output",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(html_viewer_node::HtmlViewerNode) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Web App", category: "Output",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(web_app_node::WebAppNode::default()) } } },

        // ── Shader ───────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Visuals (WGSL)", category: "Shader", factory: || NodeType::WgslViewer {
            wgsl_code: String::new(),
            uniform_names: vec![], uniform_types: vec![], uniform_values: vec![], uniform_min: vec![], uniform_max: vec![],
            canvas_w: 400.0, canvas_h: 300.0, resolution: 120, expanded: false,
            image_a_mode: crate::graph::ImageInputMode::Unused,
            image_b_mode: crate::graph::ImageInputMode::Unused,
            feedback_reset_pending: false,
            last_compile_ok: false,
            pending_shader_hash: 0,
            pending_shader_since_ms: 0,
            image_a_hint_shown: false,
            image_b_hint_shown: false,
            last_auto_reset_ms: 0,
        } },

        // ── Image ────────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Image", category: "Image",
            factory: || NodeType::ImageNode { path: String::new(), save_path: String::new(), image_data: None, preview_size: 150.0, last_save_hash: 0, cached_file: String::new() } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Image Effects", category: "Image",
            factory: || NodeType::ImageEffects { brightness: 1.0, contrast: 1.0, saturation: 1.0, hue: 0.0, exposure: 0.0, gamma: 1.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Blend", category: "Image",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(blend::BlendNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Crop", category: "Image",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(crop_node::CropNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Color Curves", category: "Image",
            factory: || NodeType::ColorCurves { master: vec![[0.0, 0.0], [1.0, 1.0]], red: vec![[0.0, 0.0], [1.0, 1.0]], green: vec![[0.0, 0.0], [1.0, 1.0]], blue: vec![[0.0, 0.0], [1.0, 1.0]], active_channel: 0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Transform", category: "Image",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(transform_node::TransformNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Image Style", category: "Image",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(image_style_node::ImageStyleNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Kaleidoscope", category: "Image",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(kaleidoscope_node::KaleidoscopeNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Color Channel", category: "Image",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(color_channel_node::ColorChannelNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Fill", category: "Image",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(fill_node::FillNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Image Scanner", category: "Image",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(image_scanner_node::ImageScannerNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Frame Recorder", category: "Image",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(frame_recorder_node::FrameRecorderNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Visual Presets (WGSL)", category: "Shader",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(wgsl_presets::WgslPresetsNode::default()) } } },

        // ── Signal ───────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Curve", category: "Signal",
            factory: || NodeType::Curve { points: vec![[0.0, 0.0], [1.0, 1.0]], mode: 0, speed: 1.0, looping: false, manual_gate: false, sustain_index: None, phase: 0.0, playing: false, last_trigger: 0.0, last_phase: -1.0, last_step_index: -1, sustain_held: false } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Draw", category: "Signal",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(draw_node::DrawNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Noise", category: "Signal",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(noise_node::NoiseNode::default()) } } },

        // ── Video ────────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Video Player", category: "Video",
            factory: || NodeType::VideoPlayer { path: String::new(), playing: false, looping: false, res_w: 640, res_h: 480, current_frame: None, duration: 0.0, speed: 1.0, status: String::new() } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Camera", category: "Video",
            factory: || NodeType::Camera { device_index: 0, res_w: 640, res_h: 480, active: false, current_frame: None, status: String::new() } },
        // New trait-based video I/O nodes. Ship alongside the legacy Camera
        // / VisualOutput during Phase 0; see docs/video_io_spec.md.
        NodeCatalogEntry { wip: false, singleton: false, label: "Video In", category: "Video",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(video_in_node::VideoInNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Video Out", category: "Video",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(video_out_node::VideoOutNode::default()) } } },

        // ── Audio ────────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Synth", category: "Audio",
            factory: || NodeType::Synth { waveform: crate::audio::Waveform::Sine, frequency: 440.0, amplitude: 0.5, active: true, fm_depth: 0.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Delay", category: "Audio",
            factory: || NodeType::AudioDelay { time_ms: 250.0, feedback: 0.5 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Distortion", category: "Audio",
            factory: || NodeType::AudioDistortion { drive: 4.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Pitch Shift", category: "Audio",
            factory: || NodeType::AudioPitchShift { semitones: 0.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Reverb", category: "Audio",
            factory: || NodeType::AudioReverb { room_size: 0.5, damping: 0.5, mix: 0.3 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Low Pass", category: "Audio",
            factory: || NodeType::AudioLowPass { cutoff: 1000.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "High Pass", category: "Audio",
            factory: || NodeType::AudioHighPass { cutoff: 200.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Noise Removal", category: "Audio",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(noise_removal_node::NoiseRemovalNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Voice Effects", category: "Audio",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(voice_effects_node::VoiceEffectsNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Gain", category: "Audio",
            factory: || NodeType::AudioGain { level: 1.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "EQ", category: "Audio",
            factory: || NodeType::AudioEq { points: vec![[0.0, 0.5], [0.25, 0.5], [0.5, 0.5], [0.75, 0.5], [1.0, 0.5]] } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Speaker", category: "Audio",
            factory: || NodeType::Speaker { active: true, volume: 0.8, pan: 0.0, channel_offset: 0, device: String::new() } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Mixer", category: "Audio",
            factory: || NodeType::AudioMixer { channel_count: 2, gains: vec![0.8, 0.8] } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Audio Player", category: "Audio",
            factory: || NodeType::AudioPlayer { file_path: String::new(), volume: 1.0, looping: false, duration_secs: 0.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Playlist", category: "Audio",
            factory: || NodeType::AudioPlaylist { tracks: Vec::new(), current_index: 0, volume: 1.0, loop_playlist: false, duration_secs: 0.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Microphone", category: "Audio",
            factory: || NodeType::AudioInput { selected_device: String::new(), gain: 1.0, active: false, agc_enabled: false } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Audio Sampler", category: "Audio",
            factory: || NodeType::AudioSampler { record_duration: 5.0, trim_start: 0.0, trim_end: 0.0, volume: 1.0, looping: false, reverse: false, play_mode: 0, range_as_duration: false, speed: 1.0, seek: 0.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "CLAP Plugin", category: "Audio",
            factory: || NodeType::ClapPlugin { plugin_path: String::new(), plugin_name: String::new(), param_names: Vec::new(), param_ranges: Vec::new(), param_flags: Vec::new(), param_values: Vec::new(), param_labels: Vec::new(), is_instrument: false } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Audio Analyzer", category: "Audio",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(audio_analyzer_node::AudioAnalyzerNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Spectrum Analyzer", category: "Audio",
            factory: || NodeType::SpectrumAnalyzer },
        NodeCatalogEntry { wip: false, singleton: false, label: "Music Visualizer", category: "Audio",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(music_visualizer_node::MusicVisualizerNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Spectral Synth", category: "Audio",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(spectral_synth_node::SpectralSynthNode::default()) } } },
        NodeCatalogEntry { wip: true, singleton: false, label: "Neural Audio", category: "Audio",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(neural_audio_node::NeuralAudioNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: true, label: "Audio Device", category: "Audio",
            factory: || NodeType::AudioDevice { selected_output: String::new(), selected_input: String::new(), master_volume: 0.8, enabled: false } },
        NodeCatalogEntry { wip: true, singleton: false, label: "TTS", category: "Audio",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(tts_node::TtsNode::default()) } } },

        // ── MIDI ─────────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "MIDI Out", category: "MIDI",
            factory: || NodeType::MidiOut { port_name: String::new(), mode: MidiMode::Note, channel: 0, manual_d1: 0, manual_d2: 64 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "MIDI In", category: "MIDI",
            factory: || NodeType::MidiIn { port_name: String::new(), channel: 0, note: 0, velocity: 0, log: Vec::new() } },
        NodeCatalogEntry { wip: false, singleton: false, label: "MIDI Note", category: "MIDI",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(midi_note_node::MidiNoteNode::default()) } } },

        // ── Serial ───────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Serial", category: "Serial",
            factory: || NodeType::Serial { port_name: String::new(), baud_rate: 115200, log: Vec::new(), last_line: String::new(), send_buf: String::new() } },

        // ── OSC ──────────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "OSC Out", category: "OSC",
            factory: || NodeType::OscOut { host: "127.0.0.1".to_string(), port: 9000, address: "/patchwork".to_string(), arg_count: 1 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "OSC In", category: "OSC",
            factory: || NodeType::OscIn { port: 8000, address_filter: String::new(), arg_count: 1, last_args: vec![0.0], last_args_text: Vec::new(), log: Vec::new(), listening: false, discovered: Vec::new() } },

        // ── Lighting (DMX / Art-Net) ─────────────────────────
        // Marked WIP until tested against real hardware (Enttec USB Pro,
        // Open DMX, and an Art-Net receiver like QLC+ / Resolume Wire).
        NodeCatalogEntry { wip: true, singleton: false, label: "DMX Out", category: "Lighting",
            factory: || NodeType::DmxOut {
                port_name: String::new(),
                adapter: crate::dmx::DmxAdapter::UsbPro,
                frame_rate_hz: 30, channel_count: 8, start_at: 1, active: false,
            } },
        NodeCatalogEntry { wip: true, singleton: false, label: "DMX In", category: "Lighting",
            factory: || NodeType::DmxIn {
                port_name: String::new(),
                channel_count: 8, start_at: 1,
                last_values: vec![0; 8], listening: false,
            } },
        NodeCatalogEntry { wip: true, singleton: false, label: "Art-Net Out", category: "Lighting",
            factory: || NodeType::ArtNetOut {
                dest_host: "2.255.255.255".into(), dest_port: 6454, universe: 0,
                channel_count: 8, start_at: 1, sequence_enabled: true, active: false,
            } },
        NodeCatalogEntry { wip: true, singleton: false, label: "Art-Net In", category: "Lighting",
            factory: || NodeType::ArtNetIn {
                listen_port: 6454, universe: 0, universe_filter: true,
                channel_count: 8, start_at: 1,
                last_values: vec![0; 8], listening: false,
            } },

        // ── Network / AI ─────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "HTTP Request", category: "Network",
            factory: || NodeType::HttpRequest {
                url: String::new(), method: "POST".into(), headers: String::new(),
                response: String::new(), status: String::new(), auto_send: false, last_hash: 0,
            } },
        // ── New AI stack (AiConfig + TextGen / ImageGen / CodeGen) ─────
        NodeCatalogEntry { wip: false, singleton: false, label: "AI Config", category: "AI",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(ai_config_node::AiConfigNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Text Gen", category: "AI",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(text_gen_node::TextGenNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Image Gen", category: "AI",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(image_gen_node::ImageGenNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Code Gen", category: "AI",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(code_gen_node::CodeGenNode::default()) } } },
        // Legacy monolithic AI node — kept so old projects load, but marked WIP
        // so it's hidden from the default catalog palette.
        NodeCatalogEntry { wip: true, singleton: false, label: "AI Request (legacy)", category: "Network",
            factory: || NodeType::AiRequest {
                provider: "anthropic".into(), model: "claude-sonnet-4-20250514".into(),
                system_prompt: String::new(), user_prompt: String::new(),
                response: String::new(), status: String::new(),
                max_tokens: 1024, temperature: 0.7, api_key: String::new(),
                response_type: 0, mode: 0, last_trigger: 0.0,
                api_key_name: String::new(), custom_url: String::new(),
            } },
        NodeCatalogEntry { wip: false, singleton: false, label: "JSON Extract", category: "Network",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(json_extract_node::JsonExtractNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Net Send", category: "Network",
            factory: || NodeType::NetworkSend {
                schema: vec![NetPortSchema { name: "value".into(), kind: NetPortKind::Float }],
                identity_mode: 0, persistent_secret: None, label: String::new(),
                last_link: None, last_peer_count: 0, initialized: false,
            } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Net Receive", category: "Network",
            factory: || NodeType::NetworkReceive {
                link: String::new(), imported_schema: Vec::new(),
                last_values: Vec::new(), last_values_text: Vec::new(),
                last_sender: String::new(), status: "Paste link to connect".into(),
                connected: false,
            } },

        NodeCatalogEntry { wip: false, singleton: false, label: "WebSocket", category: "Network",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(websocket_node::WebSocketNode::default()) } } },

        // ── Hardware ─────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "OB Hub", category: "Hardware",
            factory: || NodeType::ObHub { port_name: String::new(), selected_port: String::new(), detected_devices: Vec::new() } },
        NodeCatalogEntry { wip: false, singleton: false, label: "OB Joystick", category: "Hardware",
            factory: || NodeType::ObJoystick { device_id: 1, hub_node_id: 0, label_color: [255, 255, 255] } },
        NodeCatalogEntry { wip: false, singleton: false, label: "OB Wheel", category: "Hardware",
            factory: || NodeType::ObEncoder { device_id: 1, hub_node_id: 0, label_color: [255, 255, 255] } },
        NodeCatalogEntry { wip: false, singleton: false, label: "OB Move", category: "Hardware",
            factory: || NodeType::ObMove { device_id: 1, hub_node_id: 0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "OB Bend", category: "Hardware",
            factory: || NodeType::ObBend { device_id: 1, hub_node_id: 0, label_color: [255, 255, 255] } },
        NodeCatalogEntry { wip: false, singleton: false, label: "OB Pressure", category: "Hardware",
            factory: || NodeType::ObPressure { device_id: 1, hub_node_id: 0, label_color: [255, 255, 255] } },
        NodeCatalogEntry { wip: false, singleton: false, label: "OB Distance", category: "Hardware",
            factory: || NodeType::ObDistance { device_id: 1, hub_node_id: 0, label_color: [255, 255, 255] } },
        NodeCatalogEntry { wip: false, singleton: false, label: "OB Knob", category: "Hardware",
            factory: || NodeType::ObKnob { device_id: 1, hub_node_id: 0, label_color: [255, 255, 255] } },
        NodeCatalogEntry { wip: false, singleton: false, label: "OB Orb", category: "Hardware",
            factory: || NodeType::ObOrb { device_id: 1, hub_node_id: 0, mode: 0, color: [255, 255, 255], param1: 0.0, param2: 0.0, speed: 1.0, brightness: 1.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Printer", category: "Hardware",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(printer_node::PrinterNode::default()) } } },
        // Keypoint Extract removed from catalog — superseded by JSON Fields.
        // Registration kept in node_trait.rs for backward compat with saved projects.
        NodeCatalogEntry { wip: false, singleton: false, label: "Hand Tracking", category: "ML",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(hand_detection::HandDetectionNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Face Tracking", category: "ML",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(face_detection::FaceDetectionNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Body Tracking", category: "ML",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(pose_detection::PoseDetectionNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Match", category: "ML",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(crate::ml::match_node::MatchNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Regress", category: "ML",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(crate::ml::regress_node::RegressNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Unpack (JSON)", category: "ML",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(json_fields::JsonFieldsNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Pack (JSON)", category: "ML",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(pack_node::PackNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Vision Model", category: "ML",
            factory: || NodeType::MlModel { model_path: String::new(), labels_path: String::new(), confidence: 0.05, preset: MlPreset::default(), result_text: String::new(), result_json: String::new(), annotated_frame: None, status: String::new(), last_input_hash: 0, interval_secs: 0.5, last_inference_secs: 0.0 } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Smoother", category: "Math",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(smoother::SmootherNode::default()) } } },

        // ── Custom ───────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Script", category: "Custom",
            factory: || NodeType::Script { name: "Custom Script".to_string(), input_names: vec![], output_names: vec![], code: String::new(), last_values: vec![], error: String::new(), continuous: true, trigger: false } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Rust Plugin", category: "Custom",
            factory: || NodeType::RustPlugin { input_names: vec!["in0".into()], output_names: vec!["out0".into()], code: String::new(), last_values: vec![0.0], error: String::new() } },

        // ── Utility ──────────────────────────────────────────
        NodeCatalogEntry { wip: false, singleton: false, label: "Comment", category: "Utility",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(comment_node::CommentNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: true, label: "Theme", category: "Utility",
            factory: || {
                let accent = crate::nodes::theme::random_accent();
                NodeType::Theme {
                    dark_mode: true, accent, font_size: 14.0,
                    bg_color: [20, 20, 20], text_color: [220, 220, 220],
                    window_bg: [24, 24, 24], window_alpha: 240,
                    grid_color: [28, 28, 28], grid_style: 2, wire_style: 0,
                    wiggle_gravity: 0.0, wiggle_range: 1.0, wiggle_speed: 1.0, wiggle_signal: 0.0,
                    rounding: 16.0, spacing: 4.0, use_hsl: false,
                    wire_thickness: 6.0, background_path: String::new(),
                }
            } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Console", category: "Utility",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(console_node::ConsoleNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Monitor", category: "Utility",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(monitor_node::MonitorNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "System Profiler", category: "Utility",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(monitor_node::MonitorNode::default()) } } },

        // ── System (hidden from palette, visible in full catalog) ──
        NodeCatalogEntry { wip: false, singleton: false, label: "File Menu", category: "System",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(file_menu_node::FileMenuNode) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Zoom Control", category: "System",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(zoom_control_node::ZoomControlNode::default()) } } },
        NodeCatalogEntry { wip: false, singleton: false, label: "Node Palette", category: "System",
            factory: || NodeType::Palette { search: String::new() } },
        NodeCatalogEntry { wip: false, singleton: false, label: "MCP Server", category: "System",
            factory: || NodeType::McpServer },
        NodeCatalogEntry { wip: false, singleton: false, label: "Settings", category: "System",
            factory: || NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(settings_node::SettingsNode::default()) } } },
    ]
}

/// Dispatch content rendering.
pub fn render_content(
    ui: &mut egui::Ui,
    node_type: &mut NodeType,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    midi_out_ports: &[String],
    midi_in_ports: &[String],
    midi_connected_out: bool,
    midi_connected_in: bool,
    midi_actions: &mut Vec<MidiAction>,
    serial_ports: &[String],
    serial_connected: bool,
    serial_actions: &mut Vec<SerialAction>,
    _monitor_state: &monitor::MonitorState,
    osc_listening: bool,
    osc_actions: &mut Vec<OscAction>,
    network_actions: &mut Vec<NetworkAction>,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    http_actions: &mut Vec<HttpAction>,
    http_pending: bool,
    _api_keys: &HashMap<String, String>,
    wgpu_render_state: &Option<eframe::egui_wgpu::RenderState>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    ob_manager: &mut ObManager,
    audio_manager: &mut AudioManager,
    mcp_log: &crate::mcp::McpLog,
    mcp_active: bool,
    dmx_actions: &mut Vec<crate::dmx::DmxAction>,
    artnet_actions: &mut Vec<crate::artnet::ArtNetAction>,
    dmx_output_open: bool,
    _dmx_listening: bool,
    _artnet_listening: bool,
) {
    // ── Property-edit undo detection ────────────────────────────────────
    // Snapshot egui interaction state BEFORE rendering widgets.
    // After the match, compare to detect if a new drag or focus began — which means the
    // user started editing a property.  The caller (app/mod.rs) reads this signal and
    // pushes an undo snapshot (coalesced per gesture via property_undo_pushed).
    let focused_before = ui.ctx().memory(|mem| mem.focused());
    let dragged_before = ui.ctx().dragged_id().is_some();

    match node_type {
        NodeType::Slider { value, min, max, step, slider_color, label } => slider::render(ui, value, min, max, step, slider_color, label, node_id, values, connections, port_positions, dragging_from, pending_disconnects),
        // Display migrated to trait-based node
        // Add, Multiply migrated to trait-based nodes
        // Math migrated to trait-based node
        // File migrated to trait-based node
        // TextEditor migrated to trait-based node
        NodeType::WgslViewer {
            wgsl_code, uniform_names, uniform_types, uniform_values, canvas_w, canvas_h,
            image_a_mode, image_b_mode, feedback_reset_pending,
            last_compile_ok, pending_shader_hash, pending_shader_since_ms, last_auto_reset_ms,
            image_a_hint_shown, image_b_hint_shown, ..
        } =>
            wgsl_viewer::render(
                ui, wgsl_code, uniform_names, uniform_types, uniform_values, canvas_w, canvas_h,
                node_id, values, connections, wgpu_render_state, pending_disconnects,
                port_positions, dragging_from,
                image_a_mode, image_b_mode, feedback_reset_pending,
                last_compile_ok, pending_shader_hash, pending_shader_since_ms, last_auto_reset_ms,
                image_a_hint_shown, image_b_hint_shown,
            ),
        // Time migrated to trait-based node
        // Color migrated to trait-based node
        // MouseTracker migrated to trait-based node
        NodeType::MidiOut { port_name, mode, channel, manual_d1, manual_d2 } =>
            midi_out::render(ui, port_name, mode, channel, node_id, values, connections, midi_out_ports, midi_connected_out, midi_actions, port_positions, dragging_from, pending_disconnects, manual_d1, manual_d2),
        NodeType::MidiIn { port_name, channel, note, velocity, log } =>
            midi_in::render(ui, port_name, channel, note, velocity, log, node_id, midi_in_ports, midi_connected_in, midi_actions),
        NodeType::Serial { port_name, baud_rate, log, last_line, send_buf } =>
            serial::render(ui, port_name, baud_rate, log, last_line, send_buf, node_id, values, connections, serial_ports, serial_connected, serial_actions),
        NodeType::Theme { dark_mode, accent, font_size, bg_color, text_color, window_bg, window_alpha, grid_color, grid_style, wire_style, wiggle_gravity, wiggle_range, wiggle_speed, wiggle_signal, rounding, spacing, use_hsl, wire_thickness, background_path } =>
            theme::render(ui, dark_mode, accent, font_size, bg_color, text_color, window_bg, window_alpha, grid_color, grid_style, wire_style, wiggle_gravity, wiggle_range, wiggle_speed, wiggle_signal, rounding, spacing, use_hsl, wire_thickness, background_path, node_id, values, connections, port_positions, dragging_from, pending_disconnects),
        // Comment migrated to trait-based node
        NodeType::Console { .. } => {} // migrated to trait — legacy fallback
        // Monitor migrated to trait-based node
        NodeType::OscOut { host, port, address, arg_count } =>
            osc_out::render(ui, host, port, address, arg_count, node_id, values, osc_actions),
        NodeType::OscIn { port, address_filter, arg_count, last_args, last_args_text, log, listening, discovered, .. } =>
            osc_in::render(ui, port, address_filter, arg_count, last_args, last_args_text, log, listening, discovered, node_id, osc_listening, osc_actions),
        NodeType::DmxOut { port_name, adapter, frame_rate_hz, channel_count, start_at, active } =>
            dmx_out::render(ui, port_name, adapter, frame_rate_hz, channel_count, start_at, active,
                node_id, values, connections, serial_ports, port_positions, dragging_from,
                pending_disconnects, dmx_output_open, dmx_actions),
        NodeType::DmxIn { port_name, channel_count, start_at, last_values, listening } =>
            dmx_in::render(ui, port_name, channel_count, start_at, last_values, listening,
                node_id, connections, serial_ports, port_positions, dragging_from,
                pending_disconnects, dmx_actions),
        NodeType::ArtNetOut { dest_host, dest_port, universe, channel_count, start_at, sequence_enabled, active } =>
            artnet_out::render(ui, dest_host, dest_port, universe, channel_count, start_at, sequence_enabled, active,
                node_id, values, connections, port_positions, dragging_from,
                pending_disconnects, artnet_actions),
        NodeType::ArtNetIn { listen_port, universe, universe_filter, channel_count, start_at, last_values, listening } =>
            artnet_in::render(ui, listen_port, universe, universe_filter, channel_count, start_at,
                last_values, listening, node_id, connections, port_positions, dragging_from,
                pending_disconnects, artnet_actions),
        // KeyInput migrated to trait-based node
        NodeType::Script { name, input_names, output_names, code, last_values, error, continuous, trigger } =>
            script::render(ui, name, input_names, output_names, code, last_values, error, continuous, trigger, values, node_id, pending_disconnects),
        NodeType::Palette { search } =>
            palette::render(ui, search, node_id),
        NodeType::HttpRequest { url, method, headers, response, status, auto_send, last_hash } =>
            http_request::render(ui, url, method, headers, response, status, auto_send, last_hash, node_id, values, connections, http_pending, http_actions, port_positions, dragging_from, pending_disconnects),
        NodeType::AiRequest { provider, model, system_prompt, user_prompt, response, status, max_tokens, temperature, api_key, response_type, mode, last_trigger, .. } =>
            ai_request::render(ui, provider, model, system_prompt, user_prompt, response, status, max_tokens, temperature, api_key, response_type, mode, last_trigger, node_id, values, connections, http_pending, http_actions, port_positions, dragging_from, pending_disconnects),
        // JsonExtract migrated to trait-based node
        NodeType::FileMenu => {} // migrated to trait — legacy fallback
        NodeType::ZoomControl { .. } => {} // migrated to trait — legacy fallback
        NodeType::ObHub { .. } => ob_hub::render(ui, node_id, node_type, ob_manager),
        NodeType::ObJoystick { .. } => ob_joystick::render(ui, node_id, node_type, values, connections, ob_manager),
        NodeType::ObEncoder { .. } => ob_encoder::render(ui, node_id, node_type, values, connections, ob_manager),
        NodeType::ObMove { .. } => ob_move::render(ui, node_id, node_type, values, connections, ob_manager),
        NodeType::ObBend { .. } => ob_bend::render(ui, node_id, node_type, values, connections, ob_manager),
        NodeType::ObPressure { .. } => ob_pressure::render(ui, node_id, node_type, values, connections, ob_manager),
        NodeType::ObDistance { .. } => ob_distance::render(ui, node_id, node_type, values, connections, ob_manager),
        NodeType::ObKnob { .. } => ob_knob::render(ui, node_id, node_type, values, connections, ob_manager),
        NodeType::ObOrb { .. } => ob_orb::render(ui, node_id, node_type, values, connections, ob_manager, port_positions, dragging_from, pending_disconnects),
        NodeType::Synth { .. } => synth::render(ui, node_id, node_type, values, connections, audio_manager, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioPlayer { .. } => audio_player::render(ui, node_id, node_type, values, connections, audio_manager, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioPlaylist { .. } => audio_playlist::render(ui, node_id, node_type, values, connections, audio_manager, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioInput { .. } => audio_input::render(ui, node_id, node_type, values, connections, audio_manager, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioAnalyzer => {} // migrated to trait — legacy fallback
        NodeType::SpectrumAnalyzer => spectrum_analyzer::render(ui, node_id, values, audio_manager, port_positions, dragging_from, connections, pending_disconnects),
        NodeType::AudioDevice { .. } => audio_device::render(ui, node_id, node_type, audio_manager),
        NodeType::AudioDelay { time_ms, feedback } => audio_delay::render(ui, time_ms, feedback, node_id, values, connections, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioDistortion { drive } => audio_distortion::render(ui, drive, node_id, values, connections, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioPitchShift { semitones } => audio_pitchshift::render(ui, semitones, node_id, values, connections, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioReverb { room_size, damping, mix } => audio_reverb::render(ui, node_id, room_size, damping, mix, values, connections, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioLowPass { cutoff } => audio_filter::render_lpf(ui, cutoff, node_id, values, connections, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioHighPass { cutoff } => audio_filter::render_hpf(ui, cutoff, node_id, values, connections, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioGain { level } => audio_gain::render(ui, level, node_id, values, connections, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioEq { points } => audio_eq::render(ui, node_id, points, port_positions, dragging_from, connections, pending_disconnects),
        NodeType::Speaker { active, volume, pan, channel_offset, device } => speaker::render(ui, active, volume, pan, channel_offset, device, node_id, values, connections, audio_manager, port_positions, dragging_from, pending_disconnects),
        NodeType::AudioMixer { channel_count, gains } =>
            audio_mixer::render(ui, channel_count, gains, node_id, values, connections, port_positions, dragging_from, pending_disconnects, audio_manager),
        NodeType::AudioSampler { .. } => audio_sampler::render(ui, node_id, node_type, values, connections, audio_manager, port_positions, dragging_from, pending_disconnects),
        NodeType::ClapPlugin { .. } => clap_plugin::render(ui, node_id, node_type, values, connections, audio_manager, port_positions, dragging_from, pending_disconnects),
        NodeType::RustPlugin { .. } => rust_plugin::render(ui, node_id, node_type, values, connections),
        NodeType::HtmlViewer => {} // migrated to trait — legacy fallback
        NodeType::McpServer => mcp_server::render(ui, mcp_log, mcp_active),
        NodeType::ImageNode { .. } => image_node::render(ui, node_id, node_type, values, connections),
        // Crop migrated to trait-based node
        NodeType::ImageEffects { .. } => image_effects::render(ui, node_id, node_type, values, connections, port_positions, dragging_from, pending_disconnects),
        // Blend migrated to trait-based node
        NodeType::Curve { .. } => curve::render(ui, node_id, node_type, values, connections, port_positions, dragging_from, pending_disconnects),
        NodeType::Draw { .. } => {} // migrated to trait — legacy fallback
        // Noise migrated to trait-based node
        NodeType::ColorCurves { .. } => color_curves::render(ui, node_id, node_type, values, connections),
        NodeType::VideoPlayer { .. } => video_player::render_video(ui, node_id, node_type, values, connections),
        NodeType::Camera { .. } => video_player::render_camera(ui, node_id, node_type, values, connections),
        NodeType::MlModel { .. } => ml_model::render(ui, node_id, node_type, values, connections),
        // Gate migrated to trait-based node
        NodeType::Timer { interval, elapsed, running, pulse_width, ref_time, paused_elapsed, time_initialized, phase_offset: _, last_sync_phase: _ } =>
            timer::render(ui, interval, elapsed, running, pulse_width, ref_time, paused_elapsed, time_initialized, node_id, values, connections, port_positions, dragging_from, pending_disconnects), // legacy — new timers use Dynamic
        // MapRange, StringFormat, SampleHold, VisualOutput migrated to trait-based nodes
        // Select migrated to trait-based node
        NodeType::FolderBrowser { .. } => {} // migrated to trait — legacy fallback
        NodeType::SampleHold { .. } => {} // migrated to trait — legacy enum fallback
        NodeType::NetworkSend { schema, identity_mode, persistent_secret, label, last_link, last_peer_count, initialized, .. } =>
            network_send::render(ui, schema, identity_mode, persistent_secret, label, last_link, last_peer_count, initialized, node_id, values, connections, network_actions),
        NodeType::NetworkReceive { link, imported_schema, last_values, last_values_text, last_sender, status, connected, .. } =>
            network_receive::render(ui, link, imported_schema, last_values, last_values_text, last_sender, status, connected, node_id, network_actions),
        NodeType::Dynamic { inner } => {
            let mut ctx = crate::node_trait::RenderContext {
                node_id, values, connections,
                port_positions, dragging_from, pending_disconnects,
                wgpu_render_state: wgpu_render_state.as_ref(),
                http_actions, http_pending,
            };
            inner.node.render_with_context(ui, &mut ctx);
        }
    }

    // ── Post-render: detect if a NEW widget interaction started ──────────
    let focused_after = ui.ctx().memory(|mem| mem.focused());
    let dragged_after = ui.ctx().dragged_id().is_some();
    let new_interaction =
        (!dragged_before && dragged_after) ||
        (focused_before != focused_after && focused_after.is_some());
    if new_interaction {
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("_patchwork_prop_edit_signal"), true));
    }
}

/// Returns node types to create from Palette clicks (checked after render_content)
pub fn palette_actions(ui: &egui::Ui) -> Vec<NodeType> {
    ui.memory_mut(|mem| {
        let v: Vec<NodeType> = mem.data.get_temp(egui::Id::new("palette_spawn")).unwrap_or_default();
        if !v.is_empty() {
            mem.data.insert_temp::<Vec<NodeType>>(egui::Id::new("palette_spawn"), vec![]);
        }
        v
    })
}
