//! Global PatchWork settings, persisted to `~/.patchwork/settings.json`.
//!
//! These are *user-level* preferences (not part of any project file)
//! and survive across sessions and projects. The Settings node renders
//! a UI on top of this; loading on app startup runs cache eviction
//! according to the saved limits.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn default_cache_max_mb() -> u32 { 200 }
fn default_cache_max_files() -> u32 { 50 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Hard cap on `~/.patchwork/image_cache/` total size, in MB.
    #[serde(default = "default_cache_max_mb")]
    pub image_cache_max_mb: u32,
    /// Hard cap on `~/.patchwork/image_cache/` file count.
    #[serde(default = "default_cache_max_files")]
    pub image_cache_max_files: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            image_cache_max_mb: default_cache_max_mb(),
            image_cache_max_files: default_cache_max_files(),
        }
    }
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        Some(dirs::home_dir()?.join(".patchwork").join("settings.json"))
    }

    /// Load from disk, falling back to defaults on any error (missing
    /// file, parse failure, etc.).
    pub fn load() -> Self {
        let Some(p) = Self::path() else { return Self::default(); };
        std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist to disk; creates `~/.patchwork/` if missing.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(p) = Self::path() else { return Ok(()); };
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(p, json)
    }

    /// Run cache eviction with the current limits.
    pub fn apply_image_cache(&self) {
        crate::nodes::image_node::evict_image_cache(
            (self.image_cache_max_mb as u64) * 1_048_576,
            self.image_cache_max_files as usize,
        );
    }
}
