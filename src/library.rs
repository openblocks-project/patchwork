//! Shared custom-node Library.
//!
//! Layout (P4.0):
//!
//! ```text
//! ~/.patchwork/library/          Library root
//!     nodes/                     flat .pwnode files (current Library)
//!     packages/<id>/<version>/   reserved for versioned packages (P4.1+)
//! ```
//!
//! The previous Windows-only location (`%APPDATA%\Patchwork\library\nodes`)
//! is migrated by *copying* into `nodes/`; originals are never modified or
//! deleted, so projects that still reference the old absolute paths keep
//! working.

pub mod bundle;
pub mod index;
pub mod marketplace;
pub mod package;
pub mod project;
// P4.3.2B: obvious secrets are refused when publishing or installing.
pub mod resolve;
pub mod secrets;
pub mod trust;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Sub-folder of the Library root that holds flat `.pwnode` files.
const NODES_DIR: &str = "nodes";
/// Upper bound for conflict-rename attempts during migration.
const MAX_CONFLICT_SUFFIX: usize = 100;

// ── Paths ────────────────────────────────────────────────────────────────

/// Library root for a given home directory: `<home>/.patchwork/library`.
/// Pure (no filesystem access) so it can be tested on every platform.
pub fn library_root_for_home(home: Option<PathBuf>) -> Result<PathBuf, String> {
    let home = home
        .ok_or("Cannot locate your home directory, so the custom-node Library is unavailable.")?;
    if home.as_os_str().is_empty() {
        return Err("Home directory path is empty; the custom-node Library is unavailable.".into());
    }
    Ok(home.join(".patchwork").join("library"))
}

/// `~/.patchwork/library` for the current user.
pub fn library_root() -> Result<PathBuf, String> {
    library_root_for_home(dirs::home_dir())
}

/// Folder holding the Library's `.pwnode` files (no filesystem access).
pub fn nodes_dir_in(root: &Path) -> PathBuf {
    root.join(NODES_DIR)
}

/// Folder holding the current user's Library `.pwnode` files.
/// Does not create it; see [`ensure_dir`].
pub fn library_dir() -> Result<PathBuf, String> {
    Ok(nodes_dir_in(&library_root()?))
}

/// Create `dir` (and parents) if needed, with a readable error.
pub fn ensure_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("Cannot create the Library folder {}: {e}", dir.display()))
}

/// Pre-P4 Windows Library folder, if this platform had one.
pub fn legacy_library_dir() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("Patchwork")
            .join("library")
            .join("nodes"),
    )
}

fn is_pwnode(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pwnode"))
        .unwrap_or(false)
}

// ── Listing ──────────────────────────────────────────────────────────────

/// `.pwnode` files directly inside `dir`, sorted. A missing folder is an
/// empty Library, not an error. Unrelated files and sub-folders are ignored.
pub fn list_nodes_in(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read Library folder {}: {e}", dir.display()))?;
    let mut nodes = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_file() && is_pwnode(&path) {
            nodes.push(path);
        }
    }
    nodes.sort();
    Ok(nodes)
}

/// `.pwnode` files in the current user's Library.
#[allow(dead_code)]
pub fn list_library_nodes() -> Result<Vec<PathBuf>, String> {
    list_nodes_in(&library_dir()?)
}

// ── Exclusive copy ───────────────────────────────────────────────────────

/// Copy `reader` into a *new* file at `destination`. Never overwrites; if
/// the copy fails part-way the partial destination file is removed.
pub fn copy_exclusive_from<R: Read>(reader: &mut R, destination: &Path) -> Result<(), String> {
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("{} already exists.", destination.display())
            } else {
                format!("Failed to create {}: {e}", destination.display())
            }
        })?;
    let result = std::io::copy(reader, &mut output)
        .and_then(|_| output.flush())
        .and_then(|_| output.sync_all())
        .map_err(|e| format!("Failed to copy into {}: {e}", destination.display()));
    if result.is_err() {
        drop(output);
        let _ = std::fs::remove_file(destination);
    }
    result
}

fn copy_file_exclusive(source: &Path, destination: &Path) -> Result<(), String> {
    let mut input = std::fs::File::open(source)
        .map_err(|e| format!("Failed to read {}: {e}", source.display()))?;
    copy_exclusive_from(&mut input, destination)
}

fn same_contents(a: &Path, b: &Path) -> Result<bool, String> {
    let left = std::fs::read(a).map_err(|e| format!("Failed to read {}: {e}", a.display()))?;
    let right = std::fs::read(b).map_err(|e| format!("Failed to read {}: {e}", b.display()))?;
    Ok(left == right)
}

// ── Publishing ───────────────────────────────────────────────────────────

/// Why publishing failed. `Conflict` is separate so the UI can explain it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishError {
    Conflict(String),
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Conflict(name) => write!(
                f,
                "A Library node named '{name}' already exists. \
                 Rename the custom node (Save As) and publish again; \
                 the existing Library node was not changed."
            ),
            PublishError::Invalid(msg) | PublishError::Io(msg) => f.write_str(msg),
        }
    }
}

/// Publish `source` into `library_nodes_dir`. Refuses to overwrite.
pub fn publish_to(source: &Path, library_nodes_dir: &Path) -> Result<PathBuf, PublishError> {
    if !source.is_file() {
        return Err(PublishError::Invalid(
            "Custom node file does not exist.".to_string(),
        ));
    }
    if !is_pwnode(source) {
        return Err(PublishError::Invalid(
            "Only .pwnode files can be published.".to_string(),
        ));
    }
    let file_name = source
        .file_name()
        .ok_or_else(|| PublishError::Invalid("Invalid custom node filename.".to_string()))?;
    ensure_dir(library_nodes_dir).map_err(PublishError::Io)?;
    let destination = library_nodes_dir.join(file_name);
    if destination.exists() {
        return Err(PublishError::Conflict(
            file_name.to_string_lossy().into_owned(),
        ));
    }
    let mut input = std::fs::File::open(source)
        .map_err(|e| PublishError::Io(format!("Failed to read custom node: {e}")))?;
    // `copy_exclusive_from` uses create_new, so even a racing writer is
    // never overwritten.
    copy_exclusive_from(&mut input, &destination).map_err(PublishError::Io)?;
    Ok(destination)
}

/// Publish a standalone custom node to the current user's Library.
pub fn publish_to_library(source: &Path) -> Result<PathBuf, PublishError> {
    let dir = library_dir().map_err(PublishError::Io)?;
    publish_to(source, &dir)
}

// ── Legacy migration ─────────────────────────────────────────────────────

/// Outcome of copying the legacy Library into the new one.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Files copied under their original name.
    pub copied: Vec<PathBuf>,
    /// Name conflicts with different contents: (legacy file, renamed copy).
    pub renamed: Vec<(PathBuf, PathBuf)>,
    /// Already present with identical contents (repeat migration).
    pub already_present: Vec<PathBuf>,
    /// Files that could not be migrated: (legacy file, reason).
    pub failed: Vec<(PathBuf, String)>,
}

impl MigrationReport {
    pub fn changed_anything(&self) -> bool {
        !self.copied.is_empty() || !self.renamed.is_empty()
    }

    pub fn has_problems(&self) -> bool {
        !self.failed.is_empty()
    }

    /// One-line user-facing summary, or `None` when there's nothing to say.
    pub fn summary(&self) -> Option<String> {
        if !self.changed_anything() && !self.has_problems() {
            return None;
        }
        let mut parts = vec![format!(
            "Library moved to ~/.patchwork/library: {} node(s) copied",
            self.copied.len() + self.renamed.len()
        )];
        if !self.renamed.is_empty() {
            parts.push(format!(
                "{} kept under a '-legacy' name because a different file already existed",
                self.renamed.len()
            ));
        }
        if !self.failed.is_empty() {
            parts.push(format!(
                "{} could not be copied (see System Log)",
                self.failed.len()
            ));
        }
        parts.push("originals were left in place".to_string());
        Some(parts.join("; ") + ".")
    }
}

fn conflict_candidate(dest_dir: &Path, legacy: &Path, attempt: usize) -> Option<PathBuf> {
    let name = legacy.file_name()?;
    if attempt == 0 {
        return Some(dest_dir.join(name));
    }
    let stem = legacy.file_stem()?.to_string_lossy();
    let suffix = if attempt == 1 {
        "-legacy".to_string()
    } else {
        format!("-legacy-{attempt}")
    };
    Some(dest_dir.join(format!("{stem}{suffix}.pwnode")))
}

/// Copy every `.pwnode` from `legacy_dir` into `dest_dir`.
///
/// * Never deletes or modifies anything in `legacy_dir`.
/// * Never overwrites a file in `dest_dir`.
/// * Idempotent: a file whose identical copy already exists (under its own
///   name or a `-legacy` name) is not copied again.
/// * A failure on one file is recorded and the rest continue.
pub fn migrate_legacy_library(legacy_dir: &Path, dest_dir: &Path) -> MigrationReport {
    let mut report = MigrationReport::default();
    let legacy_nodes = match list_nodes_in(legacy_dir) {
        Ok(nodes) => nodes,
        Err(error) => {
            report.failed.push((legacy_dir.to_path_buf(), error));
            return report;
        }
    };
    if legacy_nodes.is_empty() {
        return report;
    }
    if same_folder(legacy_dir, dest_dir) {
        return report;
    }
    if let Err(error) = ensure_dir(dest_dir) {
        for legacy in legacy_nodes {
            report.failed.push((legacy, error.clone()));
        }
        return report;
    }

    'files: for legacy in legacy_nodes {
        for attempt in 0..MAX_CONFLICT_SUFFIX {
            let Some(candidate) = conflict_candidate(dest_dir, &legacy, attempt) else {
                report
                    .failed
                    .push((legacy.clone(), "Invalid file name.".to_string()));
                continue 'files;
            };
            if candidate.is_dir() {
                report.failed.push((
                    legacy.clone(),
                    format!("{} is a folder.", candidate.display()),
                ));
                continue 'files;
            }
            if candidate.exists() {
                match same_contents(&legacy, &candidate) {
                    Ok(true) => {
                        report.already_present.push(legacy.clone());
                        continue 'files;
                    }
                    Ok(false) => continue,
                    Err(error) => {
                        report.failed.push((legacy.clone(), error));
                        continue 'files;
                    }
                }
            }
            match copy_file_exclusive(&legacy, &candidate) {
                Ok(()) if attempt == 0 => report.copied.push(candidate),
                Ok(()) => report.renamed.push((legacy.clone(), candidate)),
                Err(error) => report.failed.push((legacy.clone(), error)),
            }
            continue 'files;
        }
        report.failed.push((
            legacy.clone(),
            "Too many conflicting copies already exist.".to_string(),
        ));
    }
    report
}

fn same_folder(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Write a migration report to the System Log.
pub fn log_migration_report(report: &MigrationReport) {
    for path in &report.copied {
        crate::system_log::log(format!("Library migration: copied {}", path.display()));
    }
    for (legacy, renamed) in &report.renamed {
        crate::system_log::warn(format!(
            "Library migration: {} conflicts with an existing Library node; kept as {}",
            legacy.display(),
            renamed.display()
        ));
    }
    for (legacy, reason) in &report.failed {
        crate::system_log::error(format!(
            "Library migration: could not copy {}: {reason}",
            legacy.display()
        ));
    }
}

// ── Cache ────────────────────────────────────────────────────────────────

/// Cached Library listing for the node palette, so the filesystem is not
/// scanned every frame. Reloaded on first use, after publishing, and when
/// the user presses refresh.
#[derive(Debug, Default)]
pub struct LibraryCache {
    /// Library nodes folder override (tests); `None` = the user's Library.
    dir: Option<PathBuf>,
    /// Legacy folder to migrate from on first load.
    legacy_dir: Option<PathBuf>,
    use_default_legacy: bool,
    loaded: bool,
    migrated: bool,
    entries: Vec<PathBuf>,
    error: Option<String>,
    /// P4.1: versioned packages under `<root>/packages`.
    packages: Vec<index::InstalledPackage>,
    package_problems: Vec<(PathBuf, String)>,
}

impl LibraryCache {
    /// Cache over the current user's Library (migrates the legacy Windows
    /// Library on first load).
    pub fn new() -> Self {
        Self {
            use_default_legacy: true,
            ..Self::default()
        }
    }

    /// Cache over an explicit folder (tests / future Library roots).
    #[allow(dead_code)]
    pub fn with_dirs(dir: PathBuf, legacy_dir: Option<PathBuf>) -> Self {
        Self {
            dir: Some(dir),
            legacy_dir,
            ..Self::default()
        }
    }

    fn nodes_dir(&self) -> Result<PathBuf, String> {
        match &self.dir {
            Some(dir) => Ok(dir.clone()),
            None => library_dir(),
        }
    }

    /// Library root (the parent of the nodes folder).
    pub fn root(&self) -> Result<PathBuf, String> {
        match &self.dir {
            Some(dir) => Ok(dir.parent().map(Path::to_path_buf).unwrap_or_default()),
            None => library_root(),
        }
    }

    /// Mark the listing stale; the next [`Self::ensure_loaded`] rescans.
    pub fn invalidate(&mut self) {
        self.loaded = false;
    }

    /// Load the listing if it is stale. Returns the migration report the
    /// first time a legacy Library was found (once per session).
    pub fn ensure_loaded(&mut self) -> Option<MigrationReport> {
        if self.loaded {
            return None;
        }
        self.loaded = true;
        let dir = match self.nodes_dir() {
            Ok(dir) => dir,
            Err(error) => {
                self.entries.clear();
                self.packages.clear();
                self.package_problems.clear();
                self.error = Some(error);
                return None;
            }
        };

        let mut migration = None;
        if !self.migrated {
            self.migrated = true;
            let legacy = if self.use_default_legacy {
                legacy_library_dir()
            } else {
                self.legacy_dir.clone()
            };
            if let Some(legacy) = legacy.filter(|l| l.is_dir()) {
                let report = migrate_legacy_library(&legacy, &dir);
                log_migration_report(&report);
                migration = Some(report);
            }
        }

        match list_nodes_in(&dir) {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(error) => {
                self.entries.clear();
                self.error = Some(error);
            }
        }

        // P4.1: versioned packages; index.json is rewritten from manifests.
        if let Ok(root) = self.root() {
            let scan = index::scan_packages(&root);
            for (dir, problem) in &scan.problems {
                crate::system_log::warn(format!(
                    "Library package {} skipped: {problem}",
                    dir.display()
                ));
            }
            if !scan.packages.is_empty() || package::packages_dir(&root).is_dir() {
                if let Err(error) = index::write_index(&root, &index::build_index(&scan)) {
                    crate::system_log::warn(format!("Library index not updated: {error}"));
                }
            }
            self.packages = scan.packages;
            self.package_problems = scan.problems;
        }
        migration
    }

    /// Rescan now (the palette uses [`Self::invalidate`] and rescans on
    /// the next frame).
    #[allow(dead_code)]
    pub fn refresh(&mut self) -> Option<MigrationReport> {
        self.invalidate();
        self.ensure_loaded()
    }

    /// Cached entries (may include files deleted since the last scan;
    /// callers must check before use).
    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    /// Last listing error, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Cached versioned packages, sorted by name.
    pub fn packages(&self) -> &[index::InstalledPackage] {
        &self.packages
    }

    /// Package folders that could not be read on the last scan.
    #[allow(dead_code)]
    pub fn package_problems(&self) -> &[(PathBuf, String)] {
        &self.package_problems
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique temp folder per test; never the real user Library.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patchwork_library_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    // ── Storage ──────────────────────────────────────────────────────

    #[test]
    fn library_root_is_under_home_patchwork() {
        let home = PathBuf::from("some").join("home");
        let root = library_root_for_home(Some(home.clone())).unwrap();
        assert_eq!(root, home.join(".patchwork").join("library"));
        assert_eq!(nodes_dir_in(&root), root.join("nodes"));
    }

    #[test]
    fn missing_home_directory_is_a_clear_error() {
        let error = library_root_for_home(None).unwrap_err();
        assert!(error.contains("home directory"), "{error}");
        assert!(library_root_for_home(Some(PathBuf::new())).is_err());
    }

    #[test]
    fn ensure_dir_creates_nested_library_folder() {
        let tmp = TempDir::new("create");
        let nodes = nodes_dir_in(&tmp.path().join(".patchwork").join("library"));
        assert!(!nodes.exists());
        ensure_dir(&nodes).unwrap();
        assert!(nodes.is_dir());
        ensure_dir(&nodes).unwrap(); // repeatable
    }

    #[test]
    fn listing_returns_only_pwnode_files() {
        let tmp = TempDir::new("list");
        write(&tmp.path().join("b.pwnode"), "{}");
        write(&tmp.path().join("a.PWNODE"), "{}");
        write(&tmp.path().join("notes.txt"), "x");
        write(&tmp.path().join("project.pw"), "x");
        std::fs::create_dir(tmp.path().join("folder.pwnode")).unwrap();
        std::fs::create_dir(tmp.path().join("packages")).unwrap();
        let names: Vec<_> = list_nodes_in(tmp.path())
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.PWNODE", "b.pwnode"]);
    }

    #[test]
    fn listing_missing_folder_is_empty_and_does_not_create_it() {
        let tmp = TempDir::new("missing");
        let dir = tmp.path().join("nope");
        assert!(list_nodes_in(&dir).unwrap().is_empty());
        assert!(!dir.exists());
    }

    // ── Publishing ───────────────────────────────────────────────────

    #[test]
    fn publish_copies_file() {
        let tmp = TempDir::new("publish");
        let source = tmp.path().join("Mixer.pwnode");
        write(&source, "{\"name\":\"Mixer\"}");
        let library = tmp.path().join("lib").join("nodes");
        let dest = publish_to(&source, &library).unwrap();
        assert_eq!(dest, library.join("Mixer.pwnode"));
        assert_eq!(read(&dest), read(&source));
    }

    #[test]
    fn publish_rejects_missing_source() {
        let tmp = TempDir::new("publish_missing");
        let error = publish_to(&tmp.path().join("nope.pwnode"), tmp.path()).unwrap_err();
        assert!(matches!(error, PublishError::Invalid(_)));
    }

    #[test]
    fn publish_rejects_wrong_extension() {
        let tmp = TempDir::new("publish_ext");
        let source = tmp.path().join("thing.json");
        write(&source, "{}");
        let error = publish_to(&source, &tmp.path().join("lib")).unwrap_err();
        assert!(matches!(error, PublishError::Invalid(_)));
        assert!(!tmp.path().join("lib").join("thing.json").exists());
    }

    #[test]
    fn publish_reports_duplicate_and_keeps_existing() {
        let tmp = TempDir::new("publish_dup");
        let source = tmp.path().join("Mixer.pwnode");
        write(&source, "new");
        let library = tmp.path().join("lib");
        std::fs::create_dir_all(&library).unwrap();
        write(&library.join("Mixer.pwnode"), "existing");
        let error = publish_to(&source, &library).unwrap_err();
        assert_eq!(error, PublishError::Conflict("Mixer.pwnode".into()));
        assert!(error.to_string().contains("already exists"));
        assert_eq!(read(&library.join("Mixer.pwnode")), "existing");
    }

    struct FailingReader {
        sent: bool,
    }

    impl Read for FailingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.sent {
                self.sent = true;
                buf[..4].copy_from_slice(b"part");
                return Ok(4);
            }
            Err(std::io::Error::other("disk on fire"))
        }
    }

    #[test]
    fn failed_copy_removes_partial_destination() {
        let tmp = TempDir::new("partial");
        let dest = tmp.path().join("Broken.pwnode");
        let error = copy_exclusive_from(&mut FailingReader { sent: false }, &dest).unwrap_err();
        assert!(error.contains("disk on fire"), "{error}");
        assert!(!dest.exists());
    }

    #[test]
    fn exclusive_copy_never_overwrites() {
        let tmp = TempDir::new("exclusive");
        let dest = tmp.path().join("Keep.pwnode");
        write(&dest, "keep");
        let error = copy_exclusive_from(&mut "other".as_bytes(), &dest).unwrap_err();
        assert!(error.contains("already exists"));
        assert_eq!(read(&dest), "keep");
    }

    // ── Migration ────────────────────────────────────────────────────

    #[test]
    fn migration_copies_and_leaves_originals() {
        let tmp = TempDir::new("migrate");
        let legacy = tmp.path().join("AppData").join("nodes");
        let dest = tmp.path().join("new").join("nodes");
        std::fs::create_dir_all(&legacy).unwrap();
        write(&legacy.join("A.pwnode"), "alpha");
        write(&legacy.join("B.pwnode"), "beta");
        write(&legacy.join("readme.txt"), "ignored");

        let report = migrate_legacy_library(&legacy, &dest);
        assert_eq!(report.copied.len(), 2);
        assert!(report.failed.is_empty() && report.renamed.is_empty());
        assert_eq!(read(&dest.join("A.pwnode")), "alpha");
        assert_eq!(read(&dest.join("B.pwnode")), "beta");
        assert!(!dest.join("readme.txt").exists());
        // Originals untouched.
        assert_eq!(read(&legacy.join("A.pwnode")), "alpha");
        assert_eq!(read(&legacy.join("B.pwnode")), "beta");
        assert_eq!(read(&legacy.join("readme.txt")), "ignored");
        assert!(report.summary().unwrap().contains("2 node(s) copied"));
    }

    #[test]
    fn repeated_migration_does_not_duplicate() {
        let tmp = TempDir::new("migrate_repeat");
        let legacy = tmp.path().join("legacy");
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        write(&legacy.join("A.pwnode"), "alpha");
        write(&legacy.join("C.pwnode"), "mine");
        write(&dest.join("C.pwnode"), "theirs");

        let first = migrate_legacy_library(&legacy, &dest);
        assert_eq!(first.copied.len(), 1);
        assert_eq!(first.renamed.len(), 1);
        let second = migrate_legacy_library(&legacy, &dest);
        assert!(!second.changed_anything());
        assert_eq!(second.already_present.len(), 2);
        assert!(second.summary().is_none());
        assert_eq!(list_nodes_in(&dest).unwrap().len(), 3);
    }

    #[test]
    fn migration_conflict_keeps_both_files() {
        let tmp = TempDir::new("migrate_conflict");
        let legacy = tmp.path().join("legacy");
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        write(&legacy.join("Mix.pwnode"), "legacy version");
        write(&dest.join("Mix.pwnode"), "new version");
        write(&dest.join("Mix-legacy.pwnode"), "something else");

        let report = migrate_legacy_library(&legacy, &dest);
        assert_eq!(report.renamed.len(), 1);
        let (_, renamed) = &report.renamed[0];
        assert_eq!(renamed, &dest.join("Mix-legacy-2.pwnode"));
        assert_eq!(read(renamed), "legacy version");
        assert_eq!(read(&dest.join("Mix.pwnode")), "new version");
        assert_eq!(read(&dest.join("Mix-legacy.pwnode")), "something else");
        assert!(report.summary().unwrap().contains("-legacy"));
    }

    #[test]
    fn migration_failure_on_one_file_preserves_everything() {
        let tmp = TempDir::new("migrate_fail");
        let legacy = tmp.path().join("legacy");
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        write(&legacy.join("A.pwnode"), "alpha");
        write(&legacy.join("B.pwnode"), "beta");
        write(&legacy.join("C.pwnode"), "gamma");
        // A folder occupies B's destination name, so B cannot be copied.
        std::fs::create_dir(dest.join("B.pwnode")).unwrap();

        let report = migrate_legacy_library(&legacy, &dest);
        assert_eq!(report.copied.len(), 2);
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].0.ends_with("B.pwnode"));
        assert!(report.summary().unwrap().contains("could not be copied"));
        for (name, text) in [("A", "alpha"), ("B", "beta"), ("C", "gamma")] {
            assert_eq!(read(&legacy.join(format!("{name}.pwnode"))), text);
        }
        assert!(dest.join("B.pwnode").is_dir());
    }

    #[test]
    fn legacy_paths_remain_loadable_after_migration() {
        let tmp = TempDir::new("migrate_legacy_ref");
        let legacy = tmp.path().join("legacy");
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&legacy).unwrap();
        let file = crate::document::CustomNodeFile {
            version: 1,
            name: "Legacy".into(),
            graph: crate::graph::Graph::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        };
        let legacy_file = legacy.join("Legacy.pwnode");
        file.save(&legacy_file).unwrap();

        let report = migrate_legacy_library(&legacy, &dest);
        assert_eq!(report.copied.len(), 1);
        // A project still pointing at the old absolute path keeps working.
        assert!(crate::document::CustomNodeFile::load(&legacy_file).is_ok());
        assert!(crate::document::CustomNodeFile::load(&dest.join("Legacy.pwnode")).is_ok());
    }

    #[test]
    fn missing_legacy_folder_is_a_no_op() {
        let tmp = TempDir::new("migrate_none");
        let report = migrate_legacy_library(&tmp.path().join("nope"), &tmp.path().join("dest"));
        assert_eq!(report, MigrationReport::default());
        assert!(!tmp.path().join("dest").exists());
    }

    // ── Cache ────────────────────────────────────────────────────────

    #[test]
    fn cache_loads_once_and_refreshes_explicitly() {
        let tmp = TempDir::new("cache");
        let dir = tmp.path().join("nodes");
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir.join("A.pwnode"), "a");
        let mut cache = LibraryCache::with_dirs(dir.clone(), None);
        assert!(cache.entries().is_empty());
        cache.ensure_loaded();
        assert_eq!(cache.entries().len(), 1);

        // New file is not seen until a refresh: no per-frame scanning.
        write(&dir.join("B.pwnode"), "b");
        cache.ensure_loaded();
        assert_eq!(cache.entries().len(), 1);
        cache.refresh();
        assert_eq!(cache.entries().len(), 2);
    }

    #[test]
    fn publish_then_invalidate_shows_new_entry() {
        let tmp = TempDir::new("cache_publish");
        let dir = tmp.path().join("nodes");
        let mut cache = LibraryCache::with_dirs(dir.clone(), None);
        cache.ensure_loaded();
        assert!(cache.entries().is_empty());
        assert!(cache.error().is_none());

        let source = tmp.path().join("New.pwnode");
        write(&source, "{}");
        publish_to(&source, &dir).unwrap();
        cache.invalidate();
        cache.ensure_loaded();
        assert_eq!(cache.entries(), &[dir.join("New.pwnode")]);
    }

    #[test]
    fn deleted_entries_are_harmless_and_dropped_on_refresh() {
        let tmp = TempDir::new("cache_delete");
        let dir = tmp.path().join("nodes");
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir.join("Gone.pwnode"), "x");
        let mut cache = LibraryCache::with_dirs(dir.clone(), None);
        cache.ensure_loaded();
        std::fs::remove_file(dir.join("Gone.pwnode")).unwrap();
        // Stale entry is still listed but simply no longer a file.
        assert!(!cache.entries()[0].is_file());
        cache.refresh();
        assert!(cache.entries().is_empty());

        // Even the whole folder disappearing is not an error.
        std::fs::remove_dir_all(&dir).unwrap();
        cache.refresh();
        assert!(cache.entries().is_empty());
        assert!(cache.error().is_none());
    }

    #[test]
    fn cache_lists_packages_and_unversioned_nodes_separately() {
        let tmp = TempDir::new("cache_packages");
        let root = tmp.path().join("library");
        let nodes = nodes_dir_in(&root);
        std::fs::create_dir_all(&nodes).unwrap();
        package::tests::write_node(&nodes.join("Flat.pwnode"), "Flat", None);
        let mut cache = LibraryCache::with_dirs(nodes.clone(), None);
        cache.ensure_loaded();
        assert_eq!(cache.entries().len(), 1);
        assert!(cache.packages().is_empty());

        // Convert (copy-only): the unversioned original stays.
        let req = package::tests::request("Flat", "1.0.0");
        let published = package::publish_package(&root, &nodes.join("Flat.pwnode"), &req).unwrap();
        cache.ensure_loaded();
        assert!(cache.packages().is_empty(), "no rescan until invalidated");
        cache.invalidate();
        cache.ensure_loaded();
        assert_eq!(cache.packages().len(), 1);
        assert_eq!(cache.entries().len(), 1);
        assert!(nodes.join("Flat.pwnode").is_file());
        assert_eq!(cache.root().unwrap(), root);
        // index.json written from manifests.
        let index = index::read_index(&root).unwrap();
        assert_eq!(index.packages, vec![published.manifest]);
        // Converting again is refused (same derived id).
        assert!(package::publish_package(&root, &nodes.join("Flat.pwnode"), &req).is_err());

        // Deleted version folder: harmless, gone after refresh.
        std::fs::remove_dir_all(published.dir).unwrap();
        assert!(!cache.packages()[0].latest().entry_path.is_file());
        cache.refresh();
        assert!(cache.packages().is_empty());
    }

    #[test]
    fn cache_migrates_legacy_once() {
        let tmp = TempDir::new("cache_migrate");
        let legacy = tmp.path().join("legacy");
        let dir = tmp.path().join("nodes");
        std::fs::create_dir_all(&legacy).unwrap();
        write(&legacy.join("Old.pwnode"), "old");
        let mut cache = LibraryCache::with_dirs(dir.clone(), Some(legacy.clone()));
        let report = cache.ensure_loaded().expect("migration report");
        assert_eq!(report.copied.len(), 1);
        assert_eq!(cache.entries(), &[dir.join("Old.pwnode")]);
        assert!(cache.refresh().is_none(), "migration runs once per session");
        assert!(legacy.join("Old.pwnode").is_file());
    }
}
