//! Pack — bundle N arbitrary inputs into a single JSON object.
//!
//! Mirror of JSON Fields (unpack). Each row declares a `{name, kind}` —
//! Number or Text — and exposes a dynamic input port. The node emits one
//! JSON string with `{name0: v0, name1: v1, ...}` on its single output,
//! ready to feed into Match or send over the network.

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldKind {
    Number,
    Text,
}
impl Default for FieldKind {
    fn default() -> Self { Self::Number }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackField {
    pub name: String,
    pub kind: FieldKind,
}

impl PackField {
    fn new(name: impl Into<String>, kind: FieldKind) -> Self {
        Self { name: name.into(), kind }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackNode {
    pub fields: Vec<PackField>,
    #[serde(skip)] last_output: String,
}

impl Default for PackNode {
    fn default() -> Self {
        Self {
            fields: vec![
                PackField::new("x", FieldKind::Number),
                PackField::new("y", FieldKind::Number),
            ],
            last_output: String::new(),
        }
    }
}

impl NodeBehavior for PackNode {
    fn title(&self)     -> &str    { "Pack (JSON)" }
    fn type_tag(&self)  -> &str    { "pack" }
    fn color_hint(&self) -> [u8; 3] { [120, 200, 140] }
    fn inline_ports(&self) -> bool { true }
    fn min_width(&self)  -> Option<f32> { Some(220.0) }

    fn inputs(&self) -> Vec<PortDef> {
        self.fields.iter().map(|f| {
            let kind = match f.kind { FieldKind::Number => PortKind::Number, FieldKind::Text => PortKind::Text };
            PortDef::dynamic(f.name.clone().into(), kind)
        }).collect()
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("JSON", PortKind::Text)]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let mut map = serde_json::Map::new();
        for (i, field) in self.fields.iter().enumerate() {
            let v = match (field.kind, inputs.get(i)) {
                (FieldKind::Number, Some(PortValue::Float(n))) => serde_json::json!(*n),
                (FieldKind::Number, Some(PortValue::Text(s))) => {
                    s.parse::<f64>().map(|n| serde_json::json!(n)).unwrap_or(serde_json::json!(0.0))
                }
                (FieldKind::Number, _) => serde_json::json!(0.0),
                (FieldKind::Text,   Some(PortValue::Text(s)))  => serde_json::Value::String(s.clone()),
                (FieldKind::Text,   Some(PortValue::Float(n))) => serde_json::Value::String(format!("{n}")),
                (FieldKind::Text,   _) => serde_json::Value::String(String::new()),
            };
            map.insert(field.name.clone(), v);
        }
        let s = serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| "{}".into());
        self.last_output = s.clone();
        vec![(0, PortValue::Text(s))]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<PackNode>(state.clone()) { *self = loaded; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim     = egui::Color32::from_rgb(110, 110, 120);

        // Lock the node to its min_width so adding fields doesn't grow it.
        const NODE_W: f32 = 220.0;
        ui.set_min_width(NODE_W);
        ui.set_max_width(NODE_W);

        let mut delete_idx: Option<usize> = None;

        for (i, field) in self.fields.iter_mut().enumerate() {
            ui.allocate_ui_with_layout(
                egui::vec2(NODE_W, 22.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    let port_kind = match field.kind { FieldKind::Number => PortKind::Number, FieldKind::Text => PortKind::Text };
                    crate::nodes::inline_port_circle(
                        ui, node_id, i, true,
                        ctx.connections, ctx.port_positions,
                        ctx.dragging_from, ctx.pending_disconnects,
                        port_kind,
                    );
                    ui.add_sized(
                        egui::vec2(80.0, 18.0),
                        egui::TextEdit::singleline(&mut field.name)
                            .hint_text(format!("field{i}"))
                            .font(egui::TextStyle::Small),
                    );
                    egui::ComboBox::from_id_salt(egui::Id::new(("pack_kind", node_id, i)))
                        .selected_text(match field.kind { FieldKind::Number => "Num", FieldKind::Text => "Text" })
                        .width(46.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(field.kind == FieldKind::Number, "Num").clicked() {
                                field.kind = FieldKind::Number;
                            }
                            if ui.selectable_label(field.kind == FieldKind::Text, "Text").clicked() {
                                field.kind = FieldKind::Text;
                            }
                        });
                    if ui.small_button("×").clicked() { delete_idx = Some(i); }
                },
            );
        }

        if let Some(i) = delete_idx {
            self.fields.remove(i);
            // Also clean up any connections pointing at the removed port index.
            ctx.pending_disconnects.push((node_id, i));
        }

        ui.horizontal(|ui| {
            if ui.small_button("+ Add field").clicked() {
                let n = self.fields.len();
                self.fields.push(PackField::new(format!("field{n}"), FieldKind::Number));
            }
            ui.label(RichText::new(format!("{} fields", self.fields.len())).small().color(dim));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // Output port. Keep the value label tiny ("N ✓") so it never pushes
        // the node wider; the full JSON is on the port for downstream reads.
        let value_str = format!("{} ✓", self.fields.len());
        ui.allocate_ui_with_layout(
            egui::vec2(NODE_W, 22.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                crate::nodes::output_port_row(ui, "JSON", &value_str, node_id, 0,
                    ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
                    PortKind::Text);
            },
        );
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("pack", |state| {
        let mut n = PackNode::default();
        n.load_state(state);
        Box::new(n)
    });
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_numbers_into_object() {
        let mut n = PackNode::default();
        n.fields = vec![
            PackField::new("x", FieldKind::Number),
            PackField::new("y", FieldKind::Number),
            PackField::new("z", FieldKind::Number),
        ];
        let out = n.evaluate(&[PortValue::Float(1.0), PortValue::Float(2.5), PortValue::Float(-3.0)]);
        assert_eq!(out.len(), 1);
        let PortValue::Text(ref s) = out[0].1 else { panic!("expected Text out"); };
        let v: serde_json::Value = serde_json::from_str(s).unwrap();
        assert_eq!(v.get("x").and_then(|x| x.as_f64()), Some(1.0));
        assert_eq!(v.get("y").and_then(|x| x.as_f64()), Some(2.5));
        assert_eq!(v.get("z").and_then(|x| x.as_f64()), Some(-3.0));
    }

    #[test]
    fn packs_mixed_types() {
        let mut n = PackNode::default();
        n.fields = vec![
            PackField::new("amp", FieldKind::Number),
            PackField::new("tag", FieldKind::Text),
        ];
        let out = n.evaluate(&[PortValue::Float(0.7), PortValue::Text("hi".into())]);
        let PortValue::Text(ref s) = out[0].1 else { panic!(); };
        let v: serde_json::Value = serde_json::from_str(s).unwrap();
        let amp = v.get("amp").and_then(|x| x.as_f64()).unwrap();
        assert!((amp - 0.7).abs() < 1e-5, "amp ≈ 0.7, got {amp}");
        assert_eq!(v.get("tag").and_then(|x| x.as_str()), Some("hi"));
    }

    #[test]
    fn coerces_text_to_number_when_declared_number() {
        let mut n = PackNode::default();
        n.fields = vec![PackField::new("v", FieldKind::Number)];
        let out = n.evaluate(&[PortValue::Text("42.5".into())]);
        let PortValue::Text(ref s) = out[0].1 else { panic!(); };
        let v: serde_json::Value = serde_json::from_str(s).unwrap();
        assert_eq!(v.get("v").and_then(|x| x.as_f64()), Some(42.5));
    }

    #[test]
    fn save_load_round_trips_field_config() {
        let mut n = PackNode::default();
        n.fields = vec![
            PackField::new("one", FieldKind::Text),
            PackField::new("two", FieldKind::Number),
        ];
        let state = n.save_state();
        let mut restored = PackNode::default();
        restored.load_state(&state);
        assert_eq!(restored.fields.len(), 2);
        assert_eq!(restored.fields[0].name, "one");
        assert_eq!(restored.fields[0].kind, FieldKind::Text);
        assert_eq!(restored.fields[1].name, "two");
        assert_eq!(restored.fields[1].kind, FieldKind::Number);
    }
}
