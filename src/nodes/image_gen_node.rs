//! ImageGenNode — trait-based image-generation node.
//!
//! Consumes a `Config` input (from AiConfigNode's `Image` output) plus a
//! `Prompt` input. Runs the generation POST, then asynchronously fetches
//! the returned URL (or reads the inline file:// path for Imagen) and
//! emits decoded pixels as `PortValue::Image` on its primary output.
//!
//! Outputs:
//!   0  Image  (Image)  — decoded bytes, ready for Display / Blend / Crop
//!   1  URL    (Text)   — raw response URL for chaining / saving
//!   2  Status (Text)

use crate::graph::{Graph, ImageData, PortDef, PortKind, PortValue};
use crate::http::HttpAction;
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::ai_shared::{
    build_request_for_config, extract_ai_response, IMAGE_PRESETS,
};
use crate::nodes::image_node::maybe_load_async;
use crate::nodes::text_gen_node::{apply_overrides, read_config, validation_error, GenKind};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn default_max_tokens() -> u32 { 1024 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenNode {
    #[serde(default)] pub prompt: String,
    #[serde(default = "default_max_tokens")] pub max_tokens: u32,
    #[serde(default)] pub model_override: String,
    #[serde(default)] pub provider_override: String,

    #[serde(skip)] pub response_url: String,
    #[serde(skip)] pub status: String,
    #[serde(skip)] pub last_trigger: f32,
    /// Decoded image bytes, populated asynchronously after the URL fetch
    /// completes. Emitted on output port 0 as `PortValue::Image`.
    #[serde(skip)] pub image: Option<Arc<ImageData>>,
}

impl Default for ImageGenNode {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            max_tokens: default_max_tokens(),
            model_override: String::new(),
            provider_override: String::new(),
            response_url: String::new(),
            status: String::new(),
            last_trigger: 0.0,
            image: None,
        }
    }
}

impl NodeBehavior for ImageGenNode {
    fn title(&self) -> &str { "Image Gen" }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Config", PortKind::Text),
            PortDef::new("Prompt", PortKind::Text),
            PortDef::new("Send", PortKind::Trigger),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image", PortKind::Image),
            PortDef::new("URL", PortKind::Text),
            PortDef::new("Status", PortKind::Text),
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [220, 160, 110] }

    /// Renders ports inline via `inline_port_circle` and `output_port_row` —
    /// opt out of the framework's default top/bottom port rendering.
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let img_val = match &self.image {
            Some(img) => PortValue::Image(img.clone()),
            None => PortValue::None,
        };
        vec![
            (0, img_val),
            (1, PortValue::Text(self.response_url.clone())),
            (2, PortValue::Text(self.status.clone())),
        ]
    }

    fn type_tag(&self) -> &str { "image_gen" }

    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<ImageGenNode>(state.clone()) { *self = l; }
    }

    fn apply_http_response(&mut self, body: String, status: u16) {
        if (200..300).contains(&status) {
            let provider = if body.contains("\"candidates\"") { "google" } else { "openai" };
            let new_url = extract_ai_response(provider, &body);
            // A new URL means we need to re-fetch the image — drop stale
            // pixels so downstream consumers see `None` until the fetch
            // completes, rather than the previous generation's image.
            if new_url != self.response_url {
                self.image = None;
            }
            self.response_url = new_url;
            self.status = "fetching image…".into();
        } else {
            self.response_url = body;
            self.status = format!("error: {}", status);
            self.image = None;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = egui::Color32::from_rgb(140, 140, 155);
        let accent = ui.visuals().hyperlink_color;

        let config_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let prompt_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 1);
        let trigger_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 2);

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
                        (ctx.node_id, GenKind::Image.config_output_port()),
                    );
                });
            }
        });
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("Prompt").small());
            ui.add_space(8.0);
            crate::nodes::inline_port_circle(ui, ctx.node_id, 2, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Trigger);
            ui.label(egui::RichText::new("Send").small());
        });

        ui.separator();

        // Presets
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for preset in IMAGE_PRESETS {
                if ui
                    .add(egui::Button::new(egui::RichText::new(preset.label).small()))
                    .on_hover_text(preset.system)
                    .clicked()
                {
                    // Append style to prompt (image gens typically fold style into the prompt)
                    if !self.prompt.is_empty() && !self.prompt.ends_with(' ') {
                        self.prompt.push(' ');
                    }
                    self.prompt.push_str(preset.system);
                }
            }
        });

        // Prompt
        if !prompt_wired {
            ui.add(
                egui::TextEdit::multiline(&mut self.prompt)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("Describe the image…")
                    .font(egui::TextStyle::Small),
            );
        }

        // Overrides
        ui.collapsing("Override provider/model", |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Provider").small());
                ui.add(egui::TextEdit::singleline(&mut self.provider_override)
                    .desired_width(100.0).hint_text("openai/google/custom"));
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Model").small());
                ui.add(egui::TextEdit::singleline(&mut self.model_override)
                    .desired_width(160.0).hint_text("e.g. dall-e-3"));
            });
        });

        ui.separator();

        // Resolve config + prompt
        let mut cfg_opt = read_config(ctx.connections, ctx.values, ctx.node_id, 0);
        if let Some(cfg) = cfg_opt.as_mut() {
            apply_overrides(cfg, &self.provider_override, &self.model_override);
        }
        let eff_prompt = if prompt_wired {
            match Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 1) {
                PortValue::Text(s) => s, _ => self.prompt.clone()
            }
        } else { self.prompt.clone() };

        let validation = validation_error(&cfg_opt);
        let dispatched = std::cell::Cell::new(false);

        let do_send = |ctx: &mut RenderContext, status: &mut String| {
            if ctx.http_pending || dispatched.get() { return; }
            let Some(ref cfg) = cfg_opt else { return; };
            if cfg.model.is_empty() || cfg.api_key.is_empty() { return; }
            let (url, headers, body) = build_request_for_config(
                cfg, "", &eff_prompt, self.max_tokens, 1.0, 1, None,
            );
            ctx.http_actions.push(HttpAction::SendRequest {
                node_id: ctx.node_id, url, method: "POST".into(), headers, body,
            });
            *status = "thinking…".into();
            dispatched.set(true);
        };

        ui.horizontal(|ui| {
            let in_flight = ctx.http_pending || dispatched.get();
            let can_send = !eff_prompt.is_empty() && validation.is_none() && !in_flight;
            let btn_text = if in_flight { "⏳ Painting…" } else { "▶ Generate" };
            if ui.add_enabled(can_send, egui::Button::new(btn_text)).clicked() {
                do_send(ctx, &mut self.status);
            }
            let (text, color) = if let Some(v) = validation.as_ref() {
                (v.clone(), egui::Color32::from_rgb(200, 160, 80))
            } else if eff_prompt.is_empty() {
                ("(describe the image)".into(), dim)
            } else if self.status.starts_with("error") {
                (self.status.clone(), egui::Color32::from_rgb(220, 80, 80))
            } else if in_flight || self.status == "thinking…" {
                ("painting…".into(), egui::Color32::from_rgb(200, 200, 80))
            } else if self.status == "done" {
                ("done".into(), egui::Color32::from_rgb(80, 200, 80))
            } else {
                ("ready".into(), dim)
            };
            ui.label(egui::RichText::new(text).small().color(color));
        });

        if trigger_wired {
            let v = match Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 2) {
                PortValue::Float(f) => f, _ => 0.0
            };
            if v > 0.5 && self.last_trigger <= 0.5 && !eff_prompt.is_empty() && validation.is_none() {
                do_send(ctx, &mut self.status);
            }
            self.last_trigger = v;
        }

        // ── Async URL fetch ─────────────────────────────────────
        // Once we have a URL (from apply_http_response), drive the
        // per-node fetch state machine from image_node. First call
        // kicks off a background fetch; follow-up calls with the same
        // URL drain the result when ready. Matches ImageNode's Src
        // pipeline so Display/Blend/etc. can consume the pixels
        // without an intermediate ImageNode.
        if !self.response_url.is_empty() && self.image.is_none() {
            if let Some((maybe_img, _cache)) =
                maybe_load_async(ui.ctx(), ctx.node_id, &self.response_url)
            {
                if let Some(img) = maybe_img {
                    self.image = Some(img);
                    self.status = "done".into();
                }
            }
        }

        // Output ports
        ui.separator();
        let img_preview = match &self.image {
            Some(img) => format!("[{}×{}]", img.width, img.height),
            None => "—".into(),
        };
        crate::nodes::output_port_row(
            ui, "Image", &img_preview, ctx.node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Image,
        );
        let url_preview = if self.response_url.is_empty() { "—".into() }
            else { format!("{}…", self.response_url.chars().take(32).collect::<String>()) };
        crate::nodes::output_port_row(
            ui, "URL", &url_preview, ctx.node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Text,
        );
        let st = if self.status.is_empty() { "—".to_string() } else { self.status.clone() };
        crate::nodes::output_port_row(
            ui, "Status", &st, ctx.node_id, 2,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Text,
        );

        if !self.response_url.is_empty() {
            ui.collapsing("URL", |ui| {
                ui.hyperlink(&self.response_url);
            });
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("image_gen", |state| {
        if let Ok(n) = serde_json::from_value::<ImageGenNode>(state.clone()) { Box::new(n) }
        else { Box::new(ImageGenNode::default()) }
    });
}
