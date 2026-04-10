use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapRangeNode {
    pub in_min: f32, pub in_max: f32,
    pub out_min: f32, pub out_max: f32,
    pub clamp: bool,
    #[serde(default = "default_in_val")]
    pub in_val: f32,
}

fn default_in_val() -> f32 { 0.5 }

impl Default for MapRangeNode {
    fn default() -> Self {
        Self { in_min: 0.0, in_max: 1.0, out_min: 0.0, out_max: 100.0, clamp: false, in_val: 0.5 }
    }
}

impl NodeBehavior for MapRangeNode {
    fn title(&self) -> &str { "Map/Range" }
    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Value", PortKind::Number), PortDef::new("In Min", PortKind::Number),
             PortDef::new("In Max", PortKind::Number), PortDef::new("Out Min", PortKind::Number),
             PortDef::new("Out Max", PortKind::Number)]
    }
    fn outputs(&self) -> Vec<PortDef> { vec![PortDef::new("Value", PortKind::Number)] }
    fn color_hint(&self) -> [u8; 3] { [180, 140, 220] }
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let val = match inputs.first() {
            Some(PortValue::Float(v)) => *v,
            _ => self.in_val,
        };
        // Only override from port if a real value is connected (not PortValue::None)
        if let Some(PortValue::Float(v)) = inputs.get(1) { self.in_min = *v; }
        if let Some(PortValue::Float(v)) = inputs.get(2) { self.in_max = *v; }
        if let Some(PortValue::Float(v)) = inputs.get(3) { self.out_min = *v; }
        if let Some(PortValue::Float(v)) = inputs.get(4) { self.out_max = *v; }
        let t = (val - self.in_min) / (self.in_max - self.in_min).max(0.001);
        let t_final = if self.clamp { t.clamp(0.0, 1.0) } else { t };
        vec![(0, PortValue::Float(self.out_min + t_final * (self.out_max - self.out_min)))]
    }

    fn type_tag(&self) -> &str { "map_range" }
    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<MapRangeNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let wired: Vec<bool> = (0..5).map(|p| ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == p)).collect();
        let accent = ui.visuals().hyperlink_color;
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // ── In port ──────────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("In").small().strong());
            if wired[0] {
                let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0).as_float();
                ui.label(egui::RichText::new(format!("{:.3}", v)).small().monospace().color(accent));
            } else {
                ui.add(egui::DragValue::new(&mut self.in_val).speed(0.01));
            }
        });

        ui.separator();

        // ── Input Range ───────────────────────────────────────────────────────
        ui.label(egui::RichText::new("Input Range").small().strong());
        egui::Grid::new(("map_in", ctx.node_id))
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                // Row 1: port + label
                ui.horizontal(|ui| {
                    crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
                    ui.label(egui::RichText::new("Min").small());
                });
                ui.horizontal(|ui| {
                    crate::nodes::inline_port_circle(ui, ctx.node_id, 2, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
                    ui.label(egui::RichText::new("Max").small());
                });
                ui.end_row();
                // Row 2: values
                if wired[1] {
                    ui.label(egui::RichText::new(format!("{:.2}", self.in_min)).small().monospace().color(accent));
                } else {
                    ui.add(egui::DragValue::new(&mut self.in_min).speed(0.1));
                }
                if wired[2] {
                    ui.label(egui::RichText::new(format!("{:.2}", self.in_max)).small().monospace().color(accent));
                } else {
                    ui.add(egui::DragValue::new(&mut self.in_max).speed(0.1));
                }
                ui.end_row();
            });

        ui.add_space(4.0);

        // ── Output Range ──────────────────────────────────────────────────────
        ui.label(egui::RichText::new("Output Range").small().strong());
        egui::Grid::new(("map_out", ctx.node_id))
            .num_columns(2)
            .spacing([8.0, 2.0])
            .show(ui, |ui| {
                // Row 1: port + label
                ui.horizontal(|ui| {
                    crate::nodes::inline_port_circle(ui, ctx.node_id, 3, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
                    ui.label(egui::RichText::new("Min").small());
                });
                ui.horizontal(|ui| {
                    crate::nodes::inline_port_circle(ui, ctx.node_id, 4, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
                    ui.label(egui::RichText::new("Max").small());
                });
                ui.end_row();
                // Row 2: values
                if wired[3] {
                    ui.label(egui::RichText::new(format!("{:.2}", self.out_min)).small().monospace().color(accent));
                } else {
                    ui.add(egui::DragValue::new(&mut self.out_min).speed(0.1));
                }
                if wired[4] {
                    ui.label(egui::RichText::new(format!("{:.2}", self.out_max)).small().monospace().color(accent));
                } else {
                    ui.add(egui::DragValue::new(&mut self.out_max).speed(0.1));
                }
                ui.end_row();
            });

        ui.add_space(4.0);
        ui.checkbox(&mut self.clamp, "Clamp output");
        ui.separator();

        // ── Visual graph ──────────────────────────────────────────────────────
        let graph_size = egui::vec2(ui.available_width().max(100.0), 80.0);
        let (rect, _) = ui.allocate_exact_size(graph_size, egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);

        let y_range = (self.out_max - self.out_min).abs().max(0.001);
        let o_lo = self.out_min.min(self.out_max);
        let p1 = egui::pos2(rect.left() + 4.0, rect.bottom() - 4.0 - ((self.out_min - o_lo) / y_range) * (rect.height() - 8.0));
        let p2 = egui::pos2(rect.right() - 4.0, rect.bottom() - 4.0 - ((self.out_max - o_lo) / y_range) * (rect.height() - 8.0));
        painter.line_segment([p1, p2], egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 200, 120)));

        let input_val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0).as_float();
        let t = (input_val - self.in_min) / (self.in_max - self.in_min).max(0.001);
        let t_c = if self.clamp { t.clamp(0.0, 1.0) } else { t };
        let mapped = self.out_min + t_c * (self.out_max - self.out_min);
        let dot_x = rect.left() + 4.0 + t_c * (rect.width() - 8.0);
        let dot_y = rect.bottom() - 4.0 - ((mapped - o_lo) / y_range) * (rect.height() - 8.0);
        let dot = egui::pos2(dot_x.clamp(rect.left(), rect.right()), dot_y.clamp(rect.top(), rect.bottom()));
        painter.circle_filled(dot, 4.0, egui::Color32::from_rgb(255, 220, 60));

        // ── Bottom: value display + Out port ──────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(
                format!("{:.3} → {:.3}", input_val, mapped)
            ).small().monospace().strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                crate::nodes::inline_port_circle(ui, ctx.node_id, 0, false, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
                ui.label(egui::RichText::new("Out").small());
            });
        });
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("map_range", |state| {
        if let Ok(n) = serde_json::from_value::<MapRangeNode>(state.clone()) { Box::new(n) }
        else { Box::new(MapRangeNode::default()) }
    });
}
