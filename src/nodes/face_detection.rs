use eframe::egui::{self, RichText};
use std::sync::Arc;
use crate::graph::{PortDef, PortKind, PortValue, ImageData};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::graph::MlPreset;
use crate::nodes::ml_model::{MlInferenceRequest, BUNDLED_FACE_SENTINEL};

// ── FaceDetectionNode (Face Tracking) ────────────────────────────────────────
//
// Full 2-stage MediaPipe face tracking — up to 2 faces.
//
// Ports:
//   In  0: Image       (Image)
//   Out 0: Annotated   (Image)   — frame with face mesh drawn
//   Out 1: JSON        (Text)    — full JSON array with all faces
//   Out 2: Detected    (Number)  — count: 0, 1, or 2
//   Out 3: Face 1 JSON (Text)    — first face nested JSON
//   Out 4: Face 2 JSON (Text)    — second face nested JSON

#[derive(Debug, Clone)]
pub struct FaceDetectionNode {
    pub confidence:      f32,
    pub interval_ms:     u32,

    pub status:          String,
    pub result_json:     String,   // full flat array
    pub face0_json:      String,   // nested JSON for detected face
    pub annotated_frame: Option<Arc<ImageData>>,
    pub detected:        f32,      // 0, 1, or 2

    last_inference_secs: f64,
    last_input_hash:     u64,
}

impl Default for FaceDetectionNode {
    fn default() -> Self {
        let empty = face_json_empty();
        Self {
            confidence:      0.5,
            interval_ms:     100,
            status:          String::new(),
            result_json:     "[]".into(),
            face0_json:      empty,
            annotated_frame: None,
            detected:        0.0,
            last_inference_secs: -999.0,
            last_input_hash:     0,
        }
    }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for FaceDetectionNode {
    fn title(&self)      -> &str   { "Face Tracking" }
    fn type_tag(&self)   -> &str   { "face_detection" }
    fn color_hint(&self) -> [u8;3] { [180, 100, 220] }
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
            PortDef::new("Face",      PortKind::Text),
        ]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![
            (1, PortValue::Text(self.result_json.clone())),
            (2, PortValue::Float(self.detected)),
            (3, PortValue::Text(self.face0_json.clone())),
        ]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim    = egui::Color32::from_rgb(110, 110, 120);
        let cyan   = egui::Color32::from_rgb(100, 220, 200);
        let red    = egui::Color32::from_rgb(220, 80, 80);
        let amber  = egui::Color32::from_rgb(220, 200, 60);

        // ── Poll inference result ──────────────────────────────��──────────
        let result_id = egui::Id::new(("face_det_result", node_id));
        if let Some(result) = ui.ctx().data_mut(|d| {
            d.get_temp::<crate::nodes::ml_model::MlInferenceResult>(result_id)
        }) {
            ui.ctx().data_mut(|d| d.remove::<crate::nodes::ml_model::MlInferenceResult>(result_id));
            self.status         = result.status.clone();
            self.annotated_frame = result.annotated_frame.clone();

            // Parse flat JSON → face nested JSON (single face)
            let (count, f0) = split_face_json(&result.result_json);
            self.detected    = count.min(1) as f32;
            self.face0_json  = f0;
            self.result_json = result.result_json.clone();

            if let Some(ref frame) = self.annotated_frame {
                ui.ctx().data_mut(|d| d.insert_temp(
                    egui::Id::new(("face_annotated", node_id)), frame.clone()
                ));
            }
        }

        // ── Image input port ────────────────────────────────���─────────────
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

        // ── Status ──────────────────────────��─────────────────────────────
        if !self.status.is_empty() {
            let col = if self.detected >= 1.0 { cyan }
                      else if self.status.contains("Running") { amber }
                      else { red };
            ui.label(RichText::new(&self.status).small().color(col));
            if self.detected >= 1.0 {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").small().color(cyan));
                    ui.label(RichText::new("Face detected").small().color(dim));
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

        // ── Run every ───────────────────────────────────────────────���─────
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

        // ── Output ports ───────────────────────────────��──────────────────
        let ann_str = if self.annotated_frame.is_some() { "frame" } else { "none" };
        crate::nodes::output_port_row(ui, "Annotated", ann_str, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Image);
        crate::nodes::output_port_row(ui, "JSON", if self.detected >= 1.0 { "✓" } else { "[]" }, node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Text);
        let det_str = format!("{:.0}", self.detected);
        crate::nodes::output_port_row(ui, "Detected", &det_str, node_id, 2,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);
        crate::nodes::output_port_row(ui, "Face",
            if self.detected >= 1.0 { "✓" } else { "{}" }, node_id, 3,
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
                    model_path:  BUNDLED_FACE_SENTINEL.to_string(),
                    labels_path: String::new(),
                    confidence:  self.confidence,
                    preset:      MlPreset::FaceDetection,
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

// ── JSON helpers ────────────────────────���─────────────────────────────────���───

const NAMED_FACE_LANDMARKS: [&str; 15] = [
    "nose_tip", "forehead",
    "left_eye_inner", "left_eye_outer",
    "right_eye_inner", "right_eye_outer",
    "mouth_left", "mouth_right",
    "upper_lip", "lower_lip",
    "chin",
    "left_cheek", "right_cheek",
    "left_temple", "right_temple",
];

fn face_json_empty() -> String {
    let mut face = serde_json::Map::new();
    face.insert("bbox".into(), serde_json::json!({"x1":0,"y1":0,"x2":0,"y2":0}));
    for name in NAMED_FACE_LANDMARKS {
        face.insert(name.into(), serde_json::json!({"x":0,"y":0}));
    }
    face.insert("landmarks".into(), serde_json::json!([]));
    serde_json::json!({"detected": false, "confidence": 0.0, "face": face}).to_string()
}

fn build_face_json(items: &[&serde_json::Value], face_idx: usize) -> String {
    let mut bbox = serde_json::json!({"x1":0,"y1":0,"x2":0,"y2":0});
    let mut named_kps: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
    let mut indexed_landmarks: Vec<serde_json::Value> = Vec::new();
    let mut confidence = 0.0f64;

    for item in items {
        let f = item.get("face").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if f != face_idx { continue; }
        match item.get("type").and_then(|v| v.as_str()) {
            Some("face_bbox") => {
                confidence = item.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
                bbox = serde_json::json!({
                    "x1": item.get("x1").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                    "y1": item.get("y1").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                    "x2": item.get("x2").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                    "y2": item.get("y2").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                });
            }
            Some("face_landmark") => {
                if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                    let x = item.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64;
                    let y = item.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64;
                    named_kps.insert(name.to_string(), serde_json::json!({"x": x, "y": y}));
                }
            }
            Some("face_landmark_idx") => {
                let x = item.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64;
                let y = item.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64;
                let z = item.get("z").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let z_rounded = (z * 1000.0).round() / 1000.0;
                indexed_landmarks.push(serde_json::json!({"x": x, "y": y, "z": z_rounded}));
            }
            _ => {}
        }
    }

    let mut face = serde_json::Map::new();
    face.insert("bbox".into(), bbox);
    for name in NAMED_FACE_LANDMARKS {
        face.insert(name.into(), named_kps.get(name).cloned().unwrap_or(serde_json::json!({"x":0,"y":0})));
    }
    face.insert("landmarks".into(), serde_json::Value::Array(indexed_landmarks));

    serde_json::json!({
        "detected":   true,
        "confidence": (confidence * 1000.0).round() / 1000.0,
        "face":       face,
    }).to_string()
}

/// Parse flat JSON array — extract first face only.
fn split_face_json(flat_json: &str) -> (usize, String) {
    let empty = face_json_empty();
    let arr: serde_json::Value = match serde_json::from_str(flat_json) {
        Ok(v) => v,
        Err(_) => return (0, empty),
    };
    let items = match arr.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return (0, empty),
    };

    let has_face = items.iter().any(|v| v.get("type").and_then(|t| t.as_str()) == Some("face_bbox")
        && v.get("face").and_then(|h| h.as_u64()) == Some(0));

    if has_face {
        let refs: Vec<&serde_json::Value> = items.iter().collect();
        (1, build_face_json(&refs, 0))
    } else {
        (0, empty)
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("face_detection", |state| {
        let mut n = FaceDetectionNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
