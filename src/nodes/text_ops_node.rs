use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;

// ── Case-insensitive replace helper ──────────────────────────────────────────

fn case_insensitive_replace(text: &str, search: &str, replace: &str) -> String {
    if search.is_empty() { return text.to_string(); }
    let lo_text   = text.to_lowercase();
    let lo_search = search.to_lowercase();
    let mut result = String::with_capacity(text.len());
    let mut start = 0;
    while let Some(pos) = lo_text[start..].find(lo_search.as_str()) {
        let abs = start + pos;
        result.push_str(&text[start..abs]);
        result.push_str(replace);
        start = abs + search.len();
    }
    result.push_str(&text[start..]);
    result
}

fn get_text_input(inputs: &[PortValue], port: usize, default: &str) -> String {
    match inputs.get(port) {
        Some(PortValue::Text(t)) if !t.is_empty() => t.clone(),
        _ => default.to_string(),
    }
}

// ── Node struct ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextOpsNode {
    #[serde(default)] pub search_buf:     String,
    #[serde(default)] pub replace_buf:    String,
    #[serde(default)] pub case_sensitive: bool,

    // Runtime only (for port preview in render)
    #[serde(skip)] pub last_count:  usize,
    #[serde(skip)] pub last_result: String,
    #[serde(skip)] pub last_found:  bool,
}

impl Default for TextOpsNode {
    fn default() -> Self {
        Self {
            search_buf:     String::new(),
            replace_buf:    String::new(),
            case_sensitive: false,
            last_count:     0,
            last_result:    String::new(),
            last_found:     false,
        }
    }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for TextOpsNode {
    fn title(&self)        -> &str        { "Text Ops" }
    fn type_tag(&self)     -> &str        { "text_ops" }
    fn color_hint(&self)   -> [u8; 3]     { [140, 100, 220] }
    fn inline_ports(&self) -> bool        { true }
    fn min_width(&self)    -> Option<f32> { Some(260.0) }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Text",    PortKind::Text),
            PortDef::new("Search",  PortKind::Text),
            PortDef::new("Replace", PortKind::Text),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Result", PortKind::Text),
            PortDef::new("Found",  PortKind::Trigger),
            PortDef::new("Count",  PortKind::Number),
        ]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let text    = get_text_input(inputs, 0, "");
        let search  = get_text_input(inputs, 1, &self.search_buf.clone());
        let replace = get_text_input(inputs, 2, &self.replace_buf.clone());

        let (count, result) = if search.is_empty() {
            (0, text.clone())
        } else if self.case_sensitive {
            let c = text.matches(search.as_str()).count();
            let r = text.replace(search.as_str(), &replace);
            (c, r)
        } else {
            let lo_text   = text.to_lowercase();
            let lo_search = search.to_lowercase();
            let c = lo_text.matches(lo_search.as_str()).count();
            let r = case_insensitive_replace(&text, &search, &replace);
            (c, r)
        };

        self.last_count  = count;
        self.last_found  = count > 0;
        self.last_result = result.clone();

        vec![
            (0, PortValue::Text(result)),
            (1, PortValue::Float(if count > 0 { 1.0 } else { 0.0 })),
            (2, PortValue::Float(count as f32)),
        ]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "search":  self.search_buf,
            "replace": self.replace_buf,
            "case":    self.case_sensitive,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(s) = state.get("search").and_then(|v| v.as_str()) {
            self.search_buf = s.to_string();
        }
        if let Some(s) = state.get("replace").and_then(|v| v.as_str()) {
            self.replace_buf = s.to_string();
        }
        if let Some(b) = state.get("case").and_then(|v| v.as_bool()) {
            self.case_sensitive = b;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim   = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let blue  = egui::Color32::from_rgb(80, 170, 255);

        let text_wired    = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let search_wired  = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 1);
        let replace_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 2);

        // ── Text input port ───────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("Text").small().strong());
            if text_wired {
                let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
                if let PortValue::Text(ref t) = v {
                    let preview = if t.len() > 22 { format!("{}…", &t[..22]) } else { t.clone() };
                    ui.label(egui::RichText::new(preview).small().monospace().color(blue));
                } else {
                    ui.label(egui::RichText::new("—").small().color(dim));
                }
            } else {
                ui.label(egui::RichText::new("—").small().color(dim));
            }
        });

        // ── Search port + case checkbox ───────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("Search").small().strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.case_sensitive, egui::RichText::new("Aa").small());
            });
        });

        if search_wired {
            let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 1);
            if let PortValue::Text(ref t) = v {
                let preview = if t.len() > 28 { format!("{}…", &t[..28]) } else { t.clone() };
                ui.label(egui::RichText::new(format!("  {}", preview)).small().monospace().color(blue));
            }
        } else {
            ui.add(
                egui::TextEdit::singleline(&mut self.search_buf)
                    .hint_text("substring…")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(ui.available_width()),
            );
        }

        // ── Replace port ──────────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 2, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("Replace").small().strong());
        });

        if replace_wired {
            let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 2);
            if let PortValue::Text(ref t) = v {
                let preview = if t.len() > 28 { format!("{}…", &t[..28]) } else { t.clone() };
                ui.label(egui::RichText::new(format!("  {}", preview)).small().monospace().color(blue));
            }
        } else {
            ui.add(
                egui::TextEdit::singleline(&mut self.replace_buf)
                    .hint_text("replacement…")
                    .font(egui::TextStyle::Monospace)
                    .desired_width(ui.available_width()),
            );
        }

        ui.separator();

        // ── Output ports ──────────────────────────────────────────────────
        let result_preview = {
            let s = &self.last_result;
            if s.is_empty() { "—".to_string() }
            else if s.len() > 18 { format!("{}…", &s[..18]) }
            else { s.clone() }
        };
        crate::nodes::output_port_row(ui, "Result", &result_preview,
            ctx.node_id, 0, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Text);

        crate::nodes::output_port_row(ui, "Found",
            if self.last_found { "1" } else { "0" },
            ctx.node_id, 1, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Trigger);

        crate::nodes::output_port_row(ui, "Count",
            &self.last_count.to_string(),
            ctx.node_id, 2, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Number);
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("text_ops", |state| {
        let mut n = TextOpsNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
