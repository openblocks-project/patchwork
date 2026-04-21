//! NodeBehavior trait — the interface that all node types implement.
//!
//! Phase 1: Trait defined, implemented on the NodeType enum.
//! Phase 2: Individual structs implement the trait (starting with Add).
//! Phase 3: SDK crate split.
//! Phase 4: Dynamic plugin loading.

#![allow(dead_code)]
use crate::graph::{PortDef, PortValue};
use std::collections::HashMap;
use serde_json::Value;

/// Core trait for node metadata, port definitions, and evaluation.
///
/// Every node type implements this. Built-in nodes live in the `NodeType`
/// enum (legacy) or as standalone structs (migrated). External plugins
/// implement this trait in their own crate.
///
/// The `Any` supertrait bound enables callers with `&dyn NodeBehavior` /
/// `Box<dyn NodeBehavior>` to downcast to a concrete node struct via trait
/// upcasting (stable since Rust 1.86). Used by the image-eval loop in
/// `app/mod.rs` to call type-specific GPU fast paths without a JSON
/// round-trip through `save_state()`/`from_value`.
pub trait NodeBehavior: std::any::Any + Send + Sync {
    /// Display name shown in the title bar and palette.
    fn title(&self) -> &str;

    /// Input port definitions.
    fn inputs(&self) -> Vec<PortDef>;

    /// Output port definitions.
    fn outputs(&self) -> Vec<PortDef>;

    /// RGB color hint for the node's title bar.
    fn color_hint(&self) -> [u8; 3];

    /// Whether ports render inline (inside the node body).
    fn inline_ports(&self) -> bool { false }

    /// Whether the node uses custom rendering (skips standard window).
    fn custom_render(&self) -> bool { false }

    /// Render custom background for the node window. Called with the full
    /// node rect BEFORE content rendering. Use the painter to draw anything:
    /// solid fill, gradient, image texture, vector shapes, etc.
    /// Return Some(Frame) to also set the egui window frame (margins, rounding).
    /// Return None to use the default window appearance.
    fn render_background(&self, _painter: &eframe::egui::Painter, _rect: eframe::egui::Rect) -> Option<eframe::egui::Frame> { None }

    /// Minimum width override for the node window. Return None for default.
    fn min_width(&self) -> Option<f32> { None }

    /// Whether to hide the title bar.
    fn no_title(&self) -> bool { false }

    /// Evaluate this node: given input values, produce output values.
    /// Returns a vec of (port_index, value) pairs.
    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> { let _ = inputs; vec![] }

    /// Stable type identifier for serialization (e.g. "add", "synth").
    fn type_tag(&self) -> &str;

    /// Serialize node state to JSON (for project save).
    fn save_state(&self) -> Value { Value::Object(serde_json::Map::new()) }

    /// Restore node state from JSON (for project load).
    fn load_state(&mut self, _state: &Value) {}

    /// Whether this node needs CPU pixel bytes on the given image input
    /// port. Default `true` is the safe direction: any new node type
    /// forces upstream readback. GPU-only consumers (image_effects, blend,
    /// color_curves, image_style, visual_output, image_node display path)
    /// override to `false` to opt into the GPU-resident handoff path.
    fn needs_cpu_image_input(&self, _port: usize) -> bool { true }

    /// For Blend nodes: returns (mode, mix) so the GPU image eval loop in
    /// app/mod.rs can dispatch without downcasting. Default None (all non-blend nodes).
    fn blend_params(&self) -> Option<(u8, f32)> { None }

    /// Render the node's UI content.
    /// Simple nodes can use this (no graph context needed).
    fn render_ui(&mut self, _ui: &mut eframe::egui::Ui) {}

    /// Render with full context — port values, connections, port positions.
    /// Override this instead of render_ui if you need inline ports or to read values.
    /// Default implementation calls render_ui().
    fn render_with_context(&mut self, ui: &mut eframe::egui::Ui, ctx: &mut RenderContext) {
        let _ = ctx;
        self.render_ui(ui);
    }

    /// Audio params to sync to the audio engine each frame.
    /// Override this for trait-based nodes that drive audio processors.
    /// Returns an empty slice by default (no audio params).
    fn audio_params(&self) -> &[f32] { &[] }

    /// GPU-aware evaluate. Called by the image evaluation loop in
    /// `app/mod.rs` for Dynamic nodes whose inputs include Image or
    /// GpuImage values. The `EvalCtx` carries GPU resources so nodes
    /// can run their own GPU pipelines and emit `PortValue::GpuImage`
    /// results without per-type dispatch in the caller.
    ///
    /// Default delegates to `evaluate(inputs)` — all existing nodes
    /// work unchanged. Image-producing nodes override this to access
    /// `ctx.gpu_tex_cache`, `ctx.render_state`, etc.
    ///
    /// Scalar-only call sites (Graph::evaluate, Dynamic re-eval after
    /// injection) keep calling `evaluate()` directly since they have
    /// no GPU resources to pass.
    fn evaluate_with_ctx(
        &mut self,
        inputs: &[PortValue],
        ctx: &mut EvalCtx<'_>,
    ) -> Vec<(usize, PortValue)> {
        let _ = ctx;
        self.evaluate(inputs)
    }
}

/// Context passed to `evaluate_with_ctx` — gives image-producing nodes
/// access to GPU resources so they can handle their own processing instead
/// of relying on per-type dispatch in `app/mod.rs`.
///
/// All fields are populated by the image evaluation loop in `app/mod.rs`.
/// `render_state` is `Option` so headless / test contexts can pass `None`.
pub struct EvalCtx<'a> {
    /// Identity of the node being evaluated. Needed for GPU texture cache
    /// lookups (keyed by NodeId) and `GpuImageHandle` emission.
    pub node_id: crate::graph::NodeId,

    /// wgpu device + queue + renderer. `None` in headless / no-GPU contexts.
    pub render_state: Option<&'a eframe::egui_wgpu::RenderState>,

    /// Mutable access to the shared GPU texture cache. All GPU-path free
    /// functions (`process_gpu_cached` in blend, image_effects, color_curves,
    /// image_style) take `&mut GpuTextureCache`.
    pub gpu_tex_cache: &'a mut crate::gpu_image::GpuTextureCache,

    /// Per-input: which `(NodeId, port)` produced each input value. GPU nodes
    /// use this to look up upstream textures in `gpu_tex_cache` and skip
    /// redundant CPU→GPU uploads. Length matches `inputs.len()`.
    pub input_sources: Vec<Option<(crate::graph::NodeId, usize)>>,

    /// Whether at least one downstream consumer of this node's output
    /// requires CPU pixel bytes. `false` → skip GPU→CPU readback (emit
    /// `GpuImage` handle instead). Pre-computed from the per-frame
    /// `cpu_consumer_producers` set.
    pub needs_readback: bool,
}

/// Context passed to render_with_context — gives nodes access to
/// port values, connections, port position tracking, and the wgpu render state.
pub struct RenderContext<'a> {
    pub node_id: crate::graph::NodeId,
    pub values: &'a std::collections::HashMap<(crate::graph::NodeId, usize), crate::graph::PortValue>,
    pub connections: &'a [crate::graph::Connection],
    pub port_positions: &'a mut std::collections::HashMap<(crate::graph::NodeId, usize, bool), eframe::egui::Pos2>,
    pub dragging_from: &'a mut Option<(crate::graph::NodeId, usize, bool)>,
    pub pending_disconnects: &'a mut Vec<(crate::graph::NodeId, usize)>,
    /// Available when running on a wgpu backend. Nodes that render GPU previews
    /// (e.g. Blend) can use this to create textures / callbacks.
    pub wgpu_render_state: Option<&'a eframe::egui_wgpu::RenderState>,
}

/// Registry for deserializing trait-based nodes from saved projects.
/// Populated on first access with all Dynamic node type factories.
pub static NODE_REGISTRY: std::sync::LazyLock<std::sync::Mutex<NodeRegistryInner>> =
    std::sync::LazyLock::new(|| {
        let mut r = NodeRegistryInner::new();
        // Register all Dynamic node types so clone/deserialize can reconstruct them
        crate::nodes::add_node::register(&mut r);
        crate::nodes::multiply_node::register(&mut r);
        crate::nodes::time_node::register(&mut r);
        crate::nodes::noise_node::register(&mut r);
        crate::nodes::display_node::register(&mut r);
        crate::nodes::color_node::register(&mut r);
        crate::nodes::comment_node::register(&mut r);
        crate::nodes::timer_node::register(&mut r);
        crate::nodes::gate_node::register(&mut r);
        crate::nodes::sample_hold_node::register(&mut r);
        crate::nodes::key_input_node::register(&mut r);
        crate::nodes::button_node::register(&mut r);
        crate::nodes::pack_node::register(&mut r);
        crate::nodes::mouse_tracker_node::register(&mut r);
        crate::nodes::map_range_node::register(&mut r);
        crate::nodes::string_format_node::register(&mut r);
        crate::nodes::json_extract_node::register(&mut r);
        crate::nodes::text_editor_node::register(&mut r);
        crate::nodes::draw_node::register(&mut r);
        crate::nodes::visual_output_node::register(&mut r);
        crate::nodes::file_node::register(&mut r);
        crate::nodes::file_menu_node::register(&mut r);
        crate::nodes::folder_browser_node::register(&mut r);
        crate::nodes::html_viewer_node::register(&mut r);
        crate::nodes::console_node::register(&mut r);
        crate::nodes::monitor_node::register(&mut r);
        crate::nodes::zoom_control_node::register(&mut r);
        crate::nodes::midi_note_node::register(&mut r);
        crate::nodes::crop_node::register(&mut r);
        crate::nodes::transform_node::register(&mut r);
        crate::nodes::image_style_node::register(&mut r);
        crate::nodes::color_channel_node::register(&mut r);
        crate::nodes::settings_node::register(&mut r);
        crate::nodes::wgsl_presets::register(&mut r);
        crate::nodes::route_node::register_route(&mut r);
        crate::nodes::route_node::register_switch(&mut r);
        crate::nodes::tts_node::register(&mut r);
        crate::nodes::terminal_node::register(&mut r);
        crate::nodes::text_ops_node::register(&mut r);
        crate::nodes::printer_node::register(&mut r);
        crate::nodes::keypoint_extract::register(&mut r);
        crate::nodes::smoother::register(&mut r);
        crate::nodes::point_2d_node::register(&mut r);
        crate::nodes::fill_node::register(&mut r);
        crate::nodes::image_scanner_node::register(&mut r);
        crate::nodes::frame_recorder_node::register(&mut r);
        crate::nodes::noise_removal_node::register(&mut r);
        crate::nodes::voice_effects_node::register(&mut r);
        crate::nodes::audio_analyzer_node::register(&mut r);
        crate::nodes::music_visualizer_node::register(&mut r);
        crate::nodes::spectral_synth_node::register(&mut r);
        crate::nodes::web_app_node::register(&mut r);
        crate::nodes::websocket_node::register(&mut r);
        crate::nodes::clock_node::register(&mut r);
        crate::nodes::hand_detection::register(&mut r);
        crate::nodes::face_detection::register(&mut r);
        crate::nodes::pose_detection::register(&mut r);
        crate::ml::match_node::register(&mut r);
        crate::nodes::json_fields::register(&mut r);
        crate::nodes::math_formula::register(&mut r);
        crate::nodes::blend::register(&mut r);
        crate::nodes::select::register(&mut r);
        std::sync::Mutex::new(r)
    });

pub struct NodeRegistryInner {
    factories: HashMap<String, Box<dyn Fn(&Value) -> Box<dyn NodeBehavior> + Send + Sync>>,
}

impl NodeRegistryInner {
    fn new() -> Self {
        Self { factories: HashMap::new() }
    }

    pub fn register<F>(&mut self, type_tag: &str, factory: F)
    where F: Fn(&Value) -> Box<dyn NodeBehavior> + Send + Sync + 'static
    {
        self.factories.insert(type_tag.to_string(), Box::new(factory));
    }

    pub fn create(&self, type_tag: &str, state: &Value) -> Option<Box<dyn NodeBehavior>> {
        self.factories.get(type_tag).map(|f| f(state))
    }

    pub fn type_tags(&self) -> Vec<String> {
        self.factories.keys().cloned().collect()
    }
}
