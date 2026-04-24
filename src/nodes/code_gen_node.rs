//! CodeGenNode — trait-based code-generation node.
//!
//! Shape mirrors TextGen, but adds a Language dropdown (Rust/WGSL/HTML/
//! JSON) that picks a built-in system prompt tuned for that target.
//! `strip_code_fences()` is always applied to the response, so the
//! output port carries raw code ready to feed into the WGSL Viewer /
//! HTML Viewer / JSON Extract node.
//!
//! Output port 0 is `Code` (Text kind, fences stripped).

use crate::graph::{Graph, NodeId, PortDef, PortKind, PortValue};
use crate::http::HttpAction;
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::ai_shared::{
    build_request_for_config, extract_ai_response, language_system_prompt, strip_code_fences,
    CODE_LANGUAGES,
};
use crate::nodes::text_gen_node::{apply_overrides, read_config, validation_error, GenKind};
use eframe::egui;
use serde::{Deserialize, Serialize};

fn default_temperature() -> f32 { 0.4 }
fn default_max_tokens() -> u32 { 2048 }
fn default_language() -> String { "Rust".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeGenNode {
    #[serde(default = "default_language")] pub language: String,
    #[serde(default)] pub spec: String,
    #[serde(default = "default_temperature")] pub temperature: f32,
    #[serde(default = "default_max_tokens")] pub max_tokens: u32,
    #[serde(default)] pub model_override: String,
    #[serde(default)] pub provider_override: String,

    #[serde(skip)] pub code: String,
    #[serde(skip)] pub status: String,
    #[serde(skip)] pub last_trigger: f32,
}

impl Default for CodeGenNode {
    fn default() -> Self {
        Self {
            language: default_language(),
            spec: String::new(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            model_override: String::new(),
            provider_override: String::new(),
            code: String::new(),
            status: String::new(),
            last_trigger: 0.0,
        }
    }
}

impl NodeBehavior for CodeGenNode {
    fn title(&self) -> &str { "Code Gen" }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Config", PortKind::Text),
            PortDef::new("Spec", PortKind::Text),
            PortDef::new("Send", PortKind::Trigger),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Code", PortKind::Text),
            PortDef::new("Status", PortKind::Text),
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [200, 140, 220] }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![
            (0, PortValue::Text(self.code.clone())),
            (1, PortValue::Text(self.status.clone())),
        ]
    }

    fn type_tag(&self) -> &str { "code_gen" }

    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<CodeGenNode>(state.clone()) { *self = l; }
    }

    fn apply_http_response(&mut self, body: String, status: u16) {
        if (200..300).contains(&status) {
            let provider = if self.provider_override.is_empty() {
                if body.contains("\"candidates\"") { "google" }
                else if body.contains("\"content\":[{\"type\"") { "anthropic" }
                else { "openai" }
            } else {
                self.provider_override.as_str()
            };
            let raw = extract_ai_response(provider, &body);
            self.code = strip_code_fences(&raw);
            self.status = "done".into();
        } else {
            self.code = body;
            self.status = format!("error: {}", status);
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = egui::Color32::from_rgb(140, 140, 155);
        let accent = ui.visuals().hyperlink_color;

        let config_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let spec_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 1);
        let trigger_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 2);

        // ── Input ports + gear ──────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("Config").small()
                .color(if config_wired { accent } else { dim }));
            ui.add_space(6.0);
            if ui.small_button("⚙").on_hover_text("Open AI Config").clicked() {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(
                        egui::Id::new("ai_gear_click"),
                        (ctx.node_id, GenKind::Code.config_output_port()),
                    );
                });
            }
        });
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("Spec").small());
            ui.add_space(8.0);
            crate::nodes::inline_port_circle(ui, ctx.node_id, 2, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
            ui.label(egui::RichText::new("Send").small());
        });

        ui.separator();

        // ── Language dropdown ───────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Language").small());
            egui::ComboBox::from_id_salt(format!("codegen_lang_{}", ctx.node_id))
                .selected_text(&self.language)
                .width(100.0)
                .show_ui(ui, |ui| {
                    for lang in CODE_LANGUAGES {
                        ui.selectable_value(&mut self.language, (*lang).into(), *lang);
                    }
                });
        });

        // ── Spec (prompt) ───────────────────────────────────────
        if !spec_wired {
            ui.add(
                egui::TextEdit::multiline(&mut self.spec)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("Describe what the code should do…")
                    .font(egui::TextStyle::Small),
            );
        }

        // ── Parameters ──────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Temp").small());
            ui.add(egui::Slider::new(&mut self.temperature, 0.0..=2.0).step_by(0.1).show_value(true));
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Max tokens").small());
            ui.add(egui::DragValue::new(&mut self.max_tokens).range(1..=32_000).speed(16));
        });

        // ── Override ────────────────────────────────────────────
        ui.collapsing("Override provider/model", |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Provider").small());
                ui.add(egui::TextEdit::singleline(&mut self.provider_override)
                    .desired_width(100.0).hint_text("custom/…"));
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Model").small());
                ui.add(egui::TextEdit::singleline(&mut self.model_override)
                    .desired_width(160.0).hint_text("e.g. qwen2.5-coder"));
            });
        });

        ui.separator();

        // ── Resolve config + prompts ────────────────────────────
        let mut cfg_opt = read_config(ctx.connections, ctx.values, ctx.node_id, 0);
        if let Some(cfg) = cfg_opt.as_mut() {
            apply_overrides(cfg, &self.provider_override, &self.model_override);
        }
        let eff_spec = if spec_wired {
            match Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 1) {
                PortValue::Text(s) => s, _ => self.spec.clone()
            }
        } else { self.spec.clone() };
        let system = language_system_prompt(&self.language).to_string();

        let validation = validation_error(&cfg_opt);
        let dispatched = std::cell::Cell::new(false);

        let do_send = |ctx: &mut RenderContext, status: &mut String| {
            if ctx.http_pending || dispatched.get() { return; }
            let Some(ref cfg) = cfg_opt else { return; };
            if cfg.model.is_empty() || cfg.api_key.is_empty() { return; }
            let (url, headers, body) = build_request_for_config(
                cfg, &system, &eff_spec, self.max_tokens, self.temperature, 0, None,
            );
            ctx.http_actions.push(HttpAction::SendRequest {
                node_id: ctx.node_id, url, method: "POST".into(), headers, body,
            });
            *status = "thinking…".into();
            dispatched.set(true);
        };

        // ── Send + status ───────────────────────────────────────
        ui.horizontal(|ui| {
            let in_flight = ctx.http_pending || dispatched.get();
            let can_send = !eff_spec.is_empty() && validation.is_none() && !in_flight;
            let btn_text = if in_flight { "⏳ Thinking…" } else { "▶ Generate" };
            if ui.add_enabled(can_send, egui::Button::new(btn_text)).clicked() {
                do_send(ctx, &mut self.status);
            }
            let (text, color) = if let Some(v) = validation.as_ref() {
                (v.clone(), egui::Color32::from_rgb(200, 160, 80))
            } else if eff_spec.is_empty() {
                ("(describe the code)".into(), dim)
            } else if self.status.starts_with("error") {
                (self.status.clone(), egui::Color32::from_rgb(220, 80, 80))
            } else if in_flight || self.status == "thinking…" {
                ("thinking…".into(), egui::Color32::from_rgb(200, 200, 80))
            } else if self.status == "done" {
                ("done".into(), egui::Color32::from_rgb(80, 200, 80))
            } else {
                ("ready".into(), dim)
            };
            ui.label(egui::RichText::new(text).small().color(color));
        });

        // Trigger rising edge
        if trigger_wired {
            let v = match Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 2) {
                PortValue::Float(f) => f, _ => 0.0
            };
            if v > 0.5 && self.last_trigger <= 0.5 && !eff_spec.is_empty() && validation.is_none() {
                do_send(ctx, &mut self.status);
            }
            self.last_trigger = v;
        }

        // ── Output ports ────────────────────────────────────────
        ui.separator();
        let preview = if self.code.is_empty() { "—".into() } else { format!("{}ch", self.code.len()) };
        crate::nodes::output_port_row(
            ui, "Code", &preview, ctx.node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Text,
        );
        let st = if self.status.is_empty() { "—".to_string() } else { self.status.clone() };
        crate::nodes::output_port_row(
            ui, "Status", &st, ctx.node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Text,
        );

        if !self.code.is_empty() {
            ui.collapsing(format!("Code ({} chars)", self.code.len()), |ui| {
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.code.clone())
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                });
            });
        }
        // NodeId is unused in closure captures above but Rust keeps it via closure move
        let _ = (_id_dummy(ctx.node_id),);
    }
}

fn _id_dummy(_n: NodeId) -> () { () }

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("code_gen", |state| {
        if let Ok(n) = serde_json::from_value::<CodeGenNode>(state.clone()) { Box::new(n) }
        else { Box::new(CodeGenNode::default()) }
    });
}
