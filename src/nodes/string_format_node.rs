use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringFormatNode {
    pub template: String,
    #[serde(default = "default_arg_count")]
    pub arg_count: usize,
}

fn default_arg_count() -> usize { 2 }

impl Default for StringFormatNode {
    fn default() -> Self {
        Self { template: String::new(), arg_count: 2 }
    }
}

impl NodeBehavior for StringFormatNode {
    fn title(&self) -> &str { "String Format" }

    fn inputs(&self) -> Vec<PortDef> {
        let mut ports = vec![PortDef::new("Template", PortKind::Text)];
        for i in 0..self.arg_count {
            ports.push(PortDef::dynamic(format!("Arg {}", i), PortKind::Generic));
        }
        ports
    }

    fn outputs(&self) -> Vec<PortDef> { vec![PortDef::new("Text", PortKind::Text)] }
    fn color_hint(&self) -> [u8; 3] { [220, 160, 100] }
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let effective_template = match inputs.first() {
            Some(PortValue::Text(s)) if !s.is_empty() => s.clone(),
            _ => self.template.clone(),
        };
        let mut result = effective_template;
        for i in 0..self.arg_count {
            let port = i + 1;
            let replacement = match inputs.get(port) {
                Some(PortValue::Float(f)) => {
                    let s = format!("{:.6}", f);
                    s.trim_end_matches('0').trim_end_matches('.').to_string()
                }
                Some(PortValue::Text(s)) => s.clone(),
                _ => String::new(),
            };
            result = result.replace(&format!("{{{}}}", i), &replacement);
        }
        vec![(0, PortValue::Text(result))]
    }

    fn type_tag(&self) -> &str { "string_format" }
    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<StringFormatNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let accent = ui.visuals().hyperlink_color;
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let tmpl_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);

        // Template port
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("Template:").small());
            if tmpl_wired {
                ui.label(egui::RichText::new("connected").small().color(accent));
            }
        });

        if !tmpl_wired {
            ui.add(egui::TextEdit::multiline(&mut self.template)
                .desired_rows(2).desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace)
                .hint_text("Hello {0}, you are {1}!"));
        } else {
            let t = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
            if let PortValue::Text(s) = &t {
                ui.label(egui::RichText::new(s).small().code());
            }
        }

        ui.separator();

        // Arg count
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Args:").small());
            if ui.small_button("−").clicked() && self.arg_count > 0 { self.arg_count -= 1; }
            ui.label(egui::RichText::new(format!("{}", self.arg_count)).strong());
            if ui.small_button("+").clicked() && self.arg_count < 10 { self.arg_count += 1; }
        });

        // Arg ports
        for i in 0..self.arg_count {
            let port = i + 1;
            let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == port);
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, ctx.node_id, port, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
                ui.label(egui::RichText::new(format!("{{{}}}", i)).small().code());
                if wired {
                    let val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, port);
                    let s = match &val {
                        PortValue::Float(f) => format!("{:.3}", f),
                        PortValue::Text(s) => if s.chars().count() > 20 {
                            let head: String = s.chars().take(20).collect();
                            format!("\"{}...\"", head)
                        } else { format!("\"{}\"", s) },
                        _ => "—".into(),
                    };
                    ui.label(egui::RichText::new(s).small().color(accent));
                } else {
                    ui.label(egui::RichText::new("—").small().color(dim));
                }
            });
        }

        ui.separator();

        // Preview output
        let result = ctx.values.get(&(ctx.node_id, 0));
        if let Some(PortValue::Text(s)) = result {
            ui.label(egui::RichText::new("Output:").small().strong());
            egui::ScrollArea::vertical().max_height(60.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut s.as_str())
                    .desired_width(f32::INFINITY).font(egui::TextStyle::Monospace).interactive(false));
            });
        }

        crate::nodes::output_port_row(ui, "Text", &format!("{} chars", result.map(|v| if let PortValue::Text(s) = v { s.len() } else { 0 }).unwrap_or(0)),
            ctx.node_id, 0, ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Text);
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("string_format", |state| {
        if let Ok(n) = serde_json::from_value::<StringFormatNode>(state.clone()) { Box::new(n) }
        else { Box::new(StringFormatNode::default()) }
    });
}
