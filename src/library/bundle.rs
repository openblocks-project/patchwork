//! P4.2 Stage 3: nested package dependencies.
//!
//! A published package whose custom node contains other packages bundles
//! every transitive dependency inside its own version folder, in a flat,
//! deterministic layout:
//!
//! ```text
//! <id>/<version>/package.json
//! <id>/<version>/<Entry>.pwnode
//! <id>/<version>/deps/<dep-id>/<dep-version>/package.json      (byte copy)
//! <id>/<version>/deps/<dep-id>/<dep-version>/<DepEntry>.pwnode (byte copy)
//! ```
//!
//! * Each `id@version` is bundled once (deduplicated); different versions
//!   of one package coexist side by side.
//! * A bundled dependency is an exact byte copy of its published manifest
//!   and entry, so its own checksum still verifies. Its own `deps/` is not
//!   copied: its dependencies are hoisted into the containing package's
//!   `deps/`. Nested instances therefore resolve against the outermost
//!   containing package (the "bundle root").
//! * The root manifest lists the dependency closure in `dependencies` and
//!   the SHA-256 of every bundled file in `files`. Packages without
//!   dependencies (P4.1, standalone) have no `files` and are unchanged.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use super::package::{self, MANIFEST_FILE, PackageManifest, PackageRef};
use super::resolve::{InstancePackageRef, PackageResolver, ResolvedFile};
use crate::graph::{NodeId, NodeType};

/// Folder (inside a version folder) holding bundled dependencies.
pub const DEPS_DIR: &str = "deps";
/// Same limit as nested custom-node evaluation.
pub const MAX_DEPENDENCY_DEPTH: usize = 8;
/// `bundle_format` written by this Patchwork: a complete flat bundle.
pub const BUNDLE_FORMAT: u32 = 1;

/// True when the manifest declares a complete bundle (explicit marker; never
/// inferred from the folder contents).
pub fn is_complete_bundle(manifest: &PackageManifest) -> bool {
    manifest.bundle_format >= 1
}

/// One dependency to bundle, verified where it was found.
#[derive(Debug, Clone)]
pub struct CollectedDependency {
    pub reference: PackageRef,
    /// Version folder it is copied from (never modified).
    pub dir: PathBuf,
    pub manifest: PackageManifest,
}

/// Result of collecting a custom node's dependencies before publishing.
#[derive(Debug, Clone, Default)]
pub struct DependencyPlan {
    /// Exact package for each nested instance of the custom node itself.
    pub root_references: BTreeMap<NodeId, PackageRef>,
    /// The full transitive closure, sorted by id, then version.
    pub dependencies: Vec<CollectedDependency>,
}

/// Bundle-relative folder of a dependency: `deps/<id>/<version>`.
pub fn dependency_rel_dir(reference: &PackageRef) -> String {
    format!("{DEPS_DIR}/{}/{}", reference.id, reference.version)
}

fn node_label(file: &str, id: NodeId) -> String {
    let name = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Custom node".to_string());
    format!("'{name}' (node #{id})")
}

fn chain(stack: &[PackageRef], last: &PackageRef) -> String {
    stack
        .iter()
        .chain(std::iter::once(last))
        .map(|r| format!("{}@{}", r.id, r.version))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Identify a path-only (legacy) nested instance. Only a file that is the
/// verified entry of a package version, in its own `<id>/<version>` folder,
/// counts; anything else is a loose `.pwnode` that must be published first.
fn identify_legacy(
    resolver: &mut PackageResolver,
    file: &str,
) -> Result<(PackageRef, PathBuf, PackageManifest), String> {
    if file.trim().is_empty() {
        return Err("has no file. Replace it with a published Library package.".to_string());
    }
    let name = Path::new(file)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string());
    match resolver.resolve_file(Path::new(file)) {
        Ok(ResolvedFile::Package(found)) => Ok((found.reference, found.dir, found.manifest)),
        Ok(ResolvedFile::Plain(_)) => Err(format!(
            "uses the loose custom node file {name}, which is not a published package. \
             Publish {name} to the Library first, then replace this node with the \
             published version."
        )),
        Err(error) => Err(error.to_string()),
    }
}

/// Collect every package `custom` depends on, recursively, verifying each
/// one. Nested instances must name an exact package (or be the verified
/// entry of one). Resolution uses the Library (and, for dependencies of a
/// bundled package, that package's own `deps/` first). Never modifies
/// anything.
pub fn collect_dependencies(
    custom: &crate::document::CustomNodeFile,
    library_root: &Path,
) -> Result<DependencyPlan, String> {
    let mut resolver = PackageResolver::new(None, Some(library_root.to_path_buf()));
    let mut plan = DependencyPlan::default();
    let mut collected: BTreeMap<(String, semver::Version), CollectedDependency> = BTreeMap::new();
    let mut problems: Vec<String> = Vec::new();

    let mut ids: Vec<NodeId> = custom.graph.nodes.keys().copied().collect();
    ids.sort_unstable();
    for node_id in ids {
        let NodeType::CustomNodeInstance { file, package, .. } =
            &custom.graph.nodes[&node_id].node_type
        else {
            continue;
        };
        let label = node_label(file, node_id);
        resolver.set_bundle(None);
        let found = match package {
            Some(stored) => resolver
                .resolve_instance(file, Some(stored))
                .map_err(|e| e.to_string())
                .and_then(|entry| located(stored, &entry)),
            None => identify_legacy(&mut resolver, file),
        };
        let (reference, dir, manifest) = match found {
            Ok(found) => found,
            Err(error) => {
                problems.push(format!("{label} {error}"));
                continue;
            }
        };
        let mut stack = Vec::new();
        if let Err(error) = visit(
            &mut resolver,
            &reference,
            &dir,
            manifest,
            1,
            &mut stack,
            &mut collected,
        ) {
            problems.push(format!("{label}: {error}"));
            continue;
        }
        plan.root_references.insert(node_id, reference);
    }

    if !problems.is_empty() {
        return Err(format!(
            "This custom node can't be published because of its nested custom nodes:\n• {}",
            problems.join("\n• ")
        ));
    }
    plan.dependencies = collected.into_values().collect();
    Ok(plan)
}

/// Version folder and manifest of a resolved entry.
fn located(
    stored: &InstancePackageRef,
    entry: &Path,
) -> Result<(PackageRef, PathBuf, PackageManifest), String> {
    let reference = stored.to_ref()?;
    let dir = entry
        .parent()
        .ok_or_else(|| format!("{} has no folder.", entry.display()))?
        .to_path_buf();
    let manifest = package::read_manifest(&dir)?;
    Ok((reference, dir, manifest))
}

fn visit(
    resolver: &mut PackageResolver,
    reference: &PackageRef,
    dir: &Path,
    manifest: PackageManifest,
    depth: usize,
    stack: &mut Vec<PackageRef>,
    collected: &mut BTreeMap<(String, semver::Version), CollectedDependency>,
) -> Result<(), String> {
    if stack.contains(reference) {
        return Err(format!(
            "dependency cycle {}. A package cannot contain itself.",
            chain(stack, reference)
        ));
    }
    if depth > MAX_DEPENDENCY_DEPTH {
        return Err(format!(
            "dependencies are nested more than {MAX_DEPENDENCY_DEPTH} levels deep ({}).",
            chain(stack, reference)
        ));
    }
    let key = (reference.id.clone(), reference.version.clone());
    if let Some(existing) = collected.get(&key) {
        // Deduplicate identical id@version; refuse ambiguous copies.
        if !existing
            .manifest
            .sha256
            .eq_ignore_ascii_case(&manifest.sha256)
            || existing.manifest.entry != manifest.entry
        {
            return Err(format!(
                "two different copies of {reference} were found ({} and {}). \
                 Published versions must be identical; remove or republish one.",
                existing.dir.display(),
                dir.display()
            ));
        }
        return Ok(());
    }
    collected.insert(
        key,
        CollectedDependency {
            reference: reference.clone(),
            dir: dir.to_path_buf(),
            manifest: manifest.clone(),
        },
    );

    // Dependencies of this package: its bundle root's deps/ first.
    let bundle = package::containing_package_root(&dir.join(&manifest.entry));
    let raw: crate::document::CustomNodeFile = std::fs::read_to_string(dir.join(&manifest.entry))
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        .map_err(|e| format!("{reference} cannot be read: {e}"))?;
    let mut nested: Vec<(NodeId, &NodeType)> = raw
        .graph
        .nodes
        .iter()
        .map(|(&id, node)| (id, &node.node_type))
        .collect();
    nested.sort_unstable_by_key(|(id, _)| *id);

    stack.push(reference.clone());
    for (_, node_type) in nested {
        let NodeType::CustomNodeInstance { file, package, .. } = node_type else {
            continue;
        };
        let Some(stored) = package else {
            stack.pop();
            return Err(format!(
                "{reference} contains a custom node that is not a package ({file}), \
                 so it can't be bundled. Republish {} with published nested nodes.",
                reference.id
            ));
        };
        let child = stored.to_ref().map_err(|e| {
            format!("{reference} has an invalid nested package reference {stored}: {e}")
        })?;
        resolver.set_bundle(bundle.clone());
        let resolved = resolver
            .resolve(&child)
            .map_err(|e| format!("{reference} needs {child}: {e}"))?;
        let child_manifest = resolved.manifest.clone();
        visit(
            resolver,
            &child,
            &resolved.dir,
            child_manifest,
            depth + 1,
            stack,
            collected,
        )?;
    }
    stack.pop();
    Ok(())
}

/// The root entry as it is published: every nested instance names its
/// exact package, and its compatibility `file` points at the bundled copy.
pub fn rewrite_root_entry(
    custom: &crate::document::CustomNodeFile,
    plan: &DependencyPlan,
) -> Result<String, String> {
    let mut published = custom.clone();
    for (node_id, reference) in &plan.root_references {
        let Some(node) = published.graph.nodes.get_mut(node_id) else {
            continue;
        };
        if let NodeType::CustomNodeInstance { file, package, .. } = &mut node.node_type {
            *file = format!("{}/{}", dependency_rel_dir(reference), reference.entry);
            *package = Some(InstancePackageRef::from_ref(reference));
        }
    }
    // Through a JSON value so object keys (including node ids) are sorted:
    // the same custom node always publishes to the same bytes.
    serde_json::to_value(&published)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .map_err(|e| format!("Failed to serialize custom node: {e}"))
}

/// Copy every dependency (manifest and entry, byte for byte) into
/// `staging/deps/<id>/<version>/` with exclusive creates, and return the
/// checksum of every bundled file. Sources are only read.
pub fn bundle_dependencies(
    staging: &Path,
    plan: &DependencyPlan,
) -> Result<BTreeMap<String, String>, String> {
    let mut files = BTreeMap::new();
    for dependency in &plan.dependencies {
        let rel_dir = dependency_rel_dir(&dependency.reference);
        let target = staging.join(&rel_dir);
        std::fs::create_dir_all(&target)
            .map_err(|e| format!("Cannot create {}: {e}", target.display()))?;
        for name in [MANIFEST_FILE, dependency.manifest.entry.as_str()] {
            let source = dependency.dir.join(name);
            let destination = target.join(name);
            let mut input = std::fs::File::open(&source)
                .map_err(|e| format!("Cannot read {}: {e}", source.display()))?;
            super::copy_exclusive_from(&mut input, &destination)?;
            files.insert(
                format!("{rel_dir}/{name}"),
                package::sha256_file(&destination)?,
            );
        }
        // The copy must still be the verified version (no change between
        // verification and copying).
        let entry_sha = &files[&format!("{rel_dir}/{}", dependency.manifest.entry)];
        if !entry_sha.eq_ignore_ascii_case(&dependency.manifest.sha256) {
            return Err(format!(
                "{} changed while it was being bundled; nothing was published.",
                dependency.reference
            ));
        }
    }
    Ok(files)
}

// ── Verification ─────────────────────────────────────────────────────────

/// A bundle integrity problem: `unsafe_` for links and paths that leave the
/// package, otherwise damage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleProblem {
    pub unsafe_: bool,
    pub message: String,
}

fn damaged(message: String) -> BundleProblem {
    BundleProblem {
        unsafe_: false,
        message,
    }
}

fn unsafe_problem(message: String) -> BundleProblem {
    BundleProblem {
        unsafe_: true,
        message,
    }
}

/// True when `dir` is a dependency copy bundled inside another package
/// (`<root>/deps/<id>/<version>` with a manifest in `<root>`).
pub fn is_bundled_copy(dir: &Path) -> bool {
    let Some(deps) = dir.parent().and_then(Path::parent) else {
        return false;
    };
    deps.file_name().and_then(|n| n.to_str()) == Some(DEPS_DIR)
        && deps
            .parent()
            .is_some_and(|root| root.join(MANIFEST_FILE).is_file())
}

/// A bundled-file path: relative, `/`-separated, plain names, under `deps/`.
fn check_bundle_path(rel: &str) -> Result<(), BundleProblem> {
    let plain = !rel.contains('\\')
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && rel
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if !plain || !rel.starts_with(&format!("{DEPS_DIR}/")) {
        return Err(unsafe_problem(format!(
            "Bundled file path '{rel}' is not a plain path inside {DEPS_DIR}/."
        )));
    }
    Ok(())
}

/// Every file below `dir` (not following links). Links are reported.
fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), BundleProblem> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| damaged(format!("Cannot read {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| damaged(e.to_string()))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| damaged(e.to_string()))?;
        if meta.file_type().is_symlink() || is_reparse_point(&meta) {
            return Err(unsafe_problem(format!(
                "{} is a link; links are not allowed in bundled dependencies.",
                path.display()
            )));
        }
        if meta.is_dir() {
            walk_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Verify the Stage 3 parts of a version folder: bundled files (checksums,
/// containment, no links, nothing unlisted) and that every nested package
/// reference in the entry is a declared, well-formed dependency.
///
/// For a bundled copy only the nested references are checked: its own
/// `files` describe its original location (its dependencies are hoisted
/// into the containing package, which verifies them).
pub fn verify_bundle(dir: &Path, manifest: &PackageManifest) -> Result<(), BundleProblem> {
    if !is_bundled_copy(dir) {
        verify_bundled_files(dir, manifest)?;
    }
    verify_nested_references(dir, manifest)
}

fn verify_bundled_files(dir: &Path, manifest: &PackageManifest) -> Result<(), BundleProblem> {
    let canonical_dir = std::fs::canonicalize(dir)
        .map_err(|e| damaged(format!("Cannot open package folder: {e}")))?;
    for (rel, expected) in &manifest.files {
        check_bundle_path(rel)?;
        let path = dir.join(rel);
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|_| damaged(format!("Bundled file {rel} is missing.")))?;
        if meta.file_type().is_symlink() || is_reparse_point(&meta) || !meta.is_file() {
            return Err(unsafe_problem(format!(
                "Bundled file {rel} is not a regular file inside the package."
            )));
        }
        match std::fs::canonicalize(&path) {
            Ok(p) if p.starts_with(&canonical_dir) => {}
            _ => {
                return Err(unsafe_problem(format!(
                    "Bundled file {rel} links outside the package folder."
                )));
            }
        }
        let actual = package::sha256_file(&path).map_err(damaged)?;
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(damaged(format!(
                "Bundled file {rel} was changed after publishing (checksum mismatch)."
            )));
        }
    }
    // Nothing unlisted (or linked) may sit in deps/.
    let deps = dir.join(DEPS_DIR);
    match std::fs::symlink_metadata(&deps) {
        Err(_) => {}
        Ok(meta) if meta.file_type().is_symlink() || is_reparse_point(&meta) => {
            return Err(unsafe_problem(format!(
                "{DEPS_DIR}/ is a link; links are not allowed in bundled dependencies."
            )));
        }
        Ok(_) => {
            let mut found = Vec::new();
            walk_files(&deps, &mut found)?;
            for path in found {
                let rel = path
                    .strip_prefix(dir)
                    .map(|p| {
                        p.components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .unwrap_or_default();
                if !manifest.files.contains_key(&rel) {
                    return Err(damaged(format!(
                        "{rel} is not part of the published package (unexpected file)."
                    )));
                }
            }
        }
    }
    // Everything bundled must be declared (`deps/<id>/<version>/...`).
    let mut bundled_packages: Vec<(String, String)> = manifest
        .files
        .keys()
        .filter_map(|rel| {
            let mut parts = rel.split('/');
            parts.next(); // deps
            Some((parts.next()?.to_string(), parts.next()?.to_string()))
        })
        .collect();
    bundled_packages.dedup();
    for (id, version) in &bundled_packages {
        let declared = manifest.dependencies.iter().any(|d| {
            d.id == *id
                && package::parse_stable_version(&d.version)
                    .map(|v| v.to_string() == *version)
                    .unwrap_or(false)
        });
        if !declared {
            return Err(damaged(format!(
                "The bundle contains {id}@{version}, which is not a declared dependency."
            )));
        }
    }
    let complete = is_complete_bundle(manifest);
    // A bundled dependency folder must match the dependency it claims to be.
    for dependency in &manifest.dependencies {
        let Ok(version) = package::parse_stable_version(&dependency.version) else {
            return Err(damaged(format!(
                "Dependency {}@{} has an invalid version.",
                dependency.id, dependency.version
            )));
        };
        let reference = PackageRef {
            id: dependency.id.clone(),
            version,
            entry: dependency.entry.clone().unwrap_or_default(),
        };
        let rel_dir = dependency_rel_dir(&reference);
        let manifest_rel = format!("{rel_dir}/{MANIFEST_FILE}");
        if !manifest.files.contains_key(&manifest_rel) {
            if complete {
                return Err(damaged(format!(
                    "Declared dependency {}@{} is missing from the bundle.",
                    reference.id, reference.version
                )));
            }
            // Older packages without the complete-bundle marker: a declared
            // dependency that is not bundled resolves like any package.
            continue;
        }
        let bundled = package::read_manifest(&dir.join(&rel_dir)).map_err(damaged)?;
        let entry_sha = manifest
            .files
            .get(&format!("{rel_dir}/{}", bundled.entry))
            .cloned()
            .unwrap_or_default();
        if bundled.id != reference.id
            || bundled.version != reference.version.to_string()
            || (!reference.entry.is_empty() && bundled.entry != reference.entry)
            || !entry_sha.eq_ignore_ascii_case(&bundled.sha256)
        {
            return Err(damaged(format!(
                "The bundled copy of {}@{} does not match its manifest.",
                reference.id, reference.version
            )));
        }
        // A complete bundle holds the full closure: what a bundled package
        // needs is declared (and so bundled) here too.
        if complete {
            for needed in &bundled.dependencies {
                let declared = manifest
                    .dependencies
                    .iter()
                    .any(|d| d.id == needed.id && d.version == needed.version);
                if !declared {
                    return Err(damaged(format!(
                        "The bundle is incomplete: {}@{} needs {}@{}, which is not bundled.",
                        reference.id, reference.version, needed.id, needed.version
                    )));
                }
            }
        }
    }
    Ok(())
}

fn verify_nested_references(dir: &Path, manifest: &PackageManifest) -> Result<(), BundleProblem> {
    let raw: crate::document::CustomNodeFile = std::fs::read_to_string(dir.join(&manifest.entry))
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        .map_err(|e| {
            damaged(format!(
                "{} is not a valid custom node: {e}",
                manifest.entry
            ))
        })?;
    for node in raw.graph.nodes.values() {
        let NodeType::CustomNodeInstance {
            file,
            package: Some(stored),
            ..
        } = &node.node_type
        else {
            continue;
        };
        let reference = stored.to_ref().map_err(|e| {
            unsafe_problem(format!(
                "Nested package reference {stored} is not valid: {e}"
            ))
        })?;
        if !file.is_empty() {
            let path = Path::new(file);
            let plain = !path.is_absolute()
                && path
                    .components()
                    .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
            if !plain {
                return Err(unsafe_problem(format!(
                    "Nested custom node path '{file}' leaves the package folder."
                )));
            }
        }
        let declared = manifest.dependencies.iter().any(|d| {
            d.id == reference.id
                && package::parse_stable_version(&d.version).ok().as_ref()
                    == Some(&reference.version)
                && d.entry.as_deref().is_none_or(|e| e == reference.entry)
        });
        if !declared {
            return Err(damaged(format!(
                "The custom node uses {reference}, which is not a declared dependency."
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
