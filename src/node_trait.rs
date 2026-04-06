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
pub trait NodeBehavior: Send + Sync {
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
}

/// Context passed to render_with_context — gives nodes access to
/// port values, connections, and port position tracking.
pub struct RenderContext<'a> {
    pub node_id: crate::graph::NodeId,
    pub values: &'a std::collections::HashMap<(crate::graph::NodeId, usize), crate::graph::PortValue>,
    pub connections: &'a [crate::graph::Connection],
    pub port_positions: &'a mut std::collections::HashMap<(crate::graph::NodeId, usize, bool), eframe::egui::Pos2>,
    pub dragging_from: &'a mut Option<(crate::graph::NodeId, usize, bool)>,
    pub pending_disconnects: &'a mut Vec<(crate::graph::NodeId, usize)>,
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
