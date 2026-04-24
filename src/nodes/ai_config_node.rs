//! AiConfigNode — centralised AI provider / model / key config.
//!
//! One node on the graph holds provider and model selections for the
//! three generator kinds (Text / Image / Code). It also reads keys from
//! `~/.patchwork/settings.json` (shared across projects) and emits
//! `ConfigPayload` JSON blobs on three output ports, each consumed by a
//! matching generator (`TextGen`, `ImageGen`, `CodeGen`).
//!
//! Multiple AiConfig nodes can coexist on one graph — each carries its
//! own label + profile + provider/model choices. This enables A/B
//! testing ("Claude" vs "GPT" side-by-side) and dev/prod key splits
//! (via named profiles).
//!
//! KEYS NEVER SERIALIZE INTO PROJECT FILES. `save_state()` omits them;
//! `load_state()` re-reads from `settings.json` at load time.

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::ai_shared::{
    build_custom_request, list_config_names, load_config, models_for, provider_key_hint,
    provider_label, resolve_key, save_config, ConfigBundle, ConfigPayload,
};
use crate::settings::ProviderKeys;
use eframe::egui;
use serde::{Deserialize, Serialize};

fn default_profile() -> String { "default".into() }
fn default_anthropic_text_model() -> String { "claude-sonnet-4-20250514".into() }
fn default_openai_image_model() -> String { "dall-e-3".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfigNode {
    #[serde(default)] pub label: String,
    #[serde(default = "default_profile")] pub profile: String,

    #[serde(default = "default_anthropic_text_model_provider")] pub text_provider: String,
    #[serde(default = "default_anthropic_text_model")] pub text_model: String,

    #[serde(default = "default_openai_image_provider")] pub image_provider: String,
    #[serde(default = "default_openai_image_model")] pub image_model: String,

    #[serde(default = "default_anthropic_text_model_provider")] pub code_provider: String,
    #[serde(default = "default_anthropic_text_model")] pub code_model: String,

    // Custom endpoint details (used when any of the three providers is "custom").
    #[serde(default)] pub custom_base_url: String,
    #[serde(default)] pub custom_auth_header: String,
    #[serde(default)] pub custom_auth_format: String,

    /// Cached keys for the active profile. NEVER serialized — populated
    /// at load / UI-render time from `~/.patchwork/settings.json`.
    #[serde(skip)] cached_keys: ProviderKeys,
    /// Which providers are currently covered by env-var fallback (UI hint).
    #[serde(skip)] env_fallback: Vec<String>,
    /// Track pending key edits in the UI before Save.
    #[serde(skip)] editing_keys: ProviderKeys,
    /// Dirty flag for the Save button.
    #[serde(skip)] dirty: bool,
    /// Known profiles, refreshed on UI render.
    #[serde(skip)] known_profiles: Vec<String>,
    /// New-profile input field state.
    #[serde(skip)] new_profile_name: String,
    /// Show the "new profile" input.
    #[serde(skip)] adding_profile: bool,
}

fn default_anthropic_text_model_provider() -> String { "anthropic".into() }
fn default_openai_image_provider() -> String { "openai".into() }

impl Default for AiConfigNode {
    fn default() -> Self {
        let mut n = Self {
            label: String::new(),
            profile: "default".into(),
            text_provider: "anthropic".into(),
            text_model: default_anthropic_text_model(),
            image_provider: "openai".into(),
            image_model: default_openai_image_model(),
            code_provider: "anthropic".into(),
            code_model: default_anthropic_text_model(),
            custom_base_url: String::new(),
            custom_auth_header: String::new(),
            custom_auth_format: String::new(),
            cached_keys: ProviderKeys::default(),
            env_fallback: Vec::new(),
            editing_keys: ProviderKeys::default(),
            dirty: false,
            known_profiles: vec!["default".into()],
            new_profile_name: String::new(),
            adding_profile: false,
        };
        n.refresh_from_disk();
        n
    }
}

impl AiConfigNode {
    /// Read the active config's keys from its per-file bundle
    /// (`~/.patchwork/ai_configs/<profile>.json`) and populate
    /// `cached_keys` / `editing_keys` / `env_fallback`.
    fn refresh_from_disk(&mut self) {
        self.known_profiles = list_config_names();
        let bundle = load_config(&self.profile).unwrap_or_default();
        self.editing_keys = bundle.keys.clone();
        self.cached_keys = bundle.keys.clone();
        self.env_fallback.clear();
        for provider in ["anthropic", "openai", "google", "custom"] {
            let (_, from_env) = resolve_key(provider, &self.cached_keys);
            if from_env { self.env_fallback.push(provider.into()); }
        }
        self.dirty = false;
    }

    /// Resolve the effective key for a provider — profile first, else env var.
    fn key_for(&self, provider: &str) -> String {
        let (k, _) = resolve_key(provider, &self.cached_keys);
        k
    }

    fn build_payload(&self, provider: &str, model: &str) -> ConfigPayload {
        let api_key = self.key_for(provider);
        if provider == "custom" {
            ConfigPayload {
                provider: provider.into(),
                model: model.into(),
                api_key,
                base_url: self.custom_base_url.clone(),
                auth_header: self.custom_auth_header.clone(),
                auth_format: self.custom_auth_format.clone(),
            }
        } else {
            ConfigPayload {
                provider: provider.into(),
                model: model.into(),
                api_key,
                base_url: String::new(),
                auth_header: String::new(),
                auth_format: String::new(),
            }
        }
    }

    fn save_keys(&mut self) -> std::io::Result<()> {
        let mut bundle = load_config(&self.profile).unwrap_or_else(ConfigBundle::seeded);
        bundle.keys = self.editing_keys.clone();
        save_config(&self.profile, &bundle)?;
        self.cached_keys = self.editing_keys.clone();
        self.dirty = false;
        // Refresh env-fallback indicators
        self.env_fallback.clear();
        for provider in ["anthropic", "openai", "google", "custom"] {
            let (_, from_env) = resolve_key(provider, &self.cached_keys);
            if from_env { self.env_fallback.push(provider.into()); }
        }
        Ok(())
    }
}

impl NodeBehavior for AiConfigNode {
    fn title(&self) -> &str {
        // egui doesn't give us `&'a str` lifecycle, so we store the
        // formatted title in a thread-local static only when needed.
        // Simplest correct approach: return a static and let the title
        // bar just say "AI Config" — the label is shown inline below.
        "AI Config"
    }

    fn inputs(&self) -> Vec<PortDef> { vec![] }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Text", PortKind::Text),
            PortDef::new("Image", PortKind::Text),
            PortDef::new("Code", PortKind::Text),
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [110, 160, 220] }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let text = self.build_payload(&self.text_provider, &self.text_model).to_port_string();
        let image = self.build_payload(&self.image_provider, &self.image_model).to_port_string();
        let code = self.build_payload(&self.code_provider, &self.code_model).to_port_string();
        vec![
            (0, PortValue::Text(text)),
            (1, PortValue::Text(image)),
            (2, PortValue::Text(code)),
        ]
    }

    fn type_tag(&self) -> &str { "ai_config" }

    fn save_state(&self) -> serde_json::Value {
        // Explicitly serialise only the project-safe fields. Never
        // touches cached_keys / editing_keys (which hold real secrets).
        #[derive(Serialize)]
        struct SavedState<'a> {
            label: &'a str,
            profile: &'a str,
            text_provider: &'a str,
            text_model: &'a str,
            image_provider: &'a str,
            image_model: &'a str,
            code_provider: &'a str,
            code_model: &'a str,
            custom_base_url: &'a str,
            custom_auth_header: &'a str,
            custom_auth_format: &'a str,
        }
        serde_json::to_value(SavedState {
            label: &self.label,
            profile: &self.profile,
            text_provider: &self.text_provider,
            text_model: &self.text_model,
            image_provider: &self.image_provider,
            image_model: &self.image_model,
            code_provider: &self.code_provider,
            code_model: &self.code_model,
            custom_base_url: &self.custom_base_url,
            custom_auth_header: &self.custom_auth_header,
            custom_auth_format: &self.custom_auth_format,
        })
        .unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<AiConfigNode>(state.clone()) {
            self.label = l.label;
            self.profile = l.profile;
            self.text_provider = l.text_provider;
            self.text_model = l.text_model;
            self.image_provider = l.image_provider;
            self.image_model = l.image_model;
            self.code_provider = l.code_provider;
            self.code_model = l.code_model;
            self.custom_base_url = l.custom_base_url;
            self.custom_auth_header = l.custom_auth_header;
            self.custom_auth_format = l.custom_auth_format;
        }
        self.refresh_from_disk();
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = egui::Color32::from_rgb(140, 140, 155);

        // ── Label + profile row ──────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Label").small().color(dim));
            ui.add(
                egui::TextEdit::singleline(&mut self.label)
                    .desired_width(140.0)
                    .hint_text("Name this config…"),
            );
        });

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Profile").small().color(dim));
            let prev_profile = self.profile.clone();
            egui::ComboBox::from_id_salt(format!("aicfg_prof_{}", ctx.node_id))
                .selected_text(&self.profile)
                .width(120.0)
                .show_ui(ui, |ui| {
                    for p in &self.known_profiles {
                        ui.selectable_value(&mut self.profile, p.clone(), p.clone());
                    }
                    if ui.selectable_label(false, "+ New profile…").clicked() {
                        self.adding_profile = true;
                    }
                });
            if self.profile != prev_profile {
                self.refresh_from_disk();
            }
        });

        if self.adding_profile {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Name").small());
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_profile_name)
                        .desired_width(120.0)
                        .hint_text("e.g. work, dev, prod"),
                );
                if ui.small_button("Add").clicked()
                    && !self.new_profile_name.trim().is_empty()
                {
                    let name = self.new_profile_name.trim().to_string();
                    if load_config(&name).is_none() {
                        let _ = save_config(&name, &ConfigBundle::seeded());
                    }
                    self.profile = name;
                    self.new_profile_name.clear();
                    self.adding_profile = false;
                    self.refresh_from_disk();
                }
                if ui.small_button("Cancel").clicked() {
                    self.adding_profile = false;
                    self.new_profile_name.clear();
                }
            });
        }

        ui.separator();

        // ── Per-kind sections ────────────────────────────────────
        section(ui, ctx.node_id, "Text", &mut self.text_provider, &mut self.text_model, 0);
        section(ui, ctx.node_id, "Image", &mut self.image_provider, &mut self.image_model, 1);
        section(ui, ctx.node_id, "Code", &mut self.code_provider, &mut self.code_model, 0);

        // ── Custom endpoint (only if any provider is "custom") ───
        let any_custom = self.text_provider == "custom"
            || self.image_provider == "custom"
            || self.code_provider == "custom";
        if any_custom {
            ui.separator();
            ui.label(egui::RichText::new("Custom Endpoint").small().strong());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Base URL").small());
                ui.add(
                    egui::TextEdit::singleline(&mut self.custom_base_url)
                        .desired_width(180.0)
                        .hint_text("http://localhost:11434/v1"),
                );
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Auth header").small());
                ui.add(
                    egui::TextEdit::singleline(&mut self.custom_auth_header)
                        .desired_width(140.0)
                        .hint_text("Authorization"),
                );
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Auth format").small());
                ui.add(
                    egui::TextEdit::singleline(&mut self.custom_auth_format)
                        .desired_width(140.0)
                        .hint_text("Bearer {key}"),
                );
            });
        }

        ui.separator();

        // ── API keys (per-provider, masked, in editing buffer) ───
        ui.label(egui::RichText::new(format!("API keys — profile “{}”", self.profile))
            .small().strong());
        for provider in ["anthropic", "openai", "google", "custom"] {
            let hint = provider_key_hint(provider);
            let slot = match provider {
                "anthropic" => &mut self.editing_keys.anthropic,
                "openai" => &mut self.editing_keys.openai,
                "google" => &mut self.editing_keys.google,
                "custom" => &mut self.editing_keys.custom,
                _ => unreachable!(),
            };
            let prev = slot.clone();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{:>9}", provider_label(provider)))
                        .small(),
                );
                let resp = ui.add(
                    egui::TextEdit::singleline(slot)
                        .password(true)
                        .desired_width(160.0)
                        .hint_text(hint),
                );
                if resp.changed() && *slot != prev {
                    self.dirty = true;
                }
                if self.env_fallback.iter().any(|p| p == provider) {
                    ui.label(
                        egui::RichText::new("(env)")
                            .small()
                            .color(egui::Color32::from_rgb(200, 180, 80)),
                    )
                    .on_hover_text("Falling back to env var");
                }
            });
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let can_save = self.dirty;
            if ui
                .add_enabled(can_save, egui::Button::new("Save keys"))
                .clicked()
            {
                let _ = self.save_keys();
            }
            if self.dirty {
                ui.label(
                    egui::RichText::new("unsaved")
                        .small()
                        .color(egui::Color32::from_rgb(200, 180, 80)),
                );
            }
        });

        ui.separator();

        // ── Output ports ─────────────────────────────────────────
        crate::nodes::output_port_row(
            ui, "Text Config", "—", ctx.node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Text,
        );
        crate::nodes::output_port_row(
            ui, "Image Config", "—", ctx.node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Text,
        );
        crate::nodes::output_port_row(
            ui, "Code Config", "—", ctx.node_id, 2,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Text,
        );
    }
}

/// Render one kind's (provider, model) row pair. `mode` is 0=text, 1=image
/// — used to pick which subset of models to list.
fn section(
    ui: &mut egui::Ui,
    node_id: crate::graph::NodeId,
    kind: &str,
    provider: &mut String,
    model: &mut String,
    mode: u8,
) {
    ui.label(egui::RichText::new(kind).small().strong());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Provider").small());
        let prev = provider.clone();
        egui::ComboBox::from_id_salt(format!("aicfg_prov_{}_{}", node_id, kind))
            .selected_text(provider_label(provider))
            .width(110.0)
            .show_ui(ui, |ui| {
                // Anthropic has no image API; hide for image mode.
                if mode == 0 {
                    ui.selectable_value(provider, "anthropic".into(), "Anthropic");
                }
                ui.selectable_value(provider, "openai".into(), "OpenAI");
                ui.selectable_value(provider, "google".into(), "Google");
                ui.selectable_value(provider, "custom".into(), "Custom");
            });
        if *provider != prev {
            // Reset model to the first valid one for the new provider/mode.
            if *provider == "custom" {
                *model = String::new();
            } else if let Some((id, _)) = models_for(provider, mode).first() {
                *model = id.to_string();
            }
        }
    });

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Model").small());
        if *provider == "custom" {
            ui.add(
                egui::TextEdit::singleline(model)
                    .desired_width(160.0)
                    .hint_text("e.g. llama3.2 / qwen2.5-coder"),
            );
        } else {
            let models = models_for(provider, mode);
            let display = models
                .iter()
                .find(|(id, _)| *id == model.as_str())
                .map(|(_, name)| *name)
                .unwrap_or(model.as_str());
            egui::ComboBox::from_id_salt(format!("aicfg_model_{}_{}", node_id, kind))
                .selected_text(display)
                .width(160.0)
                .show_ui(ui, |ui| {
                    for (id, name) in &models {
                        ui.selectable_value(model, id.to_string(), *name);
                    }
                });
        }
    });
}

// Silence the unused-import warning when nothing in this file needs `build_custom_request`.
// Generators will use it; the node itself only builds payloads.
#[allow(dead_code)]
fn _referenced_to_avoid_unused() {
    let _ = build_custom_request;
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("ai_config", |state| {
        let mut n = AiConfigNode::default();
        n.load_state(state);
        Box::new(n)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_state_never_contains_keys() {
        let mut n = AiConfigNode::default();
        n.editing_keys.anthropic = "sk-ant-VERY-SECRET".into();
        n.editing_keys.openai = "sk-openai-VERY-SECRET".into();
        n.cached_keys = n.editing_keys.clone();
        let s = n.save_state().to_string();
        assert!(!s.contains("sk-ant-VERY-SECRET"),
            "anthropic key leaked into serialized state: {}", s);
        assert!(!s.contains("sk-openai-VERY-SECRET"),
            "openai key leaked into serialized state: {}", s);
        assert!(!s.contains("\"editing_keys\""));
        assert!(!s.contains("\"cached_keys\""));
    }
}
