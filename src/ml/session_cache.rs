//! Process-wide cache of loaded ONNX sessions.
//!
//! `Session::builder().commit_from_*` is expensive — it does the model parse,
//! graph optimisation, and EP setup. Without this cache every ML inference
//! rebuilds the session from scratch: a 30 fps hand tracker (two-stage detect
//! → landmark pipeline) was hitting `commit_from_*` ~60×/sec, burning CPU on
//! work that's identical between frames.
//!
//! The cache keys by either a bundled-model sentinel (e.g. `"hand_detect"`)
//! or a file path. Sessions are wrapped in `Arc` so multiple concurrent
//! inferences can share the same loaded model; ort's `Session` is `Sync` and
//! `Session::run` takes `&self`, so this is sound.
//!
//! All sessions are built via `crate::ml::ep::session_builder()`, so the
//! platform's accelerator (CoreML on macOS; CUDA → DirectML on Windows; CPU
//! elsewhere) is registered automatically. Previously most call sites used
//! plain `Session::builder()` and silently ran CPU-only — switching them to
//! the cache delivers EP acceleration as a 2-for-1 with the perf fix.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use ort::session::Session;

/// Cached session handle. The outer `Arc` lets multiple call sites share the
/// same loaded model cheaply; the inner `Mutex` provides the `&mut self`
/// access that `Session::run` requires in ort 2.0-rc.12.
///
/// Concurrent inferences on the same model serialize via the Mutex. This is
/// acceptable: ML nodes typically run at 10–30 fps (rate-limited), and most
/// projects have 1–3 ML nodes. The saved `commit_from_*` cost per inference
/// (hundreds of milliseconds → microseconds) dwarfs the contention cost.
pub type CachedSession = Arc<Mutex<Session>>;

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
enum ModelKey {
    /// Bundled model identified by a stable process-lifetime sentinel.
    Bundled(&'static str),
    /// File-based model identified by absolute path string.
    Path(String),
}

static MODEL_CACHE: OnceLock<RwLock<HashMap<ModelKey, CachedSession>>> = OnceLock::new();

fn cache() -> &'static RwLock<HashMap<ModelKey, CachedSession>> {
    MODEL_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Get a cached ONNX session for a bundled model.
///
/// `sentinel` is a stable identifier (e.g. `"hand_detect"`). `bytes` is read
/// only on cache miss. Built via `ml::ep::session_builder()` so the platform
/// accelerator is registered.
pub fn get_bundled_session(
    sentinel: &'static str,
    bytes: &[u8],
) -> Result<CachedSession, String> {
    let key = ModelKey::Bundled(sentinel);

    // Fast path: read lock, return existing
    if let Ok(r) = cache().read() {
        if let Some(s) = r.get(&key) {
            return Ok(s.clone());
        }
    }

    // Slow path: build outside any lock, then insert. The `entry().or_insert`
    // handles the race where another thread inserted while we were building.
    let session = crate::ml::ep::session_builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_memory(bytes)
        .map_err(|e| format!("Load bundled model '{}': {}", sentinel, e))?;
    let handle = Arc::new(Mutex::new(session));
    let mut w = cache()
        .write()
        .map_err(|_| "Model cache lock poisoned".to_string())?;
    Ok(w.entry(key).or_insert(handle).clone())
}

/// Get a cached ONNX session for a file path.
///
/// The path string is used as the cache key; same path → same cached session.
/// If the file changes on disk, callers should invalidate via `clear()` (or
/// future per-key eviction) before the next call.
pub fn get_file_session(path: &str) -> Result<CachedSession, String> {
    let key = ModelKey::Path(path.to_string());

    if let Ok(r) = cache().read() {
        if let Some(s) = r.get(&key) {
            return Ok(s.clone());
        }
    }

    let session = crate::ml::ep::session_builder()
        .map_err(|e| format!("Session builder: {}", e))?
        .commit_from_file(path)
        .map_err(|e| format!("Load model '{}': {}", path, e))?;
    let handle = Arc::new(Mutex::new(session));
    let mut w = cache()
        .write()
        .map_err(|_| "Model cache lock poisoned".to_string())?;
    Ok(w.entry(key).or_insert(handle).clone())
}

/// Briefly lock a cached session to read the name of its first input port.
/// Convenience wrapper so call sites that just need the input name don't have
/// to open their own lock block.
pub fn input_name_of(session: &CachedSession) -> Result<String, String> {
    let s = session.lock().map_err(|_| "Session lock poisoned".to_string())?;
    Ok(s.inputs()
        .first()
        .ok_or_else(|| "Model has no inputs".to_string())?
        .name()
        .to_string())
}

/// Drop all cached sessions. Optional; safe to call on project switch to
/// release memory if the new project doesn't reuse the same models.
#[allow(dead_code)]
pub fn clear() {
    if let Some(c) = MODEL_CACHE.get() {
        if let Ok(mut w) = c.write() {
            w.clear();
        }
    }
}
