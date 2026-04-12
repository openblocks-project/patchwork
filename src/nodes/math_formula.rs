use crate::graph::*;
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Serialize, Deserialize};
use std::collections::{BTreeSet, HashMap};

/// Detect single uppercase letters A-Z used as variables in the formula
fn detect_variables(formula: &str) -> Vec<char> {
    let mut vars = BTreeSet::new();
    let chars: Vec<char> = formula.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if ch.is_ascii_uppercase() {
            let prev_alpha = i > 0 && chars[i - 1].is_ascii_alphabetic();
            let next_alpha = i + 1 < chars.len() && chars[i + 1].is_ascii_alphabetic();
            if !prev_alpha && !next_alpha {
                vars.insert(ch);
            }
        }
    }
    vars.into_iter().collect()
}

/// Evaluate formula using Rhai with variable substitution
fn evaluate_formula(formula: &str, var_values: &HashMap<char, f64>) -> Result<f64, String> {
    let mut engine = rhai::Engine::new();
    engine.set_max_operations(10000);

    engine.register_fn("map", |val: f64, in_min: f64, in_max: f64, out_min: f64, out_max: f64| -> f64 {
        let t = (val - in_min) / (in_max - in_min);
        out_min + t * (out_max - out_min)
    });
    engine.register_fn("lerp", |a: f64, b: f64, t: f64| -> f64 { a + (b - a) * t });
    engine.register_fn("clamp", |val: f64, min: f64, max: f64| -> f64 { val.max(min).min(max) });
    engine.register_fn("sign", |val: f64| -> f64 { if val > 0.0 { 1.0 } else if val < 0.0 { -1.0 } else { 0.0 } });
    engine.register_fn("deg", |rad: f64| -> f64 { rad * 180.0 / std::f64::consts::PI });
    engine.register_fn("rad", |deg: f64| -> f64 { deg * std::f64::consts::PI / 180.0 });
    engine.register_fn("quantize", |val: f64, step: f64| -> f64 {
        if step.abs() < 1e-10 { val } else { (val / step).round() * step }
    });
    engine.register_fn("wrap", |val: f64, min: f64, max: f64| -> f64 {
        let range = max - min;
        if range.abs() < 1e-10 { min } else { min + ((val - min) % range + range) % range }
    });
    engine.register_fn("pow", |base: f64, exp: f64| -> f64 { base.powf(exp) });
    engine.register_fn("sqrt", |val: f64| -> f64 { val.sqrt() });
    engine.register_fn("abs_val", |val: f64| -> f64 { val.abs() });
    engine.register_fn("ln", |val: f64| -> f64 { val.ln() });
    engine.register_fn("log2", |val: f64| -> f64 { val.log2() });
    engine.register_fn("log10", |val: f64| -> f64 { val.log10() });

    let mut expr = formula.to_string();
    let mut var_names: Vec<char> = var_values.keys().copied().collect();
    var_names.sort();

    for &ch in var_names.iter().rev() {
        let to = format!("_var_{}_", ch.to_ascii_lowercase());
        let mut result = String::new();
        let chars_vec: Vec<char> = expr.chars().collect();
        for (i, &c) in chars_vec.iter().enumerate() {
            if c == ch {
                let prev_alpha = i > 0 && chars_vec[i - 1].is_ascii_alphabetic();
                let next_alpha = i + 1 < chars_vec.len() && chars_vec[i + 1].is_ascii_alphabetic();
                if !prev_alpha && !next_alpha {
                    result.push_str(&to);
                } else { result.push(c); }
            } else { result.push(c); }
        }
        expr = result;
    }

    expr = expr.replace('×', "*").replace('÷', "/");

    let mut scope = rhai::Scope::new();
    scope.push_constant("PI", std::f64::consts::PI);
    scope.push_constant("TAU", std::f64::consts::TAU);
    scope.push_constant("E", std::f64::consts::E);

    for (&ch, &val) in var_values {
        scope.push(&format!("_var_{}_", ch.to_ascii_lowercase()), val);
    }

    match engine.eval_expression_with_scope::<rhai::Dynamic>(&mut scope, &expr) {
        Ok(val) => {
            if let Some(f) = val.as_float().ok() { Ok(f) }
            else if let Some(i) = val.as_int().ok() { Ok(i as f64) }
            else { Err("Non-numeric result".into()) }
        }
        Err(e) => Err(format!("{}", e)),
    }
}

// ── Presets organized by category ──────────────────────────────────────────

struct PresetGroup {
    name: &'static str,
    presets: &'static [(&'static str, &'static str)],
}

const PRESET_GROUPS: &[PresetGroup] = &[
    PresetGroup { name: "Basic", presets: &[
        ("+", "A + B"), ("−", "A - B"), ("×", "A * B"), ("÷", "A / B"), ("%", "A % B"),
    ]},
    PresetGroup { name: "Shape", presets: &[
        ("abs", "abs_val(A)"), ("sign", "sign(A)"), ("round", "A.round()"),
        ("floor", "A.floor()"), ("ceil", "A.ceiling()"),
    ]},
    PresetGroup { name: "Range", presets: &[
        ("clamp", "clamp(A, 0.0, 1.0)"), ("wrap", "wrap(A, 0.0, 1.0)"),
        ("map", "map(A, 0.0, 1.0, 0.0, 100.0)"), ("lerp", "lerp(A, B, C)"),
        ("quant", "quantize(A, 0.25)"),
    ]},
    PresetGroup { name: "Trig", presets: &[
        ("sin", "sin(A)"), ("cos", "cos(A)"), ("tan", "tan(A)"),
        ("deg", "deg(A)"), ("rad", "rad(A)"),
    ]},
    PresetGroup { name: "Power", presets: &[
        ("pow", "pow(A, B)"), ("sqrt", "sqrt(A)"), ("log", "ln(A)"),
        ("min", "if A < B { A } else { B }"), ("max", "if A > B { A } else { B }"),
    ]},
];

// ── Node struct ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MathNode {
    #[serde(default = "default_formula")]
    pub formula: String,
    #[serde(default)]
    pub variables: Vec<char>,
    #[serde(default)]
    pub result: f64,
    #[serde(skip)]
    pub error: String,
}

fn default_formula() -> String { "A + B".to_string() }

impl Default for MathNode {
    fn default() -> Self {
        Self {
            formula:   "A + B".to_string(),
            variables: vec!['A', 'B'],
            result:    0.0,
            error:     String::new(),
        }
    }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for MathNode {
    fn title(&self)      -> &str    { "Math" }
    fn type_tag(&self)   -> &str    { "math" }
    fn color_hint(&self) -> [u8; 3] { [200, 160, 80] }
    fn no_title(&self)   -> bool    { true }
    fn inline_ports(&self) -> bool  { true }

    fn inputs(&self) -> Vec<PortDef> {
        self.variables.iter()
            .map(|c| PortDef::dynamic(c.to_string(), PortKind::Number))
            .collect()
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Result", PortKind::Number)]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Build variable map from the inputs array (already resolved by the graph).
        // Unwired ports arrive as PortValue::None → as_float() → 0.0, same as before.
        let mut var_values: HashMap<char, f64> = HashMap::new();
        for (i, &ch) in self.variables.iter().enumerate() {
            let val = inputs.get(i).map(|v| v.as_float() as f64).unwrap_or(0.0);
            var_values.insert(ch, val);
        }

        if !self.formula.is_empty() {
            match evaluate_formula(&self.formula, &var_values) {
                Ok(val) => {
                    self.result = val;
                    self.error.clear();
                }
                Err(e) => {
                    self.error = e;
                }
            }
        }

        vec![(0, PortValue::Float(self.result as f32))]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "formula":   self.formula,
            "variables": self.variables.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
            "result":    self.result,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(f) = state.get("formula").and_then(|v| v.as_str()) {
            self.formula = f.to_string();
            self.variables = detect_variables(&self.formula);
        }
        if let Some(r) = state.get("result").and_then(|v| v.as_f64()) {
            self.result = r;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        render_content(
            ui,
            &mut self.formula,
            &mut self.variables,
            &mut self.result,
            &mut self.error,
            ctx,
        );
    }
}

// ── Render ────────────────────────────────────────────────────────────────────

fn render_content(
    ui: &mut egui::Ui,
    formula: &mut String,
    variables: &mut Vec<char>,
    result: &mut f64,
    error: &mut String,
    ctx: &mut RenderContext,
) {
    let node_id = ctx.node_id;
    let accent = ui.visuals().hyperlink_color;
    let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

    // ── Preset buttons ────────────────────────────────────────
    for group in PRESET_GROUPS {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(format!("{}:", group.name)).small().color(dim));
            for (label, preset_formula) in group.presets.iter() {
                let is_current = formula.trim() == *preset_formula;
                let text = if is_current {
                    egui::RichText::new(*label).small().strong().color(accent)
                } else {
                    egui::RichText::new(*label).small().strong()
                };
                let btn = ui.add(
                    egui::Button::new(text).small().frame(true)
                        .fill(if is_current { accent.gamma_multiply(0.2) } else { ui.visuals().widgets.inactive.bg_fill })
                );
                if btn.clicked() {
                    *formula = preset_formula.to_string();
                    *variables = detect_variables(formula);
                }
                if btn.hovered() {
                    btn.on_hover_text(*preset_formula);
                }
            }
        });
    }

    ui.separator();

    // ── Formula editor ────────────────────────────────────────
    let old_formula = formula.clone();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("f =").strong());
        ui.add(
            egui::TextEdit::singleline(formula)
                .desired_width(ui.available_width() - 4.0)
                .font(egui::FontId::monospace(13.0))
                .hint_text("A + B"),
        );
    });

    if *formula != old_formula {
        *variables = detect_variables(formula);
    }

    ui.separator();

    // ── Input ports (one per detected variable) ───────────────
    for (i, &ch) in variables.iter().enumerate() {
        let is_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == i);
        // Display the value that evaluate() already computed this frame.
        let val = Graph::static_input_value(ctx.connections, ctx.values, node_id, i).as_float() as f64;

        ui.horizontal(|ui| {
            super::inline_port_circle(ui, node_id, i, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new(format!("{}", ch)).strong());
            if is_wired {
                ui.label(egui::RichText::new(format!("{:.3}", val)).monospace().small().color(accent));
            } else {
                ui.label(egui::RichText::new("0").monospace().small().color(dim));
            }
        });
    }

    // ── Error badge management (needs node_id, lives here not in evaluate) ──
    if error.is_empty() {
        crate::node_errors::clear(node_id);
    } else {
        crate::node_errors::report_for(node_id, format!("Formula: {}", error));
    }

    // ── Result output port ────────────────────────────────────
    if error.is_empty() {
        super::output_port_row(ui, "=", &format!("{:.4}", result),
            node_id, 0, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Number);
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(255, 100, 100),
            egui::RichText::new(&*error).small(),
        );
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("math", |state| {
        let mut n = MathNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
