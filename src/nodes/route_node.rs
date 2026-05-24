use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Deserialize, Serialize};
use eframe::egui;

// ── Condition logic ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConditionOp {
    // Numeric comparisons
    #[serde(rename = ">")] GreaterThan,
    #[serde(rename = "<")] LessThan,
    #[serde(rename = ">=")] GreaterOrEqual,
    #[serde(rename = "<=")] LessOrEqual,
    #[serde(rename = "=")] NumericEqual,
    #[serde(rename = "!=")] NumericNotEqual,
    #[serde(rename = "in")] InRange,
    // Text comparisons
    #[serde(rename = "eq")] TextEquals,
    #[serde(rename = "neq")] TextNotEquals,
    #[serde(rename = "has")] TextContains,
    #[serde(rename = "sw")] TextStartsWith,
    #[serde(rename = "ew")] TextEndsWith,
    // Universal
    #[serde(rename = "none")] IsNone,
    #[serde(rename = "any")] IsConnected,
}

impl ConditionOp {
    fn label(&self) -> &'static str {
        match self {
            Self::GreaterThan     => ">",
            Self::LessThan        => "<",
            Self::GreaterOrEqual  => ">=",
            Self::LessOrEqual     => "<=",
            Self::NumericEqual    => "= (num)",
            Self::NumericNotEqual => "≠ (num)",
            Self::InRange         => "in range",
            Self::TextEquals      => "= (text)",
            Self::TextNotEquals   => "≠ (text)",
            Self::TextContains    => "contains",
            Self::TextStartsWith  => "starts with",
            Self::TextEndsWith    => "ends with",
            Self::IsNone          => "is empty",
            Self::IsConnected     => "is set",
        }
    }

    fn is_numeric(&self) -> bool {
        matches!(self,
            Self::GreaterThan | Self::LessThan | Self::GreaterOrEqual |
            Self::LessOrEqual | Self::NumericEqual | Self::NumericNotEqual |
            Self::InRange
        )
    }

    fn is_text(&self) -> bool {
        matches!(self,
            Self::TextEquals | Self::TextNotEquals | Self::TextContains |
            Self::TextStartsWith | Self::TextEndsWith
        )
    }
}

const ALL_OPS: &[ConditionOp] = &[
    ConditionOp::GreaterThan, ConditionOp::LessThan, ConditionOp::GreaterOrEqual,
    ConditionOp::LessOrEqual, ConditionOp::NumericEqual, ConditionOp::NumericNotEqual,
    ConditionOp::InRange,
    ConditionOp::TextEquals, ConditionOp::TextNotEquals, ConditionOp::TextContains,
    ConditionOp::TextStartsWith, ConditionOp::TextEndsWith,
    ConditionOp::IsNone, ConditionOp::IsConnected,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionRow {
    pub label: String,
    pub op: ConditionOp,
    #[serde(default)] pub rhs_float: f32,
    #[serde(default)] pub rhs_text: String,
    #[serde(default = "default_one")] pub range_hi: f32,
    // Runtime state — not serialized
    #[serde(skip)] pub last_matched: bool,
}

fn default_one() -> f32 { 1.0 }

fn port_as_text(v: &PortValue) -> String {
    match v {
        PortValue::Text(s) => s.clone(),
        PortValue::Float(f) => format!("{}", f),
        _ => String::new(),
    }
}

fn evaluate_condition(value: &PortValue, row: &ConditionRow) -> bool {
    match &row.op {
        ConditionOp::GreaterThan     => value.as_float() > row.rhs_float,
        ConditionOp::LessThan        => value.as_float() < row.rhs_float,
        ConditionOp::GreaterOrEqual  => value.as_float() >= row.rhs_float,
        ConditionOp::LessOrEqual     => value.as_float() <= row.rhs_float,
        ConditionOp::NumericEqual    => (value.as_float() - row.rhs_float).abs() < 1e-5,
        ConditionOp::NumericNotEqual => (value.as_float() - row.rhs_float).abs() >= 1e-5,
        ConditionOp::InRange         => {
            let v = value.as_float();
            v >= row.rhs_float && v <= row.range_hi
        }
        ConditionOp::TextEquals      => port_as_text(value) == row.rhs_text,
        ConditionOp::TextNotEquals   => port_as_text(value) != row.rhs_text,
        ConditionOp::TextContains    => port_as_text(value).contains(row.rhs_text.as_str()),
        ConditionOp::TextStartsWith  => port_as_text(value).starts_with(row.rhs_text.as_str()),
        ConditionOp::TextEndsWith    => port_as_text(value).ends_with(row.rhs_text.as_str()),
        ConditionOp::IsNone          => matches!(value, PortValue::None),
        ConditionOp::IsConnected     => !matches!(value, PortValue::None),
    }
}

fn format_value(v: &PortValue) -> String {
    match v {
        PortValue::Float(f) => format!("{:.3}", f),
        PortValue::Text(s) if s.chars().count() > 16 => {
            let head: String = s.chars().take(16).collect();
            format!("\"{}…\"", head)
        }
        PortValue::Text(s) => format!("\"{}\"", s),
        PortValue::Image(img) => format!("[{}×{}]", img.width, img.height),
        PortValue::GpuImage(h) => format!("[gpu {}×{}]", h.width, h.height),
        PortValue::Mesh(p) => format!("[mesh {}v]", p.mesh.vertices.len()),
        PortValue::GpuMesh(h) => format!("[gpu mesh {}v]", h.vertex_count),
        PortValue::None => "—".into(),
    }
}

// ── RouteNode ────────────────────────────────────────────────────────────────

fn default_conditions() -> Vec<ConditionRow> {
    vec![
        ConditionRow { label: "A".into(), op: ConditionOp::TextEquals,
            rhs_float: 0.0, rhs_text: String::new(), range_hi: 1.0, last_matched: false },
        ConditionRow { label: "B".into(), op: ConditionOp::TextEquals,
            rhs_float: 0.0, rhs_text: String::new(), range_hi: 1.0, last_matched: false },
    ]
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteNode {
    #[serde(default = "default_conditions")]
    pub conditions: Vec<ConditionRow>,
    #[serde(default = "default_true")]
    pub first_match: bool,
    #[serde(default)]
    pub default_output: bool,
}

impl Default for RouteNode {
    fn default() -> Self {
        Self {
            conditions: default_conditions(),
            first_match: true,
            default_output: false,
        }
    }
}

impl NodeBehavior for RouteNode {
    fn title(&self) -> &str { "Route" }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Value", PortKind::Generic),
            PortDef::new("Test", PortKind::Generic),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        let mut ports = Vec::new();
        for row in &self.conditions {
            ports.push(PortDef::dynamic(row.label.clone().into(), PortKind::Generic));
            ports.push(PortDef::dynamic(format!("{} gate", row.label).into(), PortKind::Gate));
        }
        if self.default_output {
            ports.push(PortDef::new("Default", PortKind::Generic));
            ports.push(PortDef::new("Default gate", PortKind::Gate));
        }
        ports
    }

    fn color_hint(&self) -> [u8; 3] { [220, 100, 60] }
    fn inline_ports(&self) -> bool { true }
    fn min_width(&self) -> Option<f32> { Some(290.0) }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let value = inputs.first().cloned().unwrap_or(PortValue::None);
        let test_val = match inputs.get(1) {
            Some(PortValue::None) | None => value.clone(),
            Some(v) => v.clone(),
        };

        let mut results: Vec<(usize, PortValue)> = Vec::new();
        let mut any_matched = false;

        for (i, row) in self.conditions.iter_mut().enumerate() {
            let matched = if self.first_match && any_matched {
                false
            } else {
                evaluate_condition(&test_val, row)
            };
            row.last_matched = matched;
            if matched { any_matched = true; }
            results.push((i * 2, if matched { value.clone() } else { PortValue::None }));
            results.push((i * 2 + 1, PortValue::Float(if matched { 1.0 } else { 0.0 })));
        }

        if self.default_output {
            let base = self.conditions.len() * 2;
            let fires = !any_matched;
            results.push((base, if fires { value.clone() } else { PortValue::None }));
            results.push((base + 1, PortValue::Float(if fires { 1.0 } else { 0.0 })));
        }

        results
    }

    fn type_tag(&self) -> &str { "route" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<RouteNode>(state.clone()) {
            *self = loaded;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let node_id = ctx.node_id;

        // ── Input ports ──────────────────────────────────────────────────────
        let val_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        let test_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);

        let val_pv = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 0);
        let test_pv = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 1);

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
            ui.label(egui::RichText::new("Value:").small());
            if val_wired {
                ui.label(egui::RichText::new(format_value(&val_pv)).small().color(ui.visuals().hyperlink_color));
            } else {
                ui.label(egui::RichText::new("—").small().color(dim));
            }
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
            ui.label(egui::RichText::new("Test:").small());
            if test_wired {
                ui.label(egui::RichText::new(format_value(&test_pv)).small().color(egui::Color32::from_rgb(200, 200, 80)));
            } else {
                ui.label(egui::RichText::new("(= Value)").small().color(dim));
            }
        });

        ui.separator();

        // ── Mode toggle ──────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Mode:").small().color(dim));
            if ui.selectable_label(self.first_match, egui::RichText::new("First match").small()).clicked() {
                self.first_match = true;
            }
            if ui.selectable_label(!self.first_match, egui::RichText::new("All matches").small()).clicked() {
                self.first_match = false;
            }
        });

        ui.separator();

        // ── Condition rows ───────────────────────────────────────────────────
        let mut remove_idx: Option<usize> = None;
        let n = self.conditions.len();

        for i in 0..n {
            let row = &mut self.conditions[i];
            let out_data_port = i * 2;
            let out_gate_port = i * 2 + 1;

            let matched = row.last_matched;
            let match_color = if matched {
                egui::Color32::from_rgb(80, 220, 100)
            } else {
                egui::Color32::from_rgb(60, 60, 70)
            };

            ui.horizontal(|ui| {
                // Remove button
                if ui.small_button(egui::RichText::new("−").color(egui::Color32::from_rgb(180, 80, 80))).clicked() {
                    remove_idx = Some(i);
                }

                // Match status dot
                let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 4.0, match_color);

                // Label
                ui.add(egui::TextEdit::singleline(&mut row.label).desired_width(38.0).font(egui::TextStyle::Small));

                // Op dropdown
                egui::ComboBox::from_id_salt(("route_op", node_id, i))
                    .selected_text(egui::RichText::new(row.op.label()).small())
                    .width(80.0)
                    .show_ui(ui, |ui| {
                        for op in ALL_OPS {
                            ui.selectable_value(&mut row.op, op.clone(), op.label());
                        }
                    });

                // RHS input
                if row.op.is_numeric() {
                    ui.add(egui::DragValue::new(&mut row.rhs_float).speed(0.01).max_decimals(3));
                    if row.op == ConditionOp::InRange {
                        ui.label(egui::RichText::new("–").small());
                        ui.add(egui::DragValue::new(&mut row.range_hi).speed(0.01).max_decimals(3));
                    }
                } else if row.op.is_text() {
                    ui.add(egui::TextEdit::singleline(&mut row.rhs_text).desired_width(60.0).font(egui::TextStyle::Small));
                }

                // Output ports — right-aligned
                let remaining = ui.available_width() - 36.0;
                if remaining > 0.0 { ui.add_space(remaining); }
                // Gate port first (rightmost), then data port
                crate::nodes::inline_port_circle(ui, node_id, out_gate_port, false,
                    ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Gate);
                crate::nodes::inline_port_circle(ui, node_id, out_data_port, false,
                    ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
            });
        }

        // Apply row removal after loop
        if let Some(idx) = remove_idx {
            self.conditions.remove(idx);
            // Push all output ports from idx*2 onwards to pending_disconnects
            // so the app's stale sweep can clean up connections on removed rows
            let old_out_count = (n * 2) + if self.default_output { 2 } else { 0 };
            for p in (idx * 2)..old_out_count {
                ctx.pending_disconnects.push((node_id, p));
            }
        }

        // Add condition button
        ui.separator();
        ui.horizontal(|ui| {
            if ui.small_button("+ Add condition").clicked() {
                let label = (b'A' + (self.conditions.len() as u8 % 26)) as char;
                self.conditions.push(ConditionRow {
                    label: label.to_string(),
                    op: ConditionOp::TextEquals,
                    rhs_float: 0.0,
                    rhs_text: String::new(),
                    range_hi: 1.0,
                    last_matched: false,
                });
            }
            // Default output toggle
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.checkbox(&mut self.default_output, egui::RichText::new("Default").small());
            });
        });

        // Default output ports (if enabled)
        if self.default_output {
            let base = self.conditions.len() * 2;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("  Default:").small().color(dim));
                let remaining = ui.available_width() - 36.0;
                if remaining > 0.0 { ui.add_space(remaining); }
                crate::nodes::inline_port_circle(ui, node_id, base + 1, false,
                    ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Gate);
                crate::nodes::inline_port_circle(ui, node_id, base, false,
                    ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
            });
        }
    }
}

// ── SwitchNode ───────────────────────────────────────────────────────────────

fn default_input_count() -> usize { 2 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchNode {
    #[serde(default = "default_input_count")]
    pub input_count: usize,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(skip)]
    pub selected_idx: usize,
}

impl Default for SwitchNode {
    fn default() -> Self {
        Self {
            input_count: 2,
            labels: vec!["In 0".into(), "In 1".into()],
            selected_idx: 0,
        }
    }
}

impl SwitchNode {
    fn label_for(&self, i: usize) -> String {
        self.labels.get(i).cloned().unwrap_or_else(|| format!("In {}", i))
    }

    fn ensure_labels(&mut self) {
        while self.labels.len() < self.input_count {
            let i = self.labels.len();
            self.labels.push(format!("In {}", i));
        }
    }
}

impl NodeBehavior for SwitchNode {
    fn title(&self) -> &str { "Switch" }

    fn inputs(&self) -> Vec<PortDef> {
        let mut ports = vec![PortDef::new("Selector", PortKind::Generic)];
        for i in 0..self.input_count {
            ports.push(PortDef::dynamic(self.label_for(i).into(), PortKind::Generic));
        }
        ports
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Out", PortKind::Generic),
            PortDef::new("Index", PortKind::Number),
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [60, 160, 220] }
    fn inline_ports(&self) -> bool { true }
    fn min_width(&self) -> Option<f32> { Some(220.0) }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let selector = inputs.first().cloned().unwrap_or(PortValue::None);

        let idx: usize = match &selector {
            PortValue::Float(f) => {
                if *f < 0.0 { 0 }
                else { (*f as usize).min(self.input_count.saturating_sub(1)) }
            }
            PortValue::Text(s) => {
                let s_low = s.to_lowercase();
                self.labels.iter()
                    .position(|l| l.to_lowercase() == s_low)
                    .unwrap_or(0)
            }
            _ => 0,
        };
        self.selected_idx = idx;

        let out = inputs.get(idx + 1).cloned().unwrap_or(PortValue::None);
        vec![
            (0, out),
            (1, PortValue::Float(idx as f32)),
        ]
    }

    fn type_tag(&self) -> &str { "switch" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<SwitchNode>(state.clone()) {
            *self = loaded;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let node_id = ctx.node_id;
        self.ensure_labels();

        // Selector port
        let sel_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        let sel_pv = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, 0);

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
            ui.label(egui::RichText::new("Sel:").small());
            if sel_wired {
                ui.label(egui::RichText::new(format_value(&sel_pv)).small().color(egui::Color32::from_rgb(200, 200, 80)));
            } else {
                ui.label(egui::RichText::new("—").small().color(dim));
            }
        });

        ui.separator();

        // Input rows
        let active_idx = self.selected_idx;
        for i in 0..self.input_count {
            let port = i + 1; // port 0 = selector, inputs start at 1
            let is_active = i == active_idx;
            let row_color = if is_active {
                egui::Color32::from_rgb(80, 220, 120)
            } else {
                dim
            };

            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, node_id, port, true, ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);

                // Editable label
                let label = self.labels.get_mut(i).unwrap();
                ui.add(egui::TextEdit::singleline(label).desired_width(80.0).font(egui::TextStyle::Small));

                if is_active {
                    ui.label(egui::RichText::new("◀").small().color(row_color));
                }
            });
            let _ = row_color; // suppress unused warning
        }

        ui.separator();

        // +/- count buttons
        ui.horizontal(|ui| {
            if ui.small_button("−").clicked() && self.input_count > 1 {
                self.input_count -= 1;
                // Stale sweep in app/mod.rs will remove connections to port input_count+1
            }
            ui.label(egui::RichText::new(format!("{} inputs", self.input_count)).small().color(dim));
            if ui.small_button("+").clicked() && self.input_count < 8 {
                self.input_count += 1;
                self.ensure_labels();
            }
        });

        ui.separator();

        // Output ports
        let out_pv = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, node_id, active_idx + 1);
        crate::nodes::output_port_row(ui, "Out", &format_value(&out_pv), node_id, 0, ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Generic);
        crate::nodes::output_port_row(ui, "Index", &format!("{}", active_idx), node_id, 1, ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register_route(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("route", |state| {
        if let Ok(node) = serde_json::from_value::<RouteNode>(state.clone()) {
            Box::new(node)
        } else {
            Box::new(RouteNode::default())
        }
    });
}

pub fn register_switch(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("switch", |state| {
        if let Ok(node) = serde_json::from_value::<SwitchNode>(state.clone()) {
            Box::new(node)
        } else {
            Box::new(SwitchNode::default())
        }
    });
}
