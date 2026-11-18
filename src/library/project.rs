//! P4.2 Stage 4: portable project bundling.
//!
//! Saving a project copies every package version it uses into
//! `<project>/packages/<id>/<version>/` (complete folders, so Stage 3
//! bundles keep their `deps/`), so the project runs on a computer with an
//! empty Library. At runtime the project copy has priority (Stage 1/2).
//!
//! Rules:
//! * Package identity is `id@version`; content identity is the verified
//!   entry checksum plus the bundled-file checksums. Two copies with the
//!   same identity are only treated as interchangeable when their content
//!   identity is equal (never by timestamps or sizes).
//! * Exact versions only; nothing is upgraded or substituted.
//! * A copy already in the project is reused (never overwritten); a
//!   damaged one stops the save.
//! * Saving is staged inside the project folder and committed additively:
//!   new package folders are renamed into place first, `project.json` is
//!   replaced last. A failure removes what this save added, so the previous
//!   `project.json` and package collection stay as they were.
//! * Loose `.pwnode` files are never copied; references outside the
//!   project get a portability warning.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use super::bundle::MAX_DEPENDENCY_DEPTH;
use super::package::{self, PACKAGES_DIR, PackageManifest, PackageRef};
use super::resolve::{
    InstancePackageRef, PackageResolver, ResolveError, ResolveErrorKind, ResolvedPackage,
};
use crate::graph::{Graph, NodeId, NodeType};

/// Prefix of the staging folder created inside the project during a save.
pub const STAGING_PREFIX: &str = ".patchwork-staging-";

/// Content identity of a verified package version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentId(String);

/// Built only from checksums that verification has checked against the
/// files (entry and bundled files), never from timestamps or sizes.
pub fn content_id(manifest: &PackageManifest) -> ContentId {
    let files: Vec<String> = manifest
        .files
        .iter()
        .map(|(rel, sha)| format!("{rel}={}", sha.to_ascii_lowercase()))
        .collect();
    ContentId(format!(
        "{}#{}|{}",
        manifest.entry,
        manifest.sha256.to_ascii_lowercase(),
        files.join(",")
    ))
}

/// A package version to copy into the project.
#[derive(Debug, Clone)]
pub struct PackageCopy {
    pub reference: PackageRef,
    pub source_dir: PathBuf,
    pub content: ContentId,
}

/// What saving a project needs to do with packages.
#[derive(Debug, Clone, Default)]
pub struct ProjectPackagePlan {
    /// Versions to copy into `<project>/packages/` (sorted).
    pub to_copy: Vec<PackageCopy>,
    /// Versions already in the project and reused as they are (sorted).
    pub reused: Vec<PackageRef>,
    /// Relative compatibility path for each packaged instance of the graph.
    pub instance_files: BTreeMap<NodeId, String>,
    /// Portability warnings (loose files outside the project, ...).
    pub warnings: Vec<String>,
    /// Path-only custom-node references that are not in the project and
    /// are not bundled (outside files, Library paths, missing files).
    pub external_custom_nodes: Vec<String>,
}

/// Result of a portable save, for the toast and the System Log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortabilityReport {
    /// Verified package versions the project now carries (copied + reused).
    pub bundled: usize,
    /// Of those, copied by this save.
    pub copied: usize,
    /// Path-only custom nodes left outside the project.
    pub external_custom_nodes: usize,
    /// Other tracked asset files (media, models, plugins) outside the project.
    pub external_assets: usize,
}

impl PortabilityReport {
    pub fn new(plan: &ProjectPackagePlan, report: &SaveReport, external_assets: usize) -> Self {
        Self {
            bundled: report.copied + report.reused,
            copied: report.copied,
            external_custom_nodes: plan.external_custom_nodes.len(),
            external_assets,
        }
    }

    /// Custom-node dependencies are all inside the project. This says
    /// nothing about other files; see [`Self::summary`].
    pub fn custom_nodes_portable(&self) -> bool {
        self.external_custom_nodes == 0
    }

    /// One concise line. Never claims the whole project is portable: only
    /// custom nodes and tracked asset paths are checked.
    pub fn summary(&self) -> String {
        let mut text = format!(
            "Saved. {} verified package(s) bundled ({} newly copied).",
            self.bundled, self.copied
        );
        if self.custom_nodes_portable() {
            text.push_str(" Custom nodes: fully portable.");
        } else {
            text.push_str(&format!(
                " Custom nodes: not portable, {} path-only node(s) outside the project.",
                self.external_custom_nodes
            ));
        }
        if self.external_assets > 0 {
            text.push_str(&format!(
                " {} other file(s) are outside the project.",
                self.external_assets
            ));
        } else {
            text.push_str(
                " Other dependencies (plugins, devices, system resources) are not checked.",
            );
        }
        text
    }
}

/// Outcome of a committed save.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveReport {
    pub copied: usize,
    pub reused: usize,
}

/// `packages/<id>/<version>/<entry>`, `/`-separated.
pub fn project_entry_path(reference: &PackageRef) -> String {
    format!(
        "{PACKAGES_DIR}/{}/{}/{}",
        reference.id, reference.version, reference.entry
    )
}

fn project_version_dir(project: &Path, reference: &PackageRef) -> PathBuf {
    package::version_dir(project, &reference.id, &reference.version)
}

// ── Planning ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Inside a containing package's bundle (copied with it).
    Bundled,
    /// Already in the destination project.
    Project,
    /// Elsewhere (current project when saving as, or the Library).
    External,
}

struct Planner<'a> {
    dest: &'a Path,
    source_project: Option<&'a Path>,
    library: Option<&'a Path>,
    /// Content identity per id@version, across every route.
    content: BTreeMap<(String, semver::Version), (ContentId, PathBuf)>,
    /// (id@version, folder) pairs whose dependencies were already walked.
    walked: HashSet<(String, semver::Version, PathBuf)>,
    copies: BTreeMap<(String, semver::Version), PackageCopy>,
    reused: BTreeMap<(String, semver::Version), PackageRef>,
    problems: Vec<String>,
    warnings: Vec<String>,
    external_custom_nodes: Vec<String>,
    loose_seen: HashSet<PathBuf>,
}

/// Plan the packages a project saved to `dest` needs. `source_project` is
/// the folder the project was loaded from (Save As), `library` the Library
/// root. Reads only; never modifies anything.
pub fn plan_project_packages(
    graph: &Graph,
    dest: &Path,
    source_project: Option<&Path>,
    library: Option<&Path>,
) -> Result<ProjectPackagePlan, String> {
    let mut planner = Planner {
        dest,
        source_project: source_project.filter(|p| !same_dir(p, dest)),
        library,
        content: BTreeMap::new(),
        walked: HashSet::new(),
        copies: BTreeMap::new(),
        reused: BTreeMap::new(),
        problems: Vec::new(),
        warnings: Vec::new(),
        external_custom_nodes: Vec::new(),
        loose_seen: HashSet::new(),
    };
    let mut plan = ProjectPackagePlan::default();
    planner.scan_graph(graph, 0, Some(&mut plan.instance_files));

    if !planner.problems.is_empty() {
        return Err(format!(
            "The project was not saved because some packages it uses cannot be bundled:\n• {}",
            planner.problems.join("\n• ")
        ));
    }
    plan.to_copy = planner.copies.into_values().collect();
    plan.reused = planner.reused.into_values().collect();
    plan.warnings = planner.warnings;
    plan.external_custom_nodes = planner.external_custom_nodes;
    Ok(plan)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// True when `path` lies in `dir`. Both are compared canonically when
/// both exist, otherwise both as written (never mixed).
pub(crate) fn is_within(path: &Path, dir: &Path) -> bool {
    match (std::fs::canonicalize(path), std::fs::canonicalize(dir)) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => path.starts_with(dir),
    }
}

fn node_label(file: &str, id: NodeId) -> String {
    let name = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Custom node".to_string());
    format!("'{name}' (node #{id})")
}

impl Planner<'_> {
    fn scan_graph(
        &mut self,
        graph: &Graph,
        depth: usize,
        mut instance_files: Option<&mut BTreeMap<NodeId, String>>,
    ) {
        let mut ids: Vec<NodeId> = graph.nodes.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let NodeType::CustomNodeInstance { file, package, .. } = &graph.nodes[&id].node_type
            else {
                continue;
            };
            match package {
                Some(stored) => {
                    if let Some(reference) = self.package_instance(id, file, stored) {
                        if let Some(files) = instance_files.as_deref_mut() {
                            files.insert(id, project_entry_path(&reference));
                        }
                    }
                }
                None => self.loose_instance(id, file, depth),
            }
        }
    }

    /// A packaged instance: check its file does not name another package
    /// (Stage 2 conflict rule), resolve the exact version and walk it.
    fn package_instance(
        &mut self,
        id: NodeId,
        file: &str,
        stored: &InstancePackageRef,
    ) -> Option<PackageRef> {
        let label = node_label(file, id);
        let reference = match stored.to_ref() {
            Ok(r) => r,
            Err(e) => {
                self.problems.push(format!(
                    "{label} has an invalid package reference {stored}: {e}"
                ));
                return None;
            }
        };
        // Identity check only (no locations): a conflicting file is an error.
        if let Err(error) = PackageResolver::new(None, None).resolve_instance(file, Some(stored)) {
            if error.is_conflict() {
                self.problems.push(format!("{label}: {error}"));
                return None;
            }
        }
        match self.resolve(&reference, None) {
            Ok((found, place)) => {
                if let Err(error) = self.visit(&reference, found, place, 1, &mut Vec::new()) {
                    self.problems.push(format!("{label}: {error}"));
                    return None;
                }
                Some(reference)
            }
            Err(error) => {
                self.problems.push(format!("{label} uses {error}"));
                None
            }
        }
    }

    /// A path-only instance: never copied. Warn when it is not portable and
    /// look inside local custom nodes for packages they use.
    fn loose_instance(&mut self, id: NodeId, file: &str, depth: usize) {
        if file.trim().is_empty() {
            return;
        }
        let path = Path::new(file);
        let label = node_label(file, id);
        let inside = is_within(path, self.dest);
        if package::is_inside_published_package(path) {
            if !inside {
                self.external_custom_nodes.push(file.to_string());
                self.warnings.push(format!(
                    "{label} refers to a Library package by its file path ({file}). \
                     It is not bundled with the project; reinsert it from the Library \
                     to make the project portable."
                ));
            }
            return;
        }
        if !path.is_file() {
            self.external_custom_nodes.push(file.to_string());
            self.warnings.push(format!(
                "{label} refers to a missing custom node file ({file})."
            ));
            return;
        }
        if !inside {
            self.external_custom_nodes.push(file.to_string());
            self.warnings.push(format!(
                "{label} uses the custom node file {file}, which is outside the project \
                 folder and will not be available on other computers."
            ));
        }
        // Packages used inside local custom nodes are bundled too.
        if depth >= MAX_DEPENDENCY_DEPTH {
            return;
        }
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if !self.loose_seen.insert(key) {
            return;
        }
        match crate::document::CustomNodeFile::load(path) {
            Ok(custom) => self.scan_graph(&custom.graph, depth + 1, None),
            Err(error) => self.warnings.push(format!("{label}: {error}")),
        }
    }

    /// Resolve an exact reference: the containing bundle (if any) decides
    /// when present; then the destination project, the source project and
    /// the Library. The first copy that is present decides; a damaged one
    /// is an error, never skipped.
    fn resolve(
        &mut self,
        reference: &PackageRef,
        bundle: Option<&Path>,
    ) -> Result<(ResolvedPackage, Place), ResolveError> {
        let mut searched = Vec::new();
        if let Some(root) = bundle {
            let mut resolver = PackageResolver::new(None, None);
            resolver.set_bundle(Some(root.to_path_buf()));
            match resolver.resolve(reference) {
                Ok(found) => return Ok((found, Place::Bundled)),
                Err(error) if error.kind != ResolveErrorKind::Missing => return Err(error),
                Err(error) => searched.extend(error.searched),
            }
        }
        let mut locations: Vec<(PackageResolver, Place)> = vec![(
            PackageResolver::new(Some(self.dest.to_path_buf()), None),
            Place::Project,
        )];
        if let Some(source) = self.source_project {
            locations.push((
                PackageResolver::new(Some(source.to_path_buf()), None),
                Place::External,
            ));
        }
        if let Some(library) = self.library {
            locations.push((
                PackageResolver::new(None, Some(library.to_path_buf())),
                Place::External,
            ));
        }
        for (mut resolver, place) in locations {
            match resolver.resolve(reference) {
                Ok(found) => return Ok((found, place)),
                Err(error) if error.kind != ResolveErrorKind::Missing => return Err(error),
                Err(error) => searched.extend(error.searched),
            }
        }
        Err(ResolveError {
            reference: reference.to_string(),
            kind: ResolveErrorKind::Missing,
            problems: Vec::new(),
            searched,
        })
    }

    fn visit(
        &mut self,
        reference: &PackageRef,
        found: ResolvedPackage,
        place: Place,
        depth: usize,
        stack: &mut Vec<PackageRef>,
    ) -> Result<(), String> {
        if stack.contains(reference) {
            let chain: Vec<String> = stack
                .iter()
                .chain(std::iter::once(reference))
                .map(|r| format!("{}@{}", r.id, r.version))
                .collect();
            return Err(format!("dependency cycle {}.", chain.join(" → ")));
        }
        if depth > MAX_DEPENDENCY_DEPTH {
            return Err(format!(
                "dependencies are nested more than {MAX_DEPENDENCY_DEPTH} levels deep."
            ));
        }
        let key = (reference.id.clone(), reference.version.clone());
        let content = content_id(&found.manifest);
        // Same id@version from different copies must have the same content.
        match self.content.get(&key) {
            Some((existing, existing_dir)) if *existing != content => {
                return Err(format!(
                    "two copies of {}@{} have different content ({} and {}). \
                     The same package version must be identical everywhere; the project \
                     was not changed.",
                    reference.id,
                    reference.version,
                    existing_dir.display(),
                    found.dir.display()
                ));
            }
            Some(_) => {}
            None => {
                self.content
                    .insert(key.clone(), (content.clone(), found.dir.clone()));
            }
        }
        match place {
            Place::Bundled => {}
            Place::Project => {
                self.reused.insert(key.clone(), reference.clone());
            }
            Place::External => {
                self.copies
                    .entry(key.clone())
                    .or_insert_with(|| PackageCopy {
                        reference: reference.clone(),
                        source_dir: found.dir.clone(),
                        content: content.clone(),
                    });
            }
        }
        if !self
            .walked
            .insert((key.0.clone(), key.1.clone(), found.dir.clone()))
        {
            return Ok(());
        }

        // Dependencies, resolved like the runtime does (bundle root first).
        let entry = found.dir.join(&found.manifest.entry);
        let bundle = package::containing_package_root(&entry);
        let raw: crate::document::CustomNodeFile = std::fs::read_to_string(&entry)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            .map_err(|e| format!("{reference} cannot be read: {e}"))?;
        let mut nested: Vec<(NodeId, InstancePackageRef)> = raw
            .graph
            .nodes
            .iter()
            .filter_map(|(&id, node)| match &node.node_type {
                NodeType::CustomNodeInstance {
                    package: Some(p), ..
                } => Some((id, p.clone())),
                _ => None,
            })
            .collect();
        nested.sort_unstable_by_key(|(id, _)| *id);
        stack.push(reference.clone());
        for (_, stored) in nested {
            let child = stored
                .to_ref()
                .map_err(|e| format!("{reference} has an invalid nested reference: {e}"))?;
            let (child_found, child_place) = self
                .resolve(&child, bundle.as_deref())
                .map_err(|e| format!("{reference} needs {e}"))?;
            self.visit(&child, child_found, child_place, depth + 1, stack)?;
        }
        stack.pop();
        Ok(())
    }
}

// ── Committing ───────────────────────────────────────────────────────────

/// Test hook: fail at a commit step to simulate an interruption.
#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_AT: std::cell::Cell<Option<FailPoint>> = const { std::cell::Cell::new(None) };
}

/// Commit steps at which tests simulate an interruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailPoint {
    /// After every package was copied into staging and verified.
    AfterStaging,
    /// After the first package folder was moved into the project.
    AfterFirstPackage,
}

#[cfg(test)]
fn fail_at(point: FailPoint) -> Result<(), String> {
    if FAIL_AT.with(|f| f.get()) == Some(point) {
        return Err(format!("simulated interruption at {point:?}"));
    }
    Ok(())
}

#[cfg(not(test))]
#[inline]
fn fail_at(_point: FailPoint) -> Result<(), String> {
    Ok(())
}

/// Copy a verified version folder (regular files and folders only) with
/// exclusive creates.
fn copy_tree(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target)
        .map_err(|e| format!("Cannot create {}: {e}", target.display()))?;
    let entries =
        std::fs::read_dir(source).map_err(|e| format!("Cannot read {}: {e}", source.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "{} is a link and cannot be bundled.",
                from.display()
            ));
        }
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            let mut input = std::fs::File::open(&from)
                .map_err(|e| format!("Cannot read {}: {e}", from.display()))?;
            super::copy_exclusive_from(&mut input, &to)?;
        }
    }
    Ok(())
}

/// Leftover staging folders from an interrupted save (never part of the
/// project).
pub fn is_staging_dir(name: &str) -> bool {
    name.starts_with(STAGING_PREFIX)
}

/// Save `project_json` and the planned packages into `dest` atomically (see
/// the module docs). On error nothing that existed before is changed.
pub fn commit_project_save(
    dest: &Path,
    plan: &ProjectPackagePlan,
    project_json: &str,
) -> Result<SaveReport, String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("Cannot create the project folder: {e}"))?;
    let staging = dest.join(format!(
        "{STAGING_PREFIX}{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir(&staging).map_err(|e| format!("Cannot create the staging folder: {e}"))?;

    // Folders this save added to the project (removed again on failure).
    let mut added: Vec<PathBuf> = Vec::new();
    let result = (|| -> Result<SaveReport, String> {
        // 1. Copy and verify every new package in staging.
        let mut staged = Vec::new();
        for copy in &plan.to_copy {
            let staged_dir = project_version_dir(&staging, &copy.reference);
            copy_tree(&copy.source_dir, &staged_dir)?;
            let verified = package::verify_version_dir(&staged_dir, true)
                .map_err(|e| format!("The copy of {} did not verify: {e}", copy.reference))?;
            if content_id(&verified.manifest) != copy.content {
                return Err(format!(
                    "{} changed while it was being copied; the project was not saved.",
                    copy.reference
                ));
            }
            staged.push((copy, staged_dir));
        }
        fail_at(FailPoint::AfterStaging)?;

        // 2. The new project.json, staged next to the packages.
        let staged_json = staging.join("project.json");
        super::copy_exclusive_from(&mut project_json.as_bytes(), &staged_json)?;

        // 3. Add package folders (never replacing an existing one).
        for (copy, staged_dir) in &staged {
            let target = project_version_dir(dest, &copy.reference);
            if std::fs::symlink_metadata(&target).is_ok() {
                // Appeared since planning: only identical content is accepted.
                let same = package::verify_version_dir(&target, true)
                    .map(|v| content_id(&v.manifest) == copy.content)
                    .unwrap_or(false);
                if same {
                    continue;
                }
                return Err(format!(
                    "The project already contains a different {} at {}; it was not \
                     overwritten and the project was not saved.",
                    copy.reference,
                    target.display()
                ));
            }
            let parent = target.parent().ok_or("Invalid package folder.")?;
            let mut created = Vec::new();
            for dir in [parent.parent(), Some(parent)].into_iter().flatten() {
                if !dir.exists() {
                    std::fs::create_dir(dir)
                        .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
                    created.push(dir.to_path_buf());
                }
            }
            std::fs::rename(staged_dir, &target)
                .map_err(|e| format!("Cannot add {} to the project: {e}", copy.reference))?;
            added.push(target);
            added.extend(created.into_iter().rev());
            fail_at(FailPoint::AfterFirstPackage)?;
        }

        // 4. Replace project.json last: it only ever names packages that
        //    are already in place.
        std::fs::rename(&staged_json, dest.join("project.json"))
            .map_err(|e| format!("Cannot write project.json: {e}"))?;
        Ok(SaveReport {
            copied: plan.to_copy.len(),
            reused: plan.reused.len(),
        })
    })();

    if result.is_err() {
        // Undo only what this save added; nothing pre-existing is touched.
        // Package folders first, then (newest first) the parent folders
        // this save created, which are empty by then.
        for path in &added {
            if path.join(package::MANIFEST_FILE).exists() {
                let _ = std::fs::remove_dir_all(path);
            }
        }
        for path in added.iter().rev() {
            if !path.join(package::MANIFEST_FILE).exists() {
                let _ = std::fs::remove_dir(path);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
    if result.is_ok() {
        // Leftovers of earlier interrupted saves.
        if let Ok(entries) = std::fs::read_dir(dest) {
            for entry in entries.flatten() {
                if is_staging_dir(&entry.file_name().to_string_lossy()) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }
    result
}
