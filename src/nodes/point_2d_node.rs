use eframe::egui::{self, RichText, Sense};
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Point2dNode ──────────────────────────────────────────────────────────────
//
// Visual 2D point — works as both a visualizer (for tracking data) and a
// manual 2D input (click/drag to set position).
//
// Ports:
//   In  0: X (Number)    In  1: Y (Number)
//   Out 0: X (Number)    Out 1: Y (Number)
//
// When inputs are connected, values pass through (canvas shows live position).
// When inputs are NOT connected, user can click/drag on the canvas or edit
// the DragValue fields to set X/Y manually.

#[derive(Debug, Clone)]
pub struct Point2dNode {
    pub x: f32,
    pub y: f32,
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
    pub auto_range: bool,
}

impl Default for Point2dNode {
    fn default() -> Self {
        Self {
            x: 0.0, y: 0.0,
            min_x: 0.0, max_x: 100.0,
            min_y: 0.0, max_y: 100.0,
            auto_range: true,
        }
    }
}

/// Round up to a "nice" number for auto-range: 10, 20, 50, 100, 200, 500, 1000, …
fn nice_ceil(v: f32) -> f32 {
    if v <= 0.0 { return 10.0; }
    let mag = 10.0f32.powf(v.log10().floor());
    let frac = v / mag;
    let nice = if frac <= 1.0 { 1.0 }
               else if frac <= 2.0 { 2.0 }
               else if frac <= 5.0 { 5.0 }
               else { 10.0 };
    nice * mag
}

fn nice_floor(v: f32) -> f32 {
    -nice_ceil(-v)
}

impl NodeBehavior for Point2dNode {
    fn title(&self)      -> &str   { "Point 2D" }
    fn type_tag(&self)   -> &str   { "point_2d" }
    fn color_hint(&self) -> [u8;3] { [100, 180, 240] }
    fn min_width(&self)  -> Option<f32> { Some(180.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("X", PortKind::Number),
            PortDef::new("Y", PortKind::Number),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("X", PortKind::Number),
            PortDef::new("Y", PortKind::Number),
        ]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![
            (0, PortValue::Float(self.x)),
            (1, PortValue::Float(self.y)),
        ]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim   = egui::Color32::from_rgb(110, 110, 120);
        let blue  = egui::Color32::from_rgb(100, 180, 240);
        let white = egui::Color32::WHITE;

        // Check which inputs are connected
        let x_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        let y_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);

        // Read connected input values
        if x_connected {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 0);
            if let PortValue::Float(f) = v { self.x = f; }
        }
        if y_connected {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 1);
            if let PortValue::Float(f) = v { self.y = f; }
        }

        // Auto-expand range
        if self.auto_range {
            let pad = 0.1; // 10% padding
            if self.x > self.max_x { self.max_x = nice_ceil(self.x * (1.0 + pad)); }
            if self.x < self.min_x { self.min_x = nice_floor(self.x * (1.0 + pad)); }
            if self.y > self.max_y { self.max_y = nice_ceil(self.y * (1.0 + pad)); }
            if self.y < self.min_y { self.min_y = nice_floor(self.y * (1.0 + pad)); }
        }

        // ── Input ports with DragValue ────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("X").small().color(if x_connected { blue } else { dim }));
            if x_connected {
                ui.label(RichText::new(format!("{:.1}", self.x)).small().monospace());
            } else {
                ui.add(egui::DragValue::new(&mut self.x).speed(0.5)
                    .max_decimals(1));
            }
        });
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(RichText::new("Y").small().color(if y_connected { blue } else { dim }));
            if y_connected {
                ui.label(RichText::new(format!("{:.1}", self.y)).small().monospace());
            } else {
                ui.add(egui::DragValue::new(&mut self.y).speed(0.5)
                    .max_decimals(1));
            }
        });

        ui.add_space(4.0);

        // ── 2D Canvas ────────────────────────────────────────────────────
        let available = ui.available_width().max(120.0).min(200.0);
        let canvas_size = egui::vec2(available, available);
        let (rect, response) = ui.allocate_exact_size(canvas_size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        let bg = egui::Color32::from_rgb(25, 28, 32);
        let grid_color = egui::Color32::from_rgba_premultiplied(60, 65, 75, 180);
        let crosshair_color = egui::Color32::from_rgba_premultiplied(80, 90, 110, 140);

        // Background
        painter.rect_filled(rect, 4.0, bg);
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 55, 65)), egui::StrokeKind::Outside);

        let range_x = (self.max_x - self.min_x).max(0.001);
        let range_y = (self.max_y - self.min_y).max(0.001);
        let pad = 6.0;
        let inner = rect.shrink(pad);

        // Grid lines (5 divisions each axis)
        for i in 1..5 {
            let t = i as f32 / 5.0;
            let gx = inner.left() + t * inner.width();
            let gy = inner.top()  + t * inner.height();
            painter.line_segment(
                [egui::pos2(gx, inner.top()), egui::pos2(gx, inner.bottom())],
                egui::Stroke::new(0.5, grid_color));
            painter.line_segment(
                [egui::pos2(inner.left(), gy), egui::pos2(inner.right(), gy)],
                egui::Stroke::new(0.5, grid_color));
        }

        // Map value → canvas position
        let nx = ((self.x - self.min_x) / range_x).clamp(0.0, 1.0);
        let ny = ((self.y - self.min_y) / range_y).clamp(0.0, 1.0);
        let cx = inner.left() + nx * inner.width();
        let cy = inner.top()  + ny * inner.height();

        // Crosshair
        painter.line_segment(
            [egui::pos2(cx, inner.top()), egui::pos2(cx, inner.bottom())],
            egui::Stroke::new(0.8, crosshair_color));
        painter.line_segment(
            [egui::pos2(inner.left(), cy), egui::pos2(inner.right(), cy)],
            egui::Stroke::new(0.8, crosshair_color));

        // Dot
        let dot_color = egui::Color32::from_rgb(100, 200, 255);
        painter.circle_filled(egui::pos2(cx, cy), 6.0, dot_color);
        painter.circle_stroke(egui::pos2(cx, cy), 6.0, egui::Stroke::new(1.5, white));

        // Click/drag interaction (only when inputs not connected)
        if (!x_connected || !y_connected) && (response.clicked() || response.dragged()) {
            if let Some(pos) = response.interact_pointer_pos() {
                let lx = ((pos.x - inner.left()) / inner.width()).clamp(0.0, 1.0);
                let ly = ((pos.y - inner.top())  / inner.height()).clamp(0.0, 1.0);
                if !x_connected { self.x = self.min_x + lx * range_x; }
                if !y_connected { self.y = self.min_y + ly * range_y; }
            }
        }

        // Axis labels (corners)
        let label_color = egui::Color32::from_rgb(80, 85, 95);
        let tiny = egui::FontId::proportional(9.0);
        painter.text(egui::pos2(rect.left() + 2.0, rect.bottom() - 11.0),
            egui::Align2::LEFT_BOTTOM, format!("{:.0}", self.min_x), tiny.clone(), label_color);
        painter.text(egui::pos2(rect.right() - 2.0, rect.bottom() - 11.0),
            egui::Align2::RIGHT_BOTTOM, format!("{:.0}", self.max_x), tiny.clone(), label_color);
        painter.text(egui::pos2(rect.left() + 2.0, rect.top() + 2.0),
            egui::Align2::LEFT_TOP, format!("{:.0}", self.min_y), tiny.clone(), label_color);
        painter.text(egui::pos2(rect.left() + 2.0, rect.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM, format!("{:.0}", self.max_y), tiny, label_color);

        ui.add_space(2.0);

        // ── Settings popup (click on node body, not canvas) ───────────────
        let popup_id = egui::Id::new(("point2d_popup", node_id));
        let popup_is_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));

        // Toggle button
        ui.horizontal(|ui| {
            if ui.small_button(RichText::new("⚙").color(dim)).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(popup_id, !popup_is_open));
            }
            if self.auto_range {
                ui.label(RichText::new("auto-range").small().color(dim).italics());
            } else {
                ui.label(RichText::new(format!("X [{:.0}–{:.0}] Y [{:.0}–{:.0}]",
                    self.min_x, self.max_x, self.min_y, self.max_y)).small().color(dim));
            }
        });

        if popup_is_open {
            let popup_pos = egui::pos2(rect.right() + 8.0, rect.top());
            egui::Area::new(egui::Id::new(("point2d_opts", node_id)))
                .fixed_pos(popup_pos)
                .order(egui::Order::Tooltip)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).corner_radius(8.0).inner_margin(10.0).show(ui, |ui| {
                        ui.set_min_width(180.0);
                        ui.label(RichText::new("Range Settings").small().strong());
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            ui.label(RichText::new("X min").small().color(dim));
                            ui.add(egui::DragValue::new(&mut self.min_x).speed(1.0).max_decimals(0));
                            ui.label(RichText::new("max").small().color(dim));
                            ui.add(egui::DragValue::new(&mut self.max_x).speed(1.0).max_decimals(0));
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Y min").small().color(dim));
                            ui.add(egui::DragValue::new(&mut self.min_y).speed(1.0).max_decimals(0));
                            ui.label(RichText::new("max").small().color(dim));
                            ui.add(egui::DragValue::new(&mut self.max_y).speed(1.0).max_decimals(0));
                        });

                        ui.add_space(4.0);
                        ui.checkbox(&mut self.auto_range, RichText::new("Auto-scale range").small());

                        ui.add_space(4.0);
                        if ui.small_button("Reset to data bounds").clicked() {
                            let px = 10.0f32;
                            self.min_x = nice_floor(self.x - px);
                            self.max_x = nice_ceil(self.x + px);
                            self.min_y = nice_floor(self.y - px);
                            self.max_y = nice_ceil(self.y + px);
                        }
                        if ui.small_button("Close").clicked() {
                            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
                        }
                    });
                });
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports ──────────────────────────────────────────────────
        let x_str = format!("{:.1}", self.x);
        let y_str = format!("{:.1}", self.y);
        crate::nodes::output_port_row(ui, "X", &x_str, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);
        crate::nodes::output_port_row(ui, "Y", &y_str, node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "x": self.x, "y": self.y,
            "min_x": self.min_x, "max_x": self.max_x,
            "min_y": self.min_y, "max_y": self.max_y,
            "auto_range": self.auto_range,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(v) = state.get("x").and_then(|v| v.as_f64()) { self.x = v as f32; }
        if let Some(v) = state.get("y").and_then(|v| v.as_f64()) { self.y = v as f32; }
        if let Some(v) = state.get("min_x").and_then(|v| v.as_f64()) { self.min_x = v as f32; }
        if let Some(v) = state.get("max_x").and_then(|v| v.as_f64()) { self.max_x = v as f32; }
        if let Some(v) = state.get("min_y").and_then(|v| v.as_f64()) { self.min_y = v as f32; }
        if let Some(v) = state.get("max_y").and_then(|v| v.as_f64()) { self.max_y = v as f32; }
        if let Some(v) = state.get("auto_range").and_then(|v| v.as_bool()) { self.auto_range = v; }
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("point_2d", |state| {
        let mut n = Point2dNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
