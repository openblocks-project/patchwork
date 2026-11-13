use crate::graph::{Graph, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiplyNode {
    #[serde(default = "default_one")]
    pub const_a: f32,
    #[serde(default = "default_one")]
    pub const_b: f32,
}

fn default_one() -> f32 {
    1.0
}

impl Default for MultiplyNode {
    fn default() -> Self {
        Self {
            const_a: 1.0,
            const_b: 1.0,
        }
    }
}

impl NodeBehavior for MultiplyNode {
    fn title(&self) -> &str {
        "Multiply"
    }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("A", PortKind::Generic),
            PortDef::new("B", PortKind::Generic),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Result", PortKind::Generic)]
    }

    fn color_hint(&self) -> [u8; 3] {
        [200, 160, 80]
    }
    fn inline_ports(&self) -> bool {
        true
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let a = match inputs.first() {
            Some(PortValue::None) | None => PortValue::Float(self.const_a),
            Some(v) => v.clone(),
        };
        let b = match inputs.get(1) {
            Some(PortValue::None) | None => PortValue::Float(self.const_b),
            Some(v) => v.clone(),
        };

        let result = match (&a, &b) {
            (PortValue::Float(x), PortValue::Float(y)) => PortValue::Float(x * y),
            (PortValue::Text(s), PortValue::Float(n)) => {
                let count = (*n as usize).min(100);
                PortValue::Text(s.repeat(count))
            }
            (val, PortValue::None) => val.clone(),
            (PortValue::None, val) => val.clone(),
            _ => a,
        };

        vec![(0, result)]
    }

    fn type_tag(&self) -> &str {
        "multiply"
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<MultiplyNode>(state.clone()) {
            *self = l;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let a_wired = ctx
            .connections
            .iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let b_wired = ctx
            .connections
            .iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 1);
        let accent = ui.visuals().hyperlink_color;

        // A input
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
                PortKind::Generic,
            );
            if a_wired {
                let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
                ui.label(
                    egui::RichText::new(format!("A: {}", v))
                        .small()
                        .color(accent),
                );
            } else {
                ui.label(egui::RichText::new("A").small());
                ui.add(egui::DragValue::new(&mut self.const_a).speed(0.1));
            }
        });

        // B input
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
                PortKind::Generic,
            );
            if b_wired {
                let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 1);
                ui.label(
                    egui::RichText::new(format!("B: {}", v))
                        .small()
                        .color(accent),
                );
            } else {
                ui.label(egui::RichText::new("B").small());
                ui.add(egui::DragValue::new(&mut self.const_b).speed(0.1));
            }
        });

        ui.separator();

        // Result + output port
        let result = ctx
            .values
            .get(&(ctx.node_id, 0))
            .cloned()
            .unwrap_or(PortValue::None);
        ui.horizontal(|ui| {
            if !matches!(result, PortValue::None) {
                ui.label(egui::RichText::new(format!("= {}", result)).strong());
            }
            crate::nodes::inline_port_circle(
                ui,
                ctx.node_id,
                0,
                false,
                ctx.connections,
                ctx.port_positions,
                ctx.dragging_from,
                ctx.pending_disconnects,
                PortKind::Generic,
            );
        });
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("multiply", |state| {
        if let Ok(n) = serde_json::from_value::<MultiplyNode>(state.clone()) {
            Box::new(n)
        } else {
            Box::new(MultiplyNode::default())
        }
    });
}
