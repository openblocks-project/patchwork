//! Global PatchWork settings, persisted to `~/.patchwork/settings.json`.
//!
//! These are *user-level* preferences (not part of any project file)
//! and survive across sessions and projects. The Settings node renders
//! a UI on top of this; loading on app startup runs cache eviction
//! according to the saved limits.
//!
//! AI provider keys and per-kind defaults live in a *separate* directory
//! (`~/.patchwork/ai_configs/<name>.json`, one file per named config) —
//! this file only remembers which config is currently active.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_cache_max_mb() -> u32 { 200 }
fn default_cache_max_files() -> u32 { 50 }
fn default_active_config() -> String { "default".into() }

/// API keys for each supported provider. Shared pool within a config —
/// one Anthropic key serves every kind in the config that uses
/// Anthropic. Empty strings trigger env-var fallback at read time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderKeys {
    #[serde(default)] pub anthropic: String,
    #[serde(default)] pub openai: String,
    #[serde(default)] pub google: String,
    #[serde(default)] pub custom: String,
}

/// Provider + model choice for one generator kind (Text / Image / Code).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KindDefault {
    #[serde(default)] pub provider: String,
    #[serde(default)] pub model: String,
}

impl Default for KindDefault {
    fn default() -> Self {
        Self { provider: String::new(), model: String::new() }
    }
}

/// OpenAI-compatible endpoint details (used when any kind's provider is
/// `"custom"` — Ollama, LM Studio, OpenRouter, vLLM, …).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CustomEndpoint {
    #[serde(default)] pub base_url: String,
    #[serde(default)] pub auth_header: String,
    #[serde(default)] pub auth_format: String,
}

/// Slim AI section of `settings.json`. Just the name of the currently
/// active config — the actual config lives in its own file under
/// `~/.patchwork/ai_configs/<active>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSettings {
    #[serde(default = "default_active_config")]
    pub active: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self { active: default_active_config() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Hard cap on `~/.patchwork/image_cache/` total size, in MB.
    #[serde(default = "default_cache_max_mb")]
    pub image_cache_max_mb: u32,
    /// Hard cap on `~/.patchwork/image_cache/` file count.
    #[serde(default = "default_cache_max_files")]
    pub image_cache_max_files: u32,
    /// Which named AI config is active. Keys and per-kind defaults for
    /// that config live in `~/.patchwork/ai_configs/<active>.json`.
    #[serde(default)]
    pub ai: AiSettings,
    /// Pre-2026-Q2 key storage shape. Loaded for migration only — keys
    /// here are copied to per-config files on first save, then this
    /// field is dropped. `#[serde(default, skip_serializing)]` means
    /// we read it if present and never write it back out.
    #[serde(default, rename = "ai_legacy_profiles", skip_serializing_if = "Option::is_none")]
    pub legacy_profiles: Option<serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            image_cache_max_mb: default_cache_max_mb(),
            image_cache_max_files: default_cache_max_files(),
            ai: AiSettings::default(),
            legacy_profiles: None,
        }
    }
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".patchwork").join("settings.json"))
    }

    /// Load from disk, falling back to defaults on any error (missing
    /// file, parse failure, etc.). Also detects and stages a migration
    /// for pre-2026-Q2 settings files that stored `ai.profiles` inline.
    pub fn load() -> Self {
        let Some(p) = Self::path() else { return Self::default(); };
        let raw = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };
        // Try the new shape first.
        if let Ok(s) = serde_json::from_str::<Self>(&raw) {
            return s;
        }
        // Fall back to legacy shape: `ai.profiles` inline. We read it as
        // a loosely-typed Value, stash it in `legacy_profiles` so
        // ai_shared can split it into per-file configs and clear it on
        // the next save.
        let mut settings = Self::default();
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(obj) = v.as_object_mut() {
                if let Some(mb) = obj.get("image_cache_max_mb").and_then(|x| x.as_u64()) {
                    settings.image_cache_max_mb = mb as u32;
                }
                if let Some(n) = obj.get("image_cache_max_files").and_then(|x| x.as_u64()) {
                    settings.image_cache_max_files = n as u32;
                }
                if let Some(ai) = obj.get_mut("ai").and_then(|x| x.as_object_mut()) {
                    if let Some(profiles) = ai.remove("profiles") {
                        settings.legacy_profiles = Some(profiles);
                    }
                    if let Some(active) = ai.get("active").and_then(|x| x.as_str()) {
                        settings.ai.active = active.to_string();
                    }
                }
            }
        }
        settings
    }

    /// Persist to disk; creates `~/.patchwork/` if missing. On Unix the
    /// file is `chmod 0600` so nothing sensitive leaks onto shared
    /// machines even though the sensitive bits (API keys) now live in
    /// separate per-config files.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = Self::path() else { return Ok(()); };
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&p, json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Run cache eviction with the current limits.
    pub fn apply_image_cache(&self) {
        crate::nodes::image_node::evict_image_cache(
            (self.image_cache_max_mb as u64) * 1_048_576,
            self.image_cache_max_files as usize,
        );
    }
}
