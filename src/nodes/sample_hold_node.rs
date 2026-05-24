//! Sample & Hold — captures a value on trigger, holds it until next trigger.
//!
//! Input 0: Value (any type — float, text, image)
//! Input 1: Trigger (rising edge captures)
//! Output 0: Held value (stays constant between triggers)
//! Output 1: Trigger echo (1.0 on capture frame)
//!
//! Capture triggers (all three converge into the same capture path):
//!   (a) Rising edge on Trigger input
//!   (b) Manual "Capture" button in the node UI
//!   (c) Auto-capture at a user-settable interval (stop-motion / time-lapse)
//!
//! Type dispatch on Value input:
//!   - Float → stores scalar, adds to history for staircase visualization
//!   - Text  → stores string
//!   - Image → clones the Arc<ImageData> (GpuImage inputs are pre-readback
//!             to CPU by the image eval loop via `needs_cpu_image_input`)

use std::sync::Arc;
use std::time::Instant;
use crate::graph::{ImageData, PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleHoldNode {
    #[serde(default)]
    pub held_float: f32,
    #[serde(default)]
    pub held_text: String,
    /// Held image — runtime-only, not persisted to project files (captured
    /// pixel data would bloat saves and isn't meaningfully portable). On
    /// reload the node simply waits for the next capture.
    #[serde(skip)]
    pub held_image: Option<Arc<ImageData>>,
    /// Which type was last captured. Persisted so reload shows the right
    /// label / preview even before the next capture fires.
    /// 0 = Float, 1 = Text, 2 = Image.
    #[serde(default)]
    pub hold_kind: u8,
    /// Legacy field — kept so old project files still deserialize. New
    /// code writes to `hold_kind` instead; `load_state` migrates.
    #[serde(default)]
    pub is_text: bool,
    #[serde(skip)]
    pub last_trigger: f32,
    #[serde(default)]
    pub history: Vec<f32>,
    /// Auto-capture every N seconds. 0 = disabled. Useful for time-lapse
    /// / stop-motion workflows. Persisted.
    #[serde(default)]
    pub auto_interval_secs: f32,
    /// Wall-clock timestamp of last auto-capture. Runtime only.
    #[serde(skip)]
    pub last_auto_capture: Option<Instant>,
    /// Incremented each time the manual "Capture" button is clicked.
    /// Included in `save_state` so the image eval loop's cache key
    /// changes on click — otherwise the cache would return the stale
    /// held value instead of re-running evaluate() to pick up the
    /// new capture. Counter rolls over at u64::MAX (not in our lifetime).
    #[serde(default)]
    pub capture_seq: u64,
    /// Last `capture_seq` that evaluate() has processed. When this differs
    /// from `capture_seq` we know a button click is pending. Runtime only.
    #[serde(skip)]
    pub processed_seq: u64,
}

impl Default for SampleHoldNode {
    fn default() -> Self {
        Self {
            held_float: 0.0, held_text: String::new(),
            held_image: None, hold_kind: 0, is_text: false,
            last_trigger: 0.0, history: Vec::new(),
            auto_interval_secs: 0.0, last_auto_capture: None,
            capture_seq: 0, processed_seq: 0,
        }
    }
}

impl NodeBehavior for SampleHoldNode {
    fn title(&self) -> &str { "Sample & Hold" }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Value", PortKind::Generic), PortDef::new("Trigger", PortKind::Trigger)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Out", PortKind::Generic), PortDef::new("Trigger", PortKind::Trigger)]
    }

    fn color_hint(&self) -> [u8; 3] { [120, 200, 160] }
    fn inline_ports(&self) -> bool { true }

    /// Force the image eval loop to readback any GpuImage upstream into a
    /// CPU `PortValue::Image` before calling our evaluate(). Sample & Hold
    /// keeps the pixels around indefinitely, so we need them in CPU form
    /// — the 2-frame LRU on `gpu_tex_cache` would evict the texture well
    /// before the next capture. Small one-time readback cost per capture;
    /// no cost on frames where we're just emitting the held value.
    fn needs_cpu_image_input(&self, _port: usize) -> bool { true }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let trigger_val = inputs.get(1).map(|v| v.as_float()).unwrap_or(0.0);
        let rising_edge = trigger_val > 0.5 && self.last_trigger <= 0.5;
        self.last_trigger = trigger_val;

        // Button-triggered capture: render thread bumps capture_seq; eval
        // notices the mismatch and processes one capture.
        let button_pressed = self.capture_seq != self.processed_seq;
        self.processed_seq = self.capture_seq;

        // Interval-triggered capture: if auto_interval_secs > 0, capture
        // once per interval using wall-clock time. First frame primes
        // `last_auto_capture` without firing, so users don't get an
        // unexpected capture the instant they enable the interval.
        let mut interval_fired = false;
        if self.auto_interval_secs > 0.0 {
            let now = Instant::now();
            match self.last_auto_capture {
                None => self.last_auto_capture = Some(now),
                Some(prev) => {
                    if now.duration_since(prev).as_secs_f32() >= self.auto_interval_secs {
                        interval_fired = true;
                        self.last_auto_capture = Some(now);
                    }
                }
            }
        } else {
            self.last_auto_capture = None;
        }

        let should_capture = rising_edge || button_pressed || interval_fired;

        if should_capture {
            if let Some(val) = inputs.first() {
                match val {
                    PortValue::Float(f) => {
                        self.held_float = *f;
                        self.hold_kind = 0;
                        self.is_text = false;
                        self.history.push(*f);
                        if self.history.len() > 40 { self.history.remove(0); }
                    }
                    PortValue::Text(t) => {
                        self.held_text = t.clone();
                        self.hold_kind = 1;
                        self.is_text = true;
                    }
                    PortValue::Image(img) => {
                        // Arc clone — cheap, shares underlying pixel buffer.
                        // The upstream may have come from a GpuImage but
                        // `needs_cpu_image_input` forced a CPU readback
                        // before we got here, so `img` is guaranteed CPU.
                        self.held_image = Some(img.clone());
                        self.hold_kind = 2;
                    }
                    // GpuImage shouldn't reach here (needs_cpu_image_input
                    // readback turns it into PortValue::Image) but guard
                    // anyway — if it does, just skip the capture rather
                    // than crash.
                    _ => {}
                }
            }
        }

        // Emit the held value based on current `hold_kind`.
        let held = match self.hold_kind {
            2 => match &self.held_image {
                Some(img) => PortValue::Image(img.clone()),
                None => PortValue::None,
            },
            1 => PortValue::Text(self.held_text.clone()),
            _ => PortValue::Float(self.held_float),
        };

        vec![
            (0, held),
            (1, PortValue::Float(if should_capture { 1.0 } else { 0.0 })),
        ]
    }

    fn type_tag(&self) -> &str { "sample_hold" }
    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<SampleHoldNode>(state.clone()) {
            self.held_float = l.held_float;
            self.held_text = l.held_text;
            // Migration from the pre-image `is_text` boolean to the new
            // three-way `hold_kind` enum. Old projects only have
            // `is_text`; new projects have both fields (kept in sync).
            // If `hold_kind` is 0 but `is_text` is true, it's a legacy
            // project — migrate.
            self.is_text = l.is_text;
            self.hold_kind = if l.hold_kind != 0 { l.hold_kind }
                             else if l.is_text { 1 }
                             else { 0 };
            self.history = l.history;
            self.auto_interval_secs = l.auto_interval_secs;
            self.capture_seq = l.capture_seq;
            // `held_image`, `last_auto_capture`, `processed_seq`, and
            // `last_trigger` are all #[serde(skip)] — defaults are correct.
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let accent = ui.visuals().hyperlink_color;
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        let val_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let trig_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 1);

        // Peek at the currently-wired input value — tells us what type is
        // about to be captured so we can label the button ("Capture Image"
        // vs "Capture") and color UI hints appropriately.
        let live_val = if val_wired {
            Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0)
        } else {
            PortValue::None
        };
        let live_kind: &'static str = match &live_val {
            PortValue::Float(_) => "Number",
            PortValue::Text(_)  => "Text",
            PortValue::Image(_) => "Image",
            PortValue::GpuImage(_) => "Image",  // shouldn't arrive; needs_cpu_image_input handles
            PortValue::Mesh(_) | PortValue::GpuMesh(_) => "Mesh",
            PortValue::None => "—",
        };

        // Value input
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
            ui.label(egui::RichText::new("Value:").small());
            if val_wired {
                let s = match &live_val {
                    PortValue::Float(f) => format!("{:.3}", f),
                    PortValue::Text(t) => if t.chars().count() > 16 {
                        let head: String = t.chars().take(16).collect();
                        format!("\"{}...\"", head)
                    } else { format!("\"{}\"", t) },
                    PortValue::Image(img) => format!("[{}×{}]", img.width, img.height),
                    PortValue::GpuImage(h) => format!("[{}×{}]", h.width, h.height),
                    _ => "—".into(),
                };
                ui.label(egui::RichText::new(s).small().color(accent));
            } else {
                ui.label(egui::RichText::new("—").small().color(dim));
            }
        });

        // Trigger input
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
            ui.label(egui::RichText::new("Trigger:").small());
            if trig_wired {
                let t = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 1).as_float();
                let col = if t > 0.5 { egui::Color32::from_rgb(255, 200, 60) } else { dim };
                ui.label(egui::RichText::new(format!("{:.1}", t)).small().color(col));
            } else {
                ui.label(egui::RichText::new("—").small().color(dim));
            }
        });

        ui.separator();

        // ── Capture controls ─────────────────────────────────────────
        // Button label reflects what's about to be captured so the user
        // isn't surprised ("Sample & Hold" node holding an image feels
        // different than holding a number).
        let trigger_val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 1).as_float();
        let rising_edge = trigger_val > 0.5 && self.last_trigger <= 0.5;

        ui.horizontal(|ui| {
            let btn_label = if val_wired { format!("Capture {}", live_kind) } else { "Capture".into() };
            if ui.add_enabled(val_wired, egui::Button::new(btn_label)).clicked() {
                // Bump the counter — evaluate() on the next eval pass will
                // notice the mismatch and do the capture. We don't mutate
                // held_* here directly because image capture may need
                // GPU readback, which happens in the image eval loop.
                self.capture_seq = self.capture_seq.wrapping_add(1);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                crate::nodes::inline_port_circle(ui, ctx.node_id, 1, false, ctx.connections,
                    ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
            });
        });

        // Auto-capture interval control. 0 disables. Small DragValue +
        // helper label so the user sees a time-lapse effect when they
        // bump it up from zero.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Every").small().color(dim));
            ui.add(
                egui::DragValue::new(&mut self.auto_interval_secs)
                    .speed(0.1)
                    .range(0.0..=3600.0)
                    .suffix(" s"),
            );
            if self.auto_interval_secs > 0.0 {
                ui.label(egui::RichText::new("auto").small().color(accent));
            } else {
                ui.label(egui::RichText::new("(off)").small().color(dim));
            }
        });

        ui.separator();

        // ── Held value display ───────────────────────────────────────
        // Dot blinks yellow on the frame a capture fires (either from
        // trigger or button), giving visual confirmation.
        let capturing_now = rising_edge || self.capture_seq != self.processed_seq;
        ui.horizontal(|ui| {
            let dot_color = if capturing_now { egui::Color32::from_rgb(255, 220, 60) } else { dim };
            let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), if capturing_now { 5.0 } else { 3.0 }, dot_color);

            ui.label(egui::RichText::new("Held:").small().strong());
            match self.hold_kind {
                2 => match &self.held_image {
                    Some(img) => ui.label(egui::RichText::new(format!("[{}×{}]", img.width, img.height))
                        .small().color(egui::Color32::from_rgb(160, 200, 255))),
                    None => ui.label(egui::RichText::new("(no image yet)").small().color(dim)),
                },
                1 => {
                    let display = if self.held_text.len() > 20 { format!("\"{}...\"", &self.held_text[..20]) }
                        else { format!("\"{}\"", &self.held_text) };
                    ui.label(egui::RichText::new(display).small().color(egui::Color32::from_rgb(80, 220, 80)))
                }
                _ => ui.label(egui::RichText::new(format!("{:.4}", self.held_float)).strong()
                    .color(egui::Color32::from_rgb(255, 220, 80))),
            }
        });

        ui.separator();

        // ── Staircase visualization (only for numeric captures) ──────
        // Images and text don't visualize usefully here; omit the chart
        // in those modes and show a compact empty placeholder instead.
        if self.hold_kind == 0 {
            if !self.history.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("{} samples", self.history.len())).small().color(dim));
                    if ui.small_button("Clear").clicked() { self.history.clear(); }
                });

                let (rect, _) = ui.allocate_exact_size(egui::vec2(150.0, 60.0), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);

                let min_v = self.history.iter().cloned().fold(f32::INFINITY, f32::min);
                let max_v = self.history.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let range = (max_v - min_v).max(0.01);
                let lo = min_v - range * 0.1;
                let hi = max_v + range * 0.1;
                let pad = 4.0;
                let n = self.history.len();
                let step_w = (rect.width() - pad * 2.0) / n.max(1) as f32;

                let stair_color = egui::Color32::from_rgb(80, 200, 120);
                for (i, val) in self.history.iter().enumerate() {
                    let x = rect.left() + pad + i as f32 * step_w;
                    let y = rect.bottom() - pad - ((val - lo) / (hi - lo)) * (rect.height() - pad * 2.0);
                    let y = y.clamp(rect.top() + pad, rect.bottom() - pad);
                    painter.line_segment([egui::pos2(x, y), egui::pos2(x + step_w, y)],
                        egui::Stroke::new(2.0, stair_color));
                    if i + 1 < n {
                        let next_y = rect.bottom() - pad - ((self.history[i + 1] - lo) / (hi - lo)) * (rect.height() - pad * 2.0);
                        painter.line_segment([egui::pos2(x + step_w, y), egui::pos2(x + step_w, next_y.clamp(rect.top() + pad, rect.bottom() - pad))],
                            egui::Stroke::new(1.0, stair_color.gamma_multiply(0.4)));
                    }
                }
            } else {
                ui.label(egui::RichText::new("No samples yet").small().color(dim));
            }

            ui.separator();
        }

        // ── Output port ──────────────────────────────────────────────
        let out_val = match self.hold_kind {
            2 => match &self.held_image {
                Some(img) => format!("[{}×{}]", img.width, img.height),
                None => "none".into(),
            },
            1 => format!("\"{}\"", if self.held_text.len() > 10 { &self.held_text[..10] } else { &self.held_text }),
            _ => format!("{:.3}", self.held_float),
        };
        crate::nodes::output_port_row(ui, "Out", &out_val, ctx.node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Generic);
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("sample_hold", |state| {
        if let Ok(n) = serde_json::from_value::<SampleHoldNode>(state.clone()) { Box::new(n) }
        else { Box::new(SampleHoldNode::default()) }
    });
}
