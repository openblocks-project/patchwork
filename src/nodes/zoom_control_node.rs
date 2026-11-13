use crate::graph::{Graph, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoomControlNode {
    #[serde(default = "default_one")]
    pub zoom_value: f32,
}

fn default_one() -> f32 {
    1.0
}

impl Default for ZoomControlNode {
    fn default() -> Self {
        Self { zoom_value: 1.0 }
    }
}

impl NodeBehavior for ZoomControlNode {
    fn title(&self) -> &str {
        "Zoom"
    }
    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Zoom", PortKind::Number)]
    }
    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Zoom", PortKind::Number)]
    }
    fn color_hint(&self) -> [u8; 3] {
        [160, 160, 160]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![(0, PortValue::Float(self.zoom_value))]
    }

    fn type_tag(&self) -> &str {
        "zoom_control"
    }
    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<ZoomControlNode>(state.clone()) {
            *self = l;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        // Read current zoom from app
        let current_zoom = ui.ctx().data_mut(|d| {
            d.get_temp::<f32>(egui::Id::new("current_zoom"))
                .unwrap_or(1.0)
        });

        // If input port is connected, use that value
        let connected = ctx
            .connections
            .iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        if connected {
            let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
            self.zoom_value = v.as_float().clamp(0.1, 5.0);
        } else {
            self.zoom_value = current_zoom;
        }

        ui.horizontal(|ui| {
            ui.label(format!("{:.0}%", self.zoom_value * 100.0));
            let mut z = self.zoom_value;
            if ui
                .add(
                    egui::Slider::new(&mut z, 0.1..=5.0)
                        .logarithmic(true)
                        .show_value(false),
                )
                .changed()
            {
                self.zoom_value = z;
                ui.ctx()
                    .data_mut(|d| d.insert_temp(egui::Id::new("zoom_action"), z));
            }
        });

        ui.horizontal(|ui| {
            for (label, val) in [("50%", 0.5), ("100%", 1.0), ("200%", 2.0)] {
                if ui.small_button(label).clicked() {
                    self.zoom_value = val;
                    ui.ctx()
                        .data_mut(|d| d.insert_temp(egui::Id::new("zoom_action"), val));
                }
            }
        });
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("zoom_control", |state| {
        if let Ok(n) = serde_json::from_value::<ZoomControlNode>(state.clone()) {
            Box::new(n)
        } else {
            Box::new(ZoomControlNode::default())
        }
    });
}
