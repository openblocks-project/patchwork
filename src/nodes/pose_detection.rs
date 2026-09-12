use eframe::egui::{self, RichText};
use std::sync::Arc;
use crate::graph::{PortDef, PortKind, PortValue, ImageData};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::graph::MlPreset;
use crate::nodes::ml_model::{MlInferenceRequest, BUNDLED_POSE_SENTINEL};

// ── Temporal smoothing ────────────────────────────────────────────────────────
// One Euro filter: heavy smoothing when the subject is still, loosens when they
// move fast, so jitter goes away without adding lag on real motion.
#[derive(Debug, Clone)]
struct OneEuro { min_cutoff: f32, beta: f32, x_prev: f32, dx_prev: f32, init: bool }

impl OneEuro {
    fn new() -> Self { Self { min_cutoff: 1.2, beta: 0.010, x_prev: 0.0, dx_prev: 0.0, init: false } }
    fn alpha(cutoff: f32, dt: f32) -> f32 {
        let tau = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
        1.0 / (1.0 + tau / dt)
    }
    fn filter(&mut self, x: f32, dt: f32) -> f32 {
        if !self.init { self.init = true; self.x_prev = x; return x; }
        let dt = dt.clamp(1e-3, 0.5);
        let dx = (x - self.x_prev) / dt;
        let a_d = Self::alpha(1.0, dt);
        let dx_hat = a_d * dx + (1.0 - a_d) * self.dx_prev;
        let cutoff = self.min_cutoff + self.beta * dx_hat.abs();
        let a = Self::alpha(cutoff, dt);
        let x_hat = a * x + (1.0 - a) * self.x_prev;
        self.x_prev = x_hat; self.dx_prev = dx_hat;
        x_hat
    }
}

#[derive(Debug, Clone)]
struct PoseSmoother {
    fx: Vec<OneEuro>, fy: Vec<OneEuro>,
    last_x: Vec<f32>, last_y: Vec<f32>, seen: Vec<bool>,
    last_t: f64,
}

impl Default for PoseSmoother {
    fn default() -> Self {
        Self {
            fx: (0..33).map(|_| OneEuro::new()).collect(),
            fy: (0..33).map(|_| OneEuro::new()).collect(),
            last_x: vec![0.0; 33], last_y: vec![0.0; 33], seen: vec![false; 33],
            last_t: -1.0,
        }
    }
}

impl PoseSmoother {
    /// MediaPipe always returns all 33 landmarks — when part of the body leaves
    /// frame it GUESSES the rest with low visibility. Accepting those guesses is
    /// what makes the skeleton change proportions. Below `min_vis` we hold the
    /// last confident position instead.
    fn update(&mut self, idx: usize, x: f32, y: f32, vis: f32, now: f64, min_vis: f32) -> (f32, f32, f32) {
        if idx >= 33 { return (x, y, vis); }
        // Results arrive irregularly; an unbounded dt makes the One Euro alpha
        // swing and the joints snap between filtered and raw positions.
        let dt = if self.last_t < 0.0 { 1.0 / 30.0 }
                 else { ((now - self.last_t) as f32).clamp(0.02, 0.15) };
        if vis < min_vis {
            if self.seen[idx] { return (self.last_x[idx], self.last_y[idx], vis); }
            return (0.0, 0.0, vis);
        }
        let sx = self.fx[idx].filter(x, dt);
        let sy = self.fy[idx].filter(y, dt);
        self.last_x[idx] = sx; self.last_y[idx] = sy; self.seen[idx] = true;
        (sx, sy, vis)
    }
    fn tick(&mut self, now: f64) { self.last_t = now; }
}

// ── PoseDetectionNode (Body Tracking) ────────────────────────────────────────
//
// Full 2-stage MediaPipe body tracking — up to 2 bodies, 33 keypoints each.
//
// Ports:
//   In  0: Image       (Image)
//   Out 0: Annotated   (Image)   — frame with skeleton drawn
//   Out 1: JSON        (Text)    — full JSON array with all bodies
//   Out 2: Detected    (Number)  — count: 0, 1, or 2
//   Out 3: Body 1 JSON (Text)    — first body nested JSON
//   Out 4: Body 2 JSON (Text)    — second body nested JSON

#[derive(Debug, Clone)]
pub struct PoseDetectionNode {
    pub confidence:      f32,
    pub interval_ms:     u32,

    pub status:          String,
    pub result_json:     String,
    pub body0_json:      String,
    pub annotated_frame: Option<Arc<ImageData>>,
    pub detected:        f32,

    last_inference_secs: f64,
    last_input_hash:     u64,

    /// Temporal smoothing on/off (One Euro filter per landmark).
    pub smoothing:       bool,
    /// Landmarks below this visibility are held at their last confident
    /// position rather than accepting the model's guess.
    pub min_visibility:  f32,
    smoother:            PoseSmoother,
}

impl Default for PoseDetectionNode {
    fn default() -> Self {
        let empty = body_json_empty();
        Self {
            confidence:      0.5,
            // 33ms ≈ 30fps. At the old 100ms the signal was undersampled, which
            // reads as steppy motion no amount of filtering can recover.
            interval_ms:     33,
            status:          String::new(),
            result_json:     "[]".into(),
            body0_json:      empty,
            annotated_frame: None,
            detected:        0.0,
            last_inference_secs: -999.0,
            last_input_hash:     0,
            smoothing:       true,
            min_visibility:  0.5,
            smoother:        PoseSmoother::default(),
        }
    }
}

// ── JSON building (needs &mut self for the smoother) ─────────────────────────

impl PoseDetectionNode {
    fn build_body_json(&mut self, items: &[&serde_json::Value], body_idx: usize, now: f64) -> String {
        let mut bbox = serde_json::json!({"x1":0,"y1":0,"x2":0,"y2":0});
        let mut kps: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
        let mut confidence = 0.0f64;

        for item in items {
            let b = item.get("body").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if b != body_idx { continue; }
            match item.get("type").and_then(|v| v.as_str()) {
                Some("body_bbox") => {
                    confidence = item.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    bbox = serde_json::json!({
                        "x1": item.get("x1").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                        "y1": item.get("y1").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                        "x2": item.get("x2").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                        "y2": item.get("y2").and_then(|v| v.as_f64()).unwrap_or(0.0).round() as i64,
                    });
                }
                Some("body_landmark") => {
                    if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                        let rx = item.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        let ry = item.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        let rvis = item.get("visibility").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
                        let idx = ALL_POSE_NAMES.iter().position(|n| *n == name).unwrap_or(usize::MAX);
                        let (x, y, vis) = if self.smoothing && idx != usize::MAX {
                            self.smoother.update(idx, rx, ry, rvis, now, self.min_visibility)
                        } else {
                            (rx, ry, rvis)
                        };
                        kps.insert(name.to_string(), serde_json::json!({
                            "x": x.round() as i64,
                            "y": y.round() as i64,
                            "visibility": ((vis as f64) * 1000.0).round() / 1000.0
                        }));
                    }
                }
                _ => {}
            }
        }

        let mut body = serde_json::Map::new();
        body.insert("bbox".into(), bbox);
        for name in ALL_POSE_NAMES {
            body.insert(name.into(), kps.get(name).cloned()
                .unwrap_or(serde_json::json!({"x":0,"y":0,"visibility":0})));
        }

        serde_json::json!({
            "detected":   true,
            "confidence": (confidence * 1000.0).round() / 1000.0,
            "body":       body,
        }).to_string()
    }

    /// Parse flat JSON array — extract first body only.
    fn split_body_json(&mut self, flat_json: &str, now: f64) -> (usize, String) {
        let empty = body_json_empty();
        let arr: serde_json::Value = match serde_json::from_str(flat_json) {
            Ok(v) => v,
            Err(_) => return (0, empty),
        };
        let items = match arr.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => return (0, empty),
        };

        let has_body = items.iter().any(|v| v.get("type").and_then(|t| t.as_str()) == Some("body_bbox")
            && v.get("body").and_then(|h| h.as_u64()) == Some(0));

        if has_body {
            let refs: Vec<&serde_json::Value> = items.iter().collect();
            let json = self.build_body_json(&refs, 0, now);
            (1, json)
        } else {
            (0, empty)
        }
    }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for PoseDetectionNode {
    fn title(&self)      -> &str   { "Body Tracking" }
    fn type_tag(&self)   -> &str   { "pose_detection" }
    fn color_hint(&self) -> [u8;3] { [220, 160, 60] }
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
            PortDef::new("Body",      PortKind::Text),
        ]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![
            (1, PortValue::Text(self.result_json.clone())),
            (2, PortValue::Float(self.detected)),
            (3, PortValue::Text(self.body0_json.clone())),
        ]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim    = egui::Color32::from_rgb(110, 110, 120);
        let gold   = egui::Color32::from_rgb(220, 190, 60);
        let red    = egui::Color32::from_rgb(220, 80, 80);
        let amber  = egui::Color32::from_rgb(220, 200, 60);

        // ── Poll inference result ─────────────────────────────────────────
        let result_id = egui::Id::new(("pose_det_result", node_id));
        if let Some(result) = ui.ctx().data_mut(|d| {
            d.get_temp::<crate::nodes::ml_model::MlInferenceResult>(result_id)
        }) {
            ui.ctx().data_mut(|d| d.remove::<crate::nodes::ml_model::MlInferenceResult>(result_id));
            self.status         = result.status.clone();
            self.annotated_frame = result.annotated_frame.clone();

            let now = ui.ctx().input(|i| i.time);
            let (count, b0) = self.split_body_json(&result.result_json, now);
            self.smoother.tick(now);
            self.detected    = count.min(1) as f32;
            self.body0_json  = b0;
            self.result_json = result.result_json.clone();

            if let Some(ref frame) = self.annotated_frame {
                ui.ctx().data_mut(|d| d.insert_temp(
                    egui::Id::new(("pose_annotated", node_id)), frame.clone()
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
            let col = if self.detected >= 1.0 { gold }
                      else if self.status.contains("Running") { amber }
                      else { red };
            ui.label(RichText::new(&self.status).small().color(col));
            if self.detected >= 1.0 {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("●").small().color(gold));
                    ui.label(RichText::new("Body detected").small().color(dim));
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

        // ── Smoothing + visibility gate ───────────────────────────────────
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.smoothing, RichText::new("Smoothing").small());
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Min vis").small().color(dim));
            ui.add(egui::Slider::new(&mut self.min_visibility, 0.2..=0.9).show_value(false));
            ui.label(RichText::new(format!("{:.0}%", self.min_visibility * 100.0)).small().monospace());
        });

        // ── Run every ─────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Run every").small().color(dim));
            let mut ms = self.interval_ms as f32;
            ui.add(egui::Slider::new(&mut ms, 20.0..=2000.0).show_value(false).logarithmic(true));
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
        crate::nodes::output_port_row(ui, "Body",
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
                    model_path:  BUNDLED_POSE_SENTINEL.to_string(),
                    labels_path: String::new(),
                    confidence:  self.confidence,
                    preset:      MlPreset::PoseEstimation,
                    image:       img.clone(),
                    node_id,
                }));
            }
        }
    }

    fn needs_cpu_image_input(&self, _port: usize) -> bool { true }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "confidence":     self.confidence,
            "interval_ms":    self.interval_ms,
            "smoothing":      self.smoothing,
            "min_visibility": self.min_visibility,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("confidence").and_then(|v| v.as_f64()) { self.confidence = v as f32; }
        if let Some(v) = state.get("interval_ms").and_then(|v| v.as_u64()) { self.interval_ms = (v as u32).clamp(20, 2000); }
        if let Some(v) = state.get("smoothing").and_then(|v| v.as_bool()) { self.smoothing = v; }
        if let Some(v) = state.get("min_visibility").and_then(|v| v.as_f64()) { self.min_visibility = (v as f32).clamp(0.0, 1.0); }
    }
}

// ── JSON helpers ──────────────────────────────────────────────────────────────

const ALL_POSE_NAMES: [&str; 33] = [
    "nose",
    "left_eye_inner", "left_eye", "left_eye_outer",
    "right_eye_inner", "right_eye", "right_eye_outer",
    "left_ear", "right_ear",
    "mouth_left", "mouth_right",
    "left_shoulder", "right_shoulder",
    "left_elbow", "right_elbow",
    "left_wrist", "right_wrist",
    "left_pinky", "right_pinky",
    "left_index", "right_index",
    "left_thumb", "right_thumb",
    "left_hip", "right_hip",
    "left_knee", "right_knee",
    "left_ankle", "right_ankle",
    "left_heel", "right_heel",
    "left_foot_index", "right_foot_index",
];

fn body_json_empty() -> String {
    let mut body = serde_json::Map::new();
    body.insert("bbox".into(), serde_json::json!({"x1":0,"y1":0,"x2":0,"y2":0}));
    for name in ALL_POSE_NAMES {
        body.insert(name.into(), serde_json::json!({"x":0,"y":0,"visibility":0}));
    }
    serde_json::json!({"detected": false, "confidence": 0.0, "body": body}).to_string()
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("pose_detection", |state| {
        let mut n = PoseDetectionNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
