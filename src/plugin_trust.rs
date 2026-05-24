//! Trust subsystem for native-code plugins.
//!
//! Any plugin that goes through `cargo build --release` + `libloading` (the
//! existing `RustPlugin` node today, the future `rust` body plugins per plan
//! §3.5) MUST be explicitly trusted before it's built and loaded. Without
//! this gate a malicious `project.json` could embed Rust source that gets
//! compiled and loaded the moment the user clicks "Build" on the node — the
//! social-engineering path "hey try my cool node!" → user clicks → pwned.
//!
//! Trust is granted per-SHA256 of the source bytes:
//! - Editing the code changes the SHA → trust is re-asked (correct: edited
//!   code is functionally a new plugin).
//! - Identical code on a different project (or moved between projects) keeps
//!   the existing trust (correct: same bytes = same behaviour).
//!
//! Storage: `~/.patchwork/trusted_plugins.json`, mode 0600 on Unix.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrustStore {
    /// Source SHA256 (hex) → user-visible metadata.
    #[serde(default)]
    pub entries: HashMap<String, TrustEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustEntry {
    /// Short user-supplied or auto-derived description ("RustPlugin node #5",
    /// "my band-pass filter", etc.). Shown in the settings UI for revocation.
    #[serde(default)]
    pub description: String,
    /// Unix epoch seconds at the time trust was granted. Used for "trusted on
    /// May 24, 2026" in the settings UI later.
    #[serde(default)]
    pub added_at_epoch: u64,
}

fn store_path() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".patchwork");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("trusted_plugins.json")
}

static STORE: OnceLock<Mutex<TrustStore>> = OnceLock::new();

fn store() -> &'static Mutex<TrustStore> {
    STORE.get_or_init(|| {
        let initial = std::fs::read_to_string(store_path())
            .ok()
            .and_then(|s| serde_json::from_str::<TrustStore>(&s).ok())
            .unwrap_or_default();
        Mutex::new(initial)
    })
}

/// Canonical hash of a plugin's source bytes. Hex-encoded SHA256, used as
/// both the trust key and the user-visible "fingerprint" in confirmation
/// dialogs.
pub fn source_hash(code: &str) -> String {
    let mut h = Sha256::new();
    h.update(code.as_bytes());
    let bytes = h.finalize();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes.iter() {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// Convenience: a short prefix of the hash suitable for in-UI display
/// ("trust this code → SHA256 abc12345…"). Full hex is too long for chips.
pub fn short_hash(sha_hex: &str) -> String {
    let n = sha_hex.len().min(8);
    sha_hex[..n].to_string()
}

pub fn is_trusted(sha: &str) -> bool {
    store().lock().map(|s| s.entries.contains_key(sha)).unwrap_or(false)
}

/// Grant trust for the given source hash. Idempotent (re-granting overwrites
/// the existing entry with a refreshed timestamp / description).
pub fn add_trust(sha: String, description: String) {
    let entry = TrustEntry {
        description,
        added_at_epoch: now_epoch(),
    };
    if let Ok(mut s) = store().lock() {
        s.entries.insert(sha, entry);
        persist(&s);
    }
}

/// Revoke trust for a specific source hash. Returns true if an entry was
/// actually removed (false if nothing matched, which is harmless).
#[allow(dead_code)]
pub fn revoke_trust(sha: &str) -> bool {
    if let Ok(mut s) = store().lock() {
        let removed = s.entries.remove(sha).is_some();
        if removed {
            persist(&s);
        }
        removed
    } else {
        false
    }
}

/// All currently-trusted entries, for a settings UI that lets the user view
/// and revoke trust grants. Defined now so the UI is one Edit away later.
#[allow(dead_code)]
pub fn list_trusted() -> Vec<(String, TrustEntry)> {
    store()
        .lock()
        .map(|s| s.entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn persist(store: &TrustStore) {
    let path = store_path();
    if let Ok(json) = serde_json::to_string_pretty(store) {
        if let Err(e) = std::fs::write(&path, json) {
            crate::system_log::warn(format!(
                "Failed to persist trusted_plugins.json: {}", e
            ));
            return;
        }
        // Mode 0600 on Unix so other users on a shared machine can't audit
        // (or modify) which plugins you've trusted.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &path,
                std::fs::Permissions::from_mode(0o600),
            );
        }
    }
}

fn now_epoch() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
