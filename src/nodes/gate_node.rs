use crate::graph::{Graph, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Deserialize, Serialize};

const MODES: &[&str] = &[">", "<", "≥", "≤", "=", "≠"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateNode {
    pub mode: u8,
    pub threshold: f32,
    pub else_value: f32,
}

impl Default for GateNode {
    fn default() -> Self {
        Self {
            mode: 0,
            threshold: 0.5,
            else_value: 0.0,
        }
    }
}

impl NodeBehavior for GateNode {
    fn title(&self) -> &str {
        "Gate"
    }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Value", PortKind::Number),
            PortDef::new("Threshold", PortKind::Number),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Out", PortKind::Number),
            PortDef::new("Pass", PortKind::Gate),
        ]
    }

    fn color_hint(&self) -> [u8; 3] {
        [220, 180, 60]
    }
    fn inline_ports(&self) -> bool {
        true
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let val = inputs.first().map(|v| v.as_float()).unwrap_or(0.0);
        let thresh = if let Some(PortValue::Float(v)) = inputs.get(1) {
            self.threshold = *v;
            *v
        } else {
            self.threshold
        };

        let pass = match self.mode {
            0 => val > thresh,
            1 => val < thresh,
            2 => val >= thresh,
            3 => val <= thresh,
            4 => (val - thresh).abs() < f32::EPSILON,
            5 => (val - thresh).abs() >= f32::EPSILON,
            _ => val > thresh,
        };
        let out = if pass { val } else { self.else_value };
        vec![
            (0, PortValue::Float(out)),
            (1, PortValue::Float(if pass { 1.0 } else { 0.0 })),
        ]
    }

    fn type_tag(&self) -> &str {
        "gate"
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<GateNode>(state.clone()) {
            *self = loaded;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // Input: Value
        let val_wired = ctx
            .connections
            .iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0).as_float();
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                0,
                true,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Number,
            );
            ui.label(egui::RichText::new("Value:").small());
            if val_wired {
                ui.label(
                    egui::RichText::new(format!("{:.2}", val))
                        .small()
                        .color(ui.visuals().hyperlink_color),
                );
            } else {
                ui.label(egui::RichText::new("—").small().color(dim));
            }
        });

        // Input: Threshold
        let thresh_wired = ctx
            .connections
            .iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 1);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                1,
                true,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Number,
            );
            ui.label(egui::RichText::new("Thresh:").small());
            if thresh_wired {
                ui.label(
                    egui::RichText::new(format!("{:.2}", self.threshold))
                        .small()
                        .color(ui.visuals().hyperlink_color),
                );
            } else {
                ui.add(egui::DragValue::new(&mut self.threshold).speed(0.1));
            }
        });

        ui.separator();

        // Mode selector
        ui.horizontal(|ui| {
            for (i, label) in MODES.iter().enumerate() {
                let selected = self.mode == i as u8;
                if ui
                    .selectable_label(selected, egui::RichText::new(*label).strong())
                    .clicked()
                {
                    self.mode = i as u8;
                }
            }
        });

        // Else value
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Else:").small().color(dim));
            ui.add(egui::DragValue::new(&mut self.else_value).speed(0.1));
        });

        ui.separator();

        // Live status
        let pass = match self.mode {
            0 => val > self.threshold,
            1 => val < self.threshold,
            2 => val >= self.threshold,
            3 => val <= self.threshold,
            4 => (val - self.threshold).abs() < f32::EPSILON,
            5 => (val - self.threshold).abs() >= f32::EPSILON,
            _ => val > self.threshold,
        };
        let status_color = if pass {
            egui::Color32::from_rgb(80, 255, 120)
        } else {
            egui::Color32::from_rgb(255, 80, 80)
        };
        let status_icon = if pass { "✓ Pass" } else { "✗ Block" };
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(status_icon)
                    .color(status_color)
                    .strong(),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{:.2} {} {:.2}",
                    val, MODES[self.mode as usize], self.threshold
                ))
                .small()
                .monospace()
                .color(dim),
            );
        });

        let out = if pass { val } else { self.else_value };
        ui.separator();

        // Output ports
        crate::nodes::output_port_row(
            ui,
            "Out",
            &format!("{:.3}", out),
            ctx.node_id,
            0,
            ctx.port_positions,
            ctx.dragging_from,
            ctx.connections,
            ctx.pending_disconnects,
            PortKind::Number,
        );
        crate::nodes::output_port_row(
            ui,
            "Pass",
            if pass { "1" } else { "0" },
            ctx.node_id,
            1,
            ctx.port_positions,
            ctx.dragging_from,
            ctx.connections,
            ctx.pending_disconnects,
            PortKind::Gate,
        );
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("gate", |state| {
        if let Ok(node) = serde_json::from_value::<GateNode>(state.clone()) {
            Box::new(node)
        } else {
            Box::new(GateNode::default())
        }
    });
}
