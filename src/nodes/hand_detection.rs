use eframe::egui::{self, RichText};
use std::sync::Arc;
use crate::graph::{PortDef, PortKind, PortValue, ImageData};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::graph::MlPreset;
use crate::nodes::ml_model::{MlInferenceRequest, BUNDLED_HAND_SENTINEL};

// ── HandDetectionNode (Hand Tracking) ────────────────────────────────────────
//
// Full 2-stage MediaPipe hand tracking — up to 2 hands.
//
// Ports:
//   In  0: Image       (Image)
//   Out 0: Annotated   (Image)   — frame with skeleton drawn for all hands
//   Out 1: JSON        (Text)    — full JSON array with all hands
//   Out 2: Detected    (Number)  — count: 0, 1, or 2
//   Out 3: Hand 1 JSON (Text)    — first hand nested JSON (or "{}")
//   Out 4: Hand 2 JSON (Text)    — second hand nested JSON (or "{}")

#[derive(Debug, Clone)]
pub struct HandDetectionNode {
    pub confidence:      f32,
    pub interval_ms:     u32,

    pub status:          String,
    pub result_json:     String,   // full flat array
    pub hand0_json:      String,   // nested JSON for hand 0
    pub hand1_json:      String,   // nested JSON for hand 1
    pub annotated_frame: Option<Arc<ImageData>>,
    pub detected:        f32,      // 0, 1, or 2

    last_inference_secs: f64,
    last_input_hash:     u64,
}

impl Default for HandDetectionNode {
    fn default() -> Self {
        let empty = hand_json_empty();
        Self {
            confidence:      0.5,
            interval_ms:     100,
            status:          String::new(),
            result_json:     "[]".into(),
            hand0_json:      empty.clone(),
            hand1_json:      empty,
            annotated_frame: None,
            detected:        0.0,
            last_inference_secs: -999.0,
            last_input_hash:     0,
        }
    }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for HandDetectionNode {
    fn title(&self)      -> &str   { "Hand Tracking" }
    fn type_tag(&self)   -> &str   { "hand_detection" }
    fn color_hint(&self) -> [u8;3] { [60, 180, 120] }
    fn min_width(&self)  -> Option<f32> { Some(220.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Image", PortKind::Image)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Annotated", PortKind::Image),
            PortDef::new("JSON",      PortKind::Text),
            PortDef::new("Detected",  PortKind::Number),
            PortDef::new("Hand 1",    PortKind::Text),
            PortDef::new("Hand 2",    PortKind::Text),
        ]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![
            (1, PortValue::Text(self.result_json.clone())),
            (2, PortValue::Float(self.detected)),
            (3, PortValue::Text(self.hand0_json.clone())),
            (4, PortValue::Text(self.hand1_json.clone())),
        ]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim   = egui::Color32::from_rgb(110, 110, 120);
        let green = egui::Color32::from_rgb(80, 220, 120);
        let blue  = egui::Color32::from_rgb(100, 160, 255);
        let red   = egui::Color32::from_rgb(220, 80, 80);
        let amber = egui::Color32::from_rgb(220, 200, 60);

        // ── Poll inference result ─────────────────────────────────────────
        let result_id = egui::Id::new(("hand_det_result", node_id));
        if let Some(result) = ui.ctx().data_mut(|d| {
            d.get_temp::<crate::nodes::ml_model::MlInferenceResult>(result_id)
        }) {
            ui.ctx().data_mut(|d| d.remove::<crate::nodes::ml_model::MlInferenceResult>(result_id));
            self.status         = result.status.clone();
            self.annotated_frame = result.annotated_frame.clone();

            // Parse flat JSON → per-hand nested JSON
            let (count, h0, h1) = split_hand_json(&result.result_json);
            self.detected    = count as f32;
            self.hand0_json  = h0;
            self.hand1_json  = h1;
            self.result_json = result.result_json.clone();

            if let Some(ref frame) = self.annotated_frame {
                ui.ctx().data_mut(|d| d.insert_temp(
                    egui::Id::new(("hand_annotated", node_id)), frame.clone()
                ));
            }
        }

        // ── Image input port ──────────────────────────────────────────────
        let img_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            let lbl = if img_connected { "Image ✓" } else { "Image" };
            let col = if img_connected { egui::Color32::from_rgb(120, 200, 120) } else { dim };
            ui.label(RichText::new(lbl).small().color(col));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Status ────────────────────────────────────────────────────────
        if !self.status.is_empty() {
            let col = if self.detected >= 1.0 { green }
                      else if self.status.contains("Running") { amber }
                      else { red };
            ui.label(RichText::new(&self.status).small().color(col));
            // Show hand count badges
            if self.detected >= 1.0 {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").small().color(green));
                    ui.label(RichText::new("Hand 1").small().color(dim));
                    if self.detected >= 2.0 {
                        ui.label(RichText::new("●").small().color(blue));
                        ui.label(RichText::new("Hand 2").small().color(dim));
                    }
                });
            }
            ui.add_space(4.0);
        } else if img_connected {
            ui.label(RichText::new("Waiting…").small().color(dim));
            ui.add_space(4.0);
        }

        // ── Threshold ─────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Threshold").small().color(dim));
            ui.add(egui::Slider::new(&mut self.confidence, 0.0..=1.0).show_value(false));
            ui.label(RichText::new(format!("{:.0}%", self.confidence * 100.0)).small().monospace());
        });

        // ── Run every ─────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Run every").small().color(dim));
            let mut ms = self.interval_ms as f32;
            ui.add(egui::Slider::new(&mut ms, 50.0..=2000.0).show_value(false).logarithmic(true));
            ui.label(RichText::new(
                if ms < 1000.0 { format!("{:.0}ms", ms) } else { format!("{:.1}s", ms/1000.0) }
            ).small().monospace());
            self.interval_ms = ms as u32;
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports ──────────────────────────────────────────────────
        let ann_str = if self.annotated_frame.is_some() { "frame" } else { "none" };
        crate::nodes::output_port_row(ui, "Annotated", ann_str, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Image);
        crate::nodes::output_port_row(ui, "JSON", if self.detected >= 1.0 { "✓" } else { "[]" }, node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Text);
        let det_str = format!("{:.0}", self.detected);
        crate::nodes::output_port_row(ui, "Detected", &det_str, node_id, 2,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);
        crate::nodes::output_port_row(ui, "Hand 1",
            if self.detected >= 1.0 { "✓" } else { "{}" }, node_id, 3,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Text);
        crate::nodes::output_port_row(ui, "Hand 2",
            if self.detected >= 2.0 { "✓" } else { "{}" }, node_id, 4,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Text);

        // ── Trigger inference ─────────────────────────────────────────────
        let input_val = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 0);
        if let PortValue::Image(img) = &input_val {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            img.width.hash(&mut h); img.height.hash(&mut h);
            if img.pixels.len() >= 32 {
                img.pixels[..16].hash(&mut h);
                img.pixels[img.pixels.len()-16..].hash(&mut h);
            }
            let hash = h.finish();
            let now = ui.ctx().input(|i| i.time);
            if now - self.last_inference_secs >= self.interval_ms as f64 / 1000.0 {
                self.last_input_hash = hash;
                self.last_inference_secs = now;
                self.status = "Running…".into();
                let inference_id = egui::Id::new(("ml_inference", node_id));
                ui.ctx().data_mut(|d| d.insert_temp(inference_id, MlInferenceRequest {
                    model_path:  BUNDLED_HAND_SENTINEL.to_string(),
                    labels_path: String::new(),
                    confidence:  self.confidence,
                    preset:      MlPreset::HandDetection,
                    image:       img.clone(),
                    node_id,
                }));
            }
        }
    }

    fn needs_cpu_image_input(&self, _port: usize) -> bool { true }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({ "confidence": self.confidence, "interval_ms": self.interval_ms })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("confidence").and_then(|v| v.as_f64()) { self.confidence = v as f32; }
        if let Some(v) = state.get("interval_ms").and_then(|v| v.as_u64()) { self.interval_ms = (v as u32).clamp(50, 2000); }
    }
}

// ── JSON helpers ──────────────────────────────────────────────────────────────

const ALL_LANDMARK_NAMES: [&str; 21] = [
    "wrist",
    "thumb_cmc","thumb_mcp","thumb_ip","thumb_tip",
    "index_mcp","index_pip","index_dip","index_tip",
    "middle_mcp","middle_pip","middle_dip","middle_tip",
    "ring_mcp","ring_pip","ring_dip","ring_tip",
    "pinky_mcp","pinky_pip","pinky_dip","pinky_tip",
];

fn hand_json_empty() -> String {
    let mut hand = serde_json::Map::new();
    hand.insert("bbox".into(), serde_json::json!({"x1":0,"y1":0,"x2":0,"y2":0}));
    for name in ALL_LANDMARK_NAMES {
        hand.insert(name.into(), serde_json::json!({"x":0,"y":0}));
    }
    serde_json::json!({"detected": false, "confidence": 0.0, "hand": hand}).to_string()
}

fn build_hand_json(items: &[&serde_json::Value], hand_idx: usize) -> String {
    let mut bbox = serde_json::json!({"x1":0,"y1":0,"x2":0,"y2":0});
    let mut kps: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
    let mut confidence = 0.0f64;

    for item in items {
        let h = item.get("hand").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if h != hand_idx { continue; }
        match item.get("type").and_then(|v| v.as_str()) {
            Some("hand_bbox") => {
                confidence = item.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
                bbox = serde_json::json!({
                    "x1": item.get("x1").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                    "y1": item.get("y1").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                    "x2": item.get("x2").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                    "y2": item.get("y2").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                });
            }
            Some("hand_landmark") => {
                if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                    let x = item.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64;
                    let y = item.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64;
                    kps.insert(name.to_string(), serde_json::json!({"x": x, "y": y}));
                }
            }
            _ => {}
        }
    }

    let mut hand = serde_json::Map::new();
    hand.insert("bbox".into(), bbox);
    for name in ALL_LANDMARK_NAMES {
        hand.insert(name.into(), kps.get(name).cloned().unwrap_or(serde_json::json!({"x":0,"y":0})));
    }

    serde_json::json!({
        "detected":   true,
        "confidence": (confidence * 1000.0).round() / 1000.0,
        "hand":       hand,
    }).to_string()
}

/// Parse flat JSON array, split by hand index.
/// Returns (detected_count, hand0_json, hand1_json).
fn split_hand_json(flat_json: &str) -> (usize, String, String) {
    let empty = hand_json_empty();
    let arr: serde_json::Value = match serde_json::from_str(flat_json) {
        Ok(v) => v,
        Err(_) => return (0, empty.clone(), empty),
    };
    let items = match arr.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return (0, empty.clone(), empty),
    };

    // Find which hand indices are present
    let has_hand: [bool; 2] = [
        items.iter().any(|v| v.get("type").and_then(|t| t.as_str()) == Some("hand_bbox")
            && v.get("hand").and_then(|h| h.as_u64()) == Some(0)),
        items.iter().any(|v| v.get("type").and_then(|t| t.as_str()) == Some("hand_bbox")
            && v.get("hand").and_then(|h| h.as_u64()) == Some(1)),
    ];

    let refs: Vec<&serde_json::Value> = items.iter().collect();
    let count = has_hand.iter().filter(|&&b| b).count();
    let h0 = if has_hand[0] { build_hand_json(&refs, 0) } else { empty.clone() };
    let h1 = if has_hand[1] { build_hand_json(&refs, 1) } else { empty };
    (count, h0, h1)
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("hand_detection", |state| {
        let mut n = HandDetectionNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
