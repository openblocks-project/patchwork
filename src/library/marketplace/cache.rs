//! P4.3.1A: the local catalogue cache and refresh.
//!
//! `~/.patchwork/marketplace/catalogue-cache.json` holds the last catalogue
//! that passed validation, verbatim, with its source and fetch times. It is
//! written atomically (temporary file + rename) and only after a refreshed
//! catalogue was fully validated and accepted by
//! [`check_refresh`](super::check_refresh), so a failed, invalid or
//! rolled-back refresh never replaces it. On load the cached text is
//! validated again; a damaged cache is never used.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::fetch::{FetchError, FetchResponse, Transport};
use super::limits::MAX_CATALOGUE_BYTES;
use super::source::CatalogueLocation;
use super::{Catalogue, CatalogueErrorKind, check_refresh, parse_catalogue};

/// Cache files are `catalogue-<hash of owner/repo@branch>.json`: one per
/// configured source, so changing the source never shows (or overwrites,
/// or weakens rollback protection for) another source's catalogue.
pub const CACHE_FILE_PREFIX: &str = "catalogue-";
pub const CACHE_FORMAT: u32 = 1;
/// A cached catalogue older than this is labelled out of date.
pub const STALE_AFTER_SECS: u64 = 7 * 24 * 60 * 60;
/// Largest cache file read (the catalogue is stored as an escaped string).
const MAX_CACHE_BYTES: u64 = MAX_CATALOGUE_BYTES as u64 * 2 + 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheFile {
    format: u32,
    owner: String,
    repo: String,
    branch: String,
    /// When this catalogue was downloaded (Unix seconds).
    fetched_at: u64,
    /// When the server last confirmed it (download or HTTP 304).
    checked_at: u64,
    #[serde(default)]
    etag: Option<String>,
    /// SHA-256 of `catalogue`.
    sha256: String,
    /// The catalogue exactly as downloaded.
    catalogue: String,
}

/// The cache file for one source.
pub fn cache_path(dir: &Path, location: &CatalogueLocation) -> PathBuf {
    let key = sha256_hex(location.label().as_bytes());
    dir.join(format!("{CACHE_FILE_PREFIX}{}.json", &key[..16]))
}

/// `~/.patchwork/marketplace`.
pub fn default_cache_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".patchwork").join("marketplace"))
        .ok_or_else(|| "Cannot locate your home directory for the marketplace cache.".to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Debug, Clone)]
pub struct CachedCatalogue {
    pub catalogue: Catalogue,
    pub fetched_at: u64,
    pub checked_at: u64,
    pub etag: Option<String>,
    raw: String,
}

#[derive(Debug, Clone)]
pub enum CacheLoad {
    Missing,
    /// The cache belongs to another configured source.
    OtherSource,
    Damaged(String),
    Loaded(Box<CachedCatalogue>),
}

/// Read and re-validate the cache for `location`.
pub fn load_cache(dir: &Path, location: &CatalogueLocation) -> CacheLoad {
    let path = cache_path(dir, location);
    let size = match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(_) => return CacheLoad::Damaged("The cache is not a file.".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return CacheLoad::Missing,
        Err(e) => return CacheLoad::Damaged(format!("Cannot read the cache: {e}")),
    };
    if size > MAX_CACHE_BYTES {
        return CacheLoad::Damaged("The cache file is too large.".to_string());
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => return CacheLoad::Damaged(format!("Cannot read the cache: {e}")),
    };
    let file: CacheFile = match serde_json::from_str(&text) {
        Ok(file) => file,
        Err(e) => return CacheLoad::Damaged(format!("The cache is not valid: {e}")),
    };
    if file.format != CACHE_FORMAT {
        return CacheLoad::Damaged(format!("Unsupported cache format {}.", file.format));
    }
    if file.owner != location.source().owner()
        || file.repo != location.source().repo()
        || file.branch != location.branch()
    {
        return CacheLoad::OtherSource;
    }
    if sha256_hex(file.catalogue.as_bytes()) != file.sha256 {
        return CacheLoad::Damaged("The cached catalogue does not match its checksum.".to_string());
    }
    match parse_catalogue(file.catalogue.as_bytes(), location.source()) {
        Ok(catalogue) => CacheLoad::Loaded(Box::new(CachedCatalogue {
            catalogue,
            fetched_at: file.fetched_at,
            checked_at: file.checked_at,
            etag: file.etag,
            raw: file.catalogue,
        })),
        Err(e) => CacheLoad::Damaged(format!("The cached catalogue is not valid: {e}")),
    }
}

/// Write the cache atomically: a temporary file in `dir`, flushed to disk,
/// then renamed over the previous cache.
pub fn store_cache(
    dir: &Path,
    location: &CatalogueLocation,
    raw: &str,
    fetched_at: u64,
    checked_at: u64,
    etag: Option<&str>,
) -> Result<(), String> {
    let file = CacheFile {
        format: CACHE_FORMAT,
        owner: location.source().owner().to_string(),
        repo: location.source().repo().to_string(),
        branch: location.branch().to_string(),
        fetched_at,
        checked_at,
        etag: etag.map(str::to_string),
        sha256: sha256_hex(raw.as_bytes()),
        catalogue: raw.to_string(),
    };
    let json = serde_json::to_vec(&file).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let target = cache_path(dir, location);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.{}-{nanos}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut out = std::fs::File::create(&temp)?;
        out.write_all(&json)?;
        out.sync_all()?;
        drop(out);
        std::fs::rename(&temp, &target)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("Cannot save the marketplace cache: {e}")
    })
}

/// What the browser shows.
#[derive(Debug, Clone, Default)]
pub struct CatalogueView {
    pub catalogue: Option<Catalogue>,
    pub fetched_at: Option<u64>,
    pub checked_at: Option<u64>,
    /// Shown from the cache (not confirmed by the server in this refresh).
    pub from_cache: bool,
    /// Why the cache could not be used, if it exists but is damaged.
    pub cache_problem: Option<String>,
    /// P4.3.2C: SHA-256 of the exact catalogue text that was validated
    /// (recorded as the revision in marketplace provenance).
    pub catalogue_sha256: Option<String>,
}

impl CatalogueView {
    pub fn is_stale(&self, now: u64) -> bool {
        self.checked_at
            .is_some_and(|checked| now.saturating_sub(checked) > STALE_AFTER_SECS)
    }

    fn from_load(load: &CacheLoad) -> Self {
        match load {
            CacheLoad::Loaded(cached) => Self {
                catalogue: Some(cached.catalogue.clone()),
                fetched_at: Some(cached.fetched_at),
                checked_at: Some(cached.checked_at),
                from_cache: true,
                cache_problem: None,
                catalogue_sha256: Some(sha256_hex(cached.raw.as_bytes())),
            },
            CacheLoad::Damaged(problem) => Self {
                cache_problem: Some(problem.clone()),
                ..Self::default()
            },
            CacheLoad::Missing | CacheLoad::OtherSource => Self::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// A new catalogue was accepted and cached.
    Updated,
    /// Accepted, but the cache could not be written (shown from memory).
    UpdatedNotSaved(String),
    /// The server confirmed the cached catalogue (HTTP 304).
    Unchanged,
    /// Network or HTTP failure; the cache (if any) is shown.
    FetchFailed(FetchError),
    /// The downloaded catalogue was invalid, rolled back or rewrote
    /// published versions; the cache (if any) is shown.
    Rejected(Vec<String>),
    /// The catalogue needs a newer Patchwork; the cache (if any) is shown.
    UpdateRequired(u64),
}

/// Browse offline: whatever valid cache exists.
pub fn open_cached(dir: &Path, location: &CatalogueLocation) -> CatalogueView {
    CatalogueView::from_load(&load_cache(dir, location))
}

/// Fetch, validate and (only if everything passes) replace the cache.
pub fn refresh(
    transport: &dyn Transport,
    dir: &Path,
    location: &CatalogueLocation,
    now: u64,
) -> (CatalogueView, RefreshOutcome) {
    let load = load_cache(dir, location);
    let cached_view = CatalogueView::from_load(&load);
    let cached = match &load {
        CacheLoad::Loaded(cached) => Some(cached.as_ref()),
        _ => None,
    };
    let etag = cached.and_then(|c| c.etag.as_deref());
    let response = match transport.fetch(&location.index_url(), etag, MAX_CATALOGUE_BYTES) {
        Ok(response) => response,
        Err(error) => return (cached_view, RefreshOutcome::FetchFailed(error)),
    };
    let (bytes, new_etag) = match response {
        FetchResponse::NotModified => {
            let Some(cached) = cached else {
                return (
                    cached_view,
                    RefreshOutcome::FetchFailed(FetchError::Status(304)),
                );
            };
            // Best effort: only the "last checked" time changes.
            let _ = store_cache(
                dir,
                location,
                &cached.raw,
                cached.fetched_at,
                now,
                cached.etag.as_deref(),
            );
            let view = CatalogueView {
                checked_at: Some(now),
                from_cache: false,
                ..cached_view
            };
            return (view, RefreshOutcome::Unchanged);
        }
        FetchResponse::Body { bytes, etag } => (bytes, etag),
    };
    let next = match parse_catalogue(&bytes, location.source()) {
        Ok(next) => next,
        Err(error) => {
            let outcome = match error.kind {
                CatalogueErrorKind::UnsupportedFormat(format) => {
                    RefreshOutcome::UpdateRequired(format)
                }
                _ => RefreshOutcome::Rejected(error.issues),
            };
            return (cached_view, outcome);
        }
    };
    if let Some(cached) = cached {
        if let Err(problems) = check_refresh(&cached.catalogue, &next) {
            return (cached_view, RefreshOutcome::Rejected(problems));
        }
    }
    // parse_catalogue succeeded, so the bytes are UTF-8.
    let raw = String::from_utf8_lossy(&bytes).into_owned();
    let outcome = match store_cache(dir, location, &raw, now, now, new_etag.as_deref()) {
        Ok(()) => RefreshOutcome::Updated,
        Err(error) => RefreshOutcome::UpdatedNotSaved(error),
    };
    let view = CatalogueView {
        catalogue: Some(next),
        fetched_at: Some(now),
        checked_at: Some(now),
        from_cache: false,
        cache_problem: None,
        catalogue_sha256: Some(sha256_hex(&bytes)),
    };
    (view, outcome)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::library::marketplace::fetch::fixture::{Route, Server};
    use crate::library::marketplace::fetch::{HttpTransport, Origin, Timeouts};
    use crate::library::package::tests::TempDir;
    use serde_json::{Value, json};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    pub(crate) const EXAMPLE: &str = include_str!("../../../docs/marketplace/index.example.json");

    pub(crate) fn location() -> CatalogueLocation {
        CatalogueLocation::new("patchwork-nodes", "catalogue", "main").unwrap()
    }

    /// The documented example with a given revision.
    pub(crate) fn catalogue_at(generated_at: u64) -> Vec<u8> {
        let mut value: Value = serde_json::from_str(EXAMPLE).unwrap();
        value["generated_at"] = json!(generated_at);
        serde_json::to_vec_pretty(&value).unwrap()
    }

    /// Replays canned responses and records the ETags it was sent.
    struct Mock {
        responses: Mutex<VecDeque<Result<FetchResponse, FetchError>>>,
        etags: Mutex<Vec<Option<String>>>,
    }

    impl Mock {
        fn new(responses: Vec<Result<FetchResponse, FetchError>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                etags: Mutex::default(),
            }
        }
    }

    impl Transport for Mock {
        fn fetch(
            &self,
            url: &str,
            etag: Option<&str>,
            _max: usize,
        ) -> Result<FetchResponse, FetchError> {
            assert!(url.ends_with("/index.json"), "{url}");
            self.etags.lock().unwrap().push(etag.map(str::to_string));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected fetch")
        }
    }

    fn body(bytes: Vec<u8>, etag: Option<&str>) -> Result<FetchResponse, FetchError> {
        Ok(FetchResponse::Body {
            bytes,
            etag: etag.map(str::to_string),
        })
    }

    fn cache_bytes(dir: &Path) -> Vec<u8> {
        std::fs::read(cache_path(dir, &location())).unwrap()
    }

    #[test]
    fn valid_catalogue_is_retrieved_over_http_and_cached() {
        let tmp = TempDir::new("mk_http");
        let server = Server::start();
        let path = "/patchwork-nodes/catalogue/main/index.json";
        server.route(
            path,
            Route::Ok {
                body: catalogue_at(100),
                etag: Some("\"r100\"".into()),
            },
        );
        let transport =
            HttpTransport::new(Origin::local_http(server.port), Timeouts::default()).unwrap();
        // The production URL is https; route the same path to the fixture.
        struct Local<'a>(&'a HttpTransport, &'a Server);
        impl Transport for Local<'_> {
            fn fetch(
                &self,
                url: &str,
                etag: Option<&str>,
                max: usize,
            ) -> Result<FetchResponse, FetchError> {
                let path = url.trim_start_matches("https://raw.githubusercontent.com");
                self.0.fetch(&self.1.url(path), etag, max)
            }
        }
        let local = Local(&transport, &server);
        let (view, outcome) = refresh(&local, &tmp.0, &location(), 1_000);
        assert_eq!(outcome, RefreshOutcome::Updated);
        assert_eq!(view.catalogue.as_ref().unwrap().packages.len(), 3);
        assert!(!view.from_cache);

        // Second refresh sends the ETag and gets 304.
        let (view, outcome) = refresh(&local, &tmp.0, &location(), 2_000);
        assert_eq!(outcome, RefreshOutcome::Unchanged);
        assert_eq!(view.fetched_at, Some(1_000));
        assert_eq!(view.checked_at, Some(2_000));
        let offline = open_cached(&tmp.0, &location());
        assert_eq!(offline.checked_at, Some(2_000));
        assert!(offline.from_cache);
    }

    #[test]
    fn failed_refreshes_keep_the_previous_good_cache() {
        let tmp = TempDir::new("mk_keep");
        let mock = Mock::new(vec![
            body(catalogue_at(100), Some("e1")),
            Err(FetchError::Timeout),
            Err(FetchError::TooLarge {
                limit: MAX_CATALOGUE_BYTES,
            }),
            Err(FetchError::Status(503)),
            body(b"<html>rate limited</html>".to_vec(), None),
            body(
                br#"{"kind":"patchwork-catalogue","catalogue_format":1,"packages":[{"id":"x"}]}"#
                    .to_vec(),
                None,
            ),
            body(
                br#"{"kind":"patchwork-catalogue","catalogue_format":9}"#.to_vec(),
                None,
            ),
        ]);
        let (_, outcome) = refresh(&mock, &tmp.0, &location(), 1_000);
        assert_eq!(outcome, RefreshOutcome::Updated);
        let good = cache_bytes(&tmp.0);

        let expected = [
            "timeout",
            "too large",
            "503",
            "invalid json",
            "invalid catalogue",
            "format 9",
        ];
        for label in expected {
            let (view, outcome) = refresh(&mock, &tmp.0, &location(), 2_000);
            match label {
                "timeout" => assert_eq!(outcome, RefreshOutcome::FetchFailed(FetchError::Timeout)),
                "too large" | "503" => assert!(matches!(outcome, RefreshOutcome::FetchFailed(_))),
                "format 9" => assert_eq!(outcome, RefreshOutcome::UpdateRequired(9)),
                _ => assert!(matches!(outcome, RefreshOutcome::Rejected(_)), "{label}"),
            }
            assert!(view.from_cache, "{label}: cached catalogue still shown");
            assert_eq!(view.catalogue.as_ref().unwrap().packages.len(), 3);
            assert_eq!(view.fetched_at, Some(1_000));
            assert_eq!(cache_bytes(&tmp.0), good, "{label}: cache untouched");
        }
        // The cached ETag was offered on every refresh after the first.
        let etags = mock.etags.lock().unwrap();
        assert_eq!(etags[0], None);
        assert!(etags[1..].iter().all(|e| e.as_deref() == Some("e1")));
    }

    #[test]
    fn rollbacks_and_rewrites_are_refused() {
        let tmp = TempDir::new("mk_rollback");
        let mut rewritten: Value = serde_json::from_str(EXAMPLE).unwrap();
        rewritten["generated_at"] = json!(300);
        rewritten["packages"][0]["versions"][0]["artifact"]["sha256"] = json!("ab".repeat(32));
        let mut unversioned: Value = serde_json::from_str(EXAMPLE).unwrap();
        unversioned.as_object_mut().unwrap().remove("generated_at");
        let mock = Mock::new(vec![
            body(catalogue_at(200), None),
            body(catalogue_at(100), None),
            body(serde_json::to_vec(&unversioned).unwrap(), None),
            body(serde_json::to_vec(&rewritten).unwrap(), None),
            body(catalogue_at(200), None),
            body(catalogue_at(250), None),
        ]);
        assert_eq!(
            refresh(&mock, &tmp.0, &location(), 1).1,
            RefreshOutcome::Updated
        );
        let good = cache_bytes(&tmp.0);
        for case in ["older", "no revision", "rewritten"] {
            let (view, outcome) = refresh(&mock, &tmp.0, &location(), 2);
            assert!(matches!(outcome, RefreshOutcome::Rejected(_)), "{case}");
            assert_eq!(view.catalogue.unwrap().generated_at, Some(200), "{case}");
            assert_eq!(cache_bytes(&tmp.0), good, "{case}");
        }
        // Same revision again and a newer one are accepted.
        assert_eq!(
            refresh(&mock, &tmp.0, &location(), 3).1,
            RefreshOutcome::Updated
        );
        assert_eq!(
            refresh(&mock, &tmp.0, &location(), 4).1,
            RefreshOutcome::Updated
        );
        assert_eq!(
            open_cached(&tmp.0, &location())
                .catalogue
                .unwrap()
                .generated_at,
            Some(250)
        );
    }

    #[test]
    fn offline_mode_shows_the_cache_and_labels_its_age() {
        let tmp = TempDir::new("mk_offline");
        // Nothing cached and offline: nothing to show, nothing written.
        let mock = Mock::new(vec![Err(FetchError::Network("offline".into()))]);
        let (view, outcome) = refresh(&mock, &tmp.0, &location(), 10);
        assert!(view.catalogue.is_none());
        assert!(matches!(outcome, RefreshOutcome::FetchFailed(_)));
        assert!(!cache_path(&tmp.0, &location()).exists());

        store_cache(
            &tmp.0,
            &location(),
            std::str::from_utf8(&catalogue_at(5)).unwrap(),
            100,
            100,
            None,
        )
        .unwrap();
        let view = open_cached(&tmp.0, &location());
        assert!(view.from_cache);
        assert!(!view.is_stale(100 + STALE_AFTER_SECS));
        assert!(view.is_stale(101 + STALE_AFTER_SECS));
        // A cache for another configured source is not shown.
        let other = CatalogueLocation::new("someone", "else", "main").unwrap();
        assert!(open_cached(&tmp.0, &other).catalogue.is_none());
        assert!(matches!(load_cache(&tmp.0, &other), CacheLoad::Missing));
    }

    #[test]
    fn malformed_cache_files_are_never_used_and_are_replaced_by_a_good_refresh() {
        let tmp = TempDir::new("mk_damaged");
        let path = cache_path(&tmp.0, &location());
        let raw = std::str::from_utf8(&catalogue_at(5)).unwrap().to_string();
        store_cache(&tmp.0, &location(), &raw, 1, 1, None).unwrap();
        let good: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();

        let mut tampered = good.clone();
        tampered["catalogue"] = json!(raw.replace("Low-frequency", "Evil"));
        let mut invalid = good.clone();
        let bad_catalogue =
            r#"{"kind":"patchwork-catalogue","catalogue_format":1,"packages":[{"id":"local.x"}]}"#;
        invalid["catalogue"] = json!(bad_catalogue);
        invalid["sha256"] = json!(sha256_hex(bad_catalogue.as_bytes()));
        let mut future = good.clone();
        future["format"] = json!(2);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("garbage", b"\x00\xffnot json".to_vec()),
            (
                "truncated",
                serde_json::to_vec(&good).unwrap()[..40].to_vec(),
            ),
            ("empty", Vec::new()),
            ("checksum", serde_json::to_vec(&tampered).unwrap()),
            ("invalid catalogue", serde_json::to_vec(&invalid).unwrap()),
            ("format", serde_json::to_vec(&future).unwrap()),
        ];
        for (label, bytes) in cases {
            std::fs::write(&path, bytes).unwrap();
            let view = open_cached(&tmp.0, &location());
            assert!(view.catalogue.is_none(), "{label}");
            assert!(view.cache_problem.is_some(), "{label}");
        }
        // Too large to read.
        std::fs::write(&path, vec![b' '; MAX_CACHE_BYTES as usize + 1]).unwrap();
        assert!(open_cached(&tmp.0, &location()).cache_problem.is_some());

        // A good refresh replaces the damaged cache.
        let mock = Mock::new(vec![body(catalogue_at(7), None)]);
        assert_eq!(
            refresh(&mock, &tmp.0, &location(), 9).1,
            RefreshOutcome::Updated
        );
        assert_eq!(
            open_cached(&tmp.0, &location())
                .catalogue
                .unwrap()
                .generated_at,
            Some(7)
        );
    }

    #[test]
    fn cache_writes_are_atomic_and_leave_no_temporary_files() {
        let tmp = TempDir::new("mk_atomic");
        let raw = std::str::from_utf8(&catalogue_at(5)).unwrap().to_string();
        for i in 0..3 {
            store_cache(&tmp.0, &location(), &raw, i, i, None).unwrap();
        }
        let names: Vec<String> = std::fs::read_dir(&tmp.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let expected = cache_path(&tmp.0, &location());
        assert_eq!(
            names,
            vec![expected.file_name().unwrap().to_string_lossy().into_owned()]
        );

        // An unwritable destination fails cleanly and keeps the old cache.
        let blocked = tmp.0.join("blocked");
        std::fs::create_dir_all(cache_path(&blocked, &location())).unwrap();
        assert!(store_cache(&blocked, &location(), &raw, 1, 1, None).is_err());
        let left: Vec<_> = std::fs::read_dir(&blocked).unwrap().collect();
        assert_eq!(left.len(), 1, "temporary file removed");

        // Refresh still shows the new catalogue when the cache can't be written.
        let mock = Mock::new(vec![body(catalogue_at(9), None)]);
        let (view, outcome) = refresh(&mock, &blocked, &location(), 3);
        assert!(matches!(outcome, RefreshOutcome::UpdatedNotSaved(_)));
        assert_eq!(view.catalogue.unwrap().generated_at, Some(9));
    }

    #[test]
    fn caches_are_bound_to_their_source() {
        let tmp = TempDir::new("mk_isolation");
        let main = location();
        let dev = CatalogueLocation::new("patchwork-nodes", "catalogue", "dev").unwrap();
        let other = CatalogueLocation::new("someone", "else", "main").unwrap();
        assert_ne!(cache_path(&tmp.0, &main), cache_path(&tmp.0, &dev));
        assert_ne!(cache_path(&tmp.0, &main), cache_path(&tmp.0, &other));

        let mock = Mock::new(vec![
            body(catalogue_at(100), None),
            body(catalogue_at(50), None),
        ]);
        assert_eq!(refresh(&mock, &tmp.0, &main, 1).1, RefreshOutcome::Updated);
        // Switching source shows nothing from the previous one.
        assert!(open_cached(&tmp.0, &dev).catalogue.is_none());
        assert!(open_cached(&tmp.0, &other).catalogue.is_none());
        // Each source has its own history: an older revision on another
        // branch is not a rollback of this one, and does not touch it.
        assert_eq!(refresh(&mock, &tmp.0, &dev, 2).1, RefreshOutcome::Updated);
        assert_eq!(
            open_cached(&tmp.0, &main).catalogue.unwrap().generated_at,
            Some(100)
        );
        assert_eq!(
            open_cached(&tmp.0, &dev).catalogue.unwrap().generated_at,
            Some(50)
        );

        // A cache file moved into another source's slot is refused.
        std::fs::copy(cache_path(&tmp.0, &main), cache_path(&tmp.0, &other)).unwrap();
        assert!(matches!(load_cache(&tmp.0, &other), CacheLoad::OtherSource));
        assert!(open_cached(&tmp.0, &other).catalogue.is_none());
        // Owner and repository are compared exactly.
        let renamed = CatalogueLocation::new("Patchwork-Nodes", "catalogue", "main").unwrap();
        std::fs::copy(cache_path(&tmp.0, &main), cache_path(&tmp.0, &renamed)).unwrap();
        assert!(matches!(
            load_cache(&tmp.0, &renamed),
            CacheLoad::OtherSource
        ));
    }
}
