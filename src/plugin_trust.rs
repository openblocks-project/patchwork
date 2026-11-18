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
    /// P4.3.2E2: native plugin binaries (CLAP) by canonical path → the
    /// SHA-256 of the binary that was approved (pinned on first load), or
    /// [`REVOKED`] (P4.3.2F: refused until approved again).
    #[serde(default)]
    pub native_pins: HashMap<String, String>,
    /// P4.3.2F: set after the store was found damaged and reset. Unknown
    /// binaries are then not pinned on first use: each needs an explicit
    /// approval (trust is never restored implicitly after corruption).
    #[serde(default)]
    pub explicit_approval_only: bool,
}

/// P4.3.2F: the pin of a binary whose approval was withdrawn.
pub const REVOKED: &str = "revoked";

/// P4.3.2F: why the store cannot be used (unreadable or invalid file).
/// While damaged nothing is trusted, pinned or written.
static DAMAGED: Mutex<Option<String>> = Mutex::new(None);

pub fn damaged() -> Option<String> {
    DAMAGED.lock().ok().and_then(|d| d.clone())
}

fn refuse_if_damaged() -> Result<(), String> {
    match damaged() {
        Some(why) => Err(format!(
            "Plugin approvals are unavailable: {why}. Recover them in Library \u{2192} Permissions."
        )),
        None => Ok(()),
    }
}

/// P4.3.2F: read the plugin approvals file: missing is empty, anything
/// unreadable or invalid is damaged (never treated as empty).
pub fn parse_store(read: std::io::Result<String>) -> Result<TrustStore, String> {
    match read {
        Ok(text) => serde_json::from_str::<TrustStore>(&text)
            .map_err(|e| format!("trusted_plugins.json is damaged ({e})")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TrustStore::default()),
        Err(e) => Err(format!("trusted_plugins.json cannot be read ({e})")),
    }
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
        let initial = match parse_store(std::fs::read_to_string(store_path())) {
            Ok(store) => store,
            Err(why) => {
                crate::system_log::warn(format!("Plugin approvals: {why}. Nothing is trusted."));
                if let Ok(mut damaged) = DAMAGED.lock() {
                    *damaged = Some(why);
                }
                TrustStore::default()
            }
        };
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
    // P4.3.2F: nothing is trusted while the approvals are damaged.
    if damaged().is_some() {
        return false;
    }
    store()
        .lock()
        .map(|s| s.entries.contains_key(sha))
        .unwrap_or(false)
}

/// Grant trust for the given source hash. Idempotent (re-granting overwrites
/// the existing entry with a refreshed timestamp / description).
pub fn add_trust(sha: String, description: String) {
    let entry = TrustEntry {
        description,
        added_at_epoch: now_epoch(),
    };
    if let Ok(mut s) = store().lock() {
        // P4.3.2F: a damaged file is never written over.
        if damaged().is_some() {
            return;
        }
        s.entries.insert(sha, entry);
        persist(&s);
        bump_native_generation();
    }
}

/// Revoke trust for a specific source hash. Returns true if an entry was
/// actually removed (false if nothing matched, which is harmless).
pub fn revoke_trust(sha: &str) -> bool {
    if damaged().is_some() {
        return false;
    }
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
/// and revoke trust grants (P4.3.2F: Library → Permissions).
pub fn list_trusted() -> Vec<(String, TrustEntry)> {
    store()
        .lock()
        .map(|s| {
            s.entries
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

// ── P4.3.2E2: native binaries referenced by path ─────────────────────────
//
// A project refers to a CLAP plugin by its path, so the project's content
// digest says nothing about the code that path holds. The binary is pinned
// by its SHA-256 the first time it is loaded (with the native-code
// capability); a different binary at that path is refused until the user
// approves it again, so replacing the file never silently runs new native
// code under an unchanged project.

/// Start of the refusal for a changed binary (the node offers to approve).
pub const NATIVE_CHANGED: &str = "This plugin binary changed since it was approved";

/// SHA-256 (hex) of a file's bytes.
pub fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn native_key(path: &std::path::Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// The pin decision: pins an unknown path, accepts the pinned binary,
/// refuses any other. Returns whether the pins changed.
#[cfg(test)]
pub fn decide_native_pin(
    pins: &mut HashMap<String, String>,
    key: &str,
    sha: &str,
) -> Result<bool, String> {
    decide_native_pin_with(pins, key, sha, false)
}

/// [`decide_native_pin`]; with `explicit_only` an unknown binary is refused
/// instead of pinned (P4.3.2F, after the approvals were reset).
pub fn decide_native_pin_with(
    pins: &mut HashMap<String, String>,
    key: &str,
    sha: &str,
    explicit_only: bool,
) -> Result<bool, String> {
    match pins.get(key) {
        None if explicit_only => Err(format!(
            "{NATIVE_CHANGED}: plugin approvals were reset, so this plugin needs your approval \
             before it loads."
        )),
        None => {
            pins.insert(key.to_string(), sha.to_string());
            Ok(true)
        }
        Some(pinned) if pinned == REVOKED => Err(format!(
            "{NATIVE_CHANGED}: its approval was withdrawn. Approve it again to load it."
        )),
        Some(pinned) if pinned == sha => Ok(false),
        Some(pinned) => Err(format!(
            "{NATIVE_CHANGED} ({} → {}). Approve it again to load it.",
            short_hash(pinned),
            short_hash(sha)
        )),
    }
}

/// Check (and on first use pin) the native binary at `path`.
pub fn check_native_binary(path: &std::path::Path) -> Result<(), String> {
    let sha = file_sha256(path)?;
    let mut store = store()
        .lock()
        .map_err(|_| "Plugin trust is unavailable".to_string())?;
    refuse_if_damaged()?;
    let explicit_only = store.explicit_approval_only;
    if decide_native_pin_with(
        &mut store.native_pins,
        &native_key(path),
        &sha,
        explicit_only,
    )? {
        persist(&store);
    }
    Ok(())
}

/// The user approved the binary now at `path`.
pub fn approve_native_binary(path: &std::path::Path) -> Result<(), String> {
    let sha = file_sha256(path)?;
    let mut store = store()
        .lock()
        .map_err(|_| "Plugin trust is unavailable".to_string())?;
    refuse_if_damaged()?;
    store.native_pins.insert(native_key(path), sha);
    persist(&store);
    bump_native_generation();
    Ok(())
}

/// P4.3.2F: bumped whenever a native approval changes, so loaded plugins
/// check their approval again (a withdrawn one is unloaded).
static NATIVE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn native_generation() -> u64 {
    NATIVE_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

fn bump_native_generation() {
    NATIVE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// P4.3.2F: whether the binary at `path` is still approved (pinned to its
/// current bytes), without pinning anything. For loaded plugins.
pub fn native_still_approved(path: &std::path::Path) -> bool {
    let Ok(sha) = file_sha256(path) else {
        return false;
    };
    damaged().is_none()
        && store()
            .lock()
            .is_ok_and(|store| store.native_pins.get(&native_key(path)) == Some(&sha))
}

/// P4.3.2F: native plugin approvals (path, pinned SHA-256 or [`REVOKED`]).
pub fn native_pins() -> Result<Vec<(String, String)>, String> {
    refuse_if_damaged()?;
    let store = store()
        .lock()
        .map_err(|_| "Plugin trust is unavailable".to_string())?;
    let mut pins: Vec<(String, String)> = store
        .native_pins
        .iter()
        .map(|(path, sha)| (path.clone(), sha.clone()))
        .collect();
    pins.sort();
    Ok(pins)
}

/// P4.3.2F: withdraw the approval of the native plugin at `key` (a path
/// from [`native_pins`]). It is refused (and unloaded where loaded) until
/// approved again; it is not pinned again on first use.
pub fn revoke_native_pin(key: &str) -> Result<(), String> {
    refuse_if_damaged()?;
    let mut store = store()
        .lock()
        .map_err(|_| "Plugin trust is unavailable".to_string())?;
    let pin = store
        .native_pins
        .get_mut(key)
        .ok_or_else(|| "This plugin has no approval.".to_string())?;
    *pin = REVOKED.to_string();
    persist(&store);
    bump_native_generation();
    Ok(())
}

/// P4.3.2F: move a damaged approvals file aside (kept for inspection) and
/// start an empty one in which every plugin needs an explicit approval.
pub fn move_damaged_aside() -> Result<PathBuf, String> {
    let why = damaged().ok_or_else(|| "Plugin approvals are not damaged.".to_string())?;
    let path = store_path();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let aside = path.with_file_name(format!("trusted_plugins.damaged-{nanos}.json"));
    if path.exists() {
        std::fs::rename(&path, &aside)
            .map_err(|e| format!("Cannot move the damaged plugin approvals ({why}): {e}"))?;
    }
    let mut store = store()
        .lock()
        .map_err(|_| "Plugin trust is unavailable".to_string())?;
    *store = TrustStore {
        explicit_approval_only: true,
        ..TrustStore::default()
    };
    if let Ok(mut damaged) = DAMAGED.lock() {
        *damaged = None;
    }
    persist(&store);
    bump_native_generation();
    Ok(aside)
}

fn persist(store: &TrustStore) {
    let path = store_path();
    if let Ok(json) = serde_json::to_string_pretty(store) {
        if let Err(e) = std::fs::write(&path, json) {
            crate::system_log::warn(format!("Failed to persist trusted_plugins.json: {}", e));
            return;
        }
        // Mode 0600 on Unix so other users on a shared machine can't audit
        // (or modify) which plugins you've trusted.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(test)]
mod native_pin_tests {
    use super::*;

    #[test]
    fn a_changed_native_binary_is_refused_until_approved_again() {
        let mut pins = HashMap::new();
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        assert_eq!(decide_native_pin(&mut pins, "/p/x.clap", &a), Ok(true));
        assert_eq!(decide_native_pin(&mut pins, "/p/x.clap", &a), Ok(false));
        let refused = decide_native_pin(&mut pins, "/p/x.clap", &b).unwrap_err();
        assert!(refused.starts_with(NATIVE_CHANGED));
        assert_eq!(pins["/p/x.clap"], a, "the approved binary stays pinned");
        // Another path is pinned on its own.
        assert_eq!(decide_native_pin(&mut pins, "/p/y.clap", &b), Ok(true));
    }
}

fn now_epoch() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn damaged_plugin_approvals_trust_nothing_and_reset_needs_explicit_approval() {
        // Missing: empty. Unreadable or invalid: damaged (never empty).
        assert!(parse_store(Err(std::io::ErrorKind::NotFound.into())).is_ok());
        assert!(parse_store(Ok("{ not json".into())).is_err());
        assert!(parse_store(Err(std::io::ErrorKind::PermissionDenied.into())).is_err());
        let store = parse_store(Ok(r#"{"native_pins":{"/p/x.clap":"revoked"}}"#.into())).unwrap();
        assert!(!store.explicit_approval_only);

        let a = "a".repeat(64);
        let mut pins = store.native_pins.clone();
        // A withdrawn approval is refused, and never pinned again on use.
        let refused = decide_native_pin_with(&mut pins, "/p/x.clap", &a, false).unwrap_err();
        assert!(refused.contains("withdrawn"), "{refused}");
        assert_eq!(pins["/p/x.clap"], REVOKED);
        // After a reset, unknown binaries need an explicit approval.
        assert!(decide_native_pin_with(&mut pins, "/p/new.clap", &a, true).is_err());
        assert!(!pins.contains_key("/p/new.clap"));
        assert_eq!(
            decide_native_pin_with(&mut pins, "/p/new.clap", &a, false),
            Ok(true)
        );
    }
}
