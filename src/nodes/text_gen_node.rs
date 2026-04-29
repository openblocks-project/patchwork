//! TextGenNode — trait-based text-generation node.
//!
//! Reads provider/model/key from a `Config` input port (JSON-encoded
//! `ConfigPayload` emitted by an `AiConfigNode`). Optional vision input
//! via the `Image` port when the selected model supports it.
//!
//! Output: `Response` (Text) and `Status` (Text). Send fires on:
//! (a) Send button click, (b) rising edge on the Send trigger port.

use crate::graph::{Connection, Graph, NodeId, PortDef, PortKind, PortValue};
use crate::http::HttpAction;
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::ai_shared::{
    build_request_for_config, encode_image_to_jpeg_b64, extract_ai_response, parse_config_port,
    provider_label, ConfigPayload, TEXT_PRESETS,
};
use eframe::egui;
use serde::{Deserialize, Serialize};

fn default_temperature() -> f32 { 0.7 }
fn default_max_tokens() -> u32 { 1024 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextGenNode {
    #[serde(default)] pub system_prompt: String,
    #[serde(default)] pub user_prompt: String,
    #[serde(default = "default_temperature")] pub temperature: f32,
    #[serde(default = "default_max_tokens")] pub max_tokens: u32,
    /// Optional per-node override — takes precedence over the config's model.
    #[serde(default)] pub model_override: String,
    #[serde(default)] pub provider_override: String,

    #[serde(skip)] pub response: String,
    #[serde(skip)] pub status: String,
    #[serde(skip)] pub last_trigger: f32,
}

impl Default for TextGenNode {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            user_prompt: String::new(),
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
            model_override: String::new(),
            provider_override: String::new(),
            response: String::new(),
            status: String::new(),
            last_trigger: 0.0,
        }
    }
}

impl NodeBehavior for TextGenNode {
    fn title(&self) -> &str { "Text Gen" }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Config", PortKind::Text),
            PortDef::new("System", PortKind::Text),
            PortDef::new("Prompt", PortKind::Text),
            PortDef::new("Send", PortKind::Trigger),
            PortDef::new("Image", PortKind::Image),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Response", PortKind::Text),
            PortDef::new("Status", PortKind::Text),
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [110, 200, 160] }

    /// Renders ports inline via `inline_port_circle` and `output_port_row` —
    /// opt out of the framework's default top/bottom port rendering.
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![
            (0, PortValue::Text(self.response.clone())),
            (1, PortValue::Text(self.status.clone())),
        ]
    }

    fn type_tag(&self) -> &str { "text_gen" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<TextGenNode>(state.clone()) { *self = l; }
    }

    fn apply_http_response(&mut self, body: String, status: u16) {
        if (200..300).contains(&status) {
            let provider = if self.provider_override.is_empty() {
                // Content-based heuristic when we don't know the provider.
                if body.contains("\"candidates\"") { "google" }
                else if body.contains("\"content\":[{\"type\"") { "anthropic" }
                else { "openai" }
            } else {
                self.provider_override.as_str()
            };
            self.response = extract_ai_response(provider, &body);
            self.status = "done".into();
        } else {
            self.response = body;
            self.status = format!("error: {}", status);
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        render_generator(
            ui, ctx, GenKind::Text,
            &mut self.system_prompt, &mut self.user_prompt,
            &mut self.temperature, &mut self.max_tokens,
            &mut self.model_override, &mut self.provider_override,
            &self.response, &mut self.status,
            &mut self.last_trigger,
        );
    }
}

// ── Shared rendering between TextGen / CodeGen (same shape with minor tweaks) ──

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GenKind { Text, Image, Code }

impl GenKind {
    pub(crate) fn config_output_port(self) -> usize {
        // Matches AiConfigNode output ports: 0=Text, 1=Image, 2=Code.
        match self { GenKind::Text => 0, GenKind::Image => 1, GenKind::Code => 2 }
    }

    pub(crate) fn label(self) -> &'static str {
        match self { GenKind::Text => "Text", GenKind::Image => "Image", GenKind::Code => "Code" }
    }
}

/// Read the `Config` input port and parse. Returns `None` if unwired or
/// malformed. Caller must handle the "no config" UI state.
pub(crate) fn read_config(
    connections: &[Connection],
    values: &std::collections::HashMap<(NodeId, usize), PortValue>,
    node_id: NodeId,
    config_port: usize,
) -> Option<ConfigPayload> {
    let is_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == config_port);
    if !is_wired { return None; }
    match Graph::static_input_value(connections, values, node_id, config_port) {
        PortValue::Text(s) => parse_config_port(&s),
        _ => None,
    }
}

/// Apply per-node overrides on top of a config payload.
pub(crate) fn apply_overrides(cfg: &mut ConfigPayload, provider_ov: &str, model_ov: &str) {
    if !provider_ov.is_empty() { cfg.provider = provider_ov.into(); }
    if !model_ov.is_empty() { cfg.model = model_ov.into(); }
}

/// Compute a user-facing status string when the config isn't usable.
/// Returns `None` if everything checks out and the Send button should
/// be enabled.
pub(crate) fn validation_error(cfg: &Option<ConfigPayload>) -> Option<String> {
    match cfg {
        None => Some("Wire to an AI Config node".into()),
        Some(c) if c.model.is_empty() => Some(format!("Pick a model in AI Config")),
        Some(c) if c.api_key.is_empty() => {
            let label = provider_label(&c.provider);
            Some(format!("Add your {} key", label))
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_generator(
    ui: &mut egui::Ui,
    ctx: &mut RenderContext,
    kind: GenKind,
    system_prompt: &mut String,
    user_prompt: &mut String,
    temperature: &mut f32,
    max_tokens: &mut u32,
    model_override: &mut String,
    provider_override: &mut String,
    response: &str,
    status: &mut String,
    last_trigger: &mut f32,
) {
    let dim = egui::Color32::from_rgb(140, 140, 155);
    let accent = ui.visuals().hyperlink_color;

    // Ports: 0=Config, 1=System, 2=Prompt, 3=Send, 4=Image
    let config_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);
    let system_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 1);
    let prompt_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 2);
    let trigger_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 3);
    let image_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 4);

    // ── Input ports row ─────────────────────────────────────────
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
            ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
        ui.label(egui::RichText::new("Config").small()
            .color(if config_wired { accent } else { dim }));
        ui.add_space(6.0);
        // Gear button opens / spawns AiConfig (uses egui memory to signal app loop).
        if ui.small_button("⚙").on_hover_text("Open AI Config").clicked() {
            ui.ctx().data_mut(|d| {
                d.insert_temp(
                    egui::Id::new("ai_gear_click"),
                    (ctx.node_id, kind.config_output_port()),
                );
            });
        }
    });

    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections,
            ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
        ui.label(egui::RichText::new("System").small());
        ui.add_space(8.0);
        crate::nodes::inline_port_circle(ui, ctx.node_id, 2, true, ctx.connections,
            ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
        ui.label(egui::RichText::new("Prompt").small());
        ui.add_space(8.0);
        crate::nodes::inline_port_circle(ui, ctx.node_id, 3, true, ctx.connections,
            ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
        ui.label(egui::RichText::new("Send").small());
    });
    if kind == GenKind::Text {
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 4, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            ui.label(egui::RichText::new("Image").small());
        });
    }

    ui.separator();

    // ── Style presets (only for Text; CodeGen has a Language dropdown) ──
    if kind == GenKind::Text && !system_wired {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for preset in TEXT_PRESETS {
                if preset.label == "WGSL shader" { continue; } // moved to CodeGen
                if ui
                    .add(egui::Button::new(egui::RichText::new(preset.label).small()))
                    .on_hover_text(preset.system)
                    .clicked()
                {
                    *system_prompt = preset.system.to_string();
                }
            }
        });
    }

    // ── System & user prompts (shown when unwired) ──────────────
    if !system_wired {
        egui::ScrollArea::vertical()
            .max_height(60.0)
            .id_salt(format!("gen_sys_scroll_{}_{:?}", ctx.node_id, kind as u8))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(system_prompt)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .hint_text("System prompt…")
                        .font(egui::TextStyle::Small),
                );
            });
    }
    if !prompt_wired {
        ui.add(
            egui::TextEdit::multiline(user_prompt)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Ask something…")
                .font(egui::TextStyle::Small),
        );
    }

    // ── Parameters ──────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Temp").small());
        ui.add(egui::Slider::new(temperature, 0.0..=2.0).step_by(0.1).show_value(true));
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Max tokens").small());
        ui.add(egui::DragValue::new(max_tokens).range(1..=32_000).speed(16));
    });

    // ── Per-node override (advanced) ────────────────────────────
    ui.collapsing("Override provider/model", |ui| {
        ui.label(egui::RichText::new(
            "Leave blank to use the config's defaults."
        ).small().color(dim));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Provider").small());
            ui.add(
                egui::TextEdit::singleline(provider_override)
                    .desired_width(100.0)
                    .hint_text("anthropic / openai / google / custom"),
            );
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Model").small());
            ui.add(
                egui::TextEdit::singleline(model_override)
                    .desired_width(160.0)
                    .hint_text("e.g. gpt-4o"),
            );
        });
    });

    ui.separator();

    // ── Resolve effective config + prompts ──────────────────────
    let mut cfg_opt = read_config(ctx.connections, ctx.values, ctx.node_id, 0);
    if let Some(cfg) = cfg_opt.as_mut() {
        apply_overrides(cfg, provider_override, model_override);
    }

    let eff_system = if system_wired {
        match Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 1) {
            PortValue::Text(s) => s, _ => system_prompt.clone()
        }
    } else { system_prompt.clone() };
    let eff_prompt = if prompt_wired {
        match Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 2) {
            PortValue::Text(s) => s, _ => user_prompt.clone()
        }
    } else { user_prompt.clone() };

    let eff_image = if image_wired && kind == GenKind::Text {
        match Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 4) {
            PortValue::Image(img) => Some(img), _ => None
        }
    } else { None };

    // ── Status + Send button ────────────────────────────────────
    let dispatched = std::cell::Cell::new(false);
    let validation = validation_error(&cfg_opt);

    // For Image mode, clear any stale text response handling — not used here.
    let _ = image_wired;

    // Close around captures for a single dispatch chokepoint.
    let do_send = |ctx: &mut RenderContext, status: &mut String| {
        if ctx.http_pending || dispatched.get() { return; }
        let Some(ref cfg) = cfg_opt else { return; };
        if cfg.model.is_empty() || cfg.api_key.is_empty() { return; }

        let image_b64 = eff_image.as_ref().and_then(|img| encode_image_to_jpeg_b64(img));
        let (url, headers, body) = build_request_for_config(
            cfg,
            &eff_system,
            &eff_prompt,
            *max_tokens,
            *temperature,
            if kind == GenKind::Image { 1 } else { 0 },
            image_b64.as_deref(),
        );
        ctx.http_actions.push(HttpAction::SendRequest {
            node_id: ctx.node_id,
            url,
            method: "POST".into(),
            headers,
            body,
        });
        *status = "thinking…".into();
        dispatched.set(true);
    };

    ui.horizontal(|ui| {
        let in_flight = ctx.http_pending || dispatched.get();
        let can_send = !eff_prompt.is_empty()
            && validation.is_none()
            && !in_flight;
        let btn_text = if in_flight { "⏳ Thinking…" } else { "▶ Send" };
        if ui.add_enabled(can_send, egui::Button::new(btn_text)).clicked() {
            do_send(ctx, status);
        }

        // Status indicator
        let (text, color) = if let Some(v) = validation.as_ref() {
            (v.clone(), egui::Color32::from_rgb(200, 160, 80))
        } else if eff_prompt.is_empty() {
            ("(type a prompt)".into(), dim)
        } else if status.starts_with("error") {
            (status.clone(), egui::Color32::from_rgb(220, 80, 80))
        } else if status == "thinking…" || in_flight {
            ("thinking…".into(), egui::Color32::from_rgb(200, 200, 80))
        } else if status == "done" {
            ("done".into(), egui::Color32::from_rgb(80, 200, 80))
        } else {
            ("ready".into(), dim)
        };
        ui.label(egui::RichText::new(text).small().color(color));
    });

    // ── Trigger port rising edge ────────────────────────────────
    if trigger_wired {
        let v = match Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 3) {
            PortValue::Float(f) => f, _ => 0.0
        };
        if v > 0.5 && *last_trigger <= 0.5 && !eff_prompt.is_empty() && validation.is_none() {
            do_send(ctx, status);
        }
        *last_trigger = v;
    }

    // ── Output ports ────────────────────────────────────────────
    ui.separator();
    let preview = if response.is_empty() { "—".into() } else { format!("{}ch", response.len()) };
    crate::nodes::output_port_row(
        ui, "Response", &preview, ctx.node_id, 0,
        ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
        PortKind::Text,
    );
    let st = if status.is_empty() { "—".to_string() } else { status.clone() };
    crate::nodes::output_port_row(
        ui, "Status", &st, ctx.node_id, 1,
        ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
        PortKind::Text,
    );

    // ── Response preview ────────────────────────────────────────
    if !response.is_empty() {
        ui.collapsing(format!("Response ({} chars)", response.len()), |ui| {
            egui::ScrollArea::vertical().max_height(150.0).show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut response.to_string())
                        .desired_width(f32::INFINITY)
                        .interactive(false),
                );
            });
        });
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("text_gen", |state| {
        if let Ok(n) = serde_json::from_value::<TextGenNode>(state.clone()) { Box::new(n) }
        else { Box::new(TextGenNode::default()) }
    });
}
