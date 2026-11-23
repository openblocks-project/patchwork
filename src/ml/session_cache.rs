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
//!
//! P6 Stage 2.8: a model being built is built once (concurrent requests for
//! it wait instead of compiling it again), and each bundled model's state
//! (loading, ready on which provider and how long it took, or failed) is
//! recorded for the Player's diagnostics ([`bundled_statuses`]).
//! [`preload_bundled`] builds models ahead of their first frame.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use ort::session::Session;

use crate::ml::ep::Provider;

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

/// P6 Stage 2.8: what is known about a bundled model's session.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelState {
    /// Being built (model parse, optimisation, accelerator compile).
    Loading {
        since: Instant,
    },
    Ready {
        provider: Provider,
        /// Why the accelerator was not used (CPU fallback).
        fallback: Option<String>,
        load_time: Duration,
    },
    Failed(String),
}

/// Models being built (by key), so a second request waits for the first.
struct Building {
    keys: Mutex<std::collections::HashSet<ModelKey>>,
    done: Condvar,
}

fn building() -> &'static Building {
    static BUILDING: OnceLock<Building> = OnceLock::new();
    BUILDING.get_or_init(|| Building {
        keys: Mutex::new(Default::default()),
        done: Condvar::new(),
    })
}

fn states() -> &'static Mutex<Vec<(&'static str, ModelState)>> {
    static STATES: OnceLock<Mutex<Vec<(&'static str, ModelState)>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(Vec::new()))
}

fn set_state(sentinel: &'static str, state: ModelState) {
    if let Ok(mut states) = states().lock() {
        match states.iter_mut().find(|(key, _)| *key == sentinel) {
            Some(entry) => entry.1 = state,
            None => states.push((sentinel, state)),
        }
    }
}

/// The bundled models built (or being built) in this process, in the order
/// they were first requested.
pub fn bundled_statuses() -> Vec<(&'static str, ModelState)> {
    states().lock().map(|s| s.clone()).unwrap_or_default()
}

/// Build `key` once: the first caller builds it (outside any lock) while
/// others asking for the same key wait, then take the cached session.
fn get_or_build(
    key: ModelKey,
    build: impl FnOnce() -> Result<Session, String>,
) -> Result<CachedSession, String> {
    let cached = || cache().read().ok().and_then(|r| r.get(&key).cloned());
    if let Some(s) = cached() {
        return Ok(s);
    }
    let gate = building();
    {
        let mut keys = gate
            .keys
            .lock()
            .map_err(|_| "Model cache lock poisoned".to_string())?;
        while keys.contains(&key) {
            keys = gate
                .done
                .wait(keys)
                .map_err(|_| "Model cache lock poisoned".to_string())?;
        }
        if let Some(s) = cached() {
            return Ok(s);
        }
        keys.insert(key.clone());
    }
    // Whatever happens, the next waiter may try.
    struct Release<'a>(&'a Building, &'a ModelKey);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            if let Ok(mut keys) = self.0.keys.lock() {
                keys.remove(self.1);
            }
            self.0.done.notify_all();
        }
    }
    let _release = Release(gate, &key);
    let handle = Arc::new(Mutex::new(build()?));
    let mut w = cache()
        .write()
        .map_err(|_| "Model cache lock poisoned".to_string())?;
    Ok(w.entry(key.clone()).or_insert(handle).clone())
}

/// P6 Stage 2.8: build bundled models in the background (each on its own
/// thread, keeping the caller's permission context), so they are ready
/// before the first frame needs them. Already loaded or loading models are
/// skipped. Bundled models are part of Patchwork itself: loading one reads
/// nothing of the project and runs none of it.
pub fn preload_bundled(models: &[(&'static str, &'static [u8])]) {
    let statuses = bundled_statuses();
    for &(sentinel, bytes) in models {
        if statuses.iter().any(|(key, _)| *key == sentinel) {
            continue;
        }
        set_state(
            sentinel,
            ModelState::Loading {
                since: Instant::now(),
            },
        );
        crate::capability::spawn(move || {
            let _ = get_bundled_session(sentinel, bytes);
        });
    }
}

/// Get a cached ONNX session for a bundled model.
///
/// `sentinel` is a stable identifier (e.g. `"hand_detect"`). `bytes` is read
/// only on cache miss. Built via `ml::ep::session_builder()` so the platform
/// accelerator is registered.
pub fn get_bundled_session(sentinel: &'static str, bytes: &[u8]) -> Result<CachedSession, String> {
    let key = ModelKey::Bundled(sentinel);
    // Built once, outside the cache lock; others asking meanwhile wait.
    get_or_build(key, || {
        let started = Instant::now();
        set_state(sentinel, ModelState::Loading { since: started });
        let built = crate::ml::ep::session_builder_with_provider()
            .map_err(|e| format!("Session builder: {}", e))
            .and_then(|mut built| {
                let provider = built.provider;
                let fallback = built.fallback;
                built
                    .builder
                    .commit_from_memory(bytes)
                    .map(|session| (session, provider, fallback))
                    .map_err(|e| format!("Load bundled model '{}': {}", sentinel, e))
            });
        match built {
            Ok((session, provider, fallback)) => {
                set_state(
                    sentinel,
                    ModelState::Ready {
                        provider,
                        fallback,
                        load_time: started.elapsed(),
                    },
                );
                Ok(session)
            }
            Err(e) => {
                set_state(sentinel, ModelState::Failed(e.clone()));
                Err(e)
            }
        }
    })
}

/// Get a cached ONNX session for a file path.
///
/// The path string is used as the cache key; same path → same cached session.
/// If the file changes on disk, callers should invalidate via `clear()` (or
/// future per-key eviction) before the next call.
pub fn get_file_session(path: &str) -> Result<CachedSession, String> {
    // P4.3.2E2: a model file is read from disk (a cached session too: the
    // graph is using that file).
    crate::capability::check(crate::capability::Capability::Filesystem)?;
    let key = ModelKey::Path(path.to_string());
    get_or_build(key, || {
        crate::ml::ep::session_builder()
            .map_err(|e| format!("Session builder: {}", e))?
            .commit_from_file(path)
            .map_err(|e| format!("Load model '{}': {}", path, e))
    })
}

/// Briefly lock a cached session to read the name of its first input port.
/// Convenience wrapper so call sites that just need the input name don't have
/// to open their own lock block.
pub fn input_name_of(session: &CachedSession) -> Result<String, String> {
    let s = session
        .lock()
        .map_err(|_| "Session lock poisoned".to_string())?;
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
    if let Ok(mut states) = states().lock() {
        states.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Concurrent requests for one model build it once.
    #[test]
    fn a_model_is_built_once_however_many_ask() {
        static BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let key = || ModelKey::Path("stage28://test/dedupe".to_string());
        let threads: Vec<_> = (0..6)
            .map(|_| {
                crate::capability::spawn(move || {
                    get_or_build(key(), || {
                        BUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(150));
                        Err("no model".to_string())
                    })
                })
            })
            .collect();
        // Every build fails here (no model): each waiter retries after the
        // previous build released the key, never two at once.
        for t in threads {
            assert!(t.join().unwrap().is_err());
        }
        assert_eq!(BUILDS.load(std::sync::atomic::Ordering::SeqCst), 6);
        // A key being built makes others wait; a successful build is reused.
        let gate = building();
        gate.keys.lock().unwrap().insert(key());
        let waiter = crate::capability::spawn(move || {
            get_or_build(key(), || Err("waited, then built".to_string()))
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !waiter.is_finished(),
            "a request waits while the model is built"
        );
        gate.keys.lock().unwrap().remove(&key());
        gate.done.notify_all();
        assert_eq!(
            waiter.join().unwrap().err().as_deref(),
            Some("waited, then built")
        );
    }

    #[test]
    fn bundled_model_states_are_recorded() {
        set_state("stage28_test_model", ModelState::Failed("x".to_string()));
        set_state(
            "stage28_test_model",
            ModelState::Ready {
                provider: Provider::Cpu,
                fallback: Some("DirectML: unavailable".to_string()),
                load_time: Duration::from_millis(5),
            },
        );
        let states = bundled_statuses();
        let mine: Vec<_> = states
            .iter()
            .filter(|(k, _)| *k == "stage28_test_model")
            .collect();
        assert_eq!(mine.len(), 1);
        assert!(matches!(
            &mine[0].1,
            ModelState::Ready {
                provider: Provider::Cpu,
                ..
            }
        ));
    }
}
