//! AddNode — polymorphic addition/combination node.
//!
//! Automatically adapts behavior based on input types:
//! - Float + Float → addition
//! - Text + Text → concatenation
//! - Image + Image → pixel-wise blend (additive)
//! - Image + Float → brightness offset
//! - Single input → passthrough
//!
//! When inputs aren't connected, the constant values (editable via drag)
//! are used instead.

use crate::graph::{Graph, ImageData, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddNode {
    #[serde(default)]
    pub const_a: f32,
    #[serde(default)]
    pub const_b: f32,
}

impl Default for AddNode {
    fn default() -> Self {
        Self {
            const_a: 0.0,
            const_b: 0.0,
        }
    }
}

impl NodeBehavior for AddNode {
    fn title(&self) -> &str {
        "Add"
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
        // Use connected value or fall back to constant
        let a = match inputs.first() {
            Some(PortValue::None) | None => PortValue::Float(self.const_a),
            Some(v) => v.clone(),
        };
        let b = match inputs.get(1) {
            Some(PortValue::None) | None => PortValue::Float(self.const_b),
            Some(v) => v.clone(),
        };

        let result = match (&a, &b) {
            (PortValue::Float(x), PortValue::Float(y)) => PortValue::Float(x + y),
            (PortValue::Text(s), PortValue::Text(t)) => PortValue::Text(format!("{}{}", s, t)),
            (PortValue::Float(x), PortValue::Text(t)) => PortValue::Text(format!("{}{}", x, t)),
            (PortValue::Text(s), PortValue::Float(y)) => PortValue::Text(format!("{}{}", s, y)),
            (PortValue::Image(img_a), PortValue::Image(img_b)) => {
                if img_a.width == img_b.width && img_a.height == img_b.height {
                    let pixels: Vec<u8> = img_a
                        .pixels
                        .iter()
                        .zip(img_b.pixels.iter())
                        .map(|(&a, &b)| a.saturating_add(b))
                        .collect();
                    PortValue::Image(Arc::new(ImageData {
                        width: img_a.width,
                        height: img_a.height,
                        pixels,
                    }))
                } else {
                    a
                }
            }
            (PortValue::Image(img), PortValue::Float(v))
            | (PortValue::Float(v), PortValue::Image(img)) => {
                let offset = (*v * 255.0) as i16;
                let pixels: Vec<u8> = img
                    .pixels
                    .chunks(4)
                    .flat_map(|px| {
                        [
                            (px[0] as i16 + offset).clamp(0, 255) as u8,
                            (px[1] as i16 + offset).clamp(0, 255) as u8,
                            (px[2] as i16 + offset).clamp(0, 255) as u8,
                            px[3],
                        ]
                    })
                    .collect();
                PortValue::Image(Arc::new(ImageData {
                    width: img.width,
                    height: img.height,
                    pixels,
                }))
            }
            (val, PortValue::None) => val.clone(),
            (PortValue::None, val) => val.clone(),
            _ => PortValue::None,
        };

        vec![(0, result)]
    }

    fn type_tag(&self) -> &str {
        "add"
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<AddNode>(state.clone()) {
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
    registry.register("add", |state| {
        if let Ok(n) = serde_json::from_value::<AddNode>(state.clone()) {
            Box::new(n)
        } else {
            Box::new(AddNode::default())
        }
    });
}
