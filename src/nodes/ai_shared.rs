//! Shared helpers for the AI generator nodes (AiConfig, TextGen,
//! ImageGen, CodeGen). Re-exports the provider-agnostic plumbing from
//! `ai_request` plus adds:
//!
//! - [`resolve_key`]: env-var fallback when the active config has no key saved.
//! - [`ConfigBundle`] / [`ConfigPayload`] — wire format between AiConfig and
//!   generator nodes.
//! - [`build_custom_request`]: OpenAI-compatible request against an
//!   arbitrary base URL (Ollama, LM Studio, OpenRouter, vLLM, …).
//! - [`language_system_prompt`]: CodeGen's per-language system prompt.
//!
//! # Storage
//!
//! Each named config lives as an individual file at
//! `~/.patchwork/ai_configs/<name>.json` (mode 0600 on Unix). `settings.json`
//! only remembers which name is currently active. Functions in this module
//! (`list_config_names`, `load_config`, `save_config`, `delete_config`,
//! `set_active_config`) hide the filesystem layout from node code.

use crate::settings::{CustomEndpoint, KindDefault, ProviderKeys, Settings};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Re-exports from ai_request ───────────────────────────────────────────────

pub use crate::nodes::ai_request::{
    base64_decode, base64_encode, build_request, encode_image_to_jpeg_b64,
    extract_ai_response, models_for, provider_label, strip_code_fences,
    try_save_imagen_image, StylePreset, IMAGE_PRESETS, TEXT_FORMATS, TEXT_PRESETS,
};

// ── CodeGen languages + system prompts ───────────────────────────────────────

pub const CODE_LANGUAGES: &[&str] = &["Rust", "WGSL", "HTML", "JSON"];

/// System prompt to pass the model for the given language label.
///
/// The `"WGSL"` preset is identical to the WGSL chip previously offered on
/// the monolithic AiRequest node — it's how you get shaders that drop
/// cleanly into PatchWork's WGSL Viewer.
pub fn language_system_prompt(language: &str) -> &'static str {
    match language {
        "Rust" => "You are a senior Rust engineer. Reply with idiomatic, well-typed Rust. \
            Output ONLY the code — no markdown fences, no prose, no explanations.",
        "WGSL" => "You are a WGSL shader expert writing fragment shaders that drop directly \
            into PatchWork's WGSL Viewer node.\n\
            \n\
            STRICT OUTPUT FORMAT:\n\
            - Output ONLY the shader source. No markdown fences, no prose, no comments outside the code.\n\
            - Provide a `@fragment fn fs_main(in: VertexOutput) -> @location(0) vec4<f32>` function. DO NOT write a vertex shader — the viewer provides one.\n\
            - Define `struct VertexOutput { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32>, };` at the top so the fragment input type resolves.\n\
            \n\
            UNIFORMS (auto-injected by the viewer — DO NOT redeclare):\n\
            - The viewer prepends a `struct Uniforms` and `@group(0) @binding(0) var<uniform> u: Uniforms;` above your code. Reference uniforms as `u.<name>`.\n\
            - Always available: `u.time: f32`, `u.resolution: vec2<f32>`, `u.mouse: vec2<f32>`.\n\
            - To expose a NEW knob, reference it: e.g. `u.speed`, `u.intensity`. The viewer scans your shader for `u.xxx` and auto-creates an input port + slider for each. Use snake_case names.\n\
            - To expose a COLOR knob, reference all three of `u.<name>_r`, `u.<name>_g`, `u.<name>_b`.\n\
            \n\
            CONVENTIONS:\n\
            - Use `in.uv` (already in [0,1], y-flipped) as your normalized coordinate.\n\
            - Animate with `u.time`. Return `vec4<f32>(rgb, 1.0)`.\n\
            - Prefer `mix`, `smoothstep`, `fract`, `sin`, `length` over heavy loops.",
        "HTML" => "You are a senior web engineer. Reply with a single self-contained HTML \
            document (inline CSS and JS only; no external assets). Output ONLY the HTML — \
            no markdown fences, no explanations.",
        "JSON" => "Reply with a single valid JSON object. No prose, no code fences, no commentary.",
        _ => "Output only source code. No markdown fences, no prose.",
    }
}

// ── Whole-config bundle (stored one-per-file) ────────────────────────────────

/// The full saved content of one named AI config (`default`, `work`, …).
/// One file per bundle lives at `~/.patchwork/ai_configs/<name>.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigBundle {
    #[serde(default)] pub text: KindDefault,
    #[serde(default)] pub image: KindDefault,
    #[serde(default)] pub code: KindDefault,
    #[serde(default)] pub keys: ProviderKeys,
    #[serde(default)] pub custom: CustomEndpoint,
}

impl ConfigBundle {
    /// Seeded defaults for a fresh "default" config — sensible provider
    /// + model choices so the node works out of the box once a key is
    /// pasted in.
    pub fn seeded() -> Self {
        Self {
            text: KindDefault { provider: "anthropic".into(), model: "claude-sonnet-4-20250514".into() },
            image: KindDefault { provider: "openai".into(), model: "dall-e-3".into() },
            code: KindDefault { provider: "anthropic".into(), model: "claude-sonnet-4-20250514".into() },
            keys: ProviderKeys::default(),
            custom: CustomEndpoint::default(),
        }
    }
}

// ── Config payload (wire format emitted on the AiConfig output port) ─────────

/// One-kind slice of a bundle, with API key already resolved via env
/// fallback. What a generator actually needs to build an HTTP request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigPayload {
    pub provider: String,
    pub model: String,
    #[serde(default)] pub api_key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_header: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth_format: String,
}

impl ConfigPayload {
    pub fn to_port_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// Parse a flat `ConfigPayload` from a generator's `Config` input port.
/// Returns `None` on empty input, non-JSON, or missing provider.
pub fn parse_config_port(raw: &str) -> Option<ConfigPayload> {
    let trimmed = raw.trim();
    if trimmed.is_empty() { return None; }
    let p: ConfigPayload = serde_json::from_str(trimmed).ok()?;
    if p.provider.is_empty() { return None; }
    Some(p)
}

/// Bundle shape emitted by `AiConfigNode::evaluate()` on its `Config`
/// output port. Carried as `PortValue::Text(json)`. Each generator picks
/// its own kind via `parse_bundle_then_pick`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigBundlePayload {
    pub text: ConfigPayload,
    pub image: ConfigPayload,
    pub code: ConfigPayload,
}

impl ConfigBundlePayload {
    pub fn to_port_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Build a wire-format bundle from a stored `ConfigBundle`, resolving
    /// keys through the env-var fallback.
    pub fn from_bundle(b: &ConfigBundle) -> Self {
        Self {
            text: payload_from_kind(&b.text, &b.keys, &b.custom),
            image: payload_from_kind(&b.image, &b.keys, &b.custom),
            code: payload_from_kind(&b.code, &b.keys, &b.custom),
        }
    }
}

fn payload_from_kind(k: &KindDefault, keys: &ProviderKeys, custom: &CustomEndpoint) -> ConfigPayload {
    let (api_key, _from_env) = resolve_key(&k.provider, keys);
    if k.provider == "custom" {
        ConfigPayload {
            provider: k.provider.clone(),
            model: k.model.clone(),
            api_key,
            base_url: custom.base_url.clone(),
            auth_header: custom.auth_header.clone(),
            auth_format: custom.auth_format.clone(),
        }
    } else {
        ConfigPayload {
            provider: k.provider.clone(),
            model: k.model.clone(),
            api_key,
            base_url: String::new(),
            auth_header: String::new(),
            auth_format: String::new(),
        }
    }
}

/// Parse a `Config` input port's raw text. Tries the bundle shape first
/// (emitted by AiConfig), then falls back to a flat `ConfigPayload`
/// (so a manually-wired Text Editor could feed a generator directly).
///
/// `kind` must be `"text"`, `"image"`, or `"code"`. Returns `None` for
/// non-JSON or when the selected kind's payload is empty.
pub fn parse_bundle_then_pick(raw: &str, kind: &str) -> Option<ConfigPayload> {
    let trimmed = raw.trim();
    if trimmed.is_empty() { return None; }
    // Bundle shape (preferred).
    if let Ok(b) = serde_json::from_str::<ConfigBundlePayload>(trimmed) {
        let picked = match kind {
            "text" => b.text,
            "image" => b.image,
            "code" => b.code,
            _ => return None,
        };
        if picked.provider.is_empty() { return None; }
        return Some(picked);
    }
    // Flat payload fallback.
    if let Ok(p) = serde_json::from_str::<ConfigPayload>(trimmed) {
        if !p.provider.is_empty() { return Some(p); }
    }
    None
}

// ── Filesystem layout for per-config files ───────────────────────────────────

/// `~/.patchwork/ai_configs/` (created if missing). `None` only when
/// `dirs::home_dir()` returns `None` (shouldn't happen on supported OSes).
pub fn ai_configs_dir() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join(".patchwork").join("ai_configs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn config_path(name: &str) -> Option<PathBuf> {
    let name = sanitize_config_name(name);
    if name.is_empty() { return None; }
    Some(ai_configs_dir()?.join(format!("{}.json", name)))
}

/// Strip filesystem-hostile characters from a config name. Defensive —
/// the UI only offers a small text input, but we don't want `../` etc.
/// to ever reach the path.
fn sanitize_config_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect()
}

/// List saved configs by scanning `ai_configs/*.json`. Alphabetical;
/// `"default"` first. Ensures a `"default"` entry exists — if the
/// directory is empty or missing, a blank `default.json` is seeded.
pub fn list_config_names() -> Vec<String> {
    let Some(dir) = ai_configs_dir() else { return vec!["default".into()]; };
    let mut names: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") { continue; }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    if !names.iter().any(|n| n == "default") {
        // Seed a blank default so first-launch UI has something to list.
        let _ = save_config("default", &ConfigBundle::seeded());
        names.push("default".into());
    }
    names.sort();
    if let Some(pos) = names.iter().position(|p| p == "default") {
        names.swap(0, pos);
    }
    names
}

/// Load a named config. Returns `None` if the file is missing or parse
/// fails. Does not seed new files.
pub fn load_config(name: &str) -> Option<ConfigBundle> {
    let p = config_path(name)?;
    let s = std::fs::read_to_string(&p).ok()?;
    serde_json::from_str(&s).ok()
}

/// Persist a named config. Creates `ai_configs/` and `chmod 0600` on
/// Unix. Returns the path written on success.
pub fn save_config(name: &str, bundle: &ConfigBundle) -> std::io::Result<PathBuf> {
    let p = config_path(name).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid config name")
    })?;
    let json = serde_json::to_string_pretty(bundle)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(&p, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(p)
}

/// Delete a named config file. `"default"` is refused (the UI also
/// disables the button, but we enforce here too).
pub fn delete_config(name: &str) -> std::io::Result<()> {
    if name == "default" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "cannot delete the default config",
        ));
    }
    let p = config_path(name).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid config name")
    })?;
    if p.exists() { std::fs::remove_file(&p)?; }
    Ok(())
}

/// Write the active-config name into `settings.json`. Leaves the on-disk
/// config file(s) untouched.
pub fn set_active_config(name: &str) -> std::io::Result<()> {
    let mut s = Settings::load();
    s.ai.active = name.to_string();
    s.save()
}

/// Read `settings.ai.active` (defaults to `"default"`).
pub fn active_config_name() -> String {
    Settings::load().ai.active
}

/// Load the currently-active config bundle. Falls back to a blank seeded
/// bundle if the active config's file is missing.
pub fn load_active_bundle() -> ConfigBundle {
    let name = active_config_name();
    load_config(&name).unwrap_or_else(ConfigBundle::seeded)
}

/// One-shot helper used by generators when their `Config` input port is
/// unwired — reads the active bundle and extracts the kind's payload.
/// Returns `None` if the kind has no provider/model set AND no env-var
/// fallback is available.
pub fn load_active_payload(kind: &str) -> Option<ConfigPayload> {
    let bundle = load_active_bundle();
    let payload = match kind {
        "text" => payload_from_kind(&bundle.text, &bundle.keys, &bundle.custom),
        "image" => payload_from_kind(&bundle.image, &bundle.keys, &bundle.custom),
        "code" => payload_from_kind(&bundle.code, &bundle.keys, &bundle.custom),
        _ => return None,
    };
    if payload.provider.is_empty() { return None; }
    Some(payload)
}

/// Migrate pre-2026-Q2 settings (keys stored inline under `ai.profiles`)
/// into per-file configs. Called once at launch; a no-op if the legacy
/// shape isn't present. Each old profile becomes a full config with
/// hard-coded default providers/models.
pub fn migrate_legacy_profiles_if_any() {
    let settings = Settings::load();
    let Some(legacy) = settings.legacy_profiles.as_ref() else { return; };
    let Some(profiles) = legacy.as_object() else { return; };
    for (name, keys_val) in profiles {
        if load_config(name).is_some() {
            // A per-file config already exists; don't clobber it.
            continue;
        }
        let keys: ProviderKeys = serde_json::from_value(keys_val.clone())
            .unwrap_or_default();
        let mut bundle = ConfigBundle::seeded();
        bundle.keys = keys;
        let _ = save_config(name, &bundle);
    }
    // Drop the legacy field so the next settings save won't carry it.
    let mut s = Settings::load();
    s.legacy_profiles = None;
    let _ = s.save();
}

/// Resolve the effective API key for `provider` using a bundle's key
/// pool. Returns (key, came_from_env).
///
/// - Pool key, if non-empty.
/// - Else env-var fallback: `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` /
///   `GOOGLE_API_KEY` / `PATCHWORK_CUSTOM_API_KEY`.
/// - Else empty string with `came_from_env = false`.
pub fn resolve_key(provider: &str, keys: &ProviderKeys) -> (String, bool) {
    let from_pool = match provider {
        "anthropic" => &keys.anthropic,
        "openai" => &keys.openai,
        "google" => &keys.google,
        "custom" => &keys.custom,
        _ => return (String::new(), false),
    };
    if !from_pool.is_empty() {
        return (from_pool.clone(), false);
    }
    let env_var = match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "google" => "GOOGLE_API_KEY",
        "custom" => "PATCHWORK_CUSTOM_API_KEY",
        _ => return (String::new(), false),
    };
    match std::env::var(env_var) {
        Ok(v) if !v.is_empty() => (v, true),
        _ => (String::new(), false),
    }
}

// ── Custom (OpenAI-compatible) endpoint request builder ──────────────────────

/// Build an OpenAI-compatible chat completion request against an
/// arbitrary base URL. Used when the config payload's provider is
/// `"custom"` (Ollama, LM Studio, OpenRouter, vLLM, …).
///
/// Auth header is skipped entirely if `api_key` is empty — matches
/// Ollama's "no auth required" default.
pub fn build_custom_request(
    base_url: &str,
    auth_header: &str,
    auth_format: &str,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    max_tokens: u32,
    temperature: f32,
    image_b64: Option<&str>,
) -> (String, Vec<(String, String)>, String) {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut headers: Vec<(String, String)> = Vec::new();
    headers.push(("Content-Type".into(), "application/json".into()));
    if !api_key.is_empty() {
        let header = if auth_header.is_empty() { "Authorization" } else { auth_header };
        let fmt = if auth_format.is_empty() { "Bearer {key}" } else { auth_format };
        let value = fmt.replace("{key}", api_key);
        headers.push((header.into(), value));
    }
    let mut messages: Vec<serde_json::Value> = Vec::new();
    if !system_prompt.is_empty() {
        messages.push(serde_json::json!({ "role": "system", "content": system_prompt }));
    }
    let user_content: serde_json::Value = if let Some(b64) = image_b64 {
        serde_json::json!([
            { "type": "text", "text": user_prompt },
            { "type": "image_url", "image_url": { "url": format!("data:image/jpeg;base64,{}", b64) } }
        ])
    } else {
        serde_json::Value::String(user_prompt.into())
    };
    messages.push(serde_json::json!({ "role": "user", "content": user_content }));
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "temperature": temperature,
    });
    (url, headers, body.to_string())
}

/// Dispatch to the right request builder based on provider. Combined
/// entry point so generator nodes don't branch on "custom" themselves.
pub fn build_request_for_config(
    cfg: &ConfigPayload,
    system_prompt: &str,
    user_prompt: &str,
    max_tokens: u32,
    temperature: f32,
    mode: u8,
    image_b64: Option<&str>,
) -> (String, Vec<(String, String)>, String) {
    if cfg.provider == "custom" {
        build_custom_request(
            &cfg.base_url,
            &cfg.auth_header,
            &cfg.auth_format,
            &cfg.api_key,
            &cfg.model,
            system_prompt,
            user_prompt,
            max_tokens,
            temperature,
            image_b64,
        )
    } else {
        build_request(
            &cfg.provider,
            &cfg.model,
            &cfg.api_key,
            system_prompt,
            user_prompt,
            max_tokens,
            temperature,
            mode,
            image_b64,
        )
    }
}

// ── Provider key hint text (for AiConfig UI) ─────────────────────────────────

pub fn provider_key_hint(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "Used by Claude models",
        "openai" => "Used by GPT / DALL·E",
        "google" => "Used by Gemini / Imagen",
        "custom" => "Used by custom endpoint (Ollama, LM Studio, …)",
        _ => "",
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_key_uses_pool_when_set() {
        let mut p = ProviderKeys::default();
        p.anthropic = "sk-ant-pool".into();
        let (k, env) = resolve_key("anthropic", &p);
        assert_eq!(k, "sk-ant-pool");
        assert!(!env);
    }

    #[test]
    fn resolve_key_returns_empty_when_no_pool_no_env() {
        let p = ProviderKeys::default();
        if std::env::var("OPENAI_API_KEY").is_ok() { return; }
        let (k, env) = resolve_key("openai", &p);
        assert_eq!(k, "");
        assert!(!env);
    }

    #[test]
    fn bundle_roundtrip_parses_text_kind() {
        let payload = ConfigBundlePayload {
            text: ConfigPayload {
                provider: "anthropic".into(),
                model: "claude-sonnet-4".into(),
                api_key: "sk-ant-xxx".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let s = payload.to_port_string();
        let picked = parse_bundle_then_pick(&s, "text").unwrap();
        assert_eq!(picked.provider, "anthropic");
        assert_eq!(picked.model, "claude-sonnet-4");
        assert_eq!(picked.api_key, "sk-ant-xxx");
    }

    #[test]
    fn parse_bundle_returns_none_for_empty_kind() {
        let payload = ConfigBundlePayload::default();
        let s = payload.to_port_string();
        // `image` has no provider set; should be filtered out.
        assert!(parse_bundle_then_pick(&s, "image").is_none());
    }

    #[test]
    fn parse_bundle_rejects_garbage() {
        assert!(parse_bundle_then_pick("", "text").is_none());
        assert!(parse_bundle_then_pick("not json", "text").is_none());
    }

    #[test]
    fn parse_bundle_falls_back_to_flat_payload() {
        let flat = ConfigPayload {
            provider: "openai".into(),
            model: "gpt-4o".into(),
            api_key: "sk-flat".into(),
            ..Default::default()
        };
        let raw = serde_json::to_string(&flat).unwrap();
        let picked = parse_bundle_then_pick(&raw, "text").unwrap();
        assert_eq!(picked.provider, "openai");
        assert_eq!(picked.model, "gpt-4o");
    }

    #[test]
    fn sanitize_config_name_strips_path_separators() {
        assert_eq!(sanitize_config_name("../../etc"), "etc");
        assert_eq!(sanitize_config_name("work/dev"), "workdev");
        assert_eq!(sanitize_config_name("work_test-1"), "work_test-1");
    }

    #[test]
    fn custom_request_skips_auth_when_key_empty() {
        let (url, headers, _body) = build_custom_request(
            "http://localhost:11434/v1", "", "", "", "llama3", "", "hi", 256, 0.7, None,
        );
        assert_eq!(url, "http://localhost:11434/v1/chat/completions");
        assert!(headers.iter().all(|(h, _)| h != "Authorization"));
    }

    #[test]
    fn custom_request_applies_auth_template() {
        let (_, headers, _) = build_custom_request(
            "https://openrouter.ai/api/v1", "Authorization", "Bearer {key}",
            "sk-or-xxx", "anthropic/claude-3.5-sonnet", "", "hi", 256, 0.7, None,
        );
        let auth = headers.iter().find(|(h, _)| h == "Authorization");
        assert!(auth.is_some());
        assert_eq!(auth.unwrap().1, "Bearer sk-or-xxx");
    }
}
