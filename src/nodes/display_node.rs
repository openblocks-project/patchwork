use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;

// ── Color palette for multi-signal oscilloscope ─────────────────────────────

const SIGNAL_COLORS: &[[u8; 3]] = &[
    [80, 200, 120],   // green
    [255, 100, 100],  // red
    [100, 150, 255],  // blue
    [255, 200, 80],   // yellow
    [200, 80, 255],   // purple
    [80, 220, 220],   // cyan
    [255, 140, 60],   // orange
    [200, 200, 200],  // white
];

fn signal_color(idx: usize) -> egui::Color32 {
    let c = SIGNAL_COLORS[idx % SIGNAL_COLORS.len()];
    egui::Color32::from_rgb(c[0], c[1], c[2])
}

// ── DisplayNode ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayNode {
    #[serde(default = "default_signal_count")]
    pub signal_count: usize,
    /// Per-signal history buffers. histories[i] = samples for signal i.
    #[serde(default)]
    pub histories: Vec<Vec<f32>>,
    /// Legacy single history — migrated to histories[0] on load.
    #[serde(default)]
    history: Vec<f32>,
    #[serde(default = "default_history_max")]
    pub history_max: usize,
    #[serde(default)]
    pub scope_min: f32,
    #[serde(default = "default_one")]
    pub scope_max: f32,
    #[serde(default = "default_scope_height")]
    pub scope_height: f32,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_true")]
    pub auto_fit: bool,
    /// Legacy color field — kept for deserialization compat, ignored for rendering.
    #[serde(default = "default_display_color")]
    display_color: [u8; 3],
}

fn default_signal_count() -> usize { 1 }
fn default_history_max() -> usize { 200 }
fn default_one() -> f32 { 1.0 }
fn default_scope_height() -> f32 { 80.0 }
fn default_display_color() -> [u8; 3] { [80, 200, 120] }
fn default_true() -> bool { true }

impl Default for DisplayNode {
    fn default() -> Self {
        Self {
            signal_count: 1,
            histories: vec![Vec::new()],
            history: Vec::new(),
            history_max: 200,
            scope_min: 0.0,
            scope_max: 1.0,
            scope_height: 80.0,
            paused: false,
            label: String::new(),
            auto_fit: true,
            display_color: [80, 200, 120],
        }
    }
}

impl DisplayNode {
    /// Ensure histories vec matches signal_count.
    fn ensure_histories(&mut self) {
        // Migrate legacy single history
        if self.histories.is_empty() && !self.history.is_empty() {
            self.histories.push(std::mem::take(&mut self.history));
        }
        while self.histories.len() < self.signal_count {
            self.histories.push(Vec::new());
        }
        self.histories.truncate(self.signal_count);
    }
}

impl NodeBehavior for DisplayNode {
    fn title(&self) -> &str { "Scope" }

    fn inputs(&self) -> Vec<PortDef> {
        (0..self.signal_count).map(|i| {
            if i == 0 {
                PortDef::new("Value", PortKind::Number)
            } else {
                PortDef::dynamic(format!("Sig {}", i + 1), PortKind::Number)
            }
        }).collect()
    }

    fn outputs(&self) -> Vec<PortDef> { vec![] }

    fn color_hint(&self) -> [u8; 3] { SIGNAL_COLORS[0] }
    fn inline_ports(&self) -> bool { true }
    fn custom_render(&self) -> bool { true }
    fn no_title(&self) -> bool { true }
    fn min_width(&self) -> Option<f32> { Some(160.0) }

    fn render_background(&self, painter: &egui::Painter, rect: egui::Rect) -> Option<egui::Frame> {
        painter.rect_filled(rect, 8.0, egui::Color32::from_rgb(15, 15, 20));
        Some(egui::Frame::NONE.inner_margin(6.0))
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Pass through first input for downstream (image or value)
        if let Some(v) = inputs.first() {
            vec![(0, v.clone())]
        } else {
            vec![]
        }
    }

    fn type_tag(&self) -> &str { "display" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<DisplayNode>(state.clone()) {
            *self = loaded;
            self.ensure_histories();
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        self.ensure_histories();

        // ── Multi-signal oscilloscope ────────────────────────────
        let any_wired = (0..self.signal_count).any(|i| {
            ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == i)
        });

        // Push to per-signal histories
        if !self.paused {
            for sig in 0..self.signal_count {
                let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == sig);
                if wired {
                    let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, sig).as_float();
                    if sig < self.histories.len() {
                        self.histories[sig].push(v);
                        while self.histories[sig].len() > self.history_max {
                            self.histories[sig].remove(0);
                        }
                    }
                }
            }
        }

        // Auto-fit range across ALL signals
        if self.auto_fit {
            let mut min_v = f32::INFINITY;
            let mut max_v = f32::NEG_INFINITY;
            for hist in &self.histories {
                for &v in hist {
                    min_v = min_v.min(v);
                    max_v = max_v.max(v);
                }
            }
            if min_v.is_finite() && max_v.is_finite() {
                let margin = (max_v - min_v).max(0.01) * 0.1;
                self.scope_min = min_v - margin;
                self.scope_max = max_v + margin;
            }
        }

        // Input ports with current values
        for sig in 0..self.signal_count {
            let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == sig);
            let v = if wired {
                Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, sig).as_float()
            } else { 0.0 };
            let color = signal_color(sig);
            let kind = PortKind::Number;
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, ctx.node_id, sig, true, ctx.connections,
                    ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, kind);
                if wired {
                    ui.label(egui::RichText::new(format!("{:.3}", v)).monospace().strong().color(color));
                } else {
                    ui.label(egui::RichText::new("—").small().color(egui::Color32::from_rgb(50, 50, 55)));
                }
            });
        }

        // Oscilloscope
        let scope_w = ui.available_width().max(80.0);
        let scope_h = self.scope_height.max(60.0);
        let (rect, body_response) = ui.allocate_exact_size(
            egui::vec2(scope_w, scope_h), egui::Sense::click(),
        );
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(10, 10, 15));

        // Grid lines
        let grid_color = egui::Color32::from_rgba_unmultiplied(60, 60, 70, 40);
        for i in 1..4 {
            let y = rect.top() + rect.height() * i as f32 / 4.0;
            painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                egui::Stroke::new(0.5, grid_color));
        }

        // Draw all waveforms
        let range = (self.scope_max - self.scope_min).max(0.001);
        for (sig, hist) in self.histories.iter().enumerate() {
            if hist.len() < 2 { continue; }
            let color = signal_color(sig);
            let points: Vec<egui::Pos2> = hist.iter().enumerate().map(|(i, &v)| {
                let x = rect.left() + (i as f32 / (hist.len() - 1).max(1) as f32) * rect.width();
                let t = ((v - self.scope_min) / range).clamp(0.0, 1.0);
                let y = rect.bottom() - t * rect.height();
                egui::pos2(x, y)
            }).collect();

            for w in points.windows(2) {
                painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, color));
            }
            if let Some(&last) = points.last() {
                painter.circle_filled(last, 3.0, color);
            }
        }

        // Signal labels (top-right of scope, only if multiple signals connected)
        let connected_count = (0..self.signal_count).filter(|&i| {
            ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == i)
        }).count();
        if connected_count > 1 {
            let mut label_y = rect.top() + 3.0;
            for sig in 0..self.signal_count {
                let conn = ctx.connections.iter().find(|c| c.to_node == ctx.node_id && c.to_port == sig);
                if let Some(c) = conn {
                    let src_name = ctx.values.iter()
                        .find(|((nid, _), _)| *nid == c.from_node)
                        .map(|_| format!("Sig {}", sig + 1))
                        .unwrap_or_else(|| format!("#{}", sig + 1));
                    let color = signal_color(sig);
                    painter.text(egui::pos2(rect.right() - 4.0, label_y), egui::Align2::RIGHT_TOP,
                        &src_name, egui::FontId::proportional(8.0), color);
                    label_y += 10.0;
                }
            }
        }

        if !any_wired && self.histories.iter().all(|h| h.is_empty()) {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "No input",
                egui::FontId::proportional(11.0), egui::Color32::from_rgb(60, 60, 65));
        }

        // Range labels
        painter.text(egui::pos2(rect.left() + 2.0, rect.top() + 2.0), egui::Align2::LEFT_TOP,
            format!("{:.1}", self.scope_max), egui::FontId::proportional(8.0), egui::Color32::from_rgb(60, 60, 70));
        painter.text(egui::pos2(rect.left() + 2.0, rect.bottom() - 2.0), egui::Align2::LEFT_BOTTOM,
            format!("{:.1}", self.scope_min), egui::FontId::proportional(8.0), egui::Color32::from_rgb(60, 60, 70));

        if self.paused {
            painter.text(egui::pos2(rect.right() - 4.0, rect.top() + 2.0), egui::Align2::RIGHT_TOP,
                "⏸", egui::FontId::proportional(10.0), egui::Color32::from_rgb(200, 200, 80));
        }

        // ── Inline controls ─────────────────────────────────────
        // Sit directly under the canvas so the most-used adjustments
        // (signal count, pause/reset/auto-fit) don't require opening the
        // options popup. Secondary tuning (samples, height, label,
        // manual min/max) stays in the popup.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Signals").small());
            if ui.small_button("−").clicked() && self.signal_count > 1 {
                let removed = self.signal_count - 1;
                ctx.pending_disconnects.push((ctx.node_id, removed));
                self.signal_count -= 1;
                self.ensure_histories();
            }
            ui.label(egui::RichText::new(format!("{}", self.signal_count)).strong());
            if ui.small_button("+").clicked() && self.signal_count < 8 {
                self.signal_count += 1;
                self.ensure_histories();
            }
            ui.separator();
            if ui.selectable_label(self.paused, "⏸").on_hover_text("Pause").clicked() { self.paused = !self.paused; }
            if ui.small_button("↺").on_hover_text("Reset").clicked() {
                for h in &mut self.histories { h.clear(); }
            }
            if ui.selectable_label(self.auto_fit, "Auto").on_hover_text("Auto-fit Y range").clicked() { self.auto_fit = !self.auto_fit; }
        });

        // Label
        if !self.label.is_empty() {
            let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
            ui.label(egui::RichText::new(self.label.as_str()).small().color(dim));
        }

        // ── Popup on click ──────────────────────────────────────
        let popup_id = egui::Id::new(("display_popup_d", ctx.node_id));
        let popup_time_id = egui::Id::new(("display_popup_time_d", ctx.node_id));
        let now = ui.ctx().input(|i| i.time);

        if body_response.clicked() {
            let is_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
            if !is_open {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(popup_id, true);
                    d.insert_temp(popup_time_id, now);
                });
            }
        }

        let popup_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
        if popup_open {
            // No local accent ring — the framework already draws the
            // selection highlight around every selected node (see
            // `app/mod.rs` "Draw selection highlight"), and the popup
            // panel appearing alongside is its own "options open" signal.
            // Drawing one here doubled up with the global ring.
            let popup_pos = egui::pos2(rect.right() + 8.0, rect.top());
            let opened_time = ui.ctx().data_mut(|d| d.get_temp::<f64>(popup_time_id).unwrap_or(0.0));

            let area_resp = egui::Area::new(egui::Id::new(("display_opts_d", ctx.node_id)))
                .fixed_pos(popup_pos).order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).corner_radius(10.0).inner_margin(10.0).show(ui, |ui| {
                        ui.set_min_width(170.0);

                        // Signals / Pause / Reset / Auto live inline under
                        // the canvas — see render_with_context above. The
                        // popup keeps only the secondary tuning.

                        if !self.auto_fit {
                            ui.horizontal(|ui| {
                                ui.label("Min"); ui.add(egui::DragValue::new(&mut self.scope_min).speed(0.1));
                                ui.label("Max"); ui.add(egui::DragValue::new(&mut self.scope_max).speed(0.1));
                            });
                            ui.separator();
                        }

                        ui.horizontal(|ui| {
                            ui.label("Samples");
                            let mut hm = self.history_max as f32;
                            ui.add(egui::DragValue::new(&mut hm).speed(10.0).range(10.0..=10000.0));
                            self.history_max = hm as usize;
                        });
                        ui.horizontal(|ui| {
                            ui.label("Height");
                            ui.add(egui::DragValue::new(&mut self.scope_height).speed(1.0).range(40.0..=400.0).suffix("px"));
                        });

                        ui.separator();
                        ui.add(egui::TextEdit::singleline(&mut self.label).hint_text("Label").desired_width(140.0));
                    });
                });

            let popup_rect = area_resp.response.rect;
            let esc = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
            let click_outside = if now - opened_time > 0.3 {
                ui.ctx().input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
                    && ui.ctx().pointer_latest_pos().map(|p| !popup_rect.contains(p) && !rect.contains(p)).unwrap_or(false)
            } else { false };

            if esc || click_outside {
                ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
            }
        }
    }
}


#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("display", |state| {
        if let Ok(mut node) = serde_json::from_value::<DisplayNode>(state.clone()) {
            node.ensure_histories();
            Box::new(node)
        } else {
            Box::new(DisplayNode::default())
        }
    });
}
