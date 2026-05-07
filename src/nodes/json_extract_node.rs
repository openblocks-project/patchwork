use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use crate::nodes::ScrollAreaExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonExtractNode {
    pub path: String,
}

impl Default for JsonExtractNode {
    fn default() -> Self {
        Self { path: String::new() }
    }
}

impl NodeBehavior for JsonExtractNode {
    fn title(&self) -> &str { "JSON Extract" }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("JSON", PortKind::Text)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Value", PortKind::Generic)]
    }

    fn color_hint(&self) -> [u8; 3] { [200, 160, 60] }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let json_text = match inputs.first() {
            Some(PortValue::Text(s)) => s.clone(),
            _ => String::new(),
        };
        // Idle states (no input yet, no path yet) are not errors — just
        // pass an empty string through and let the badge stay cleared.
        if json_text.is_empty() || self.path.is_empty() {
            return vec![(0, PortValue::Text(String::new()))];
        }

        // Validate the JSON ourselves so a parse failure becomes a real
        // node error instead of the legacy "(parse error)" sentinel.
        // `extract_json_path_pub` returns that literal on failure today,
        // but surfacing it via `node_errors::report` makes it show up on
        // the canvas badge + Console.
        if let Err(e) = serde_json::from_str::<serde_json::Value>(&json_text) {
            crate::node_errors::report(format!("JSON parse: {}", e));
            return vec![(0, PortValue::Text(String::new()))];
        }

        let extracted = crate::graph::extract_json_path_pub(&json_text, &self.path);
        // `extract_json_path` walks missing keys silently and returns "".
        // A truly-empty result when both inputs are non-empty almost always
        // means the path didn't match — surface it so the user knows their
        // dot-path is wrong rather than staring at a blank Value port.
        if extracted.is_empty() {
            crate::node_errors::report(format!("JSON path not found: {}", self.path));
        }
        vec![(0, PortValue::Text(extracted))]
    }

    fn type_tag(&self) -> &str { "json_extract" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<JsonExtractNode>(state.clone()) {
            *self = loaded;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        ui.horizontal(|ui| {
            ui.label("Path:");
            ui.text_edit_singleline(&mut self.path);
        });
        ui.label(egui::RichText::new("(dot-separated, e.g. data.items.0.name)").small()
            .color(ui.visuals().widgets.noninteractive.fg_stroke.color));

        let output_val = ctx.values.get(&(ctx.node_id, 0));
        ui.separator();
        match output_val {
            Some(PortValue::Text(s)) if !s.is_empty() => {
                ui.label("Extracted:");
                egui::ScrollArea::vertical().max_height(80.0).show_pannable(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut s.clone())
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .interactive(false));
                });
            }
            _ => {
                let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
                if self.path.is_empty() {
                    ui.colored_label(dim, "(enter path)");
                } else {
                    ui.colored_label(egui::Color32::from_rgb(200, 80, 80), "(no match)");
                }
            }
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("json_extract", |state| {
        if let Ok(node) = serde_json::from_value::<JsonExtractNode>(state.clone()) {
            Box::new(node)
        } else {
            Box::new(JsonExtractNode::default())
        }
    });
}
