use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use crate::nodes::ScrollAreaExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextEditorNode {
    pub content: String,
}

impl Default for TextEditorNode {
    fn default() -> Self {
        Self { content: String::new() }
    }
}

impl NodeBehavior for TextEditorNode {
    fn title(&self) -> &str { "Text Editor" }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Text In", PortKind::Text)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Text Out", PortKind::Text)]
    }

    fn color_hint(&self) -> [u8; 3] { [160, 140, 220] }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        if let Some(PortValue::Text(t)) = inputs.first() {
            vec![(0, PortValue::Text(t.clone()))]
        } else {
            vec![(0, PortValue::Text(self.content.clone()))]
        }
    }

    fn type_tag(&self) -> &str { "text_editor" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<TextEditorNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let input_val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
        let has_input = matches!(input_val, PortValue::Text(_));
        let upstream = if let PortValue::Text(ref t) = input_val { Some(t.clone()) } else { None };

        ui.horizontal(|ui| {
            if has_input {
                let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
                ui.label(egui::RichText::new("(edit to disconnect)").small().color(dim));
            }
            if !has_input {
                if ui.button("Open...").clicked() {
                    if let Some(fp) = rfd::FileDialog::new()
                        .add_filter("All files", &["*"])
                        .add_filter("Text", &["txt", "json", "csv", "wgsl", "toml", "yaml", "rs", "py"])
                        .pick_file()
                    {
                        self.content = std::fs::read_to_string(&fp)
                            .unwrap_or_else(|e| format!("Error: {e}"));
                    }
                }
            }
            if ui.button("Save as...").clicked() {
                if let Some(fp) = rfd::FileDialog::new().save_file() {
                    let _ = std::fs::write(&fp, self.content.as_str());
                }
            }
        });

        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        ui.label(egui::RichText::new(format!("{} chars", self.content.len())).small().color(dim));

        egui::ScrollArea::vertical().max_height(250.0).show_pannable(ui, |ui| {
            if let Some(ref upstream_text) = upstream {
                self.content = upstream_text.clone();
                let before = self.content.clone();
                ui.add(
                    egui::TextEdit::multiline(&mut self.content)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(10),
                );
                if self.content != before {
                    ctx.pending_disconnects.push((ctx.node_id, 0));
                }
            } else {
                ui.add(
                    egui::TextEdit::multiline(&mut self.content)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(10),
                );
            }
        });

        // Open in external editor
        if ui.small_button(egui::RichText::new("Open in editor").small()).clicked() {
            let tmp = std::env::temp_dir().join(format!("patchwork_text_{}.txt", ctx.node_id));
            if std::fs::write(&tmp, &self.content).is_ok() {
                #[cfg(target_os = "macos")]
                let _ = std::process::Command::new("open").arg(&tmp).spawn();
                #[cfg(target_os = "windows")]
                let _ = std::process::Command::new("cmd").args(["/C", "start", "", &tmp.display().to_string()]).spawn();
                #[cfg(target_os = "linux")]
                let _ = std::process::Command::new("xdg-open").arg(&tmp).spawn();
            }
        }

        // Show truncated output
        let short = if self.content.len() > 30 {
            format!("\"{}...\"", &self.content[..30])
        } else {
            format!("\"{}\"", &self.content)
        };
        let dim2 = ui.visuals().widgets.noninteractive.fg_stroke.color;
        ui.label(egui::RichText::new(format!("Text Out: {}", short)).small().color(dim2));
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("text_editor", |state| {
        if let Ok(n) = serde_json::from_value::<TextEditorNode>(state.clone()) { Box::new(n) }
        else { Box::new(TextEditorNode::default()) }
    });
}
