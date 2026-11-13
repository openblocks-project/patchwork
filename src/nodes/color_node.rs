use crate::graph::{Graph, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorNode {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Default for ColorNode {
    fn default() -> Self {
        Self {
            r: 128,
            g: 128,
            b: 255,
        }
    }
}

impl NodeBehavior for ColorNode {
    fn title(&self) -> &str {
        "Color"
    }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("R", PortKind::Color),
            PortDef::new("G", PortKind::Color),
            PortDef::new("B", PortKind::Color),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("R", PortKind::Color),
            PortDef::new("G", PortKind::Color),
            PortDef::new("B", PortKind::Color),
        ]
    }

    fn color_hint(&self) -> [u8; 3] {
        [self.r, self.g, self.b]
    }

    fn inline_ports(&self) -> bool {
        true
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Override from connected inputs
        if let Some(PortValue::Float(f)) = inputs.first() {
            self.r = (*f as i32).clamp(0, 255) as u8;
        }
        if let Some(PortValue::Float(f)) = inputs.get(1) {
            self.g = (*f as i32).clamp(0, 255) as u8;
        }
        if let Some(PortValue::Float(f)) = inputs.get(2) {
            self.b = (*f as i32).clamp(0, 255) as u8;
        }

        vec![
            (0, PortValue::Float(self.r as f32)),
            (1, PortValue::Float(self.g as f32)),
            (2, PortValue::Float(self.b as f32)),
        ]
    }

    fn type_tag(&self) -> &str {
        "color"
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<ColorNode>(state.clone()) {
            *self = loaded;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        // Override from connected inputs
        for i in 0..3 {
            let input = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, i);
            if let PortValue::Float(f) = input {
                match i {
                    0 => self.r = (f as i32).clamp(0, 255) as u8,
                    1 => self.g = (f as i32).clamp(0, 255) as u8,
                    2 => self.b = (f as i32).clamp(0, 255) as u8,
                    _ => {}
                }
            }
        }

        // Color swatch + hex editor
        let mut color = egui::Color32::from_rgb(self.r, self.g, self.b);
        let hex_id = egui::Id::new(("color_hex_d", ctx.node_id));
        let mut hex_str = ui.ctx().data_mut(|d| {
            d.get_temp::<String>(hex_id)
                .unwrap_or_else(|| format!("{:02X}{:02X}{:02X}", self.r, self.g, self.b))
        });

        ui.horizontal(|ui| {
            ui.color_edit_button_srgba(&mut color);
            ui.label("#");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut hex_str)
                    .desired_width(52.0)
                    .font(egui::TextStyle::Monospace)
                    .char_limit(6),
            );
            if resp.changed() || resp.lost_focus() {
                let clean: String = hex_str
                    .chars()
                    .filter(|c| c.is_ascii_hexdigit())
                    .take(6)
                    .collect();
                if clean.len() == 6 {
                    if let Ok(val) = u32::from_str_radix(&clean, 16) {
                        color = egui::Color32::from_rgb(
                            ((val >> 16) & 0xFF) as u8,
                            ((val >> 8) & 0xFF) as u8,
                            (val & 0xFF) as u8,
                        );
                    }
                }
            }
        });

        self.r = color.r();
        self.g = color.g();
        self.b = color.b();
        hex_str = format!("{:02X}{:02X}{:02X}", self.r, self.g, self.b);
        ui.ctx().data_mut(|d| d.insert_temp(hex_id, hex_str));

        ui.separator();

        // Channel rows: input port | label + DragValue | output port
        let channels = ["R", "G", "B"];
        for i in 0..3 {
            let is_wired = ctx
                .connections
                .iter()
                .any(|c| c.to_node == ctx.node_id && c.to_port == i);
            ui.horizontal(|ui| {
                // Input port
                crate::nodes::inline_port_circle(
                    ui,
                    ctx.node_id,
                    i,
                    true,
                    ctx.connections,
                    ctx.port_positions,
                    ctx.dragging_from,
                    ctx.pending_disconnects,
                    PortKind::Color,
                );

                let val = match i {
                    0 => self.r,
                    1 => self.g,
                    2 => self.b,
                    _ => 0,
                };
                if is_wired {
                    ui.label(egui::RichText::new(format!("{} {}", channels[i], val)).monospace());
                } else {
                    let mut v = val as i32;
                    ui.add(
                        egui::DragValue::new(&mut v)
                            .range(0..=255)
                            .prefix(format!("{} ", channels[i])),
                    );
                    let new_val = v.clamp(0, 255) as u8;
                    match i {
                        0 => self.r = new_val,
                        1 => self.g = new_val,
                        2 => self.b = new_val,
                        _ => {}
                    }
                }

                // Output port
                crate::nodes::inline_port_circle(
                    ui,
                    ctx.node_id,
                    i,
                    false,
                    ctx.connections,
                    ctx.port_positions,
                    ctx.dragging_from,
                    ctx.pending_disconnects,
                    PortKind::Color,
                );
            });
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("color", |state| {
        if let Ok(node) = serde_json::from_value::<ColorNode>(state.clone()) {
            Box::new(node)
        } else {
            Box::new(ColorNode::default())
        }
    });
}
