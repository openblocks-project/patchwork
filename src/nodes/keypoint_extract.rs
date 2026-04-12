use eframe::egui::{self, RichText};
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── KeypointExtractNode ───────────────────────────────────────────────────────
//
// Takes the JSON output from ML Model and extracts a named keypoint's
// X and Y coordinates as separate Number ports.
//
// JSON format expected (from ML Model):
//   [{"type":"palm_keypoint","name":"index_tip","x":120,"y":85}, ...]
//   [{"name":"nose","x":310,"y":200,"confidence":0.9}, ...]     (pose)
//   [{"type":"hand_bbox","x1":0,"y1":0,"x2":200,"y2":300}, ...] (bbox — x/y = center)

#[derive(Debug, Clone)]
pub struct KeypointExtractNode {
    /// Name of the keypoint to extract (e.g. "index_tip", "wrist", "nose")
    pub keypoint_name: String,
    /// When true, hold the last good X/Y/Conf when detection fails instead of outputting 0
    pub hold_on_loss: bool,
    /// Cached output values (updated each frame)
    out_x: f32,
    out_y: f32,
    out_conf: f32,
    /// Last known-good values (used when hold_on_loss is true and detection fails)
    last_good_x: f32,
    last_good_y: f32,
    last_good_conf: f32,
    found: bool,
}

impl Default for KeypointExtractNode {
    fn default() -> Self {
        Self {
            keypoint_name: "index_tip".into(),
            hold_on_loss: true, // on by default
            out_x: 0.0,
            out_y: 0.0,
            out_conf: 0.0,
            last_good_x: 0.0,
            last_good_y: 0.0,
            last_good_conf: 0.0,
            found: false,
        }
    }
}

/// Common keypoint names to show in the quick-select dropdown.
const COMMON_KEYPOINTS: &[&str] = &[
    // Hand / Palm (MediaPipe Hand Detection)
    "wrist",
    "index_mcp",
    "middle_mcp",
    "ring_mcp",
    "pinky_mcp",
    "index_tip",
    "thumb_tip",
    // Pose (COCO 17-point)
    "nose",
    "left_eye", "right_eye",
    "left_ear", "right_ear",
    "left_shoulder", "right_shoulder",
    "left_elbow", "right_elbow",
    "left_wrist", "right_wrist",
    "left_hip", "right_hip",
    "left_knee", "right_knee",
    "left_ankle", "right_ankle",
];

impl KeypointExtractNode {
    fn parse_json(&mut self, json_str: &str) {
        self.found = false;

        if let Ok(arr) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(items) = arr.as_array() {
                for item in items {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name.eq_ignore_ascii_case(&self.keypoint_name) {
                        self.out_x    = item.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        self.out_y    = item.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        self.out_conf = item.get("confidence").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                        self.found = true;
                        break;
                    }
                    // Also handle bbox entries: type match → output center x/y
                    let typ = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if typ.eq_ignore_ascii_case(&self.keypoint_name) {
                        let x1 = item.get("x1").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        let y1 = item.get("y1").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        let x2 = item.get("x2").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        let y2 = item.get("y2").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        self.out_x    = (x1 + x2) / 2.0;
                        self.out_y    = (y1 + y2) / 2.0;
                        self.out_conf = item.get("confidence").and_then(|v| v.as_f64()).unwrap_or(1.0) as f32;
                        self.found = true;
                        break;
                    }
                }
            }
        }

        if self.found {
            // Update last-known-good
            self.last_good_x    = self.out_x;
            self.last_good_y    = self.out_y;
            self.last_good_conf = self.out_conf;
        } else if self.hold_on_loss {
            // No detection this frame — hold the previous value
            self.out_x    = self.last_good_x;
            self.out_y    = self.last_good_y;
            self.out_conf = self.last_good_conf;
        } else {
            self.out_x    = 0.0;
            self.out_y    = 0.0;
            self.out_conf = 0.0;
        }
    }
}

impl NodeBehavior for KeypointExtractNode {
    fn title(&self)      -> &str    { "Keypoint Extract" }
    fn type_tag(&self)   -> &str    { "keypoint_extract" }
    fn color_hint(&self) -> [u8;3]  { [100, 180, 240] }
    fn min_width(&self)  -> Option<f32> { Some(200.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("JSON", PortKind::Text)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("X",    PortKind::Number),
            PortDef::new("Y",    PortKind::Number),
            PortDef::new("Conf", PortKind::Normalized),
        ]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Parsing is done in render_with_context() so it runs AFTER the ML Model
        // image loop has placed its JSON into the values map. If we parse here,
        // we run before ML Model and always see an empty string.
        vec![
            (0, PortValue::Float(self.out_x)),
            (1, PortValue::Float(self.out_y)),
            (2, PortValue::Float(self.out_conf)),
        ]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim     = egui::Color32::from_rgb(110, 110, 120);
        let green   = egui::Color32::from_rgb(80, 210, 120);
        let red     = egui::Color32::from_rgb(220, 80, 80);

        // ── Parse JSON here (not in evaluate) so we see ML Model output ───
        // ML Model runs in the image loop AFTER the Dynamic re-eval pass,
        // so evaluate() always sees an empty string. Reading here gets the
        // fresh value. We store results in egui temp data so app/mod.rs can
        // inject them into the values map (same pattern as AudioPlaylist).
        let json_val = crate::graph::Graph::static_input_value(
            ctx.connections, ctx.values, node_id, 0
        );
        if let PortValue::Text(ref s) = json_val {
            if !s.is_empty() {
                self.parse_json(s);
            }
        }
        ui.ctx().data_mut(|d| {
            d.insert_temp(egui::Id::new(("kp_x",    node_id)), self.out_x);
            d.insert_temp(egui::Id::new(("kp_y",    node_id)), self.out_y);
            d.insert_temp(egui::Id::new(("kp_conf", node_id)), self.out_conf);
        });

        // ── JSON input port ───────────────────────────────────────────────
        let json_connected = ctx.connections.iter()
            .any(|c| c.to_node == node_id && c.to_port == 0);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Text,
            );
            let lbl = if json_connected { "JSON ✓" } else { "JSON" };
            let col = if json_connected { egui::Color32::from_rgb(100, 200, 255) } else { dim };
            ui.label(RichText::new(lbl).small().color(col));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Keypoint name selector ────────────────────────────────────────
        ui.label(RichText::new("Keypoint").small().color(dim));

        // Quick-select dropdown
        egui::ComboBox::from_id_salt(egui::Id::new(("kp_select", node_id)))
            .selected_text(&self.keypoint_name)
            .width(180.0)
            .show_ui(ui, |ui| {
                for &name in COMMON_KEYPOINTS {
                    if ui.selectable_label(self.keypoint_name == name, name).clicked() {
                        self.keypoint_name = name.to_string();
                    }
                }
            });

        // Manual text entry
        ui.horizontal(|ui| {
            ui.label(RichText::new("or type:").small().color(dim));
            ui.add(egui::TextEdit::singleline(&mut self.keypoint_name)
                .desired_width(120.0)
                .font(egui::TextStyle::Small));
        });

        // ── Hold on loss toggle ───────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.hold_on_loss, RichText::new("Hold last value").small());
        });

        ui.add_space(4.0);

        // ── Live value display ────────────────────────────────────────────
        if self.found {
            ui.horizontal(|ui| {
                ui.label(RichText::new("✓").small().color(green));
                ui.label(RichText::new(format!("({:.0}, {:.0})", self.out_x, self.out_y))
                    .small().monospace());
            });
        } else if self.hold_on_loss && (self.last_good_x != 0.0 || self.last_good_y != 0.0) {
            ui.horizontal(|ui| {
                ui.label(RichText::new("~ held").small().color(egui::Color32::from_rgb(180, 160, 80)));
                ui.label(RichText::new(format!("({:.0}, {:.0})", self.out_x, self.out_y))
                    .small().monospace().color(egui::Color32::from_rgb(180, 160, 80)));
            });
        } else {
            ui.label(RichText::new(if json_connected { "✗ Not found" } else { "No input" })
                .small().color(red));
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports ──────────────────────────────────────────────────
        let x_str = format!("{:.1}", self.out_x);
        let y_str = format!("{:.1}", self.out_y);
        let c_str = format!("{:.2}", self.out_conf);

        crate::nodes::output_port_row(ui, "X",    &x_str, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Number);
        crate::nodes::output_port_row(ui, "Y",    &y_str, node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Number);
        crate::nodes::output_port_row(ui, "Conf", &c_str, node_id, 2,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Normalized);
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "keypoint_name": self.keypoint_name,
            "hold_on_loss":  self.hold_on_loss,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("keypoint_name").and_then(|v| v.as_str()) {
            self.keypoint_name = v.into();
        }
        if let Some(v) = state.get("hold_on_loss").and_then(|v| v.as_bool()) {
            self.hold_on_loss = v;
        }
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("keypoint_extract", |state| {
        let mut n = KeypointExtractNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
