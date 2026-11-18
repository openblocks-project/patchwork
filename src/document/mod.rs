use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::app::undo::UndoHistory;
use crate::egui::Vec2;
use crate::graph::{Graph, NodeId};

pub mod manager;

pub use manager::DocumentManager;

/// P4.3.2F: the rights a published package version open in a tab runs
/// with now (its authorization as a package runtime), or why it may not run.
pub fn authorize_package_tab(
    policy: &crate::library::trust::authorize::TrustPolicy,
    entry: &std::path::Path,
) -> Result<crate::capability::CapSet, String> {
    let mut resolver =
        crate::library::resolve::PackageResolver::new(None, crate::library::library_root().ok());
    resolver.set_trust(policy.clone());
    crate::library::trust::authorize::authorize_entry(policy, entry, &resolver)
        .map(|authorized| authorized.capabilities)
        .map_err(|blocked| blocked.message())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Project,
    CustomNode,
}

#[cfg(test)]
thread_local! {
    /// Number of `.pwnode` loads on this thread (tests use it to prove
    /// that cached failures cause no repeated file reads).
    pub static LOAD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// P4.3.2A: `.pwnode` files read statically (no node construction).
    pub static STATIC_READ_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomNodeFile {
    pub version: u32,
    pub name: String,
    pub graph: Graph,
    #[serde(default)]
    pub inputs: Vec<CustomNodePort>,
    #[serde(default)]
    pub outputs: Vec<CustomNodePort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomNodePort {
    pub name: String,
    pub kind: String,
}

/// Build a custom node's external interface from its internal graph.
///
/// Ports are ordered by NodeId, matching how the runtime maps instance
/// ports to internal `CustomInput` / `CustomOutput` nodes.
pub fn custom_node_interface(graph: &Graph) -> (Vec<CustomNodePort>, Vec<CustomNodePort>) {
    let mut interface_nodes: Vec<_> = graph.nodes.values().collect();
    // HashMap iteration order is not stable.
    interface_nodes.sort_by_key(|node| node.id);

    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for node in interface_nodes {
        match &node.node_type {
            crate::graph::NodeType::CustomInput { name, kind } => inputs.push(CustomNodePort {
                name: name.clone(),
                kind: format!("{:?}", kind),
            }),
            crate::graph::NodeType::CustomOutput { name, kind } => outputs.push(CustomNodePort {
                name: name.clone(),
                kind: format!("{:?}", kind),
            }),
            _ => {}
        }
    }
    (inputs, outputs)
}

/// Stage 1 fix: Save As for a custom node. Writes `graph` as a new
/// `.pwnode` at `destination` (named after the file) and returns what was
/// written. Never touches any other file. The caller confirms overwriting
/// an existing destination.
/// Used by tests (the editor records the digest: [`save_custom_node_as_with_digest`]).
#[cfg(test)]
pub fn save_custom_node_as(
    graph: &Graph,
    destination: &std::path::Path,
) -> Result<CustomNodeFile, String> {
    save_custom_node_as_with_digest(graph, destination).map(|(file, _)| file)
}

/// [`save_custom_node_as`], also returning (P4.3.2E1) the content digest of
/// the exact text written.
pub fn save_custom_node_as_with_digest(
    graph: &Graph,
    destination: &std::path::Path,
) -> Result<(CustomNodeFile, String), String> {
    if !Document::path_fits_kind(DocumentKind::CustomNode, destination) {
        return Err("A custom node must be saved as a .pwnode file.".to_string());
    }
    if destination.is_dir() {
        return Err("A folder with that name already exists.".to_string());
    }
    Graph::check_custom_node_content(graph)?;
    crate::graph::group::check_group_dependencies(graph, destination, &[])?;
    let name = destination
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
        .ok_or("Invalid custom node file name.")?;
    let (inputs, outputs) = custom_node_interface(graph);
    let file = CustomNodeFile {
        version: 1,
        name,
        graph: graph.clone(),
        inputs,
        outputs,
    };
    // `save` also refuses published Library package versions.
    let digest = file.save_with_digest(destination)?;
    Ok((file, digest))
}

impl CustomNodeFile {
    /// Used by tests (the editor records the digest: [`Self::save_with_digest`]).
    #[cfg(test)]
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        self.save_with_digest(path).map(drop)
    }

    /// Save, returning (P4.3.2E1) the content digest of the exact text
    /// written, so the caller can record its provenance.
    pub fn save_with_digest(&self, path: &std::path::Path) -> Result<String, String> {
        // P4.1: published Library package versions are immutable from every
        // editing workflow (tab save, grouping, duplicate, ...).
        if crate::library::package::is_inside_published_package(path) {
            return Err(format!(
                "{} is a published Library package version and is read-only. \
                 Use Duplicate for Editing to make an editable copy.",
                path.display()
            ));
        }
        let absolute_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| format!("Cannot determine working directory: {e}"))?
                .join(path)
        };

        let normalized_path = absolute_path
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();

        let normalized_path = std::path::PathBuf::from(normalized_path);

        let parent = normalized_path
            .parent()
            .ok_or("Cannot determine custom node directory")?;

        // Work on a copy so the editor's live graph remains unchanged.
        let mut portable = self.clone();
        for node in portable.graph.nodes.values_mut() {
            if let crate::graph::NodeType::CustomNodeInstance { file, .. } = &mut node.node_type {
                let child = std::path::Path::new(file);
                if child.is_absolute() {
                    if let Ok(relative) = child.strip_prefix(parent) {
                        *file = relative.to_string_lossy().into_owned();
                    }
                }
            }
        }
        let json = serde_json::to_string_pretty(&portable)
            .map_err(|e| format!("Failed to serialize custom node: {e}"))?;

        let digest = crate::library::trust::content::custom_node_digest(&json)?;
        std::fs::write(path, json).map_err(|e| format!("Failed to save custom node: {e}"))?;

        Ok(digest)
    }
    /// Load a custom node, validating nested references with `policy`.
    pub fn load_with_policy(
        path: &std::path::Path,
        policy: &DependencyPolicy,
    ) -> Result<Self, String> {
        check_contained_file(path, policy)?;
        let json = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read custom node: {e}"))?;
        Self::load_text_with_policy(path, &json, policy)
    }

    /// P4.3.2D: build a custom node from text that was already read (and,
    /// for packages, authorized byte for byte), as if loaded from `path`.
    /// This is where node objects are constructed.
    pub fn load_text_with_policy(
        path: &std::path::Path,
        json: &str,
        policy: &DependencyPolicy,
    ) -> Result<Self, String> {
        #[cfg(test)]
        LOAD_COUNT.with(|count| count.set(count.get() + 1));
        let mut custom_node: Self =
            serde_json::from_str(&json).map_err(|e| format!("Failed to parse custom node: {e}"))?;

        if custom_node.version != 1 {
            return Err(format!(
                "Unsupported custom node version: {}",
                custom_node.version
            ));
        }

        custom_node.graph.fix_next_id();

        // Resolve nested custom-node references relative to this .pwnode file,
        // validating every reference before any nested file is touched.
        let parent = path.parent().unwrap_or(std::path::Path::new("."));

        for node in custom_node.graph.nodes.values_mut() {
            if let crate::graph::NodeType::CustomNodeInstance { file, package, .. } =
                &mut node.node_type
            {
                if let Some(resolved) =
                    check_instance_reference(path, parent, file, package.is_some(), policy)?
                {
                    *file = resolved.to_string_lossy().into_owned();
                }
            }
        }

        Ok(custom_node)
    }

    /// P4.3.2A: validate a custom node exactly like [`Self::load_with_policy`]
    /// (format, containment, every nested reference) but read it as plain
    /// JSON: no `NodeType` or dynamic node is constructed, so no node code
    /// runs. Used by package verification.
    pub fn check_static(
        path: &std::path::Path,
        policy: &DependencyPolicy,
    ) -> Result<RawCustomNode, String> {
        check_contained_file(path, policy)?;
        let raw = read_custom_node_static(path)?;
        if raw.version != 1 {
            return Err(format!("Unsupported custom node version: {}", raw.version));
        }
        let parent = path.parent().unwrap_or(std::path::Path::new("."));
        for instance in &raw.instances {
            check_instance_reference(
                path,
                parent,
                &instance.file,
                instance.package.is_some(),
                policy,
            )?;
        }
        Ok(raw)
    }

    /// Load a local custom node (the user's own files and Library).
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        Self::load_with_policy(path, &DependencyPolicy::TrustedLocal)
    }
}

/// A `Contained` file must resolve inside its root.
fn check_contained_file(path: &std::path::Path, policy: &DependencyPolicy) -> Result<(), String> {
    if let DependencyPolicy::Contained { root } = policy {
        let canonical_root = std::fs::canonicalize(root)
            .map_err(|e| format!("Cannot resolve dependency folder {}: {e}", root.display()))?;
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| format!("Cannot resolve custom node {}: {e}", path.display()))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(format!("{} is outside its package folder.", path.display()));
        }
    }
    Ok(())
}

/// The rules for one nested instance reference in the `.pwnode` at `path`
/// (shared by the loader and the static check). `Ok(Some(path))` is the
/// path to record, `Ok(None)` keeps the reference unchanged.
fn check_instance_reference(
    path: &std::path::Path,
    parent: &std::path::Path,
    file: &str,
    packaged: bool,
    policy: &DependencyPolicy,
) -> Result<Option<PathBuf>, String> {
    if file.is_empty() && *policy == DependencyPolicy::TrustedLocal {
        // Unchanged legacy behaviour for an unset reference.
        return Ok(None);
    }
    // P4.2 Stage 3: in a package, a packaged nested instance is resolved by
    // its reference (often from the containing package's deps/), so its
    // compatibility path may be absent. It must still be a plain path
    // inside the folder.
    if packaged && matches!(policy, DependencyPolicy::Contained { .. }) {
        if file.is_empty() {
            return Ok(None);
        }
        let lexical = resolve_nested_reference(parent, file, &DependencyPolicy::TrustedLocal)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if std::path::Path::new(file).is_absolute() {
            return Err(format!(
                "{}: Nested custom node uses an absolute path ({file}).",
                path.display()
            ));
        }
        let resolved = if lexical.exists() {
            resolve_nested_reference(parent, file, policy)
                .map_err(|e| format!("{}: {e}", path.display()))?
        } else {
            lexical
        };
        return Ok(Some(resolved));
    }
    match resolve_nested_reference(parent, file, policy) {
        Ok(resolved) => Ok(Some(resolved)),
        // P4.2 Stage 2: in a local file, a packaged instance is identified
        // by its package; an unusable compatibility path (e.g. written on
        // another OS) is kept as-is and the package is resolved instead.
        Err(_) if packaged && *policy == DependencyPolicy::TrustedLocal => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// P4.3.2A: one nested custom-node instance, read without constructing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawInstance {
    pub file: String,
    pub package: Option<crate::library::resolve::InstancePackageRef>,
}

/// P4.3.2A: what package verification needs from a `.pwnode`, read as
/// plain JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCustomNode {
    pub version: u64,
    pub instances: Vec<RawInstance>,
}

// Shadow of the `.pwnode` layout with the loader's required fields
// (`CustomNodeFile`, `Graph`, `Node`, `Connection`) but every node type kept
// as raw JSON, so reading it constructs no node.
#[derive(Deserialize)]
struct ShadowFile {
    version: u32,
    #[allow(dead_code)]
    name: String,
    graph: ShadowGraph,
    #[serde(default)]
    #[allow(dead_code)]
    inputs: Vec<CustomNodePort>,
    #[serde(default)]
    #[allow(dead_code)]
    outputs: Vec<CustomNodePort>,
}

#[derive(Deserialize)]
struct ShadowGraph {
    nodes: std::collections::HashMap<crate::graph::NodeId, ShadowNode>,
    #[allow(dead_code)]
    connections: Vec<crate::graph::Connection>,
    #[allow(dead_code)]
    next_id: u64,
}

#[derive(Deserialize)]
struct ShadowNode {
    #[allow(dead_code)]
    id: crate::graph::NodeId,
    node_type: serde_json::Value,
    #[allow(dead_code)]
    pos: [f32; 2],
}

/// `NodeType::CustomNodeInstance`'s fields, with the same requirements.
#[derive(Deserialize)]
struct ShadowInstance {
    file: String,
    #[allow(dead_code)]
    inputs: Vec<CustomNodePort>,
    #[allow(dead_code)]
    outputs: Vec<CustomNodePort>,
    #[serde(default)]
    package: Option<crate::library::resolve::InstancePackageRef>,
}

/// P4.3.2B: check one node's serialized type against the loader's own
/// definitions without running node code. Built-in variants are plain data
/// (no custom deserializers; skipped fields default to plain values —
/// guarded by a test), so they are checked with the real `NodeType`
/// definition. `Dynamic` is never deserialized here (that would run a
/// registry factory): only its required `inner` object is checked.
fn check_node_payload_static(node_type: &serde_json::Value) -> Result<(), String> {
    if let Some(dynamic) = node_type.get("Dynamic") {
        let valid = node_type.as_object().is_some_and(|m| m.len() == 1)
            && dynamic
                .get("inner")
                .is_some_and(serde_json::Value::is_object);
        return if valid {
            Ok(())
        } else {
            Err("Failed to parse custom node: a dynamic node has no `inner` object".to_string())
        };
    }
    serde_json::from_value::<crate::graph::NodeType>(node_type.clone())
        .map(drop)
        .map_err(|e| format!("Failed to parse custom node: invalid node: {e}"))
}

/// P4.3.2A: read a `.pwnode` without constructing any node and extract its
/// nested instances. The file layout (format version, name, graph, node
/// ids, positions, connections, instance fields) is checked with the same
/// required fields as the loader; every other node type stays raw JSON
/// (a malformed one is reported when the graph is loaded for use).
pub fn read_custom_node_static(path: &std::path::Path) -> Result<RawCustomNode, String> {
    #[cfg(test)]
    STATIC_READ_COUNT.with(|count| count.set(count.get() + 1));
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("Failed to read custom node: {e}"))?;
    read_custom_node_static_text(&text)
}

/// P4.3.2D: a custom node's name and external ports, read statically
/// (nothing is constructed). Used to insert or re-target an instance of a
/// package before its runtime is authorized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticInterface {
    pub name: String,
    pub inputs: Vec<CustomNodePort>,
    pub outputs: Vec<CustomNodePort>,
}

pub fn read_custom_node_interface_static(
    path: &std::path::Path,
) -> Result<StaticInterface, String> {
    #[derive(Deserialize)]
    struct Interface {
        name: String,
        #[serde(default)]
        inputs: Vec<CustomNodePort>,
        #[serde(default)]
        outputs: Vec<CustomNodePort>,
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("Failed to read custom node: {e}"))?;
    let raw = read_custom_node_static_text(&text)?;
    if raw.version != 1 {
        return Err(format!("Unsupported custom node version: {}", raw.version));
    }
    let interface: Interface =
        serde_json::from_str(&text).map_err(|e| format!("Failed to parse custom node: {e}"))?;
    Ok(StaticInterface {
        name: interface.name,
        inputs: interface.inputs,
        outputs: interface.outputs,
    })
}

/// P4.3.2D: [`read_custom_node_static`] on text already in memory.
pub fn read_custom_node_static_text(text: &str) -> Result<RawCustomNode, String> {
    let file: ShadowFile =
        serde_json::from_str(text).map_err(|e| format!("Failed to parse custom node: {e}"))?;
    let mut instances = Vec::new();
    for node in file.graph.nodes.values() {
        check_node_payload_static(&node.node_type)?;
        let Some(body) = node.node_type.get("CustomNodeInstance") else {
            continue;
        };
        let instance: ShadowInstance = serde_json::from_value(body.clone()).map_err(|e| {
            format!("Failed to parse custom node: invalid custom node instance: {e}")
        })?;
        instances.push(RawInstance {
            file: instance.file,
            package: instance.package,
        });
    }
    Ok(RawCustomNode {
        version: u64::from(file.version),
        instances,
    })
}

/// How strictly nested `.pwnode` references inside a file are validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyPolicy {
    /// The user's own local files. Absolute references are accepted for
    /// compatibility (older files and nodes outside the file's folder);
    /// relative references must not climb out with `..` or be rooted.
    TrustedLocal,
    /// Imported / downloaded files (P4.3+). References must be relative,
    /// must exist, and must resolve inside `root`.
    #[allow(dead_code)]
    Contained { root: PathBuf },
}

/// Validate one nested reference from a `.pwnode` in folder `parent` and
/// return the path to load. Does not read the referenced file.
pub fn resolve_nested_reference(
    parent: &std::path::Path,
    reference: &str,
    policy: &DependencyPolicy,
) -> Result<PathBuf, String> {
    use std::path::Component;

    if reference.trim().is_empty() {
        return Err("Nested custom node has an empty file reference.".to_string());
    }
    let child = std::path::Path::new(reference);

    if child.is_absolute() {
        return match policy {
            DependencyPolicy::TrustedLocal => Ok(child.to_path_buf()),
            DependencyPolicy::Contained { .. } => Err(format!(
                "Nested custom node uses an absolute path ({reference}), \
                 which is not allowed in imported nodes."
            )),
        };
    }

    for component in child.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "Nested custom node path '{reference}' uses '..' to leave its folder."
                ));
            }
            // "\foo" or "C:foo" on Windows: rooted but not absolute.
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "Nested custom node path '{reference}' is not a plain relative path."
                ));
            }
        }
    }

    let resolved = parent.join(child);
    if let DependencyPolicy::Contained { root } = policy {
        if !resolved.is_file() {
            return Err(format!("Missing nested custom node: {reference}"));
        }
        let canonical_root = std::fs::canonicalize(root)
            .map_err(|e| format!("Cannot resolve dependency folder {}: {e}", root.display()))?;
        let canonical = std::fs::canonicalize(&resolved)
            .map_err(|e| format!("Cannot resolve nested custom node {reference}: {e}"))?;
        // Also catches symlinks pointing outside the root.
        if !canonical.starts_with(&canonical_root) {
            return Err(format!(
                "Nested custom node '{reference}' resolves outside its package folder."
            ));
        }
        return Ok(canonical);
    }
    Ok(resolved)
}
pub struct Document {
    pub id: u64,
    pub kind: DocumentKind,
    pub title: String,
    pub path: Option<PathBuf>,

    pub graph: Graph,

    pub selected_nodes: HashSet<NodeId>,
    pub pinned_nodes: HashSet<NodeId>,

    pub canvas_offset: Vec2,
    pub canvas_zoom: f32,
    pub target_zoom: f32,

    pub is_dirty: bool,
    pub undo_history: UndoHistory,
    /// P4.1: a published Library package version; never editable or saved.
    pub read_only: bool,
}

impl Document {
    pub fn new(id: u64, title: impl Into<String>) -> Self {
        Self {
            id,
            kind: DocumentKind::Project,
            title: title.into(),
            path: None,

            graph: Graph::new(),
            undo_history: UndoHistory::new(50),

            selected_nodes: HashSet::new(),
            pinned_nodes: HashSet::new(),

            canvas_offset: Vec2::ZERO,
            canvas_zoom: 1.0,
            target_zoom: 1.0,

            is_dirty: false,
            read_only: false,
        }
    }

    /// Stage 0: read-only documents (published package versions) never
    /// accept graph changes.
    pub fn is_editable(&self) -> bool {
        !self.read_only
    }

    /// Stage 1 fix: whether `path` is a valid location for a document of
    /// `kind`. Custom nodes live in `.pwnode` files; projects never do.
    pub fn path_fits_kind(kind: DocumentKind, path: &std::path::Path) -> bool {
        let is_pwnode = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pwnode"));
        match kind {
            DocumentKind::CustomNode => is_pwnode,
            DocumentKind::Project => !is_pwnode,
        }
    }

    /// Change the document's location. Refuses a path that does not fit the
    /// document kind, so a document is never silently converted.
    pub fn set_path(&mut self, path: Option<PathBuf>) -> Result<(), String> {
        if let Some(path) = &path {
            if !Self::path_fits_kind(self.kind, path) {
                return Err(format!(
                    "{} cannot be the location of this {} document.",
                    path.display(),
                    match self.kind {
                        DocumentKind::CustomNode => "custom-node",
                        DocumentKind::Project => "project",
                    }
                ));
            }
        }
        self.path = path;
        Ok(())
    }

    /// For a read-only document, replace `live` with the stored
    /// (authoritative) graph. Returns false for editable documents, whose
    /// live graph is the newer one.
    pub fn restore_into(&self, live: &mut Graph) -> bool {
        if !self.read_only {
            return false;
        }
        *live = self.graph.clone();
        true
    }

    /// Store the editor's live graph into this document. A read-only
    /// document keeps the graph it was loaded with and reports `false`,
    /// so in-memory edits can never become part of it.
    pub fn store_graph(&mut self, graph: &Graph) -> bool {
        if self.read_only {
            return false;
        }
        self.graph = graph.clone();
        true
    }
}

#[cfg(test)]
mod dependency_tests {
    use super::*;
    use crate::graph::NodeType;
    use std::path::Path;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patchwork_deps_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn empty_node(name: &str) -> CustomNodeFile {
        CustomNodeFile {
            version: 1,
            name: name.into(),
            graph: Graph::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// Write a .pwnode whose single nested instance references `reference`
    /// verbatim (bypassing `save`, as a downloaded file would).
    fn write_outer(path: &Path, reference: &str) {
        let mut outer = empty_node("Outer");
        outer.graph.add_node(
            NodeType::CustomNodeInstance {
                file: reference.to_string(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                package: None,
            },
            [0.0, 0.0],
        );
        std::fs::write(path, serde_json::to_string(&outer).unwrap()).unwrap();
    }

    fn nested_file(file: &CustomNodeFile) -> String {
        file.graph
            .nodes
            .values()
            .find_map(|n| match &n.node_type {
                NodeType::CustomNodeInstance { file, .. } => Some(file.clone()),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn relative_nested_reference_is_accepted() {
        let tmp = TempDir::new("relative");
        std::fs::create_dir_all(tmp.0.join("sub")).unwrap();
        empty_node("Inner")
            .save(&tmp.0.join("sub").join("inner.pwnode"))
            .unwrap();
        let outer = tmp.0.join("outer.pwnode");
        write_outer(&outer, "sub/inner.pwnode");

        let trusted = CustomNodeFile::load(&outer).unwrap();
        assert!(Path::new(&nested_file(&trusted)).is_file());

        let policy = DependencyPolicy::Contained {
            root: tmp.0.clone(),
        };
        let contained = CustomNodeFile::load_with_policy(&outer, &policy).unwrap();
        assert!(Path::new(&nested_file(&contained)).is_file());
    }

    #[test]
    fn parent_dir_traversal_is_rejected() {
        let tmp = TempDir::new("traversal");
        let outer = tmp.0.join("outer.pwnode");
        for reference in [
            "../secret.pwnode",
            "sub/../../secret.pwnode",
            r"..\x.pwnode",
        ] {
            write_outer(&outer, reference);
            if cfg!(not(windows)) && reference.contains('\\') {
                continue; // backslash is a normal character on Unix
            }
            let error = CustomNodeFile::load(&outer).unwrap_err();
            assert!(error.contains(".."), "{reference}: {error}");
            let policy = DependencyPolicy::Contained {
                root: tmp.0.clone(),
            };
            assert!(CustomNodeFile::load_with_policy(&outer, &policy).is_err());
        }
    }

    #[test]
    fn rooted_relative_reference_is_rejected() {
        let error = resolve_nested_reference(
            Path::new("base"),
            if cfg!(windows) {
                r"\inner.pwnode"
            } else {
                "/inner.pwnode"
            },
            &DependencyPolicy::Contained {
                root: PathBuf::from("base"),
            },
        )
        .unwrap_err();
        assert!(
            error.contains("not") || error.contains("absolute"),
            "{error}"
        );
        if cfg!(windows) {
            assert!(
                resolve_nested_reference(
                    Path::new("base"),
                    "C:inner.pwnode",
                    &DependencyPolicy::TrustedLocal
                )
                .is_err()
            );
        }
    }

    #[test]
    fn absolute_reference_rejected_only_for_imported_nodes() {
        let tmp = TempDir::new("absolute");
        let inner = tmp.0.join("inner.pwnode");
        empty_node("Inner").save(&inner).unwrap();
        let outer = tmp.0.join("outer.pwnode");
        let absolute = inner.to_string_lossy().into_owned();
        write_outer(&outer, &absolute);

        // Legitimate local file with an absolute reference keeps working.
        let trusted = CustomNodeFile::load(&outer).unwrap();
        assert_eq!(nested_file(&trusted), absolute);

        let policy = DependencyPolicy::Contained {
            root: tmp.0.clone(),
        };
        let error = CustomNodeFile::load_with_policy(&outer, &policy).unwrap_err();
        assert!(error.contains("absolute"), "{error}");
    }

    #[test]
    fn missing_nested_dependency() {
        let tmp = TempDir::new("missing");
        let outer = tmp.0.join("outer.pwnode");
        write_outer(&outer, "missing.pwnode");

        // Local load still succeeds (instance reports a missing file at
        // runtime, as before); the dependency check reports it clearly.
        assert!(CustomNodeFile::load(&outer).is_ok());
        let error =
            crate::graph::Graph::check_custom_node_dependencies(&outer, &mut HashSet::new(), 0)
                .unwrap_err();
        assert!(error.contains("missing.pwnode"), "{error}");

        let policy = DependencyPolicy::Contained {
            root: tmp.0.clone(),
        };
        let error = CustomNodeFile::load_with_policy(&outer, &policy).unwrap_err();
        assert!(error.contains("Missing"), "{error}");
    }

    #[test]
    fn contained_policy_rejects_file_outside_root() {
        let tmp = TempDir::new("outside");
        std::fs::create_dir_all(tmp.0.join("pkg")).unwrap();
        let outer = tmp.0.join("outer.pwnode");
        empty_node("Outer").save(&outer).unwrap();
        let policy = DependencyPolicy::Contained {
            root: tmp.0.join("pkg"),
        };
        assert!(CustomNodeFile::load_with_policy(&outer, &policy).is_err());
    }

    #[test]
    fn saved_nested_nodes_round_trip_under_validation() {
        // Files written by `save` (P2 grouping, tab save) stay loadable.
        let tmp = TempDir::new("roundtrip");
        let inner = tmp.0.join("inner.pwnode");
        empty_node("Inner").save(&inner).unwrap();
        let mut outer = empty_node("Outer");
        outer.graph.add_node(
            NodeType::CustomNodeInstance {
                file: inner.to_string_lossy().into_owned(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                package: None,
            },
            [0.0, 0.0],
        );
        let outer_path = tmp.0.join("outer.pwnode");
        outer.save(&outer_path).unwrap();
        let loaded = CustomNodeFile::load(&outer_path).unwrap();
        assert!(Path::new(&nested_file(&loaded)).is_file());
        let policy = DependencyPolicy::Contained {
            root: tmp.0.clone(),
        };
        assert!(CustomNodeFile::load_with_policy(&outer_path, &policy).is_ok());
    }
}
