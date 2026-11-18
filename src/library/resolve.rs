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

// Stage 2 uses `resolve_instance` during evaluation. Parts of the API
// (`resolve_in`, origins, error details) are reserved for explicit repair
// UI and the standalone Player.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use super::package::{
    self, MANIFEST_FILE, MANIFEST_FORMAT, PACKAGES_DIR, PackageManifest, PackageRef,
};

/// Where a package was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackageOrigin {
    /// P4.2 Stage 3: `<bundle root>/deps/...`, the dependencies bundled in
    /// the package that contains the instance being resolved.
    Bundle,
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
            PackageOrigin::Bundle => "bundled",
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
pub type Fingerprint = Vec<(PathBuf, Option<(std::time::SystemTime, u64)>)>;

/// Metadata fingerprint (paths, mtimes, sizes; no hashing) of everything
/// inside a version folder, bundled dependencies included. Used to notice
/// changes cheaply before re-verifying.
pub fn package_fingerprint(dir: &Path) -> Fingerprint {
    let mut paths = vec![dir.join(MANIFEST_FILE)];
    version_dir_paths(dir, &mut paths);
    paths.sort();
    paths.dedup();
    fingerprint(&paths)
}

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

/// Every path below `dir` (links not followed), sorted. P4.2 Stage 3: a
/// verification depends on bundled files too, so any change, addition or
/// removal inside the version folder invalidates it.
fn version_dir_paths(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_dir = std::fs::symlink_metadata(&path)
            .map(|m| m.is_dir())
            .unwrap_or(false);
        out.push(path.clone());
        if is_dir {
            version_dir_paths(&path, out);
        }
    }
}

type CandidateResult = Result<(PackageManifest, PathBuf), CandidateProblem>;

/// Resolves package references against a project folder and the Library.
#[derive(Debug, Clone, Default)]
pub struct PackageResolver {
    /// P4.2 Stage 3: outermost package containing the custom node whose
    /// nested instances are resolved; its `deps/` is searched first.
    bundle: Option<PathBuf>,
    project_dir: Option<PathBuf>,
    library_root: Option<PathBuf>,
    /// Verification results per version folder, reused while none of the
    /// files they depended on changed.
    /// Keyed by folder and expected identity (a result verified without an
    /// identity check is never reused for one with it).
    cache: HashMap<(PathBuf, Option<PackageRef>), (Fingerprint, CandidateResult)>,
}

impl PackageResolver {
    /// `project_dir`: folder holding `project.json` (its `packages/` is
    /// searched first). `library_root`: e.g. `~/.patchwork/library`.
    pub fn new(project_dir: Option<PathBuf>, library_root: Option<PathBuf>) -> Self {
        Self {
            bundle: None,
            project_dir,
            library_root,
            cache: HashMap::new(),
        }
    }

    /// The package whose bundled `deps/` is searched first.
    pub fn bundle(&self) -> Option<&Path> {
        self.bundle.as_deref()
    }

    /// Set (or clear) the containing package. Verification results stay
    /// cached (they are keyed by folder).
    pub fn set_bundle(&mut self, bundle: Option<PathBuf>) {
        self.bundle = bundle;
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
        if let Some(bundle) = &self.bundle {
            let deps = bundle.join(super::bundle::DEPS_DIR);
            out.push((
                PackageOrigin::Bundle,
                deps.clone(),
                deps.join(&reference.id).join(reference.version.to_string()),
            ));
        }
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

        // P4.2 Stage 3: inside a complete bundle, a dependency is only ever
        // taken from that bundle (no project or Library fallback).
        if let Some(root) = self.bundle.clone() {
            let complete = package::read_manifest(&root)
                .map(|m| super::bundle::is_complete_bundle(&m))
                .unwrap_or(false);
            if complete {
                let candidates: Vec<_> = self
                    .candidates(reference)
                    .into_iter()
                    .filter(|(origin, _, _)| *origin == PackageOrigin::Bundle)
                    .collect();
                let bundled_dir = candidates.first().map(|(_, _, dir)| dir.clone());
                return match self.resolve_candidates(reference, label.clone(), candidates) {
                    Err(error) if error.kind == ResolveErrorKind::Missing => {
                        let dir = bundled_dir.unwrap_or_default();
                        Err(ResolveError {
                            reference: label,
                            kind: ResolveErrorKind::Corrupted,
                            problems: vec![problem(
                                PackageOrigin::Bundle,
                                &dir,
                                ResolveErrorKind::Corrupted,
                                format!(
                                    "it is missing from the bundle of {}; complete bundles \
                                     never use copies from elsewhere.",
                                    root.display()
                                ),
                            )],
                            searched: error.searched,
                        })
                    }
                    other => other,
                };
            }
        }
        let candidates = self.candidates(reference);
        self.resolve_candidates(reference, label, candidates)
    }

    /// P4.2 Stage 3: verify a plain file reference that lies inside a
    /// published package (a P4.1-style path-only instance). The outermost
    /// package is verified first (so a bundled copy is only used while its
    /// bundle verifies), then the file itself if it is a package entry.
    /// Returns the file to load.
    pub fn verify_package_file(&mut self, file: &Path) -> Result<PathBuf, ResolveError> {
        let Some(root) = package::containing_package_root(file) else {
            return Ok(file.to_path_buf());
        };
        let packages_root = root
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.clone());
        if let Err(problem) = self.verify_cached(PackageOrigin::Direct, &packages_root, &root, None)
        {
            let label = package::read_manifest(&root)
                .map(|m| format!("{}@{}/{}", m.id, m.version, m.entry))
                .unwrap_or_else(|_| root.display().to_string());
            return Err(ResolveError {
                reference: label,
                kind: problem.kind,
                searched: vec![root.clone()],
                problems: vec![problem],
            });
        }
        match self.resolve_file(file)? {
            ResolvedFile::Package(found) => Ok(found.entry_path),
            ResolvedFile::Plain(path) => Ok(path),
        }
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
            // A bundled copy is only trusted while the package that bundles
            // it verifies (its manifest holds the bundled files' checksums).
            if origin == PackageOrigin::Bundle {
                if let Some(root) = self.bundle.clone() {
                    let root_packages = root
                        .parent()
                        .and_then(Path::parent)
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| root.clone());
                    if let Err(mut problem) =
                        self.verify_cached(PackageOrigin::Bundle, &root_packages, &root, None)
                    {
                        problem.message = format!(
                            "the package that bundles it ({}) is damaged: {}",
                            root.display(),
                            problem.message
                        );
                        return Err(ResolveError {
                            reference: label,
                            kind: problem.kind,
                            problems: vec![problem],
                            searched,
                        });
                    }
                }
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
        version_dir_paths(dir, &mut deps);
        deps.sort();
        deps.dedup();
        let print = fingerprint(&deps);
        let key = (dir.to_path_buf(), expected.cloned());
        if let Some((cached_print, result)) = self.cache.get(&key) {
            if *cached_print == print {
                return result.clone().map_err(|mut p| {
                    p.origin = origin;
                    p
                });
            }
        }
        let result = verify_candidate(origin, packages_root, dir, expected);
        self.cache.insert(key, (print, result.clone()));
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
    if manifest.bundle_format > super::bundle::BUNDLE_FORMAT {
        return fail(
            Incompatible,
            format!(
                "Dependency bundle format {} is not supported (this Patchwork reads format {}).",
                manifest.bundle_format,
                super::bundle::BUNDLE_FORMAT
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
    // P4.2 Stage 3: bundled files, and nested package references (which
    // must be declared dependencies; they are resolved when loaded).
    if let Err(bundle_problem) = super::bundle::verify_bundle(dir, &manifest) {
        let kind = if bundle_problem.unsafe_ {
            Unsafe
        } else {
            Corrupted
        };
        return fail(kind, bundle_problem.message);
    }
    for node in raw.graph.nodes.values() {
        let crate::graph::NodeType::CustomNodeInstance { file, package, .. } = &node.node_type
        else {
            continue;
        };
        if package.is_some() {
            continue;
        }
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

// ── P4.2 Stage 2: portable instance references ───────────────────────────

/// Package reference as stored on a custom-node instance
/// (`"package": { "id", "version", "entry" }`).
///
/// Kept as plain strings so a malformed or future reference never stops a
/// project from loading; it is validated when the instance is resolved.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct InstancePackageRef {
    pub id: String,
    pub version: String,
    pub entry: String,
}

impl InstancePackageRef {
    pub fn from_ref(reference: &PackageRef) -> Self {
        Self {
            id: reference.id.clone(),
            version: reference.version.to_string(),
            entry: reference.entry.clone(),
        }
    }

    /// Validate into an exact reference (stable version, safe id and entry).
    pub fn to_ref(&self) -> Result<PackageRef, String> {
        let reference = PackageRef {
            id: self.id.clone(),
            version: package::parse_stable_version(&self.version)?,
            entry: self.entry.clone(),
        };
        check_reference(&reference)?;
        Ok(reference)
    }
}

impl std::fmt::Display for InstancePackageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}/{}", self.id, self.version, self.entry)
    }
}

/// Why a packaged instance cannot be evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceError {
    /// The stored reference itself is not valid.
    Invalid { reference: String, message: String },
    /// The instance `file` is a different package, version or file.
    Conflict {
        reference: String,
        file: String,
        found: Option<String>,
    },
    /// The package could not be resolved (missing, corrupted, ...).
    Package(ResolveError),
}

impl InstanceError {
    /// Category for callers and tests: `None` for a conflict or an invalid
    /// reference.
    pub fn resolve_kind(&self) -> Option<ResolveErrorKind> {
        match self {
            InstanceError::Package(error) => Some(error.kind),
            _ => None,
        }
    }

    pub fn is_conflict(&self) -> bool {
        matches!(self, InstanceError::Conflict { .. })
    }
}

impl std::fmt::Display for InstanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstanceError::Invalid { reference, message } => {
                write!(f, "Invalid package reference {reference}: {message}")
            }
            InstanceError::Conflict {
                reference,
                file,
                found: Some(found),
            } => write!(
                f,
                "Package conflict: this node uses {reference}, but its file {file} is {found}. \
                 Nothing was changed; fix the file or reinsert the node."
            ),
            InstanceError::Conflict {
                reference,
                file,
                found: None,
            } => write!(
                f,
                "Package conflict: this node uses {reference}, but its file {file} is not that \
                 package. Nothing was changed; fix the file or reinsert the node."
            ),
            InstanceError::Package(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for InstanceError {}

/// What an instance's `file` field points at.
enum FileIdentity {
    /// Empty, or nothing exists there.
    Missing,
    /// The entry of this exact package version.
    Package(PackageRef),
    /// Some other file.
    Other,
}

fn file_identity(file: &str) -> FileIdentity {
    if file.is_empty() {
        return FileIdentity::Missing;
    }
    let path = Path::new(file);
    if std::fs::symlink_metadata(path).is_err() {
        return FileIdentity::Missing;
    }
    match package_ref_for_path(path) {
        Some((reference, _)) => FileIdentity::Package(reference),
        None => FileIdentity::Other,
    }
}

impl PackageResolver {
    /// Decide which `.pwnode` a custom-node instance loads (Stage 2).
    ///
    /// * No package reference (legacy, unversioned, ordinary custom nodes):
    ///   `file` is returned unchanged, exactly as before Stage 2.
    /// * With a package reference, the package is authoritative:
    ///   - `file` naming another package, version or file is a conflict;
    ///   - the exact version is resolved (project copy first, then the
    ///     Library); a present but damaged/incompatible/unsafe copy is an
    ///     error and is never replaced by another copy;
    ///   - if it is installed nowhere but `file` is that exact package entry
    ///     (e.g. a Library on another drive), it is verified in place;
    ///   - otherwise it is missing. Versions are never upgraded.
    ///
    /// Never modifies the instance; callers keep its ports and connections
    /// on error.
    pub fn resolve_instance(
        &mut self,
        file: &str,
        package: Option<&InstancePackageRef>,
    ) -> Result<PathBuf, InstanceError> {
        let Some(stored) = package else {
            return Ok(PathBuf::from(file));
        };
        let reference = stored.to_ref().map_err(|message| InstanceError::Invalid {
            reference: stored.to_string(),
            message,
        })?;
        let identity = file_identity(file);
        let conflict = |found: Option<String>| InstanceError::Conflict {
            reference: reference.to_string(),
            file: file.to_string(),
            found,
        };
        if let FileIdentity::Package(other) = &identity {
            if *other != reference {
                return Err(conflict(Some(other.to_string())));
            }
        }
        match self.resolve(&reference) {
            Ok(resolved) => match identity {
                FileIdentity::Other => Err(conflict(None)),
                _ => Ok(resolved.entry_path),
            },
            Err(error) if error.kind != ResolveErrorKind::Missing => {
                Err(InstanceError::Package(error))
            }
            Err(missing) => match identity {
                FileIdentity::Package(_) => match self.resolve_file(Path::new(file)) {
                    Ok(ResolvedFile::Package(resolved)) => Ok(resolved.entry_path),
                    Ok(ResolvedFile::Plain(_)) => Err(InstanceError::Package(missing)),
                    Err(error) => Err(InstanceError::Package(error)),
                },
                FileIdentity::Other => Err(conflict(None)),
                FileIdentity::Missing => Err(InstanceError::Package(missing)),
            },
        }
    }
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
            files: Default::default(),
            bundle_format: 0,
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
        // (`deps/` is reserved for Stage 3 bundles, so a loose nested file
        // lives in another subfolder.)
        let dir = make_package(&library, "local.outer", "1.0.0", Some("lib/Inner.pwnode"));
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
        std::fs::create_dir_all(dir.join("lib")).unwrap();
        write_node(&dir.join("lib").join("Inner.pwnode"), "Inner", None);
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

    // ── Stage 2: instance references ─────────────────────────────────

    fn stored(id: &str, version: &str) -> InstancePackageRef {
        InstancePackageRef {
            id: id.into(),
            version: version.into(),
            entry: "Node.pwnode".into(),
        }
    }

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn legacy_instances_resolve_to_their_file_unchanged() {
        let mut resolver = PackageResolver::new(None, None);
        for file in ["", "C:/nowhere/Missing.pwnode", "relative/Node.pwnode"] {
            assert_eq!(
                resolver.resolve_instance(file, None).unwrap(),
                PathBuf::from(file)
            );
        }
    }

    #[test]
    fn matching_file_and_package_load_the_exact_version() {
        let tmp = TempDir::new("inst_match");
        let (_, library) = roots(&tmp);
        let v1 = make_package(&library, "local.node", "1.0.0", None);
        make_package(&library, "local.node", "1.1.0", None);
        let mut resolver = PackageResolver::new(None, Some(library));
        let entry = v1.join("Node.pwnode");
        assert_eq!(
            resolver
                .resolve_instance(&text(&entry), Some(&stored("local.node", "1.0.0")))
                .unwrap(),
            entry
        );
    }

    #[test]
    fn missing_file_resolves_the_installed_exact_package() {
        let tmp = TempDir::new("inst_missing_file");
        let (_, library) = roots(&tmp);
        let dir = make_package(&library, "local.node", "1.0.0", None);
        let mut resolver = PackageResolver::new(None, Some(library));
        let reference = stored("local.node", "1.0.0");
        let elsewhere = tmp.0.join("other-machine").join("Node.pwnode");
        for file in [text(&elsewhere), String::new()] {
            assert_eq!(
                resolver.resolve_instance(&file, Some(&reference)).unwrap(),
                dir.join("Node.pwnode")
            );
        }
    }

    #[test]
    fn file_of_another_version_or_package_is_a_conflict() {
        let tmp = TempDir::new("inst_conflict");
        let (_, library) = roots(&tmp);
        make_package(&library, "local.node", "1.0.0", None);
        let newer = make_package(&library, "local.node", "1.1.0", None);
        let other = make_package(&library, "local.other", "1.0.0", None);
        let plain = tmp.0.join("Plain.pwnode");
        write_node(&plain, "Plain", None);
        let mut resolver = PackageResolver::new(None, Some(library));
        let reference = stored("local.node", "1.0.0");

        let error = resolver
            .resolve_instance(&text(&newer.join("Node.pwnode")), Some(&reference))
            .unwrap_err();
        assert!(error.is_conflict());
        assert!(error.to_string().contains("local.node@1.1.0"));

        let error = resolver
            .resolve_instance(&text(&other.join("Node.pwnode")), Some(&reference))
            .unwrap_err();
        assert!(error.is_conflict());
        assert!(error.to_string().contains("local.other@1.0.0"));

        let error = resolver
            .resolve_instance(&text(&plain), Some(&reference))
            .unwrap_err();
        assert!(error.is_conflict(), "a plain file is not the package");
    }

    #[test]
    fn package_missing_everywhere_is_missing_and_never_upgraded() {
        let tmp = TempDir::new("inst_never_upgrade");
        let (project, library) = roots(&tmp);
        let newer = make_package(&library, "local.node", "1.1.0", None);
        let mut resolver = PackageResolver::new(Some(project), Some(library));
        let reference = stored("local.node", "1.0.0");
        let gone = tmp.0.join("gone").join("Node.pwnode");
        let error = resolver
            .resolve_instance(&text(&gone), Some(&reference))
            .unwrap_err();
        assert_eq!(error.resolve_kind(), Some(ResolveErrorKind::Missing));
        // A file naming the installed newer version is not used either.
        let error = resolver
            .resolve_instance(&text(&newer.join("Node.pwnode")), Some(&reference))
            .unwrap_err();
        assert!(error.is_conflict());
    }

    #[test]
    fn corrupted_or_incompatible_package_is_refused() {
        let tmp = TempDir::new("inst_corrupt");
        let (_, library) = roots(&tmp);
        let damaged = make_package(&library, "local.node", "1.0.0", None);
        write_node(
            &damaged.join("Node.pwnode"),
            "Changed after publishing",
            None,
        );
        let newer_format = make_package(&library, "local.future", "1.0.0", None);
        edit_manifest(&newer_format, |v| v["format"] = serde_json::json!(99));
        let mut resolver = PackageResolver::new(None, Some(library));

        let reference = stored("local.node", "1.0.0");
        for file in [text(&damaged.join("Node.pwnode")), String::new()] {
            let error = resolver
                .resolve_instance(&file, Some(&reference))
                .unwrap_err();
            assert_eq!(error.resolve_kind(), Some(ResolveErrorKind::Corrupted));
        }
        let error = resolver
            .resolve_instance(
                &text(&newer_format.join("Node.pwnode")),
                Some(&stored("local.future", "1.0.0")),
            )
            .unwrap_err();
        assert_eq!(error.resolve_kind(), Some(ResolveErrorKind::Incompatible));
    }

    #[test]
    fn damaged_project_copy_is_not_replaced_by_the_library_copy() {
        let tmp = TempDir::new("inst_project_damaged");
        let (project, library) = roots(&tmp);
        let project_copy = make_package(&project, "local.node", "1.0.0", None);
        write_node(
            &project_copy.join("Node.pwnode"),
            "Damaged project copy",
            None,
        );
        let library_copy = make_package(&library, "local.node", "1.0.0", None);
        let mut resolver = PackageResolver::new(Some(project), Some(library));
        // Even when the instance file points at the intact Library copy.
        let error = resolver
            .resolve_instance(
                &text(&library_copy.join("Node.pwnode")),
                Some(&stored("local.node", "1.0.0")),
            )
            .unwrap_err();
        assert_eq!(error.resolve_kind(), Some(ResolveErrorKind::Corrupted));
        assert!(error.to_string().contains("project copy"));
    }

    #[test]
    fn exact_entry_outside_known_locations_is_verified_in_place() {
        let tmp = TempDir::new("inst_direct");
        let (_, library) = roots(&tmp);
        let shared = make_package(&tmp.0.join("shared"), "local.node", "1.0.0", None);
        let entry = shared.join("Node.pwnode");
        let mut resolver = PackageResolver::new(None, Some(library));
        let reference = stored("local.node", "1.0.0");
        assert_eq!(
            resolver
                .resolve_instance(&text(&entry), Some(&reference))
                .unwrap(),
            entry
        );
        write_node(&entry, "Changed after publishing", None);
        let error = resolver
            .resolve_instance(&text(&entry), Some(&reference))
            .unwrap_err();
        assert_eq!(error.resolve_kind(), Some(ResolveErrorKind::Corrupted));
    }

    #[test]
    fn invalid_stored_references_are_rejected_without_lookup() {
        let mut resolver = PackageResolver::new(None, None);
        for (id, version, entry) in [
            ("../evil", "1.0.0", "Node.pwnode"),
            ("local.node", "latest", "Node.pwnode"),
            ("local.node", "1.0.0-beta", "Node.pwnode"),
            ("local.node", "1.0.0", "../Node.pwnode"),
            ("local.node", "1.0.0", "sub/Node.pwnode"),
        ] {
            let reference = InstancePackageRef {
                id: id.into(),
                version: version.into(),
                entry: entry.into(),
            };
            let error = resolver.resolve_instance("", Some(&reference)).unwrap_err();
            assert!(
                matches!(error, InstanceError::Invalid { .. }),
                "{id}@{version}/{entry}: {error}"
            );
        }
    }

    #[test]
    fn instance_package_ref_serializes_portably() {
        let exact = reference("local.node", "1.2.3", "Node.pwnode");
        let stored = InstancePackageRef::from_ref(&exact);
        assert_eq!(
            serde_json::to_value(&stored).unwrap(),
            serde_json::json!({"id": "local.node", "version": "1.2.3", "entry": "Node.pwnode"})
        );
        let back: InstancePackageRef =
            serde_json::from_value(serde_json::to_value(&stored).unwrap()).unwrap();
        assert_eq!(back, stored);
        assert_eq!(back.to_ref().unwrap(), exact);
        // A malformed reference still deserializes; it is refused when used.
        let odd: InstancePackageRef =
            serde_json::from_str(r#"{"id":"x/../y","version":"one","entry":"a"}"#).unwrap();
        assert!(odd.to_ref().is_err());
    }

    #[test]
    fn resolving_instances_does_not_reverify_unchanged_packages() {
        let tmp = TempDir::new("inst_cache");
        let (_, library) = roots(&tmp);
        make_package(&library, "local.node", "1.0.0", None);
        let mut resolver = PackageResolver::new(None, Some(library));
        let reference = stored("local.node", "1.0.0");
        resolver.resolve_instance("", Some(&reference)).unwrap();
        let loads = LOAD_COUNT.with(|c| c.get());
        for _ in 0..10 {
            resolver.resolve_instance("", Some(&reference)).unwrap();
        }
        assert_eq!(LOAD_COUNT.with(|c| c.get()), loads, "no re-verification");
    }
}
