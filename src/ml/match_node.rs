//! Match — Teachable-Machine-style classifier that works on any JSON input.
//!
//! Capture named classes from live JSON (landmark output, packed feature
//! vectors, sensor snapshots, any object of numbers) and output the
//! best-matching class label + confidence. Optionally pulse a trigger
//! when the match flips to a new class above a user-set threshold.
//!
//! Algorithm: nearest-centroid similarity (cosine or L2) over flattened
//! normalized feature vectors. Input shape is auto-detected — landmark
//! JSON uses bbox-relative normalization (translation/scale invariant),
//! everything else uses the numbers as-is in a deterministic order.
//!
//! type_tag is kept as `"pose_match"` so projects saved before this
//! refactor still load cleanly; only the UI label changes to "Match".

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};

// ── Enums ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Metric {
    Cosine,
    L2,
}
impl Default for Metric {
    fn default() -> Self { Self::Cosine }
}

/// Auto-detected input shape. Landmark variants keep their names for
/// backwards compat with projects saved under the pose-specific node;
/// generic variants are new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Schema {
    Unknown,
    Hand21,
    Pose33,
    Face468,
    /// Flat JSON array of numbers — vector of known dim.
    Vector(usize),
    /// Flat JSON object with numeric values — flattened in sorted-key order.
    Object(usize),
    /// Nested structure — DFS-flattened numeric leaves in sorted-path order.
    Nested(usize),
}
impl Default for Schema {
    fn default() -> Self { Self::Unknown }
}

impl Schema {
    fn label(&self) -> String {
        match self {
            Self::Unknown   => "— (waiting for input)".into(),
            Self::Hand21    => "Hand-21".into(),
            Self::Pose33    => "Pose-33".into(),
            Self::Face468   => "Face-468".into(),
            Self::Vector(n) => format!("Vector({n})"),
            Self::Object(n) => format!("Object({n})"),
            Self::Nested(n) => format!("Nested({n})"),
        }
    }

    fn from_landmark_count(n: usize) -> Self {
        match n {
            21  => Self::Hand21,
            33  => Self::Pose33,
            468 => Self::Face468,
            _   => Self::Unknown,
        }
    }
}

// ── Class data ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchClass {
    pub name: String,
    /// Each sample is a flattened feature vector.
    pub samples: Vec<Vec<f32>>,
    /// Arithmetic mean of samples. Empty until first sample captured.
    pub centroid: Vec<f32>,
    pub color: [u8; 3],
}

impl MatchClass {
    fn new(name: impl Into<String>, color: [u8; 3]) -> Self {
        Self { name: name.into(), samples: Vec::new(), centroid: Vec::new(), color }
    }

    fn recompute_centroid(&mut self) {
        if self.samples.is_empty() {
            self.centroid.clear();
            return;
        }
        let dim = self.samples[0].len();
        let mut sum = vec![0.0f32; dim];
        for s in &self.samples {
            if s.len() != dim { continue; }
            for i in 0..dim { sum[i] += s[i]; }
        }
        let n = self.samples.len() as f32;
        for v in &mut sum { *v /= n; }
        self.centroid = sum;
    }
}

// Palette used when auto-assigning colors to new classes.
const CLASS_PALETTE: &[[u8; 3]] = &[
    [220, 100,  60],   // orange
    [ 80, 200, 160],   // teal
    [100, 160, 240],   // blue
    [220, 180,  60],   // yellow
    [200, 100, 200],   // magenta
    [120, 210, 100],   // green
    [240, 120, 130],   // coral
    [160, 140, 230],   // lavender
];

/// Frames the node waits after emitting a Matched pulse before allowing
/// the next one — prevents flicker between two nearly-tied classes.
const HOLD_FRAMES: u32 = 3;

fn default_capture_frames() -> u32 { 5 }
fn default_threshold() -> f32 { 0.7 }

// ── Node state ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchNode {
    pub classes: Vec<MatchClass>,
    #[serde(default)]
    pub active_class: usize,
    #[serde(default)]
    pub metric: Metric,
    #[serde(default)]
    pub schema: Schema,
    #[serde(default = "default_capture_frames")]
    pub capture_buffer_frames: u32,
    /// Minimum confidence for the Matched trigger to fire.
    #[serde(default = "default_threshold")]
    pub match_threshold: f32,

    // ── Transient runtime state (not persisted) ──
    #[serde(skip)]  last_label: String,
    #[serde(skip)]  last_conf: f32,
    #[serde(skip)]  last_probs: Vec<f32>,
    /// Last banner text (schema mismatch / unsupported / empty etc).
    #[serde(skip)]  banner: String,
    /// Rolling capture buffer — filled while `capturing` is true.
    #[serde(skip)]  capture_buf: Vec<Vec<f32>>,
    #[serde(skip)]  capturing: bool,
    /// Edge detector for the Capture trigger port.
    #[serde(skip)]  last_trigger: f32,
    /// Rename box content keyed by class index.
    #[serde(skip)]  rename_buf: Option<(usize, String)>,
    /// Matched-trigger hysteresis bookkeeping.
    #[serde(skip)]  last_emitted_label: String,
    #[serde(skip)]  frames_since_emit: u32,
    /// Whether this frame is pulsing (1.0) on the Matched output.
    #[serde(skip)]  pulse_this_frame: bool,
}

impl Default for MatchNode {
    fn default() -> Self {
        Self {
            classes: Vec::new(),
            active_class: 0,
            metric: Metric::Cosine,
            schema: Schema::Unknown,
            capture_buffer_frames: 5,
            match_threshold: 0.7,
            last_label: String::new(),
            last_conf: 0.0,
            last_probs: Vec::new(),
            banner: String::new(),
            capture_buf: Vec::new(),
            capturing: false,
            last_trigger: 0.0,
            rename_buf: None,
            last_emitted_label: String::new(),
            frames_since_emit: HOLD_FRAMES,
            pulse_this_frame: false,
        }
    }
}

// ── Input parsing + feature extraction ───────────────────────────────────────

impl MatchNode {
    /// Smart feature extractor. Tries landmark-JSON path first (bbox-normalized),
    /// then flat array → vector, then object → sorted-key flatten, then nested
    /// → DFS numeric leaves sorted by dotted path. Returns `None` on
    /// unsupported / empty / non-JSON input. Sets `self.banner` to describe
    /// why when extraction fails.
    fn extract_features(&mut self, json_str: &str) -> Option<Vec<f32>> {
        let v: serde_json::Value = match serde_json::from_str(json_str) {
            Ok(v) => v,
            Err(_) => { self.banner = "Invalid JSON".into(); return None; }
        };

        // Path A: tracker landmark JSON → bbox-normalize.
        if let Some(features) = extract_landmark_features(&v) {
            if matches!(self.schema, Schema::Unknown) {
                self.schema = Schema::from_landmark_count(features.len() / 2);
            }
            self.banner.clear();
            return Some(features);
        }

        // Path B: flat array of numbers.
        if let Some(arr) = v.as_array() {
            if !arr.is_empty() && arr.iter().all(|x| x.is_number()) {
                let features: Vec<f32> = arr.iter()
                    .map(|x| x.as_f64().unwrap_or(0.0) as f32)
                    .collect();
                if matches!(self.schema, Schema::Unknown) {
                    self.schema = Schema::Vector(features.len());
                }
                self.banner.clear();
                return Some(features);
            }
        }

        // Path C: flat object of numbers → sorted-key flatten.
        if let Some(obj) = v.as_object() {
            if !obj.is_empty() && obj.values().all(|x| x.is_number()) {
                let mut pairs: Vec<(String, f32)> = obj.iter()
                    .map(|(k, x)| (k.clone(), x.as_f64().unwrap_or(0.0) as f32))
                    .collect();
                pairs.sort_by(|a, b| a.0.cmp(&b.0));
                let features: Vec<f32> = pairs.into_iter().map(|(_, v)| v).collect();
                if matches!(self.schema, Schema::Unknown) {
                    self.schema = Schema::Object(features.len());
                }
                self.banner.clear();
                return Some(features);
            }
        }

        // Path D: nested structure → DFS numeric leaves sorted by path.
        let mut leaves: Vec<(String, f32)> = Vec::new();
        collect_numeric_leaves(&v, "", &mut leaves);
        if !leaves.is_empty() {
            leaves.sort_by(|a, b| a.0.cmp(&b.0));
            let features: Vec<f32> = leaves.into_iter().map(|(_, v)| v).collect();
            if matches!(self.schema, Schema::Unknown) {
                self.schema = Schema::Nested(features.len());
            }
            self.banner.clear();
            return Some(features);
        }

        self.banner = "Unsupported input shape".into();
        None
    }

    /// Compute per-class probabilities and top-1 (label, conf). Returns
    /// zeros if no classes have centroids yet.
    fn classify(&self, features: &[f32]) -> (Vec<f32>, String, f32) {
        let mut probs = vec![0.0f32; self.classes.len()];
        if self.classes.is_empty() { return (probs, String::new(), 0.0); }

        let mut any_ready = false;
        for (i, c) in self.classes.iter().enumerate() {
            if c.centroid.len() != features.len() { continue; }
            any_ready = true;
            probs[i] = match self.metric {
                Metric::Cosine => cosine_similarity(features, &c.centroid),
                Metric::L2     => -l2_distance(features, &c.centroid),
            };
        }
        if !any_ready { return (probs, String::new(), 0.0); }

        // Softmax → probabilities in [0,1] summing to 1.
        let max = probs.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let mut exp_sum = 0.0f32;
        for p in &mut probs {
            *p = (*p - max).exp();
            exp_sum += *p;
        }
        if exp_sum > 0.0 { for p in &mut probs { *p /= exp_sum; } }

        let (top_i, &top_p) = probs.iter().enumerate().fold(
            (0usize, &0.0f32),
            |(bi, bv), (i, v)| if v > bv { (i, v) } else { (bi, bv) },
        );
        let label = self.classes.get(top_i).map(|c| c.name.clone()).unwrap_or_default();
        (probs, label, top_p)
    }

    /// Compute whether to pulse the Matched trigger this frame. Must be
    /// called at most once per frame. Updates hysteresis bookkeeping.
    fn update_matched_trigger(&mut self) {
        self.frames_since_emit = self.frames_since_emit.saturating_add(1);
        let should_pulse = !self.last_label.is_empty()
            && self.last_label != self.last_emitted_label
            && self.last_conf > self.match_threshold
            && self.frames_since_emit >= HOLD_FRAMES;
        if should_pulse {
            self.last_emitted_label = self.last_label.clone();
            self.frames_since_emit = 0;
            self.pulse_this_frame = true;
        } else {
            self.pulse_this_frame = false;
        }
    }

    /// Average the capture buffer into one sample, push into the active class,
    /// recompute centroid, clear buffer.
    fn commit_capture(&mut self) {
        if self.capture_buf.is_empty() { self.capturing = false; return; }
        let dim = self.capture_buf[0].len();
        let mut avg = vec![0.0f32; dim];
        let n = self.capture_buf.len() as f32;
        for s in &self.capture_buf {
            if s.len() != dim { continue; }
            for i in 0..dim { avg[i] += s[i]; }
        }
        for v in &mut avg { *v /= n; }

        if let Some(class) = self.classes.get_mut(self.active_class) {
            class.samples.push(avg);
            class.recompute_centroid();
        }

        self.capture_buf.clear();
        self.capturing = false;
    }

    /// Hard reset: wipe classes, unlock schema.
    fn reset_all(&mut self) {
        self.classes.clear();
        self.schema = Schema::Unknown;
        self.active_class = 0;
        self.last_label.clear();
        self.last_conf = 0.0;
        self.last_probs.clear();
        self.last_emitted_label.clear();
        self.frames_since_emit = HOLD_FRAMES;
        self.pulse_this_frame = false;
        self.banner.clear();
    }
}

// ── Standalone feature extractors ────────────────────────────────────────────

/// Landmark JSON path: recognises `body_landmark` / `hand_landmark` / `face_landmark`
/// entries, picks the first detection index, normalises by bbox of visible
/// joints. Returns None if no landmark entries present.
fn extract_landmark_features(v: &serde_json::Value) -> Option<Vec<f32>> {
    let items = v.as_array()?;
    let mut primary_idx: Option<i64> = None;
    let mut kps: Vec<(String, f32, f32)> = Vec::new();

    for item in items {
        let ty = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let is_landmark = ty == "body_landmark"
            || ty == "hand_landmark"
            || ty == "face_landmark";
        if !is_landmark { continue; }

        let idx = item.get("body").or_else(|| item.get("face")).or_else(|| item.get("hand"))
            .and_then(|x| x.as_i64());
        match (primary_idx, idx) {
            (None, Some(i)) => { primary_idx = Some(i); }
            (Some(p), Some(i)) if p != i => { continue; }
            _ => {}
        }

        let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if name.is_empty() { continue; }
        let x = item.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        let y = item.get("y").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
        kps.push((name, x, y));
    }

    if kps.is_empty() { return None; }

    kps.sort_by(|a, b| a.0.cmp(&b.0));

    let n = kps.len() as f32;
    let (cx, cy) = kps.iter().fold((0.0f32, 0.0f32), |(sx, sy), (_, x, y)| (sx + x, sy + y));
    let (cx, cy) = (cx / n, cy / n);
    let (min_x, min_y, max_x, max_y) = kps.iter().fold(
        (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY),
        |(mix, miy, max, may), (_, x, y)| (mix.min(*x), miy.min(*y), max.max(*x), may.max(*y)),
    );
    let diag = ((max_x - min_x).powi(2) + (max_y - min_y).powi(2)).sqrt().max(1e-6);

    let mut vec = Vec::with_capacity(kps.len() * 2);
    for (_, x, y) in &kps {
        vec.push((x - cx) / diag);
        vec.push((y - cy) / diag);
    }
    Some(vec)
}

/// DFS-flatten a JSON value into (path, numeric-leaf) pairs. Objects are
/// traversed in key order (sorting happens after collection in the caller);
/// arrays get `[i]` suffixes; only number leaves are kept.
fn collect_numeric_leaves(v: &serde_json::Value, path: &str, out: &mut Vec<(String, f32)>) {
    match v {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                out.push((path.to_string(), f as f32));
            }
        }
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                let new_path = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                collect_numeric_leaves(v, &new_path, out);
            }
        }
        serde_json::Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                let new_path = if path.is_empty() { format!("[{i}]") } else { format!("{path}[{i}]") };
                collect_numeric_leaves(v, &new_path, out);
            }
        }
        _ => {}
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na  += x * x;
        nb  += y * y;
    }
    let denom = (na.sqrt() * nb.sqrt()).max(1e-9);
    dot / denom
}

fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        let d = x - y;
        sum += d * d;
    }
    sum.sqrt()
}

// ── NodeBehavior impl ────────────────────────────────────────────────────────

impl NodeBehavior for MatchNode {
    fn title(&self)      -> &str    { "Match" }
    // Keep the legacy tag so saved projects keep loading.
    fn type_tag(&self)   -> &str    { "pose_match" }
    fn color_hint(&self) -> [u8; 3] { [180, 120, 230] }
    fn min_width(&self)  -> Option<f32> { Some(240.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("JSON",    PortKind::Text),
            PortDef::new("Capture", PortKind::Trigger),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Label",   PortKind::Text),
            PortDef::new("Conf",    PortKind::Normalized),
            PortDef::new("Probs",   PortKind::Text),
            PortDef::new("Matched", PortKind::Trigger),
        ]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // The heavy path (classify + pulse) runs in render_with_context after
        // the ML image loop. Here we just echo last-computed state. The
        // temp-data injection in app/mod.rs overwrites these values after
        // render with the fresh ones.
        let probs_json = serde_json::to_string(
            &self.classes.iter().zip(self.last_probs.iter())
                .map(|(c, p)| (c.name.clone(), *p))
                .collect::<Vec<_>>()
        ).unwrap_or_else(|_| "[]".into());

        vec![
            (0, PortValue::Text(self.last_label.clone())),
            (1, PortValue::Float(self.last_conf)),
            (2, PortValue::Text(probs_json)),
            (3, PortValue::Float(if self.pulse_this_frame { 1.0 } else { 0.0 })),
        ]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<MatchNode>(state.clone()) {
            *self = loaded;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim     = egui::Color32::from_rgb(110, 110, 120);
        let green   = egui::Color32::from_rgb(80, 210, 120);
        let red     = egui::Color32::from_rgb(220, 110, 110);

        // ── Read live input + capture trigger ────────────────────────────────
        let json_val = crate::graph::Graph::static_input_value(
            ctx.connections, ctx.values, node_id, 0,
        );
        let trigger_val = crate::graph::Graph::static_input_value(
            ctx.connections, ctx.values, node_id, 1,
        );

        // Extract live features + classify.
        let live_features = if let PortValue::Text(ref s) = json_val {
            if s.is_empty() { None } else { self.extract_features(s) }
        } else { None };

        // Guard against dim mismatch after schema lock — if the first captured
        // sample's dim differs from live features, skip this frame and warn.
        let locked_dim = self.classes.iter().find_map(|c| (!c.centroid.is_empty()).then_some(c.centroid.len()));
        let dim_ok = match (locked_dim, live_features.as_ref()) {
            (Some(d), Some(f)) => f.len() == d,
            _ => true,
        };
        if !dim_ok {
            self.banner = format!(
                "Input dim mismatch ({} vs locked {})",
                live_features.as_ref().map(|f| f.len()).unwrap_or(0),
                locked_dim.unwrap_or(0),
            );
        }

        if dim_ok {
            if let Some(ref f) = live_features {
                let (probs, label, conf) = self.classify(f);
                self.last_probs = probs;
                self.last_label = label;
                self.last_conf  = conf;
            }
        }

        // Hysteresis-gated Matched trigger for this frame.
        self.update_matched_trigger();

        // External capture trigger on rising edge.
        let trig_now = if let PortValue::Float(t) = trigger_val { t } else { 0.0 };
        let trig_rising = trig_now > 0.5 && self.last_trigger <= 0.5;
        self.last_trigger = trig_now;
        if trig_rising && !self.classes.is_empty() { self.capturing = true; }

        // Accumulate capture frames.
        if self.capturing {
            if let (true, Some(f)) = (dim_ok, live_features.clone()) {
                self.capture_buf.push(f);
            }
            if self.capture_buf.len() >= self.capture_buffer_frames as usize {
                self.commit_capture();
            }
        }

        // Push outputs into egui temp data — app/mod.rs injects them into
        // the values map after render, before the next Dynamic re-eval pass.
        let probs_json = serde_json::to_string(
            &self.classes.iter().zip(self.last_probs.iter())
                .map(|(c, p)| (c.name.clone(), *p))
                .collect::<Vec<_>>()
        ).unwrap_or_else(|_| "[]".into());
        let pulse_value: f32 = if self.pulse_this_frame { 1.0 } else { 0.0 };
        ui.ctx().data_mut(|d| {
            d.insert_temp(egui::Id::new(("pm_label",   node_id)), self.last_label.clone());
            d.insert_temp(egui::Id::new(("pm_conf",    node_id)), self.last_conf);
            d.insert_temp(egui::Id::new(("pm_probs",   node_id)), probs_json);
            d.insert_temp(egui::Id::new(("pm_matched", node_id)), pulse_value);
        });

        // ── Input ports ──────────────────────────────────────────────────────
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
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 1, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Trigger,
            );
            ui.label(RichText::new("Capture").small().color(dim));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Schema + metric ──────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Schema:").small().color(dim));
            let lock = !self.classes.is_empty();
            let schema_lbl = format!(
                "{}{}",
                self.schema.label(),
                if lock { " (locked)" } else { " (auto)" }
            );
            ui.label(RichText::new(schema_lbl).small());
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Metric:").small().color(dim));
            egui::ComboBox::from_id_salt(egui::Id::new(("pm_metric", node_id)))
                .selected_text(match self.metric { Metric::Cosine => "Cosine", Metric::L2 => "L2" })
                .width(80.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(self.metric == Metric::Cosine, "Cosine").clicked() {
                        self.metric = Metric::Cosine;
                    }
                    if ui.selectable_label(self.metric == Metric::L2, "L2").clicked() {
                        self.metric = Metric::L2;
                    }
                });
        });

        if !self.banner.is_empty() {
            ui.label(RichText::new(&self.banner).small().color(red));
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Classes list ─────────────────────────────────────────────────────
        ui.label(RichText::new("Classes").small().color(dim));
        let mut delete_idx: Option<usize> = None;
        let mut clear_samples_idx: Option<usize> = None;
        let mut select_idx: Option<usize> = None;
        let mut commit_rename = false;

        for (i, class) in self.classes.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let is_active = i == self.active_class;
                let marker = if is_active { "●" } else { "○" };
                let swatch_color = egui::Color32::from_rgb(class.color[0], class.color[1], class.color[2]);
                if ui.selectable_label(is_active, RichText::new(marker).color(swatch_color)).clicked() {
                    select_idx = Some(i);
                }

                let editing = matches!(self.rename_buf, Some((idx, _)) if idx == i);
                if editing {
                    if let Some((_, ref mut buf)) = self.rename_buf {
                        let r = ui.add(egui::TextEdit::singleline(buf)
                            .desired_width(120.0)
                            .font(egui::TextStyle::Small));
                        if r.lost_focus() {
                            let trimmed = buf.trim();
                            if !trimmed.is_empty() { class.name = trimmed.to_string(); }
                            commit_rename = true;
                        }
                    }
                } else {
                    let lbl = ui.add(egui::Label::new(RichText::new(&class.name).small())
                        .sense(egui::Sense::click()));
                    if lbl.double_clicked() {
                        self.rename_buf = Some((i, class.name.clone()));
                    }
                }

                ui.label(RichText::new(format!("({})", class.samples.len())).small().color(dim));

                if ui.small_button("Clear").clicked() { clear_samples_idx = Some(i); }
                if ui.small_button("×").clicked()     { delete_idx = Some(i); }
            });
        }

        if commit_rename { self.rename_buf = None; }
        if let Some(i) = select_idx { self.active_class = i; }
        if let Some(i) = clear_samples_idx {
            if let Some(c) = self.classes.get_mut(i) {
                c.samples.clear();
                c.centroid.clear();
            }
        }
        if let Some(i) = delete_idx {
            self.classes.remove(i);
            if self.active_class >= self.classes.len() {
                self.active_class = self.classes.len().saturating_sub(1);
            }
        }

        ui.horizontal(|ui| {
            if ui.small_button("+ Add class").clicked() {
                let color = CLASS_PALETTE[self.classes.len() % CLASS_PALETTE.len()];
                let name = format!("Class {}", self.classes.len() + 1);
                self.classes.push(MatchClass::new(name, color));
                self.active_class = self.classes.len() - 1;
            }
            if ui.small_button("Reset all").on_hover_text("Clear classes and unlock schema").clicked() {
                self.reset_all();
            }
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Capture ──────────────────────────────────────────────────────────
        let can_capture = !self.classes.is_empty() && live_features.is_some() && dim_ok && !self.capturing;
        let btn_text = if self.capturing {
            format!("Capturing {}/{}", self.capture_buf.len(), self.capture_buffer_frames)
        } else if self.classes.is_empty() {
            "Capture (add a class)".into()
        } else if live_features.is_none() {
            "Capture (no input)".into()
        } else if !dim_ok {
            "Capture (dim mismatch)".into()
        } else if let Some(c) = self.classes.get(self.active_class) {
            format!("Capture → {}", c.name)
        } else {
            "Capture".into()
        };
        let btn = egui::Button::new(RichText::new(btn_text).strong())
            .min_size(egui::vec2(ui.available_width(), 26.0));
        let r = ui.add_enabled(can_capture, btn);
        if r.clicked() { self.capturing = true; }

        ui.horizontal(|ui| {
            ui.label(RichText::new("Rate:").small().color(dim));
            let mut rate = self.capture_buffer_frames as i32;
            if ui.add(egui::DragValue::new(&mut rate).range(1..=60)).changed() {
                self.capture_buffer_frames = rate.max(1) as u32;
            }
            ui.label(RichText::new("frames").small().color(dim));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Live readout + threshold ─────────────────────────────────────────
        if !self.last_label.is_empty() {
            ui.horizontal(|ui| {
                let active = self.classes.iter().find(|c| c.name == self.last_label);
                let col = active
                    .map(|c| egui::Color32::from_rgb(c.color[0], c.color[1], c.color[2]))
                    .unwrap_or(green);
                ui.label(RichText::new(&self.last_label).color(col).strong());
                ui.label(RichText::new(format!("{:.0}%", self.last_conf * 100.0)).small().color(dim));
            });
        } else {
            ui.label(RichText::new("Live: —").small().color(dim));
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Fire above:").small().color(dim));
            ui.add(egui::Slider::new(&mut self.match_threshold, 0.0..=1.0)
                .show_value(false));
            ui.label(RichText::new(format!("{:.0}%", self.match_threshold * 100.0))
                .small().color(dim));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports ─────────────────────────────────────────────────────
        crate::nodes::output_port_row(ui, "Label", &self.last_label, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Text);
        let conf_str = format!("{:.2}", self.last_conf);
        crate::nodes::output_port_row(ui, "Conf", &conf_str, node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Normalized);
        crate::nodes::output_port_row(ui, "Probs", "JSON", node_id, 2,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Text);
        let m_str = if self.pulse_this_frame { "▶" } else { "·" };
        crate::nodes::output_port_row(ui, "Matched", m_str, node_id, 3,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Trigger);
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("pose_match", |state| {
        let mut n = MatchNode::default();
        n.load_state(state);
        Box::new(n)
    });
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn body_landmark_json(points: &[(&str, f32, f32)]) -> String {
        let entries: Vec<String> = points.iter().map(|(name, x, y)| {
            format!(r#"{{"type":"body_landmark","body":0,"name":"{}","x":{},"y":{}}}"#, name, x, y)
        }).collect();
        format!("[{}]", entries.join(","))
    }

    #[test]
    fn features_are_translation_and_scale_invariant() {
        let mut n = MatchNode::default();
        let a = n.extract_features(&body_landmark_json(&[
            ("l_shoulder", 100.0, 100.0),
            ("r_shoulder", 200.0, 100.0),
            ("l_hip",      110.0, 300.0),
            ("r_hip",      210.0, 300.0),
        ])).expect("features A");

        let mut n2 = MatchNode::default();
        let b = n2.extract_features(&body_landmark_json(&[
            ("l_shoulder", 500.0, 500.0),
            ("r_shoulder", 700.0, 500.0),
            ("l_hip",      520.0, 900.0),
            ("r_hip",      720.0, 900.0),
        ])).expect("features B");

        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x - y).abs() < 1e-4, "expected translation/scale invariance: {x} vs {y}");
        }
    }

    #[test]
    fn captured_pose_matches_itself_with_high_confidence() {
        let mut n = MatchNode::default();
        n.classes.push(MatchClass::new("A", [0, 0, 0]));
        n.classes.push(MatchClass::new("B", [0, 0, 0]));
        n.active_class = 0;

        let pose_a = body_landmark_json(&[
            ("a", 10.0, 10.0), ("b", 20.0, 10.0), ("c", 10.0, 30.0),
        ]);
        let pose_b = body_landmark_json(&[
            ("a", 10.0, 10.0), ("b", 10.0, 30.0), ("c", 30.0, 10.0),
        ]);

        for _ in 0..3 {
            let f = n.extract_features(&pose_a).unwrap();
            n.capture_buf.push(f);
        }
        n.commit_capture();
        assert_eq!(n.classes[0].samples.len(), 1);

        n.active_class = 1;
        for _ in 0..3 {
            let f = n.extract_features(&pose_b).unwrap();
            n.capture_buf.push(f);
        }
        n.commit_capture();

        let live_a = n.extract_features(&pose_a).unwrap();
        let (probs, label, _conf) = n.classify(&live_a);
        assert_eq!(label, "A");
        assert!(probs[0] > probs[1]);

        let live_b = n.extract_features(&pose_b).unwrap();
        let (probs, label, _conf) = n.classify(&live_b);
        assert_eq!(label, "B");
        assert!(probs[1] > probs[0]);
    }

    #[test]
    fn save_and_load_round_trips_classes() {
        let mut n = MatchNode::default();
        n.classes.push(MatchClass::new("Wave", [200, 100, 50]));
        n.classes[0].samples.push(vec![0.1, 0.2, 0.3]);
        n.classes[0].recompute_centroid();
        n.metric = Metric::L2;
        n.schema = Schema::Pose33;
        n.match_threshold = 0.85;

        let state = n.save_state();
        let mut restored = MatchNode::default();
        restored.load_state(&state);

        assert_eq!(restored.classes.len(), 1);
        assert_eq!(restored.classes[0].name, "Wave");
        assert_eq!(restored.classes[0].samples, vec![vec![0.1, 0.2, 0.3]]);
        assert_eq!(restored.metric, Metric::L2);
        assert_eq!(restored.schema, Schema::Pose33);
        assert!((restored.match_threshold - 0.85).abs() < 1e-6);
    }

    #[test]
    fn schema_auto_detects_from_keypoint_count() {
        let mut n = MatchNode::default();
        assert_eq!(n.schema, Schema::Unknown);
        let pts: Vec<(&str, f32, f32)> = (0..21)
            .map(|i| (Box::leak(format!("kp{i}").into_boxed_str()) as &str, i as f32, 0.0))
            .collect();
        let _ = n.extract_features(&body_landmark_json(&pts));
        assert_eq!(n.schema, Schema::Hand21);
    }

    #[test]
    fn vector_input_classifies_correctly() {
        let mut n = MatchNode::default();
        n.classes.push(MatchClass::new("X", [0, 0, 0]));
        n.classes.push(MatchClass::new("Y", [0, 0, 0]));

        let vec_a = "[1.0, 0.0, 0.0, 0.0]";
        let vec_b = "[0.0, 1.0, 0.0, 0.0]";

        n.active_class = 0;
        let f = n.extract_features(vec_a).unwrap();
        assert_eq!(n.schema, Schema::Vector(4));
        n.capture_buf.push(f);
        n.commit_capture();

        n.active_class = 1;
        let f = n.extract_features(vec_b).unwrap();
        n.capture_buf.push(f);
        n.commit_capture();

        let live_a = n.extract_features(vec_a).unwrap();
        let (_probs, label, _) = n.classify(&live_a);
        assert_eq!(label, "X");

        let live_b = n.extract_features(vec_b).unwrap();
        let (_probs, label, _) = n.classify(&live_b);
        assert_eq!(label, "Y");
    }

    #[test]
    fn object_input_uses_sorted_key_order() {
        let mut n = MatchNode::default();
        // Same logical object in two different key orders — should produce
        // identical feature vectors because we sort by key.
        let a = n.extract_features(r#"{"b":2.0, "a":1.0, "c":3.0}"#).unwrap();
        let mut n2 = MatchNode::default();
        let b = n2.extract_features(r#"{"a":1.0, "c":3.0, "b":2.0}"#).unwrap();

        assert_eq!(a, vec![1.0, 2.0, 3.0]);
        assert_eq!(b, vec![1.0, 2.0, 3.0]);
        assert_eq!(n.schema, Schema::Object(3));
    }

    #[test]
    fn nested_input_dfs_flattens_numeric_leaves() {
        let mut n = MatchNode::default();
        let f = n.extract_features(r#"{"z":{"b":2.0}, "a":{"y":1.0}}"#).unwrap();
        // Paths: "a.y" < "z.b" lexically → [1.0, 2.0].
        assert_eq!(f, vec![1.0, 2.0]);
        assert_eq!(n.schema, Schema::Nested(2));
    }

    #[test]
    fn matched_trigger_pulses_once_per_label_change() {
        let mut n = MatchNode::default();
        n.match_threshold = 0.1; // keep threshold low so any softmax result fires
        n.classes.push(MatchClass::new("A", [0, 0, 0]));
        n.classes.push(MatchClass::new("B", [0, 0, 0]));

        n.active_class = 0;
        let f = n.extract_features("[1.0, 0.0]").unwrap();
        n.capture_buf.push(f);
        n.commit_capture();
        n.active_class = 1;
        let f = n.extract_features("[0.0, 1.0]").unwrap();
        n.capture_buf.push(f);
        n.commit_capture();

        // Drive A many frames in a row: first hit pulses, subsequent hits don't.
        let live_a = n.extract_features("[1.0, 0.0]").unwrap();
        let mut pulses_a = 0usize;
        for _ in 0..10 {
            let (_probs, label, conf) = n.classify(&live_a);
            n.last_label = label; n.last_conf = conf;
            n.update_matched_trigger();
            if n.pulse_this_frame { pulses_a += 1; }
        }
        assert_eq!(pulses_a, 1, "expected exactly 1 pulse for sustained A");

        // Switch to B: should pulse once when the label flips (respecting HOLD_FRAMES).
        let live_b = n.extract_features("[0.0, 1.0]").unwrap();
        let mut pulses_b = 0usize;
        for _ in 0..10 {
            let (_probs, label, conf) = n.classify(&live_b);
            n.last_label = label; n.last_conf = conf;
            n.update_matched_trigger();
            if n.pulse_this_frame { pulses_b += 1; }
        }
        assert_eq!(pulses_b, 1, "expected exactly 1 pulse when label flips to B");
    }
}
