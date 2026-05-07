//! Regress — N→M weighted-kNN regression node.
//!
//! Sibling of [`MatchNode`](crate::ml::match_node::MatchNode): where Match is
//! a discrete classifier (JSON in → label out), Regress is a continuous
//! interpolator (N float inputs + M float targets in → M float outputs).
//!
//! Use case ("FluCoMa 2D corpus" pattern): wire a 2-D XY pad into the N
//! inputs and M synth/visual params into the M target ports. Dial the params
//! to a desired sound, hit Capture, move the XY puck, dial different params,
//! capture again, etc. The node remembers each (input, target) pair and at
//! runtime smoothly blends between them with weighted k-nearest-neighbours
//! (Shepard's inverse-distance weighting).
//!
//! Algorithm: for a query q, find k nearest training inputs by Euclidean (or
//! Manhattan) distance; weight each by 1/d (clamped by a small epsilon for
//! stability); emit the weighted average of their stored target vectors.
//! Exact-hit short-circuit: if any d < epsilon, return that example's
//! output directly (avoids NaN and gives perfect recall at training points).

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui::{self, RichText};
use crate::nodes::ScrollAreaExt;
use serde::{Deserialize, Serialize};

// ── Hyperparams ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Distance {
    Euclidean,
    Manhattan,
}
impl Default for Distance {
    fn default() -> Self { Self::Euclidean }
}

fn default_k() -> usize { 4 }
fn default_epsilon() -> f32 { 1e-4 }
fn default_capture_frames() -> u32 { 5 }

/// Frames after a Changed-trigger pulse before the next one is allowed —
/// prevents flicker when query is on a boundary between two near-tied examples.
const HOLD_FRAMES: u32 = 3;

// ── Example ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Example {
    pub name: String,
    pub input: Vec<f32>,
    pub output: Vec<f32>,
}

// ── Node ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressNode {
    pub n_inputs: usize,
    pub n_outputs: usize,
    pub examples: Vec<Example>,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default)]
    pub distance: Distance,
    #[serde(default = "default_epsilon")]
    pub epsilon: f32,
    /// Number of frames to average into one captured example. Mirrors
    /// MatchNode's "Rate" — soaks up sensor jitter at click time.
    #[serde(default = "default_capture_frames")]
    pub capture_buffer_frames: u32,

    // ── Transient (not persisted) ──
    #[serde(skip)] last_outputs: Vec<f32>,
    #[serde(skip)] last_conf: f32,
    #[serde(skip)] last_pulse: bool,
    #[serde(skip)] last_trigger: f32,
    #[serde(skip)] banner: String,
    /// One-shot flag set by the UI Capture button; consumed in next evaluate().
    #[serde(skip)] capture_armed: bool,
    /// Rolling capture buffers — filled while `capturing` is true.
    #[serde(skip)] capture_buf_inputs: Vec<Vec<f32>>,
    #[serde(skip)] capture_buf_targets: Vec<Vec<f32>>,
    #[serde(skip)] capturing: bool,
    /// Active rename UI state: which example index, and the in-flight buffer.
    #[serde(skip)] rename_buf: Option<(usize, String)>,
    /// Hysteresis bookkeeping for the Changed trigger.
    #[serde(skip)] last_nearest_idx: Option<usize>,
    #[serde(skip)] frames_since_emit: u32,
    /// Reveals the Distance / k / Rate panel when toggled. Transient; defaults
    /// closed so non-ML users see only the action surface.
    #[serde(skip)] show_advanced: bool,
    /// Reveals the per-example list (with rename/update/delete). Collapsed by
    /// default — examples header just shows the count and Clear all, mirroring
    /// Match's count-first vibe.
    #[serde(skip)] show_examples: bool,
    /// One-frame pulse after `commit_capture` runs; consumed by `evaluate`
    /// to emit the `Captured` trigger output.
    #[serde(skip)] last_captured_pulse: bool,
}

impl Default for RegressNode {
    fn default() -> Self {
        Self {
            n_inputs: 2,
            n_outputs: 2,
            examples: Vec::new(),
            k: default_k(),
            distance: Distance::default(),
            epsilon: default_epsilon(),
            capture_buffer_frames: default_capture_frames(),
            last_outputs: vec![0.0; 2],
            last_conf: 0.0,
            last_pulse: false,
            last_trigger: 0.0,
            banner: String::new(),
            capture_armed: false,
            capture_buf_inputs: Vec::new(),
            capture_buf_targets: Vec::new(),
            capturing: false,
            rename_buf: None,
            last_nearest_idx: None,
            frames_since_emit: HOLD_FRAMES,
            show_advanced: false,
            show_examples: false,
            last_captured_pulse: false,
        }
    }
}

// ── Algorithm ────────────────────────────────────────────────────────────────

impl RegressNode {
    /// Distance between two equal-length vectors. Caller guarantees lengths match.
    fn dist(&self, a: &[f32], b: &[f32]) -> f32 {
        match self.distance {
            Distance::Euclidean => {
                let mut s = 0.0f32;
                for (x, y) in a.iter().zip(b.iter()) {
                    let d = x - y;
                    s += d * d;
                }
                s.sqrt()
            }
            Distance::Manhattan => {
                let mut s = 0.0f32;
                for (x, y) in a.iter().zip(b.iter()) {
                    s += (x - y).abs();
                }
                s
            }
        }
    }

    /// All examples whose input dim matches the query, sorted ascending by
    /// distance. Empty if no examples or all dim-mismatched.
    pub fn nearest_neighbours(&self, query: &[f32]) -> Vec<(usize, f32)> {
        let mut dists: Vec<(usize, f32)> = self.examples.iter().enumerate()
            .filter(|(_, ex)| ex.input.len() == query.len())
            .map(|(i, ex)| (i, self.dist(&ex.input, query)))
            .collect();
        dists.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        dists
    }

    /// IDW blend over the k nearest of a precomputed neighbour list.
    /// Caller is responsible for empty-list handling.
    fn predict_from(&self, nbrs: &[(usize, f32)]) -> Vec<f32> {
        let k_eff = self.k.max(1).min(nbrs.len());
        let nearest = &nbrs[..k_eff];

        if nearest[0].1 < self.epsilon {
            return self.examples[nearest[0].0].output.clone();
        }

        let mut out = vec![0.0f32; self.n_outputs];
        let mut wsum = 0.0f32;
        for (i, d) in nearest {
            let w = 1.0 / d.max(self.epsilon);
            let ex_out = &self.examples[*i].output;
            for j in 0..self.n_outputs.min(ex_out.len()) {
                out[j] += w * ex_out[j];
            }
            wsum += w;
        }
        if wsum > 0.0 {
            for v in out.iter_mut() { *v /= wsum; }
        }
        out
    }

    /// Public entry point used by tests.
    pub fn predict(&self, query: &[f32]) -> Vec<f32> {
        if self.examples.is_empty() {
            return vec![0.0; self.n_outputs];
        }
        let nbrs = self.nearest_neighbours(query);
        if nbrs.is_empty() {
            return self.last_outputs.clone();
        }
        self.predict_from(&nbrs)
    }

    /// Average the multi-frame capture buffers into one example, append to
    /// the dataset, and clear the buffers.
    fn commit_capture(&mut self) {
        if self.capture_buf_inputs.is_empty() {
            self.capturing = false;
            return;
        }
        let n = self.capture_buf_inputs.len() as f32;
        let dim_in = self.capture_buf_inputs[0].len();
        let dim_out = if self.capture_buf_targets.is_empty() { 0 }
                      else { self.capture_buf_targets[0].len() };

        let mut avg_in = vec![0.0f32; dim_in];
        for s in &self.capture_buf_inputs {
            for i in 0..dim_in.min(s.len()) { avg_in[i] += s[i]; }
        }
        for v in &mut avg_in { *v /= n; }

        let mut avg_out = vec![0.0f32; dim_out];
        for s in &self.capture_buf_targets {
            for i in 0..dim_out.min(s.len()) { avg_out[i] += s[i]; }
        }
        for v in &mut avg_out { *v /= n; }

        let name = format!("ex{}", self.examples.len() + 1);
        self.examples.push(Example { name, input: avg_in, output: avg_out });
        self.capture_buf_inputs.clear();
        self.capture_buf_targets.clear();
        self.capturing = false;
        self.banner.clear();
        self.last_captured_pulse = true;
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn read_float(inputs: &[PortValue], idx: usize) -> f32 {
    match inputs.get(idx) {
        Some(PortValue::Float(f)) => *f,
        Some(PortValue::Text(s)) => s.parse::<f32>().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn live_float(ctx: &RenderContext, node_id: crate::graph::NodeId, port_idx: usize) -> f32 {
    match crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, port_idx) {
        PortValue::Float(f) => f,
        PortValue::Text(s) => s.parse::<f32>().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn fmt_vec(v: &[f32]) -> String {
    let parts: Vec<String> = v.iter().map(|x| format!("{x:.2}")).collect();
    parts.join(",")
}

// ── NodeBehavior ─────────────────────────────────────────────────────────────

impl NodeBehavior for RegressNode {
    fn title(&self)      -> &str    { "Regress" }
    fn type_tag(&self)   -> &str    { "regress" }
    fn color_hint(&self) -> [u8; 3] { [140, 180, 230] }
    fn min_width(&self)  -> Option<f32> { Some(240.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        let mut v = Vec::with_capacity(self.n_inputs + self.n_outputs + 1);
        for i in 0..self.n_inputs {
            v.push(PortDef::dynamic(format!("in{i}"), PortKind::Number));
        }
        for j in 0..self.n_outputs {
            v.push(PortDef::dynamic(format!("target{j}"), PortKind::Number));
        }
        v.push(PortDef::new("Capture", PortKind::Trigger));
        v
    }

    fn outputs(&self) -> Vec<PortDef> {
        let mut v = Vec::with_capacity(self.n_outputs + 3);
        for j in 0..self.n_outputs {
            v.push(PortDef::dynamic(format!("out{j}"), PortKind::Number));
        }
        v.push(PortDef::new("Conf",     PortKind::Normalized));
        v.push(PortDef::new("Changed",  PortKind::Trigger));
        v.push(PortDef::new("Captured", PortKind::Trigger));
        v
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Live query.
        let query: Vec<f32> = (0..self.n_inputs).map(|i| read_float(inputs, i)).collect();

        // Capture: rising edge on the trigger port OR one-shot UI flag.
        let trig_idx = self.n_inputs + self.n_outputs;
        let trig_now = read_float(inputs, trig_idx);
        let trig_rising = trig_now > 0.5 && self.last_trigger <= 0.5;
        self.last_trigger = trig_now;
        let start_capture = (trig_rising || self.capture_armed) && !self.capturing;
        self.capture_armed = false;
        if start_capture {
            self.capturing = true;
            self.capture_buf_inputs.clear();
            self.capture_buf_targets.clear();
        }
        if self.capturing {
            let targets: Vec<f32> = (0..self.n_outputs)
                .map(|j| read_float(inputs, self.n_inputs + j))
                .collect();
            self.capture_buf_inputs.push(query.clone());
            self.capture_buf_targets.push(targets);
            if self.capture_buf_inputs.len() >= self.capture_buffer_frames.max(1) as usize {
                self.commit_capture();
            }
        }

        // One pass through the corpus → derive prediction, confidence, change.
        let nbrs = self.nearest_neighbours(&query);
        let nearest_idx = nbrs.first().map(|(i, _)| *i);
        let min_dist = nbrs.first().map(|(_, d)| *d);

        self.last_outputs = if self.examples.is_empty() {
            vec![0.0; self.n_outputs]
        } else if nbrs.is_empty() {
            self.last_outputs.clone()
        } else {
            self.predict_from(&nbrs)
        };

        // Confidence: 1/(1+d) — bounded [0, 1], 1.0 at exact hits, decays
        // smoothly with distance. Scale-dependent (a v2 normalize step would
        // make this comparable across projects).
        self.last_conf = match min_dist {
            Some(d) => 1.0 / (1.0 + d),
            None => 0.0,
        };

        // Active-changed trigger: pulse one frame when nearest example flips,
        // gated by HOLD_FRAMES to avoid flicker on near-ties.
        self.frames_since_emit = self.frames_since_emit.saturating_add(1);
        let changed = nearest_idx.is_some()
            && nearest_idx != self.last_nearest_idx
            && self.frames_since_emit >= HOLD_FRAMES;
        if changed {
            self.last_nearest_idx = nearest_idx;
            self.frames_since_emit = 0;
            self.last_pulse = true;
        } else {
            self.last_pulse = false;
        }

        // Emit M outputs + Conf + Changed + Captured.
        let captured_now = self.last_captured_pulse;
        self.last_captured_pulse = false;
        let mut out = Vec::with_capacity(self.n_outputs + 3);
        for (j, v) in self.last_outputs.iter().enumerate() {
            out.push((j, PortValue::Float(*v)));
        }
        out.push((self.n_outputs,     PortValue::Float(self.last_conf)));
        out.push((self.n_outputs + 1, PortValue::Float(if self.last_pulse { 1.0 } else { 0.0 })));
        out.push((self.n_outputs + 2, PortValue::Float(if captured_now    { 1.0 } else { 0.0 })));
        out
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<RegressNode>(state.clone()) {
            *self = loaded;
            self.last_outputs = vec![0.0; self.n_outputs];
            self.frames_since_emit = HOLD_FRAMES;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim     = egui::Color32::from_rgb(110, 110, 120);
        let red     = egui::Color32::from_rgb(220, 110, 110);

        const NODE_W: f32 = 240.0;
        ui.set_min_width(NODE_W);
        ui.set_max_width(NODE_W);

        let dims_locked = !self.examples.is_empty();
        let lock_tip = "Clear examples to change the input/target structure.";

        // ── Header: just the gear toggle, right-aligned ──────────────────────
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.show_advanced { "⚙ ▴" } else { "⚙" };
                if ui.small_button(label).on_hover_text("Advanced settings").clicked() {
                    self.show_advanced = !self.show_advanced;
                }
            });
        });

        // ── Advanced panel (hidden by default) ───────────────────────────────
        if self.show_advanced {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.label(RichText::new("Advanced").small().color(dim));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Distance:").small().color(dim));
                        egui::ComboBox::from_id_salt(egui::Id::new(("rg_dist", node_id)))
                            .selected_text(match self.distance {
                                Distance::Euclidean => "Euclidean",
                                Distance::Manhattan => "Manhattan",
                            })
                            .width(110.0)
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(self.distance == Distance::Euclidean, "Euclidean")
                                    .on_hover_text("Smooth blend in any direction (default).").clicked()
                                {
                                    self.distance = Distance::Euclidean;
                                }
                                if ui.selectable_label(self.distance == Distance::Manhattan, "Manhattan")
                                    .on_hover_text("Treats each input dim independently. Rarely needed.").clicked()
                                {
                                    self.distance = Distance::Manhattan;
                                }
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Neighbours (k):").small().color(dim));
                        let mut k = self.k as i32;
                        if ui.add(egui::DragValue::new(&mut k).range(1..=32)).changed() {
                            self.k = k.max(1) as usize;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Capture rate:").small().color(dim));
                        let mut rate = self.capture_buffer_frames as i32;
                        if ui.add(egui::DragValue::new(&mut rate).range(1..=60)).changed() {
                            self.capture_buffer_frames = rate.max(1) as u32;
                        }
                        ui.label(RichText::new("frames").small().color(dim));
                    });
                });
            ui.add_space(4.0);
        }

        ui.separator();
        ui.add_space(4.0);

        // ── Input ports + per-row × + add button ─────────────────────────────
        let mut remove_input: Option<usize> = None;
        ui.label(RichText::new("inputs").small().color(dim));
        for i in 0..self.n_inputs {
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(
                    ui, node_id, i, true,
                    ctx.connections, ctx.port_positions,
                    ctx.dragging_from, ctx.pending_disconnects,
                    PortKind::Number,
                );
                ui.label(RichText::new(format!("in{i}")).small().color(dim));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add_enabled(!dims_locked, egui::Button::new("×").small());
                    let r = if dims_locked { r.on_hover_text(lock_tip) }
                            else            { r.on_hover_text("Remove this input") };
                    if r.clicked() && self.n_inputs > 1 { remove_input = Some(i); }
                });
            });
        }
        ui.horizontal(|ui| {
            let add_btn = ui.add_enabled(!dims_locked, egui::Button::new("+ add input").small());
            let add_btn = if dims_locked { add_btn.on_hover_text(lock_tip) }
                          else            { add_btn.on_hover_text("Add a new input dimension") };
            if add_btn.clicked() {
                // New input slot lands at old index n_inputs; targets + capture
                // shift down by 1, so disconnect everything from there onward
                // (their input port indices are about to change).
                let total_input_ports = self.n_inputs + self.n_outputs + 1;
                for p in self.n_inputs..total_input_ports {
                    ctx.pending_disconnects.push((node_id, p));
                }
                self.n_inputs += 1;
            }
        });

        ui.add_space(2.0);

        // ── Target ports + per-row × + add button ────────────────────────────
        let mut remove_target: Option<usize> = None;
        ui.label(RichText::new("targets").small().color(dim));
        for j in 0..self.n_outputs {
            let port_idx = self.n_inputs + j;
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(
                    ui, node_id, port_idx, true,
                    ctx.connections, ctx.port_positions,
                    ctx.dragging_from, ctx.pending_disconnects,
                    PortKind::Number,
                );
                ui.label(RichText::new(format!("target{j}")).small().color(dim));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = ui.add_enabled(!dims_locked, egui::Button::new("×").small());
                    let r = if dims_locked { r.on_hover_text(lock_tip) }
                            else            { r.on_hover_text("Remove this target") };
                    if r.clicked() && self.n_outputs > 1 { remove_target = Some(j); }
                });
            });
        }
        ui.horizontal(|ui| {
            let add_btn = ui.add_enabled(!dims_locked, egui::Button::new("+ add target").small());
            let add_btn = if dims_locked { add_btn.on_hover_text(lock_tip) }
                          else            { add_btn.on_hover_text("Add a new target/output dimension") };
            if add_btn.clicked() {
                // New target lands at old index n_outputs; capture port shifts
                // down by 1. (Output-side wires to Conf/Changed silently shift
                // — accepted since dim changes only happen during setup.)
                let old_capture_port = self.n_inputs + self.n_outputs;
                ctx.pending_disconnects.push((node_id, old_capture_port));
                self.n_outputs += 1;
                if self.last_outputs.len() != self.n_outputs {
                    self.last_outputs = vec![0.0; self.n_outputs];
                }
            }
        });

        // Apply pending input/target removals (deferred to outside the row loops).
        if let Some(i) = remove_input {
            let total_input_ports = self.n_inputs + self.n_outputs + 1;
            for p in i..total_input_ports {
                ctx.pending_disconnects.push((node_id, p));
            }
            self.n_inputs -= 1;
        }
        if let Some(j) = remove_target {
            let target_port = self.n_inputs + j;
            let total_input_ports = self.n_inputs + self.n_outputs + 1;
            for p in target_port..total_input_ports {
                ctx.pending_disconnects.push((node_id, p));
            }
            self.n_outputs -= 1;
            if self.last_outputs.len() != self.n_outputs {
                self.last_outputs = vec![0.0; self.n_outputs];
            }
        }

        ui.add_space(2.0);
        let cap_idx = self.n_inputs + self.n_outputs;
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, cap_idx, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Trigger,
            );
            ui.label(RichText::new("Capture").small().color(dim));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Examples header: collapsible (▸/▾) + Clear all ───────────────────
        ui.horizontal(|ui| {
            let arrow = if self.show_examples { "▾" } else { "▸" };
            let header = format!("{}  Examples ({})", arrow, self.examples.len());
            let resp = ui.add(
                egui::Label::new(RichText::new(header).small().color(dim))
                    .sense(egui::Sense::click())
            ).on_hover_text("Show / hide example list");
            if resp.clicked() { self.show_examples = !self.show_examples; }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.add_enabled(!self.examples.is_empty(), egui::Button::new("Clear all").small());
                if r.on_hover_text("Remove all examples and unlock structure").clicked() {
                    self.examples.clear();
                    self.last_nearest_idx = None;
                    self.banner.clear();
                }
            });
        });
        if self.examples.is_empty() {
            ui.label(RichText::new("Wire inputs and targets, then Capture.")
                .small().italics().color(dim));
        } else if self.show_examples {
            // Snapshot live values once for the per-row Update button.
            let live_inputs: Vec<f32> = (0..self.n_inputs)
                .map(|i| live_float(ctx, node_id, i)).collect();
            let live_targets: Vec<f32> = (0..self.n_outputs)
                .map(|j| live_float(ctx, node_id, self.n_inputs + j)).collect();

            let mut delete_idx: Option<usize> = None;
            let mut update_idx: Option<usize> = None;
            let mut commit_rename = false;

            egui::ScrollArea::vertical()
                .max_height(120.0)
                .auto_shrink([false, true])
                .show_pannable(ui, |ui| {
                    for (i, ex) in self.examples.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            let editing = matches!(self.rename_buf, Some((idx, _)) if idx == i);
                            if editing {
                                if let Some((_, ref mut buf)) = self.rename_buf {
                                    let r = ui.add(egui::TextEdit::singleline(buf)
                                        .desired_width(80.0)
                                        .font(egui::TextStyle::Small));
                                    if r.lost_focus() {
                                        let trimmed = buf.trim();
                                        if !trimmed.is_empty() { ex.name = trimmed.to_string(); }
                                        commit_rename = true;
                                    }
                                }
                            } else {
                                let preview = format!("[{}] → [{}]",
                                    fmt_vec(&ex.input), fmt_vec(&ex.output));
                                let lbl = ui.add(
                                    egui::Label::new(RichText::new(&ex.name).small().monospace())
                                        .sense(egui::Sense::click())
                                ).on_hover_text(preview);
                                if lbl.double_clicked() {
                                    self.rename_buf = Some((i, ex.name.clone()));
                                }
                            }
                            // Push the buttons to the right.
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text("Delete").clicked() {
                                    delete_idx = Some(i);
                                }
                                if ui.small_button("↻").on_hover_text("Update with current live inputs/targets").clicked() {
                                    update_idx = Some(i);
                                }
                            });
                        });
                    }
                });

            if commit_rename { self.rename_buf = None; }
            if let Some(i) = update_idx {
                if let Some(ex) = self.examples.get_mut(i) {
                    ex.input = live_inputs.clone();
                    ex.output = live_targets.clone();
                }
            }
            if let Some(i) = delete_idx {
                self.examples.remove(i);
                if let Some(last) = self.last_nearest_idx {
                    if last == i { self.last_nearest_idx = None; }
                    else if last > i { self.last_nearest_idx = Some(last - 1); }
                }
            }
        }

        ui.add_space(4.0);

        // ── Capture button + inline Undo (Rate lives in Advanced) ────────────
        ui.horizontal(|ui| {
            let undo_visible = !self.examples.is_empty() && !self.capturing;
            let undo_w = if undo_visible { 30.0 } else { 0.0 };
            let cap_w = (ui.available_width() - undo_w - 4.0).max(60.0);
            let cap_text = if self.capturing {
                format!("Capturing {}/{}", self.capture_buf_inputs.len(), self.capture_buffer_frames)
            } else {
                "Capture".into()
            };
            let cap_btn = egui::Button::new(RichText::new(cap_text).strong())
                .min_size(egui::vec2(cap_w, 26.0));
            if ui.add_enabled(!self.capturing, cap_btn).clicked() { self.capture_armed = true; }
            if undo_visible {
                let undo_btn = egui::Button::new("↶").min_size(egui::vec2(26.0, 26.0));
                if ui.add(undo_btn).on_hover_text("Undo last capture").clicked() {
                    self.examples.pop();
                    if let Some(last) = self.last_nearest_idx {
                        if last >= self.examples.len() { self.last_nearest_idx = None; }
                    }
                }
            }
        });

        if !self.banner.is_empty() {
            ui.label(RichText::new(&self.banner).small().color(red));
        }

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports: out0..out{M-1} + Conf + Changed ───────────────────
        for j in 0..self.n_outputs {
            let val = self.last_outputs.get(j).copied().unwrap_or(0.0);
            let val_str = format!("{val:.3}");
            crate::nodes::output_port_row(
                ui, &format!("out{j}"), &val_str, node_id, j,
                ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
                PortKind::Number,
            );
        }
        let conf_str = format!("{:.2}", self.last_conf);
        crate::nodes::output_port_row(
            ui, "Conf", &conf_str, node_id, self.n_outputs,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Normalized,
        );
        let chg_str = if self.last_pulse { "▶" } else { "·" };
        crate::nodes::output_port_row(
            ui, "Changed", chg_str, node_id, self.n_outputs + 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Trigger,
        );
        // Captured pulses for one frame after each commit_capture(). Useful for
        // chaining (auto-name, log, advance step, etc.).
        let cap_str = if self.last_captured_pulse { "▶" } else { "·" };
        crate::nodes::output_port_row(
            ui, "Captured", cap_str, node_id, self.n_outputs + 2,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Trigger,
        );
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("regress", |state| {
        let mut n = RegressNode::default();
        n.load_state(state);
        Box::new(n)
    });
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(name: &str, input: Vec<f32>, output: Vec<f32>) -> Example {
        Example { name: name.into(), input, output }
    }

    /// Build the full evaluate() inputs vector for an N=2, M=2 node:
    /// [in0, in1, target0, target1, capture]
    fn pv(in0: f32, in1: f32, t0: f32, t1: f32, cap: f32) -> Vec<PortValue> {
        vec![
            PortValue::Float(in0), PortValue::Float(in1),
            PortValue::Float(t0),  PortValue::Float(t1),
            PortValue::Float(cap),
        ]
    }

    #[test]
    fn untrained_returns_zeros() {
        let n = RegressNode::default();
        let out = n.predict(&[0.5, 0.5]);
        assert_eq!(out, vec![0.0, 0.0]);
    }

    #[test]
    fn single_example_returns_its_output_everywhere() {
        let mut n = RegressNode::default();
        n.examples.push(ex("a", vec![0.0, 0.0], vec![0.7, 0.3]));
        let out_at = n.predict(&[0.0, 0.0]);
        let out_far = n.predict(&[10.0, -5.0]);
        assert!((out_at[0] - 0.7).abs() < 1e-5);
        assert!((out_at[1] - 0.3).abs() < 1e-5);
        assert!((out_far[0] - 0.7).abs() < 1e-5);
        assert!((out_far[1] - 0.3).abs() < 1e-5);
    }

    #[test]
    fn exact_hit_returns_that_examples_output() {
        let mut n = RegressNode::default();
        n.examples.push(ex("a", vec![0.0, 0.0], vec![1.0, 0.0]));
        n.examples.push(ex("b", vec![1.0, 1.0], vec![0.0, 1.0]));
        let out = n.predict(&[0.0, 0.0]);
        assert!((out[0] - 1.0).abs() < 1e-5);
        assert!((out[1] - 0.0).abs() < 1e-5);
        let out = n.predict(&[1.0, 1.0]);
        assert!((out[0] - 0.0).abs() < 1e-5);
        assert!((out[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn midpoint_blends_two_examples_equally() {
        let mut n = RegressNode::default();
        n.examples.push(ex("a", vec![0.0, 0.0], vec![1.0, 0.0]));
        n.examples.push(ex("b", vec![1.0, 1.0], vec![0.0, 1.0]));
        let out = n.predict(&[0.5, 0.5]);
        assert!((out[0] - 0.5).abs() < 1e-3, "out[0] = {} expected ≈ 0.5", out[0]);
        assert!((out[1] - 0.5).abs() < 1e-3, "out[1] = {} expected ≈ 0.5", out[1]);
    }

    #[test]
    fn farther_neighbours_get_less_weight() {
        let mut n = RegressNode::default();
        n.examples.push(ex("a", vec![0.0], vec![1.0]));
        n.examples.push(ex("b", vec![1.0], vec![0.0]));
        n.examples.push(ex("c", vec![10.0], vec![100.0]));
        let out = n.predict(&[0.05]);
        assert!(out[0] > 0.5, "out = {}, expected closer to 1.0 than to 0 or 100", out[0]);
        assert!(out[0] < 5.0, "outlier should not dominate, got {}", out[0]);
    }

    #[test]
    fn capture_trigger_rising_edge_snapshots_once() {
        let mut n = RegressNode::default();
        n.capture_buffer_frames = 1; // commit immediately, one frame per example
        let _ = n.evaluate(&pv(0.1, 0.1, 0.9, 0.9, 0.0));
        assert_eq!(n.examples.len(), 0);
        let _ = n.evaluate(&pv(0.1, 0.1, 0.9, 0.9, 1.0));
        assert_eq!(n.examples.len(), 1);
        assert_eq!(n.examples[0].input, vec![0.1, 0.1]);
        assert_eq!(n.examples[0].output, vec![0.9, 0.9]);
        // Held high → no further capture (no rising edge, capturing already false).
        let _ = n.evaluate(&pv(0.2, 0.2, 0.8, 0.8, 1.0));
        let _ = n.evaluate(&pv(0.3, 0.3, 0.7, 0.7, 1.0));
        assert_eq!(n.examples.len(), 1);
        // Released and re-rising → one more example.
        let _ = n.evaluate(&pv(0.4, 0.4, 0.6, 0.6, 0.0));
        let _ = n.evaluate(&pv(0.4, 0.4, 0.6, 0.6, 1.0));
        assert_eq!(n.examples.len(), 2);
    }

    #[test]
    fn multi_frame_averaging_commits_after_buffer_full() {
        let mut n = RegressNode::default();
        n.capture_buffer_frames = 4;

        // Rising edge starts capture; first frame is buffered.
        let _ = n.evaluate(&pv(0.10, 0.10, 0.50, 0.50, 1.0));
        assert!(n.capturing);
        assert_eq!(n.examples.len(), 0);

        // Three more frames with varying values; commit on the 4th push.
        let _ = n.evaluate(&pv(0.20, 0.30, 0.60, 0.40, 0.0));
        let _ = n.evaluate(&pv(0.30, 0.20, 0.40, 0.60, 0.0));
        let _ = n.evaluate(&pv(0.40, 0.40, 0.50, 0.50, 0.0));

        assert!(!n.capturing, "should have committed after 4 frames");
        assert_eq!(n.examples.len(), 1);
        // Means: in0=(0.10+0.20+0.30+0.40)/4=0.25, in1=(0.10+0.30+0.20+0.40)/4=0.25
        // t0=(0.50+0.60+0.40+0.50)/4=0.50,  t1=(0.50+0.40+0.60+0.50)/4=0.50
        assert!((n.examples[0].input[0] - 0.25).abs() < 1e-5);
        assert!((n.examples[0].input[1] - 0.25).abs() < 1e-5);
        assert!((n.examples[0].output[0] - 0.50).abs() < 1e-5);
        assert!((n.examples[0].output[1] - 0.50).abs() < 1e-5);
    }

    #[test]
    fn dim_mismatch_after_resize_keeps_last_outputs() {
        let mut n = RegressNode::default();
        n.examples.push(ex("a", vec![0.5, 0.5], vec![0.42, 0.58]));
        n.last_outputs = vec![0.42, 0.58];
        let out = n.predict(&[0.1, 0.1, 0.1]);
        assert_eq!(out, vec![0.42, 0.58]);
    }

    #[test]
    fn save_load_round_trips_examples_and_hyperparams() {
        let mut n = RegressNode::default();
        n.n_inputs = 3;
        n.n_outputs = 4;
        n.k = 7;
        n.distance = Distance::Manhattan;
        n.capture_buffer_frames = 12;
        n.examples.push(ex("foo", vec![0.1, 0.2, 0.3], vec![1.0, 2.0, 3.0, 4.0]));
        n.examples.push(ex("bar", vec![0.4, 0.5, 0.6], vec![5.0, 6.0, 7.0, 8.0]));

        let state = n.save_state();
        let mut restored = RegressNode::default();
        restored.load_state(&state);

        assert_eq!(restored.n_inputs, 3);
        assert_eq!(restored.n_outputs, 4);
        assert_eq!(restored.k, 7);
        assert_eq!(restored.distance, Distance::Manhattan);
        assert_eq!(restored.capture_buffer_frames, 12);
        assert_eq!(restored.examples.len(), 2);
        assert_eq!(restored.examples[0].name, "foo");
        assert_eq!(restored.examples[0].input, vec![0.1, 0.2, 0.3]);
        assert_eq!(restored.examples[1].output, vec![5.0, 6.0, 7.0, 8.0]);
    }

    #[test]
    fn k_clamped_to_examples_len() {
        let mut n = RegressNode::default();
        n.k = 100;
        n.examples.push(ex("a", vec![0.0], vec![1.0]));
        n.examples.push(ex("b", vec![1.0], vec![0.0]));
        n.examples.push(ex("c", vec![2.0], vec![0.0]));
        let out = n.predict(&[0.5]);
        assert!(out[0] > 0.0 && out[0] < 1.0);
    }

    #[test]
    fn evaluate_emits_m_plus_three_outputs() {
        let mut n = RegressNode::default();
        n.n_outputs = 3;
        n.last_outputs = vec![0.0; 3];
        n.examples.push(ex("a", vec![0.0, 0.0], vec![0.1, 0.2, 0.3]));
        let inputs = vec![
            PortValue::Float(0.0), PortValue::Float(0.0),  // in0, in1
            PortValue::Float(0.0), PortValue::Float(0.0), PortValue::Float(0.0),  // target0..2
            PortValue::Float(0.0),  // capture
        ];
        let out = n.evaluate(&inputs);
        assert_eq!(out.len(), 6, "M=3 outputs + Conf + Changed + Captured = 6");
        let vals: Vec<f32> = out.iter().map(|(_, v)| match v { PortValue::Float(f) => *f, _ => 0.0 }).collect();
        // Exact-hit at training point → its output verbatim; Conf ≈ 1.0; Changed ≈ 1.0 (first nearest).
        assert!((vals[0] - 0.1).abs() < 1e-5);
        assert!((vals[1] - 0.2).abs() < 1e-5);
        assert!((vals[2] - 0.3).abs() < 1e-5);
        // Conf = 1/(1+0) = 1.0
        assert!((vals[3] - 1.0).abs() < 1e-5);
        // Changed pulses on first encounter (last_nearest_idx was None)
        assert!((vals[4] - 1.0).abs() < 1e-5);
        // Captured = 0.0 (no commit happened in this evaluate)
        assert!((vals[5] - 0.0).abs() < 1e-5);
    }

    #[test]
    fn captured_trigger_pulses_on_commit_then_resets() {
        let mut n = RegressNode::default();
        n.capture_buffer_frames = 1; // commit on first buffered frame

        // Frame 1: no trigger, no capture, Captured = 0.
        let out = n.evaluate(&pv(0.1, 0.1, 0.5, 0.5, 0.0));
        let captured_idx = n.n_outputs + 2;
        assert_eq!(out[captured_idx].1, PortValue::Float(0.0));

        // Frame 2: rising edge → commit happens THIS frame. Captured = 1.
        let out = n.evaluate(&pv(0.1, 0.1, 0.5, 0.5, 1.0));
        assert_eq!(n.examples.len(), 1);
        assert_eq!(out[captured_idx].1, PortValue::Float(1.0));

        // Frame 3: trigger held high, no new commit. Captured returns to 0.
        let out = n.evaluate(&pv(0.1, 0.1, 0.5, 0.5, 1.0));
        assert_eq!(out[captured_idx].1, PortValue::Float(0.0));
    }

    #[test]
    fn confidence_is_high_at_training_point_and_decays() {
        let mut n = RegressNode::default();
        n.examples.push(ex("a", vec![0.0, 0.0], vec![1.0, 1.0]));
        // At the training point: distance 0 → conf = 1/(1+0) = 1.
        let _ = n.evaluate(&pv(0.0, 0.0, 0.0, 0.0, 0.0));
        assert!((n.last_conf - 1.0).abs() < 1e-5);
        // Far away: distance ≫ 0 → conf < 0.5.
        let _ = n.evaluate(&pv(10.0, 10.0, 0.0, 0.0, 0.0));
        assert!(n.last_conf < 0.5, "expected low conf far from training, got {}", n.last_conf);
    }

    #[test]
    fn changed_trigger_pulses_once_per_flip() {
        let mut n = RegressNode::default();
        n.examples.push(ex("a", vec![0.0, 0.0], vec![1.0, 0.0]));
        n.examples.push(ex("b", vec![1.0, 1.0], vec![0.0, 1.0]));

        // Sit near A for many frames: only the first eval after each flip should pulse.
        let mut pulses_a = 0;
        for _ in 0..10 {
            let _ = n.evaluate(&pv(0.0, 0.0, 0.0, 0.0, 0.0));
            if n.last_pulse { pulses_a += 1; }
        }
        assert_eq!(pulses_a, 1, "sustained near A should pulse exactly once");

        // Now flip to B for many frames: one pulse, then quiet.
        let mut pulses_b = 0;
        for _ in 0..10 {
            let _ = n.evaluate(&pv(1.0, 1.0, 0.0, 0.0, 0.0));
            if n.last_pulse { pulses_b += 1; }
        }
        assert_eq!(pulses_b, 1, "flip to B should pulse exactly once");
    }

    #[test]
    fn update_overwrites_example_with_new_values() {
        let mut n = RegressNode::default();
        n.examples.push(ex("a", vec![0.0, 0.0], vec![0.1, 0.1]));
        // Simulate the per-row Update button: caller writes new (input, output)
        // into the existing example. Test the data flow only (UI is render-side).
        if let Some(e) = n.examples.get_mut(0) {
            e.input = vec![0.5, 0.5];
            e.output = vec![0.9, 0.9];
        }
        let out = n.predict(&[0.5, 0.5]);
        assert!((out[0] - 0.9).abs() < 1e-5);
        assert!((out[1] - 0.9).abs() < 1e-5);
    }

    #[test]
    fn rename_persists_through_save_load() {
        let mut n = RegressNode::default();
        n.examples.push(ex("ex1", vec![0.0, 0.0], vec![1.0, 1.0]));
        // Simulate the rename UI committing a new name.
        n.examples[0].name = "dark pad".into();

        let state = n.save_state();
        let mut restored = RegressNode::default();
        restored.load_state(&state);
        assert_eq!(restored.examples[0].name, "dark pad");
    }
}
