//! P4.2 Stage 1: package resolution.
//!
//! Resolves an exact `id@version/entry` package reference to a verified
//! `.pwnode` file. Independent of egui and the editor so the standalone
//! Player can reuse it.
//!
//! Rules:
//! * Exact match only: a request for `1.0.0` never resolves to another
//!   version, even if one is installed.
//! * A project-local copy (`<project>/packages/<id>/<version>/`) has
//!   priority over the global Library (`<library>/packages/<id>/<version>/`).
//!   Only a project copy that is *absent* lets the Library be used. A
//!   project copy that is present but damaged, incompatible or unsafe stops
//!   resolution with that exact error; it never silently falls back. An
//!   explicit repair/fallback can be built on [`PackageResolver::resolve_in`].
//! * Every candidate is checked for traversal and symlink escapes, manifest
//!   validity and compatibility, identity, checksum, and contained nested
//!   references before it is accepted.
//! * Plain file references that are not inside a package are returned
//!   unchanged, so existing absolute-path projects keep working.
//!
//! # Serialization rules for P4.2 Stage 2 (agreed before implementation)
//!
//! 1. For packaged nodes the package reference (`id@version/entry`) is
//!    authoritative; it identifies what the instance is.
//! 2. The instance `file` field stays, for backward compatibility with
//!    builds that do not understand package references.
//! 3. If `package` and `file` disagree (the file is not that package's
//!    entry, or is another version), loading reports the conflict instead
//!    of silently choosing one.
//! 4. Legacy instances without `package` keep working exactly as before
//!    (resolved by `file` only; see [`PackageResolver::resolve_file`]).
//! 5. A package that cannot be resolved keeps the node instance, its ports
//!    and its connections; it only reports an error.
//! 6. Opening a project never rewrites its file; references are only
//!    written by an explicit save.
//! 7. Package references are portable between Windows, macOS and Linux:
//!    they contain only an id, a stable version and a plain file name,
//!    never an absolute path or a path separator.
//! 8. Package identity (id, version, entry) and integrity (manifest,
//!    checksum, containment) are checked separately from file location; a
//!    file's location never decides which package or version it is.

// Not wired into the editor yet: Stage 2 (serialization) and the Player
// are the first callers. Until then only the tests use it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use super::package::{
    self, MANIFEST_FILE, MANIFEST_FORMAT, PACKAGES_DIR, PackageManifest, PackageRef,
};

/// Where a package was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageOrigin {
    /// `<project>/packages/...`
    Project,
    /// `<library root>/packages/...`
    Library,
    /// A package folder named directly by a file reference.
    Direct,
}

impl std::fmt::Display for PackageOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PackageOrigin::Project => "project",
            PackageOrigin::Library => "Library",
            PackageOrigin::Direct => "file location",
        })
    }
}

/// Category of a resolution problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResolveErrorKind {
    /// No candidate folder exists.
    Missing,
    /// A candidate exists but its contents are damaged (manifest, checksum,
    /// missing entry or nested file, unreadable custom node).
    Corrupted,
    /// A candidate is intact but cannot be used by this Patchwork
    /// (manifest format, custom node format, minimum app version).
    Incompatible,
    /// A reference or candidate would read outside its package folder.
    Unsafe,
}

/// A problem with one candidate location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateProblem {
    pub origin: PackageOrigin,
    pub dir: PathBuf,
    pub kind: ResolveErrorKind,
    pub message: String,
}

/// Why a reference could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    /// `id@version/entry` as requested.
    pub reference: String,
    /// The problem that decides the outcome (first existing candidate in
    /// priority order, or `Missing`).
    pub kind: ResolveErrorKind,
    /// Every candidate that existed but failed, in priority order.
    pub problems: Vec<CandidateProblem>,
    /// Every folder that was looked at.
    pub searched: Vec<PathBuf>,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.problems.first() {
            None => write!(
                f,
                "Package {} is not installed in this project or the Library.",
                self.reference
            ),
            Some(problem) => {
                let what = match problem.kind {
                    ResolveErrorKind::Missing => "is missing",
                    ResolveErrorKind::Corrupted => "is damaged",
                    ResolveErrorKind::Incompatible => "is not compatible",
                    ResolveErrorKind::Unsafe => "is unsafe and was not used",
                };
                write!(
                    f,
                    "Package {} ({} copy) {what}: {}",
                    self.reference, problem.origin, problem.message
                )?;
                if problem.origin == PackageOrigin::Project {
                    write!(
                        f,
                        " The project copy takes priority, so no other copy was used."
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// A verified package entry.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub reference: PackageRef,
    pub origin: PackageOrigin,
    /// Version folder (as found, not canonicalised).
    pub dir: PathBuf,
    /// The `.pwnode` to load.
    pub entry_path: PathBuf,
    pub manifest: PackageManifest,
}

/// Result of resolving a plain file reference.
#[derive(Debug, Clone)]
pub enum ResolvedFile {
    /// The file is the entry of a package version; it was verified in place.
    Package(Box<ResolvedPackage>),
    /// Not inside a package: returned unchanged and not verified (legacy and
    /// local custom nodes). The file may not exist; callers report that.
    Plain(PathBuf),
}

/// Fingerprint of the files a verification depended on.
type Fingerprint = Vec<(PathBuf, Option<(std::time::SystemTime, u64)>)>;

fn fingerprint(paths: &[PathBuf]) -> Fingerprint {
    paths
        .iter()
        .map(|p| {
            let stamp = std::fs::metadata(p)
                .ok()
                .and_then(|m| m.modified().ok().map(|t| (t, m.len())));
            (p.clone(), stamp)
        })
        .collect()
}

type CandidateResult = Result<(PackageManifest, PathBuf), CandidateProblem>;

/// Resolves package references against a project folder and the Library.
#[derive(Debug, Default)]
pub struct PackageResolver {
    project_dir: Option<PathBuf>,
    library_root: Option<PathBuf>,
    /// Verification results per version folder, reused while none of the
    /// files they depended on changed.
    cache: HashMap<PathBuf, (Fingerprint, CandidateResult)>,
}

impl PackageResolver {
    /// `project_dir`: folder holding `project.json` (its `packages/` is
    /// searched first). `library_root`: e.g. `~/.patchwork/library`.
    pub fn new(project_dir: Option<PathBuf>, library_root: Option<PathBuf>) -> Self {
        Self {
            project_dir,
            library_root,
            cache: HashMap::new(),
        }
    }

    pub fn project_dir(&self) -> Option<&Path> {
        self.project_dir.as_deref()
    }

    pub fn library_root(&self) -> Option<&Path> {
        self.library_root.as_deref()
    }

    /// Forget cached verification results.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    fn candidates(&self, reference: &PackageRef) -> Vec<(PackageOrigin, PathBuf, PathBuf)> {
        let mut out = Vec::new();
        if let Some(project) = &self.project_dir {
            out.push((
                PackageOrigin::Project,
                project.join(PACKAGES_DIR),
                package::version_dir(project, &reference.id, &reference.version),
            ));
        }
        if let Some(library) = &self.library_root {
            out.push((
                PackageOrigin::Library,
                library.join(PACKAGES_DIR),
                package::version_dir(library, &reference.id, &reference.version),
            ));
        }
        out
    }

    /// Resolve an exact package reference.
    pub fn resolve(&mut self, reference: &PackageRef) -> Result<ResolvedPackage, ResolveError> {
        let label = reference.to_string();
        // The reference itself must not be able to name a path outside its
        // package folder (fields are public, so re-check).
        unsafe_reference_error(reference)?;

        let candidates = self.candidates(reference);
        self.resolve_candidates(reference, label, candidates)
    }

    /// Resolve from one location only (no priority, no fallback). This is
    /// the building block for an explicit, user-confirmed repair such as
    /// "use the Library copy instead of the damaged project copy".
    pub fn resolve_in(
        &mut self,
        reference: &PackageRef,
        origin: PackageOrigin,
    ) -> Result<ResolvedPackage, ResolveError> {
        let label = reference.to_string();
        if let Err(error) = unsafe_reference_error(reference) {
            return Err(error);
        }
        let candidates = self
            .candidates(reference)
            .into_iter()
            .filter(|(o, _, _)| *o == origin)
            .collect();
        self.resolve_candidates(reference, label, candidates)
    }

    /// The first candidate that is *present* decides the outcome: it is
    /// either used or reported. Absent candidates are skipped.
    fn resolve_candidates(
        &mut self,
        reference: &PackageRef,
        label: String,
        candidates: Vec<(PackageOrigin, PathBuf, PathBuf)>,
    ) -> Result<ResolvedPackage, ResolveError> {
        let mut searched = Vec::new();
        for (origin, packages_root, dir) in candidates {
            searched.push(dir.clone());
            if std::fs::symlink_metadata(&dir).is_err() {
                continue; // absent here: a lower-priority location may be used
            }
            return match self.verify_cached(origin, &packages_root, &dir, Some(reference)) {
                Ok((manifest, entry_path)) => Ok(ResolvedPackage {
                    reference: reference.clone(),
                    origin,
                    dir,
                    entry_path,
                    manifest,
                }),
                // Present but unusable: report it; never fall back silently.
                Err(problem) => Err(ResolveError {
                    reference: label,
                    kind: problem.kind,
                    problems: vec![problem],
                    searched,
                }),
            };
        }
        Err(ResolveError {
            reference: label,
            kind: ResolveErrorKind::Missing,
            problems: Vec::new(),
            searched,
        })
    }

    /// Resolve a custom-node instance `file` value. A file that is the
    /// entry of a package version is verified where it is (its exact
    /// version, never relocated); any other file is returned unchanged.
    pub fn resolve_file(&mut self, file: &Path) -> Result<ResolvedFile, ResolveError> {
        let Some((reference, dir)) = package_ref_for_path(file) else {
            return Ok(ResolvedFile::Plain(file.to_path_buf()));
        };
        let packages_root = dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.clone());
        match self.verify_cached(
            PackageOrigin::Direct,
            &packages_root,
            &dir,
            Some(&reference),
        ) {
            Ok((manifest, entry_path)) => Ok(ResolvedFile::Package(Box::new(ResolvedPackage {
                reference,
                origin: PackageOrigin::Direct,
                dir,
                entry_path,
                manifest,
            }))),
            Err(problem) => Err(ResolveError {
                reference: reference.to_string(),
                kind: problem.kind,
                searched: vec![problem.dir.clone()],
                problems: vec![problem],
            }),
        }
    }

    fn verify_cached(
        &mut self,
        origin: PackageOrigin,
        packages_root: &Path,
        dir: &Path,
        expected: Option<&PackageRef>,
    ) -> CandidateResult {
        let mut deps = vec![dir.join(MANIFEST_FILE)];
        if let Some(reference) = expected {
            deps.push(dir.join(&reference.entry));
        }
        let print = fingerprint(&deps);
        if let Some((cached_print, result)) = self.cache.get(dir) {
            if *cached_print == print {
                return result.clone().map_err(|mut p| {
                    p.origin = origin;
                    p
                });
            }
        }
        let result = verify_candidate(origin, packages_root, dir, expected);
        self.cache
            .insert(dir.to_path_buf(), (print, result.clone()));
        result
    }
}

/// [`check_reference`] as a resolution error.
fn unsafe_reference_error(reference: &PackageRef) -> Result<(), ResolveError> {
    check_reference(reference).map_err(|message| ResolveError {
        reference: reference.to_string(),
        kind: ResolveErrorKind::Unsafe,
        problems: vec![CandidateProblem {
            origin: PackageOrigin::Direct,
            dir: PathBuf::new(),
            kind: ResolveErrorKind::Unsafe,
            message,
        }],
        searched: Vec::new(),
    })
}

/// Reject reference fields that could name a path outside a package folder.
fn check_reference(reference: &PackageRef) -> Result<(), String> {
    package::validate_package_id(&reference.id)?;
    package::validate_entry_name(&reference.entry)?;
    if !reference.version.pre.is_empty() || !reference.version.build.is_empty() {
        return Err(format!(
            "Version {} is not a stable MAJOR.MINOR.PATCH version.",
            reference.version
        ));
    }
    Ok(())
}

/// If `path` is the entry of a package version (a `package.json` in its
/// folder names it, and the folder layout is `<id>/<version>/`), return the
/// exact reference and the version folder. Never relocates the file.
pub fn package_ref_for_path(path: &Path) -> Option<(PackageRef, PathBuf)> {
    let dir = path.parent()?;
    let file_name = path.file_name()?.to_str()?;
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE)).ok()?;
    let manifest: PackageManifest = serde_json::from_str(&text).ok()?;
    if manifest.entry != file_name {
        return None;
    }
    let version = package::parse_stable_version(&manifest.version).ok()?;
    Some((
        PackageRef {
            id: manifest.id,
            version,
            entry: manifest.entry,
        },
        dir.to_path_buf(),
    ))
}

fn problem(
    origin: PackageOrigin,
    dir: &Path,
    kind: ResolveErrorKind,
    message: impl Into<String>,
) -> CandidateProblem {
    CandidateProblem {
        origin,
        dir: dir.to_path_buf(),
        kind,
        message: message.into(),
    }
}

/// True when `child` (canonical) is inside `root` (canonical).
fn contained(child: &Path, root: &Path) -> bool {
    child.starts_with(root)
}

/// Full verification of one version folder.
fn verify_candidate(
    origin: PackageOrigin,
    packages_root: &Path,
    dir: &Path,
    expected: Option<&PackageRef>,
) -> CandidateResult {
    use ResolveErrorKind::*;
    let fail = |kind, message: String| Err(problem(origin, dir, kind, message));

    // ── Location: no symlink escapes out of the packages folder ─────────
    let canonical_root = match std::fs::canonicalize(packages_root) {
        Ok(root) => root,
        Err(e) => return fail(Missing, format!("Cannot open packages folder: {e}")),
    };
    let canonical_dir = match std::fs::canonicalize(dir) {
        Ok(d) => d,
        Err(e) => return fail(Missing, format!("Cannot open package folder: {e}")),
    };
    if !contained(&canonical_dir, &canonical_root) {
        return fail(
            Unsafe,
            "The package folder links to a location outside the packages folder.".into(),
        );
    }
    if !canonical_dir.is_dir() {
        return fail(Corrupted, "The package location is not a folder.".into());
    }

    // ── Manifest ─────────────────────────────────────────────────────────
    let manifest_path = dir.join(MANIFEST_FILE);
    match std::fs::canonicalize(&manifest_path) {
        Ok(p) if !contained(&p, &canonical_dir) => {
            return fail(
                Unsafe,
                "package.json links outside the package folder.".into(),
            );
        }
        Ok(_) => {}
        Err(_) => return fail(Corrupted, "package.json is missing.".into()),
    }
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => return fail(Corrupted, format!("Cannot read package.json: {e}")),
    };
    let manifest: PackageManifest = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(e) => return fail(Corrupted, format!("package.json is not valid: {e}")),
    };

    // ── Compatibility (before structural checks: a newer format may have
    //    a different structure) ─────────────────────────────────────────
    if manifest.format != MANIFEST_FORMAT {
        return fail(
            Incompatible,
            format!(
                "Manifest format {} is not supported (this Patchwork reads format {MANIFEST_FORMAT}).",
                manifest.format
            ),
        );
    }
    if manifest.pwnode_version != 1 {
        return fail(
            Incompatible,
            format!(
                "Custom node format {} is not supported.",
                manifest.pwnode_version
            ),
        );
    }
    if let Some(min) = &manifest.min_app_version {
        match semver::Version::parse(min) {
            Ok(min) => {
                let app = app_version();
                if min > app {
                    return fail(
                        Incompatible,
                        format!("Requires Patchwork {min} or newer (this is {app})."),
                    );
                }
            }
            Err(e) => return fail(Corrupted, format!("Invalid min_app_version '{min}': {e}")),
        }
    }

    // ── Structure and identity ───────────────────────────────────────────
    // An unsafe entry name is a safety problem, not mere damage.
    if let Err(message) = package::validate_entry_name(&manifest.entry) {
        return fail(Unsafe, message);
    }
    let version = match manifest.validate() {
        Ok(v) => v,
        Err(message) => return fail(Corrupted, message),
    };
    let dir_version = dir.file_name().map(|n| n.to_string_lossy().into_owned());
    let dir_id = dir
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    if dir_version.as_deref() != Some(version.to_string().as_str())
        || dir_id.as_deref() != Some(manifest.id.as_str())
    {
        return fail(
            Corrupted,
            format!(
                "The folder does not match the package it contains ({}@{}).",
                manifest.id, version
            ),
        );
    }
    if let Some(expected) = expected {
        if manifest.id != expected.id
            || version != expected.version
            || manifest.entry != expected.entry
        {
            return fail(
                Corrupted,
                format!(
                    "The folder contains {}@{}/{} instead of {expected}.",
                    manifest.id, version, manifest.entry
                ),
            );
        }
    }

    // ── Entry: contained, present, checksum ─────────────────────────────
    let entry_path = dir.join(&manifest.entry);
    match std::fs::canonicalize(&entry_path) {
        Ok(p) if !contained(&p, &canonical_dir) => {
            return fail(
                Unsafe,
                format!("{} links outside the package folder.", manifest.entry),
            );
        }
        Ok(p) if !p.is_file() => {
            return fail(Corrupted, format!("{} is not a file.", manifest.entry));
        }
        Ok(_) => {}
        Err(_) => return fail(Corrupted, format!("{} is missing.", manifest.entry)),
    }
    match package::sha256_file(&entry_path) {
        Ok(actual) if actual.eq_ignore_ascii_case(&manifest.sha256) => {}
        Ok(_) => {
            return fail(
                Corrupted,
                format!(
                    "{} failed its checksum check (changed after publishing).",
                    manifest.entry
                ),
            );
        }
        Err(message) => return fail(Corrupted, message),
    }

    // ── Nested references: classify before loading ─────────────────────
    let raw: crate::document::CustomNodeFile = match std::fs::read_to_string(&entry_path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(file) => file,
        Err(e) => {
            return fail(
                Corrupted,
                format!("{} is not a valid custom node: {e}", manifest.entry),
            );
        }
    };
    if raw.version != 1 {
        return fail(
            Incompatible,
            format!("Custom node format {} is not supported.", raw.version),
        );
    }
    for node in raw.graph.nodes.values() {
        let crate::graph::NodeType::CustomNodeInstance { file, .. } = &node.node_type else {
            continue;
        };
        if let Err(problem) = check_nested_reference(origin, dir, &canonical_dir, file) {
            return Err(problem);
        }
    }
    // Final parse with the contained policy (same rules the loader uses).
    if let Err(message) = crate::document::CustomNodeFile::load_with_policy(
        &entry_path,
        &crate::document::DependencyPolicy::Contained {
            root: dir.to_path_buf(),
        },
    ) {
        return fail(Corrupted, message);
    }

    Ok((manifest, entry_path))
}

/// Classify one nested reference inside a package entry.
fn check_nested_reference(
    origin: PackageOrigin,
    dir: &Path,
    canonical_dir: &Path,
    reference: &str,
) -> Result<(), CandidateProblem> {
    use ResolveErrorKind::*;
    let child = Path::new(reference);
    if reference.trim().is_empty() {
        return Err(problem(
            origin,
            dir,
            Corrupted,
            "A nested custom node has no file.",
        ));
    }
    if child.is_absolute() {
        return Err(problem(
            origin,
            dir,
            Unsafe,
            format!("Nested custom node uses an absolute path ({reference})."),
        ));
    }
    for component in child.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => {
                return Err(problem(
                    origin,
                    dir,
                    Unsafe,
                    format!("Nested custom node path '{reference}' leaves the package folder."),
                ));
            }
        }
    }
    let joined = dir.join(child);
    match std::fs::canonicalize(&joined) {
        Ok(p) if !contained(&p, canonical_dir) => Err(problem(
            origin,
            dir,
            Unsafe,
            format!("Nested custom node '{reference}' links outside the package folder."),
        )),
        Ok(_) => Ok(()),
        Err(_) => Err(problem(
            origin,
            dir,
            Corrupted,
            format!("Nested custom node '{reference}' is missing."),
        )),
    }
}

/// This build's version, for `min_app_version` checks.
pub fn app_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::LOAD_COUNT;
    use crate::library::package::tests::{TempDir, write_node};

    fn reference(id: &str, version: &str, entry: &str) -> PackageRef {
        PackageRef {
            id: id.into(),
            version: semver::Version::parse(version).unwrap(),
            entry: entry.into(),
        }
    }

    /// Write a package version by hand (so tests can build any content,
    /// including unsafe nested references the publisher would refuse).
    fn make_package(root: &Path, id: &str, version: &str, nested: Option<&str>) -> PathBuf {
        let dir = root.join(PACKAGES_DIR).join(id).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("Node.pwnode");
        write_node(&entry, "Node", nested);
        let manifest = PackageManifest {
            format: MANIFEST_FORMAT,
            id: id.into(),
            name: "Node".into(),
            version: version.into(),
            description: String::new(),
            author: String::new(),
            tags: Vec::new(),
            entry: "Node.pwnode".into(),
            sha256: package::sha256_file(&entry).unwrap(),
            pwnode_version: 1,
            min_app_version: None,
            dependencies: Vec::new(),
            source: "local".into(),
            published_at: 0,
        };
        std::fs::write(
            dir.join(MANIFEST_FILE),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        dir
    }

    fn edit_manifest(dir: &Path, change: impl FnOnce(&mut serde_json::Value)) {
        let path = dir.join(MANIFEST_FILE);
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        change(&mut value);
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    }

    /// Re-hash the entry after deliberately editing it (so only the edit
    /// under test is wrong, not the checksum).
    fn reseal(dir: &Path) {
        let sha = package::sha256_file(&dir.join("Node.pwnode")).unwrap();
        edit_manifest(dir, |v| v["sha256"] = serde_json::Value::String(sha));
    }

    fn roots(tmp: &TempDir) -> (PathBuf, PathBuf) {
        let project = tmp.0.join("project");
        let library = tmp.0.join("library");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        (project, library)
    }

    fn kind_of(result: Result<ResolvedPackage, ResolveError>) -> ResolveErrorKind {
        result.expect_err("expected a resolution error").kind
    }

    // ── Resolution and priority ──────────────────────────────────────

    #[test]
    fn resolves_exact_library_package() {
        let tmp = TempDir::new("res_library");
        let (project, library) = roots(&tmp);
        let dir = make_package(&library, "local.node", "1.0.0", None);
        let mut resolver = PackageResolver::new(Some(project), Some(library));
        let resolved = resolver
            .resolve(&reference("local.node", "1.0.0", "Node.pwnode"))
            .unwrap();
        assert_eq!(resolved.origin, PackageOrigin::Library);
        assert_eq!(resolved.dir, dir);
        assert_eq!(resolved.entry_path, dir.join("Node.pwnode"));
    }

    #[test]
    fn valid_project_copy_wins_over_library() {
        let tmp = TempDir::new("res_priority");
        let (project, library) = roots(&tmp);
        let project_dir = make_package(&project, "local.node", "1.0.0", None);
        make_package(&library, "local.node", "1.0.0", None);
        let mut resolver = PackageResolver::new(Some(project), Some(library));
        let resolved = resolver
            .resolve(&reference("local.node", "1.0.0", "Node.pwnode"))
            .unwrap();
        assert_eq!(resolved.origin, PackageOrigin::Project);
        assert_eq!(resolved.dir, project_dir);
    }

    #[test]
    fn absent_project_copy_resolves_from_library() {
        let tmp = TempDir::new("res_absent_project");
        let (project, library) = roots(&tmp);
        // The project has other packages, just not this one.
        make_package(&project, "local.other", "1.0.0", None);
        let library_dir = make_package(&library, "local.node", "1.0.0", None);
        let resolved = PackageResolver::new(Some(project), Some(library))
            .resolve(&reference("local.node", "1.0.0", "Node.pwnode"))
            .unwrap();
        assert_eq!(resolved.origin, PackageOrigin::Library);
        assert_eq!(resolved.dir, library_dir);
    }

    /// A present project copy that fails must block the valid Library copy.
    fn assert_project_copy_blocks(
        tag: &str,
        damage: impl FnOnce(&Path),
        expected: ResolveErrorKind,
    ) {
        let tmp = TempDir::new(tag);
        let (project, library) = roots(&tmp);
        let project_dir = make_package(&project, "local.node", "1.0.0", None);
        damage(&project_dir);
        make_package(&library, "local.node", "1.0.0", None);
        let r = reference("local.node", "1.0.0", "Node.pwnode");
        let mut resolver = PackageResolver::new(Some(project), Some(library));

        let error = resolver.resolve(&r).unwrap_err();
        assert_eq!(error.kind, expected, "{error}");
        assert_eq!(error.problems.len(), 1);
        assert_eq!(error.problems[0].origin, PackageOrigin::Project);
        assert_eq!(error.problems[0].dir, project_dir);
        assert_eq!(error.searched, vec![project_dir], "Library not consulted");
        let text = error.to_string();
        assert!(text.contains("local.node@1.0.0/Node.pwnode"), "{text}");
        assert!(text.contains("no other copy was used"), "{text}");

        // An explicit, deliberate fallback remains possible.
        let explicit = resolver.resolve_in(&r, PackageOrigin::Library).unwrap();
        assert_eq!(explicit.origin, PackageOrigin::Library);
        assert_eq!(explicit.reference, r);
    }

    #[test]
    fn corrupted_project_copy_does_not_fall_back_to_library() {
        assert_project_copy_blocks(
            "res_block_corrupt",
            |dir| std::fs::write(dir.join("Node.pwnode"), "tampered").unwrap(),
            ResolveErrorKind::Corrupted,
        );
    }

    #[test]
    fn incompatible_project_copy_does_not_fall_back_to_library() {
        assert_project_copy_blocks(
            "res_block_incompatible",
            |dir| edit_manifest(dir, |v| v["min_app_version"] = "999.0.0".into()),
            ResolveErrorKind::Incompatible,
        );
    }

    #[test]
    fn unsafe_project_copy_does_not_fall_back_to_library() {
        assert_project_copy_blocks(
            "res_block_unsafe",
            |dir| edit_manifest(dir, |v| v["entry"] = "../Node.pwnode".into()),
            ResolveErrorKind::Unsafe,
        );
    }

    #[test]
    fn manifestless_project_copy_does_not_fall_back_to_library() {
        assert_project_copy_blocks(
            "res_block_manifest",
            |dir| std::fs::remove_file(dir.join(MANIFEST_FILE)).unwrap(),
            ResolveErrorKind::Corrupted,
        );
    }

    #[test]
    fn resolve_in_checks_only_the_named_location() {
        let tmp = TempDir::new("res_in");
        let (project, library) = roots(&tmp);
        make_package(&library, "local.node", "1.0.0", None);
        let r = reference("local.node", "1.0.0", "Node.pwnode");
        let mut resolver = PackageResolver::new(Some(project), Some(library));
        let error = resolver.resolve_in(&r, PackageOrigin::Project).unwrap_err();
        assert_eq!(error.kind, ResolveErrorKind::Missing);
        assert_eq!(
            resolver
                .resolve_in(&r, PackageOrigin::Library)
                .unwrap()
                .origin,
            PackageOrigin::Library
        );
        // Unsafe references are refused here too.
        let bad = reference("local.node", "1.0.0", "../x.pwnode");
        assert_eq!(
            resolver
                .resolve_in(&bad, PackageOrigin::Library)
                .unwrap_err()
                .kind,
            ResolveErrorKind::Unsafe
        );
    }

    #[test]
    fn project_copy_never_resolves_a_different_version() {
        let tmp = TempDir::new("res_block_version");
        let (project, library) = roots(&tmp);
        make_package(&project, "local.node", "2.0.0", None);
        let library_dir = make_package(&library, "local.node", "1.0.0", None);
        let resolved = PackageResolver::new(Some(project), Some(library))
            .resolve(&reference("local.node", "1.0.0", "Node.pwnode"))
            .unwrap();
        // Project has only 2.0.0, so 1.0.0 is absent there: Library 1.0.0.
        assert_eq!(resolved.dir, library_dir);
        assert_eq!(resolved.reference.version.to_string(), "1.0.0");
    }

    #[test]
    fn never_switches_to_another_version() {
        let tmp = TempDir::new("res_exact");
        let (project, library) = roots(&tmp);
        make_package(&library, "local.node", "1.1.0", None);
        make_package(&project, "local.node", "2.0.0", None);
        let mut resolver = PackageResolver::new(Some(project.clone()), Some(library.clone()));
        let error = resolver
            .resolve(&reference("local.node", "1.0.0", "Node.pwnode"))
            .unwrap_err();
        assert_eq!(error.kind, ResolveErrorKind::Missing);
        assert!(error.problems.is_empty());
        assert_eq!(error.searched.len(), 2);
        assert!(error.searched[0].starts_with(&project));
        assert!(error.searched[1].starts_with(&library));
        assert!(error.to_string().contains("not installed"), "{error}");
    }

    #[test]
    fn missing_everywhere_and_without_roots() {
        let tmp = TempDir::new("res_missing");
        let (project, library) = roots(&tmp);
        let mut resolver = PackageResolver::new(Some(project), Some(library));
        assert_eq!(
            kind_of(resolver.resolve(&reference("local.none", "1.0.0", "Node.pwnode"))),
            ResolveErrorKind::Missing
        );
        let mut empty = PackageResolver::new(None, None);
        let error = empty
            .resolve(&reference("local.none", "1.0.0", "Node.pwnode"))
            .unwrap_err();
        assert_eq!(error.kind, ResolveErrorKind::Missing);
        assert!(error.searched.is_empty());
    }

    #[test]
    fn wrong_entry_name_is_not_resolved() {
        let tmp = TempDir::new("res_entry");
        let (_, library) = roots(&tmp);
        make_package(&library, "local.node", "1.0.0", None);
        let mut resolver = PackageResolver::new(None, Some(library));
        assert_eq!(
            kind_of(resolver.resolve(&reference("local.node", "1.0.0", "Other.pwnode"))),
            ResolveErrorKind::Corrupted
        );
    }

    // ── Integrity and corrupted manifests ────────────────────────────

    #[test]
    fn checksum_mismatch_is_corrupted() {
        let tmp = TempDir::new("res_checksum");
        let (_, library) = roots(&tmp);
        let dir = make_package(&library, "local.node", "1.0.0", None);
        write_node(&dir.join("Node.pwnode"), "Changed", None);
        let mut resolver = PackageResolver::new(None, Some(library));
        let error = resolver
            .resolve(&reference("local.node", "1.0.0", "Node.pwnode"))
            .unwrap_err();
        assert_eq!(error.kind, ResolveErrorKind::Corrupted);
        assert!(error.to_string().contains("checksum"), "{error}");
    }

    #[test]
    fn corrupted_manifests_are_distinguished() {
        let tmp = TempDir::new("res_manifest");
        let (_, library) = roots(&tmp);
        let r = reference("local.node", "1.0.0", "Node.pwnode");

        let dir = make_package(&library, "local.node", "1.0.0", None);
        std::fs::write(dir.join(MANIFEST_FILE), "{ not json").unwrap();
        assert_eq!(
            kind_of(PackageResolver::new(None, Some(library.clone())).resolve(&r)),
            ResolveErrorKind::Corrupted
        );

        std::fs::remove_file(dir.join(MANIFEST_FILE)).unwrap();
        let error = PackageResolver::new(None, Some(library.clone()))
            .resolve(&r)
            .unwrap_err();
        assert_eq!(error.kind, ResolveErrorKind::Corrupted);
        assert!(error.to_string().contains("package.json"), "{error}");

        // Manifest describes a different version than its folder.
        let dir = make_package(&library, "local.other", "1.0.0", None);
        edit_manifest(&dir, |v| v["version"] = "1.0.1".into());
        assert_eq!(
            kind_of(
                PackageResolver::new(None, Some(library.clone())).resolve(&reference(
                    "local.other",
                    "1.0.0",
                    "Node.pwnode"
                ))
            ),
            ResolveErrorKind::Corrupted
        );

        // Entry file missing.
        let dir = make_package(&library, "local.gone", "1.0.0", None);
        std::fs::remove_file(dir.join("Node.pwnode")).unwrap();
        assert_eq!(
            kind_of(
                PackageResolver::new(None, Some(library.clone())).resolve(&reference(
                    "local.gone",
                    "1.0.0",
                    "Node.pwnode"
                ))
            ),
            ResolveErrorKind::Corrupted
        );

        // Entry present but not a custom node (checksum resealed).
        let dir = make_package(&library, "local.junk", "1.0.0", None);
        std::fs::write(dir.join("Node.pwnode"), "junk").unwrap();
        reseal(&dir);
        assert_eq!(
            kind_of(
                PackageResolver::new(None, Some(library.clone())).resolve(&reference(
                    "local.junk",
                    "1.0.0",
                    "Node.pwnode"
                ))
            ),
            ResolveErrorKind::Corrupted
        );

        // Malformed min_app_version.
        let dir = make_package(&library, "local.minbad", "1.0.0", None);
        edit_manifest(&dir, |v| v["min_app_version"] = "soon".into());
        assert_eq!(
            kind_of(
                PackageResolver::new(None, Some(library)).resolve(&reference(
                    "local.minbad",
                    "1.0.0",
                    "Node.pwnode"
                ))
            ),
            ResolveErrorKind::Corrupted
        );
    }

    #[test]
    fn incompatible_packages_are_distinguished() {
        let tmp = TempDir::new("res_incompatible");
        let (_, library) = roots(&tmp);
        let cases: [(&str, Box<dyn Fn(&mut serde_json::Value)>); 3] = [
            ("local.fmt", Box::new(|v| v["format"] = 2.into())),
            ("local.pw", Box::new(|v| v["pwnode_version"] = 2.into())),
            (
                "local.newer",
                Box::new(|v| v["min_app_version"] = "999.0.0".into()),
            ),
        ];
        for (id, change) in cases {
            let dir = make_package(&library, id, "1.0.0", None);
            edit_manifest(&dir, |v| change(v));
            let error = PackageResolver::new(None, Some(library.clone()))
                .resolve(&reference(id, "1.0.0", "Node.pwnode"))
                .unwrap_err();
            assert_eq!(error.kind, ResolveErrorKind::Incompatible, "{id}: {error}");
        }
        // A satisfied minimum version is fine.
        let dir = make_package(&library, "local.older", "1.0.0", None);
        edit_manifest(&dir, |v| v["min_app_version"] = "0.0.1".into());
        assert!(
            PackageResolver::new(None, Some(library))
                .resolve(&reference("local.older", "1.0.0", "Node.pwnode"))
                .is_ok()
        );
    }

    // ── Unsafe references and paths ──────────────────────────────────

    #[test]
    fn unsafe_reference_fields_are_rejected_before_any_lookup() {
        let tmp = TempDir::new("res_unsafe_ref");
        let (_, library) = roots(&tmp);
        let mut resolver = PackageResolver::new(None, Some(library));
        for r in [
            reference("local.node", "1.0.0", "../Node.pwnode"),
            reference("local.node", "1.0.0", "sub/Node.pwnode"),
            reference("../../etc", "1.0.0", "Node.pwnode"),
            reference("local.node", "1.0.0-beta", "Node.pwnode"),
        ] {
            let error = resolver.resolve(&r).unwrap_err();
            assert_eq!(error.kind, ResolveErrorKind::Unsafe, "{r}");
            assert!(error.searched.is_empty(), "nothing was read for {r}");
        }
    }

    #[test]
    fn unsafe_manifest_entry_is_rejected() {
        let tmp = TempDir::new("res_unsafe_entry");
        let (_, library) = roots(&tmp);
        let dir = make_package(&library, "local.node", "1.0.0", None);
        edit_manifest(&dir, |v| v["entry"] = "../Node.pwnode".into());
        // The request names a safe entry; the manifest tries to escape.
        assert_eq!(
            kind_of(
                PackageResolver::new(None, Some(library)).resolve(&reference(
                    "local.node",
                    "1.0.0",
                    "Node.pwnode"
                ))
            ),
            ResolveErrorKind::Unsafe
        );
    }

    #[test]
    fn nested_traversal_and_absolute_references_are_unsafe() {
        let tmp = TempDir::new("res_nested");
        let (_, library) = roots(&tmp);
        let outside = tmp.0.join("outside.pwnode");
        write_node(&outside, "Outside", None);
        let outside_abs = outside.to_string_lossy().into_owned();
        for (id, nested) in [
            ("local.dotdot", "../../../outside.pwnode"),
            ("local.abs", outside_abs.as_str()),
        ] {
            make_package(&library, id, "1.0.0", Some(nested));
            let error = PackageResolver::new(None, Some(library.clone()))
                .resolve(&reference(id, "1.0.0", "Node.pwnode"))
                .unwrap_err();
            assert_eq!(error.kind, ResolveErrorKind::Unsafe, "{id}: {error}");
        }
    }

    #[test]
    fn nested_dependency_inside_package_is_accepted_and_missing_one_is_corrupted() {
        let tmp = TempDir::new("res_nested_ok");
        let (_, library) = roots(&tmp);
        let dir = make_package(&library, "local.outer", "1.0.0", Some("deps/Inner.pwnode"));
        assert_eq!(
            kind_of(
                PackageResolver::new(None, Some(library.clone())).resolve(&reference(
                    "local.outer",
                    "1.0.0",
                    "Node.pwnode"
                ))
            ),
            ResolveErrorKind::Corrupted
        );
        std::fs::create_dir_all(dir.join("deps")).unwrap();
        write_node(&dir.join("deps").join("Inner.pwnode"), "Inner", None);
        assert!(
            PackageResolver::new(None, Some(library))
                .resolve(&reference("local.outer", "1.0.0", "Node.pwnode"))
                .is_ok()
        );
    }

    #[cfg(unix)]
    fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
        // Symlinks need a privilege on many Windows setups; a directory
        // junction does not and escapes the same way.
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return Ok(());
        }
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other("mklink /J failed"))
        }
    }
    #[cfg(unix)]
    fn link_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    fn link_file(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[test]
    fn symlink_escapes_are_unsafe() {
        let tmp = TempDir::new("res_symlink");
        let (_, library) = roots(&tmp);

        // A version folder that is a link to a package outside the Library.
        let elsewhere = tmp.0.join("elsewhere");
        let real = make_package(&elsewhere, "local.node", "1.0.0", None);
        let link_parent = library.join(PACKAGES_DIR).join("local.node");
        std::fs::create_dir_all(&link_parent).unwrap();
        if let Err(e) = link_dir(&real, &link_parent.join("1.0.0")) {
            // Creating symlinks needs extra rights on some Windows setups.
            eprintln!("symlink test skipped: {e}");
            return;
        }
        assert_eq!(
            kind_of(
                PackageResolver::new(None, Some(library.clone())).resolve(&reference(
                    "local.node",
                    "1.0.0",
                    "Node.pwnode"
                ))
            ),
            ResolveErrorKind::Unsafe
        );

        // An entry file that links outside its version folder.
        let dir = make_package(&library, "local.entry", "1.0.0", None);
        let outside = tmp.0.join("outside.pwnode");
        std::fs::copy(dir.join("Node.pwnode"), &outside).unwrap();
        std::fs::remove_file(dir.join("Node.pwnode")).unwrap();
        if let Err(e) = link_file(&outside, &dir.join("Node.pwnode")) {
            eprintln!("file symlink part skipped: {e}");
            return;
        }
        assert_eq!(
            kind_of(
                PackageResolver::new(None, Some(library)).resolve(&reference(
                    "local.entry",
                    "1.0.0",
                    "Node.pwnode"
                ))
            ),
            ResolveErrorKind::Unsafe
        );
    }

    // ── File references (existing projects) ──────────────────────────

    #[test]
    fn plain_absolute_paths_are_returned_unchanged() {
        let tmp = TempDir::new("res_plain");
        let local = tmp.0.join("My Node.pwnode");
        write_node(&local, "Mine", None);
        let missing = tmp.0.join("gone.pwnode");
        let mut resolver = PackageResolver::new(None, None);
        for path in [&local, &missing] {
            match resolver.resolve_file(path).unwrap() {
                ResolvedFile::Plain(p) => assert_eq!(&p, path),
                ResolvedFile::Package(_) => panic!("not a package"),
            }
        }
    }

    #[test]
    fn package_entry_paths_are_verified_in_place() {
        let tmp = TempDir::new("res_in_place");
        let (project, library) = roots(&tmp);
        let dir = make_package(&library, "local.node", "1.0.0", None);
        // Even with a project copy present, a direct path keeps its location.
        make_package(&project, "local.node", "1.0.0", None);
        let entry = dir.join("Node.pwnode");
        let mut resolver = PackageResolver::new(Some(project), Some(library));
        match resolver.resolve_file(&entry).unwrap() {
            ResolvedFile::Package(p) => {
                assert_eq!(p.origin, PackageOrigin::Direct);
                assert_eq!(p.entry_path, entry);
                assert_eq!(p.reference, reference("local.node", "1.0.0", "Node.pwnode"));
            }
            ResolvedFile::Plain(_) => panic!("expected a package"),
        }
        write_node(&entry, "Tampered", None);
        let error = resolver.resolve_file(&entry).unwrap_err();
        assert_eq!(error.kind, ResolveErrorKind::Corrupted);
    }

    #[test]
    fn package_ref_for_path_ignores_unrelated_manifests() {
        let tmp = TempDir::new("res_npm");
        std::fs::write(tmp.0.join(MANIFEST_FILE), r#"{"name":"npm-app"}"#).unwrap();
        let node = tmp.0.join("Mine.pwnode");
        write_node(&node, "Mine", None);
        assert!(package_ref_for_path(&node).is_none());
    }

    // ── Caching ──────────────────────────────────────────────────────

    #[test]
    fn verification_is_cached_until_files_change() {
        let tmp = TempDir::new("res_cache");
        let (_, library) = roots(&tmp);
        let dir = make_package(&library, "local.node", "1.0.0", None);
        let r = reference("local.node", "1.0.0", "Node.pwnode");
        let mut resolver = PackageResolver::new(None, Some(library));
        resolver.resolve(&r).unwrap();
        let loads = LOAD_COUNT.with(|c| c.get());
        for _ in 0..10 {
            resolver.resolve(&r).unwrap();
        }
        assert_eq!(LOAD_COUNT.with(|c| c.get()), loads, "cached, no reloads");

        // A changed entry (different size) is noticed without clearing.
        write_node(&dir.join("Node.pwnode"), "A longer changed name", None);
        assert_eq!(kind_of(resolver.resolve(&r)), ResolveErrorKind::Corrupted);
        resolver.clear_cache();
        assert_eq!(kind_of(resolver.resolve(&r)), ResolveErrorKind::Corrupted);
    }
}
