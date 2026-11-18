use super::{CustomNodeFile, Document, DocumentKind};
use crate::graph::{Graph, NodeType};
use std::collections::HashMap;
use std::path::PathBuf;

pub struct DocumentManager {
    pub documents: Vec<Document>,
    pub active: usize,
    next_id: u64,
    /// Cache of custom-node file references resolved to canonical paths.
    /// Avoids filesystem lookups every frame when counting usages.
    resolved_paths: HashMap<String, Option<PathBuf>>,
    /// While true (quit dialog open) documents cannot be activated, closed
    /// or opened, except through `activate_for_quit`.
    locked: bool,
    /// P4.3.2D: authorizes package versions opened in a tab (their graph
    /// becomes the evaluated graph, so it is a package runtime too).
    trust: crate::library::trust::authorize::TrustPolicy,
    /// The package refused by the last `open_custom_node`, if any.
    blocked: Option<crate::library::trust::authorize::Blocked>,
}

impl DocumentManager {
    pub fn new() -> Self {
        Self {
            documents: vec![Document::new(1, "Untitled")],
            active: 0,
            next_id: 2,
            resolved_paths: HashMap::new(),
            locked: false,
            trust: Default::default(),
            blocked: None,
        }
    }

    /// Lock or unlock document changes (quit dialog).
    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn active(&self) -> &Document {
        &self.documents[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Document {
        &mut self.documents[self.active]
    }

    pub fn new_document(&mut self, title: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        self.documents.push(Document::new(id, title));

        id
    }

    pub fn new_custom_node_document(&mut self, title: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        let mut document = Document::new(id, title);
        document.kind = super::DocumentKind::CustomNode;

        self.documents.push(document);

        id
    }
    pub fn set_trust_policy(&mut self, policy: crate::library::trust::authorize::TrustPolicy) {
        if self.trust != policy {
            self.trust = policy;
        }
    }

    /// The package refused by the last [`Self::open_custom_node`].
    pub fn take_blocked(&mut self) -> Option<crate::library::trust::authorize::Blocked> {
        self.blocked.take()
    }

    pub fn open_custom_node(&mut self, path: PathBuf) -> Result<u64, String> {
        if self.locked {
            return Err("Documents cannot be opened while the quit dialog is open.".to_string());
        }
        let canonical_path =
            std::fs::canonicalize(&path).map_err(|e| format!("Invalid custom node path: {e}"))?;

        // Reuse an existing tab if this file is already open.
        if let Some(document) = self.documents.iter().find(|doc| {
            doc.kind == DocumentKind::CustomNode && doc.path.as_ref() == Some(&canonical_path)
        }) {
            return Ok(document.id);
        }

        // P4.3.2D: a published package version (Library, project copy or a
        // bundled dependency) is authorized like any package runtime, and
        // the tab is built from the authorized bytes. Plain custom-node
        // files are the user's own and open as before.
        self.blocked = None;
        let custom_node = if crate::library::package::is_inside_published_package(&canonical_path) {
            let mut resolver = crate::library::resolve::PackageResolver::new(
                None,
                crate::library::library_root().ok(),
            );
            resolver.set_trust(self.trust.clone());
            match crate::library::trust::authorize::authorize_entry(
                &self.trust,
                &canonical_path,
                &resolver,
            ) {
                Ok(authorized) => {
                    let mut file = CustomNodeFile::load_text_with_policy(
                        &canonical_path,
                        &authorized.entry_text,
                        &super::DependencyPolicy::TrustedLocal,
                    )?;
                    // Authorized as a package: its graph may run, with the
                    // package's rights (P4.3.2E2), while the package's
                    // authorization stands (P4.3.2F).
                    file.graph.content = crate::library::trust::content::ContentState::Allowed(
                        authorized.capabilities,
                    );
                    file.graph.content_basis =
                        Some(crate::library::trust::content::ContentBasis::Package {
                            entry: canonical_path.clone(),
                            seen: Some(authorized.capabilities),
                        });
                    file
                }
                Err(blocked) => {
                    let message = blocked.message();
                    self.blocked = Some(blocked);
                    return Err(message);
                }
            }
        } else {
            // P4.3.2E1: a loose custom node is identified by its content
            // and runs (as the tab's graph) only if authored or allowed.
            let text = std::fs::read_to_string(&canonical_path)
                .map_err(|e| format!("Failed to read custom node: {e}"))?;
            let mut file = CustomNodeFile::load_text_with_policy(
                &canonical_path,
                &text,
                &super::DependencyPolicy::TrustedLocal,
            )?;
            file.graph.content = crate::library::trust::content::ContentState::loaded(
                crate::library::trust::content::ContentKind::CustomNode,
                &text,
            );
            file
        };

        let id = self.new_custom_node_document(custom_node.name);

        let document = self
            .documents
            .iter_mut()
            .find(|doc| doc.id == id)
            .expect("Newly created document must exist");

        document.graph = custom_node.graph;
        // P4.1: published package versions open read-only.
        // Published packages and their bundled dependencies are read-only.
        document.read_only = crate::library::package::is_inside_published_package(&canonical_path);
        document.path = Some(canonical_path);
        document.is_dirty = false;

        Ok(id)
    }
    /// Count how many custom-node instances in the open documents reference
    /// each open custom-node document. Returns document id -> usage count for
    /// every custom-node tab that has a path (including zero counts).
    ///
    /// `active_graph` is the live graph of the active document, which is newer
    /// than the copy stored in `documents[active]`.
    pub fn custom_node_usage(&mut self, active_graph: &Graph) -> HashMap<u64, usize> {
        let targets: Vec<(u64, PathBuf)> = self
            .documents
            .iter()
            .filter(|doc| doc.kind == DocumentKind::CustomNode)
            .filter_map(|doc| Some((doc.id, doc.path.clone()?)))
            .collect();

        let mut usage: HashMap<u64, usize> = targets.iter().map(|(id, _)| (*id, 0)).collect();
        if targets.is_empty() {
            return usage;
        }

        for (index, doc) in self.documents.iter().enumerate() {
            let graph = if index == self.active {
                active_graph
            } else {
                &doc.graph
            };

            for node in graph.nodes.values() {
                let NodeType::CustomNodeInstance { file, .. } = &node.node_type else {
                    continue;
                };
                let resolved = self
                    .resolved_paths
                    .entry(file.clone())
                    .or_insert_with(|| crate::graph::canonical_custom_node_path(file));
                let Some(resolved) = resolved else {
                    continue;
                };
                for (id, path) in &targets {
                    if path == resolved {
                        *usage.entry(*id).or_insert(0) += 1;
                    }
                }
            }
        }

        usage
    }

    /// Forget cached path resolutions (e.g. after files were saved or moved).
    pub fn clear_resolved_paths(&mut self) {
        self.resolved_paths.clear();
    }

    pub fn activate(&mut self, id: u64) -> bool {
        if self.locked {
            return false;
        }
        self.activate_unchecked(id)
    }

    /// Activate while locked. Only the quit dialog's Save & Continue uses
    /// this, to move to the next unsaved document.
    pub fn activate_for_quit(&mut self, id: u64) -> bool {
        self.activate_unchecked(id)
    }

    fn activate_unchecked(&mut self, id: u64) -> bool {
        if let Some(index) = self.documents.iter().position(|doc| doc.id == id) {
            self.active = index;
            true
        } else {
            false
        }
    }
    pub fn close_document(&mut self, id: u64) -> bool {
        if self.locked {
            return false;
        }
        let Some(index) = self.documents.iter().position(|doc| doc.id == id) else {
            return false;
        };

        // Never close the final remaining document.
        if self.documents.len() <= 1 {
            return false;
        }

        // Don't discard unsaved changes.
        if self.documents[index].is_dirty {
            return false;
        }

        self.documents.remove(index);

        // Keep the active index valid.
        if index < self.active {
            self.active -= 1;
        } else if self.active >= self.documents.len() {
            self.active = self.documents.len() - 1;
        }

        true
    }
}

impl Default for DocumentManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(file: &std::path::Path) -> NodeType {
        NodeType::CustomNodeInstance {
            file: file.to_string_lossy().into_owned(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            package: None,
        }
    }

    #[test]
    fn usage_counts_references_across_open_documents() {
        let dir = std::env::temp_dir().join(format!("patchwork_usage_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let used = dir.join("used.pwnode");
        let unused = dir.join("unused.pwnode");
        for (path, name) in [(&used, "used"), (&unused, "unused")] {
            CustomNodeFile {
                version: 1,
                name: name.to_string(),
                graph: Graph::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
            }
            .save(path)
            .unwrap();
        }

        let mut manager = DocumentManager::new();
        let used_id = manager.open_custom_node(used.clone()).unwrap();
        let unused_id = manager.open_custom_node(unused.clone()).unwrap();
        // Reopening reuses the existing tab.
        assert_eq!(manager.open_custom_node(used.clone()).unwrap(), used_id);

        // A background project document with one reference.
        let background = manager.new_document("Background");
        manager
            .documents
            .iter_mut()
            .find(|doc| doc.id == background)
            .unwrap()
            .graph
            .add_node(instance(&used), [0.0, 0.0]);

        // The active document's live graph with two references,
        // one through a non-canonical path.
        let mut active_graph = Graph::new();
        active_graph.add_node(instance(&used), [0.0, 0.0]);
        active_graph.add_node(instance(&dir.join(".").join("used.pwnode")), [0.0, 0.0]);

        let usage = manager.custom_node_usage(&active_graph);
        assert_eq!(usage.get(&used_id), Some(&3));
        assert_eq!(usage.get(&unused_id), Some(&0));
        // Project documents get no usage entry.
        assert_eq!(usage.get(&manager.active().id), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Stage 0: New Project isolation (H1) and read-only documents.
#[cfg(test)]
mod stage0_tests {
    use super::*;
    use crate::app::undo::UndoSnapshot;
    use crate::document::{CustomNodeFile, CustomNodePort, DocumentKind};
    use crate::graph::{Graph, NodeType, PortKind};
    use std::collections::HashSet;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patchwork_stage0_doc_{tag}_{}_{}",
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

    fn custom_node(path: &std::path::Path) {
        let mut graph = Graph::new();
        graph.add_node(
            NodeType::CustomOutput {
                name: "out".into(),
                kind: PortKind::Number,
            },
            [0.0, 0.0],
        );
        CustomNodeFile {
            version: 1,
            name: "Mine".into(),
            graph,
            inputs: Vec::new(),
            outputs: vec![CustomNodePort {
                name: "out".into(),
                kind: "Number".into(),
            }],
        }
        .save(path)
        .unwrap();
    }

    /// Mirrors `PatchworkApp::switch_document` + the New Project handler:
    /// the live editor graph is stored into the active document, then a new
    /// document is created and activated, and only then is the live graph
    /// replaced by the new project graph.
    #[test]
    fn new_project_never_replaces_an_editable_custom_node_tab() {
        let tmp = TempDir::new("h1");
        let path = tmp.0.join("Mine.pwnode");
        custom_node(&path);
        let original_bytes = std::fs::read(&path).unwrap();

        let mut manager = DocumentManager::new();
        let custom_id = manager.open_custom_node(path.clone()).unwrap();
        assert!(manager.activate(custom_id));

        // Editor state of the custom-node tab: edited, dirty, with undo.
        let mut live = manager.active().graph.clone();
        live.add_node(
            NodeType::CustomInput {
                name: "in".into(),
                kind: PortKind::Number,
            },
            [0.0, 0.0],
        );
        let mut undo = manager.active().undo_history.clone();
        undo.push(UndoSnapshot::new(
            manager.active().graph.clone(),
            HashSet::new(),
        ));

        // ── New Project ──
        let new_id = manager.new_document("Untitled");
        {
            let current = manager.active_mut();
            assert!(current.store_graph(&live));
            current.is_dirty = true;
            current.undo_history = undo.clone();
        }
        assert!(manager.activate(new_id));
        let mut project = Graph::new();
        project.add_node(NodeType::FileMenu, [10.0, 10.0]); // default chrome

        // The custom-node document is intact.
        let custom = manager
            .documents
            .iter()
            .find(|d| d.id == custom_id)
            .unwrap();
        assert_eq!(custom.kind, DocumentKind::CustomNode);
        assert_eq!(
            custom.path.as_deref(),
            std::fs::canonicalize(&path).ok().as_deref()
        );
        assert_eq!(custom.graph.nodes.len(), live.nodes.len());
        assert!(custom.is_dirty);
        assert!(custom.undo_history.can_undo());
        assert!(
            !custom
                .graph
                .nodes
                .values()
                .any(|n| matches!(n.node_type, NodeType::FileMenu))
        );

        // The new tab is a separate, untitled project.
        assert_eq!(manager.active().id, new_id);
        assert_eq!(manager.active().kind, DocumentKind::Project);
        assert!(manager.active().path.is_none());

        // Even if a project graph ever reached the custom-node save path,
        // the save is refused and the .pwnode is untouched.
        assert!(Graph::validate_custom_node_save(&project, &path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
    }

    #[test]
    fn read_only_documents_keep_their_loaded_graph() {
        let mut document = Document::new(1, "Package");
        document.read_only = true;
        let mut edited = Graph::new();
        edited.add_node(NodeType::FileMenu, [0.0, 0.0]);
        assert!(!document.is_editable());
        assert!(!document.store_graph(&edited));
        assert!(document.graph.nodes.is_empty());

        let mut editable = Document::new(2, "Mine");
        assert!(editable.is_editable());
        assert!(editable.store_graph(&edited));
        assert_eq!(editable.graph.nodes.len(), 1);
    }
}

/// Stage 1 review fixes: Save As per document kind, path/kind invariant,
/// read-only restoration.
#[cfg(test)]
mod save_as_tests {
    use super::*;
    use crate::document::{CustomNodeFile, CustomNodePort, DocumentKind, save_custom_node_as};
    use crate::graph::{Graph, NodeType, PortKind};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patchwork_saveas_{tag}_{}_{}",
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

    fn graph_with_output(name: &str) -> Graph {
        let mut graph = Graph::new();
        graph.add_node(
            NodeType::CustomOutput {
                name: name.into(),
                kind: PortKind::Number,
            },
            [0.0, 0.0],
        );
        graph
    }

    fn write_custom_node(path: &std::path::Path, output: &str) {
        CustomNodeFile {
            version: 1,
            name: "Original".into(),
            graph: graph_with_output(output),
            inputs: Vec::new(),
            outputs: vec![CustomNodePort {
                name: output.into(),
                kind: "Number".into(),
            }],
        }
        .save(path)
        .unwrap();
    }

    // ── Path / kind invariant ────────────────────────────────────────

    #[test]
    fn document_path_must_fit_its_kind() {
        let tmp = TempDir::new("kind");
        let mut custom = Document::new(1, "Node");
        custom.kind = DocumentKind::CustomNode;
        let pwnode = tmp.0.join("Node.pwnode");
        let upper = tmp.0.join("Node.PWNODE");
        let folder = tmp.0.join("MyProject");

        assert!(custom.set_path(Some(pwnode.clone())).is_ok());
        assert!(custom.set_path(Some(upper)).is_ok());
        assert!(custom.set_path(Some(pwnode.clone())).is_ok());
        assert!(custom.set_path(Some(folder.clone())).is_err());
        assert!(custom.set_path(Some(folder.join("project.json"))).is_err());
        assert_eq!(
            custom.path.as_ref(),
            Some(&pwnode),
            "unchanged after refusal"
        );
        assert_eq!(custom.kind, DocumentKind::CustomNode);
        assert!(custom.set_path(None).is_ok());

        let mut project = Document::new(2, "Project");
        assert!(project.set_path(Some(folder.clone())).is_ok());
        assert!(project.set_path(Some(pwnode)).is_err());
        assert_eq!(project.path.as_ref(), Some(&folder));
        assert_eq!(project.kind, DocumentKind::Project);
    }

    // ── Custom-node Save As ──────────────────────────────────────────

    #[test]
    fn custom_node_save_as_writes_new_file_and_preserves_original() {
        let tmp = TempDir::new("custom");
        let original = tmp.0.join("Original.pwnode");
        write_custom_node(&original, "out");
        let original_bytes = std::fs::read(&original).unwrap();

        let mut manager = DocumentManager::new();
        let id = manager.open_custom_node(original.clone()).unwrap();
        assert!(manager.activate(id));
        let mut live = manager.active().graph.clone();
        live.add_node(
            NodeType::CustomInput {
                name: "in".into(),
                kind: PortKind::Number,
            },
            [0.0, 0.0],
        );

        let copy = tmp.0.join("Copy.pwnode");
        let written = save_custom_node_as(&live, &copy).unwrap();
        assert_eq!(written.name, "Copy");
        assert_eq!(written.inputs.len(), 1);
        assert_eq!(written.outputs.len(), 1);

        // The tab moves to the new file and stays a custom node.
        let document = manager.active_mut();
        document
            .set_path(Some(std::fs::canonicalize(&copy).unwrap()))
            .unwrap();
        assert_eq!(document.kind, DocumentKind::CustomNode);

        // The original file is untouched and still opens as before.
        assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
        let reopened = CustomNodeFile::load(&original).unwrap();
        assert_eq!(reopened.name, "Original");
        assert!(reopened.inputs.is_empty());
        let saved = CustomNodeFile::load(&copy).unwrap();
        assert_eq!(saved.graph.nodes.len(), 2);
    }

    #[test]
    fn custom_node_save_as_refuses_invalid_targets_and_content() {
        let tmp = TempDir::new("custom_refuse");
        let graph = graph_with_output("out");

        // Not a .pwnode (would turn a custom node into something else).
        assert!(save_custom_node_as(&graph, &tmp.0.join("project.json")).is_err());
        assert!(save_custom_node_as(&graph, &tmp.0.join("Folder")).is_err());
        let folder = tmp.0.join("Folder.pwnode");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(save_custom_node_as(&graph, &folder).is_err());

        // A project graph (editor chrome) is never written as a custom node.
        let mut project = Graph::new();
        project.add_node(NodeType::FileMenu, [0.0, 0.0]);
        let target = tmp.0.join("FromProject.pwnode");
        assert!(save_custom_node_as(&project, &target).is_err());
        assert!(!target.exists());

        // Saving a graph that uses the destination itself is recursive.
        let target = tmp.0.join("Self.pwnode");
        write_custom_node(&target, "out");
        let before = std::fs::read(&target).unwrap();
        let mut recursive = graph_with_output("out");
        recursive.add_node(
            NodeType::CustomNodeInstance {
                file: target.to_string_lossy().into_owned(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                package: None,
            },
            [0.0, 0.0],
        );
        assert!(save_custom_node_as(&recursive, &target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), before);
    }

    #[test]
    fn custom_node_save_as_never_writes_a_published_version() {
        let tmp = TempDir::new("custom_published");
        let source = tmp.0.join("Mixer.pwnode");
        write_custom_node(&source, "out");
        let request = crate::library::package::PublishRequest {
            name: "Mixer".into(),
            version: "1.0.0".into(),
            ..Default::default()
        };
        let published =
            crate::library::package::publish_package(&tmp.0.join("library"), &source, &request)
                .unwrap();
        let before = std::fs::read(&published.entry_path).unwrap();
        let error =
            save_custom_node_as(&graph_with_output("x"), &published.entry_path).unwrap_err();
        assert!(error.contains("read-only"), "{error}");
        assert_eq!(std::fs::read(&published.entry_path).unwrap(), before);
    }

    #[test]
    fn custom_node_save_as_can_replace_an_ordinary_file_when_confirmed() {
        let tmp = TempDir::new("custom_replace");
        let target = tmp.0.join("Target.pwnode");
        write_custom_node(&target, "old");
        let written = save_custom_node_as(&graph_with_output("new"), &target).unwrap();
        assert_eq!(written.outputs[0].name, "new");
        assert_eq!(
            CustomNodeFile::load(&target).unwrap().outputs[0].name,
            "new"
        );
    }

    // ── Read-only restoration ────────────────────────────────────────

    #[test]
    fn read_only_document_restores_the_live_graph() {
        let mut document = Document::new(1, "Package");
        document.kind = DocumentKind::CustomNode;
        document.graph = graph_with_output("out");
        document.read_only = true;
        let stored: Vec<_> = document.graph.nodes.keys().copied().collect();

        let mut live = document.graph.clone();
        live.add_node(NodeType::FileMenu, [0.0, 0.0]);
        assert!(document.restore_into(&mut live));
        let mut restored: Vec<_> = live.nodes.keys().copied().collect();
        restored.sort();
        assert_eq!(restored, stored);
        assert!(document.read_only, "still read-only");
        assert!(!document.is_dirty);

        let editable = Document::new(2, "Mine");
        let mut live = graph_with_output("x");
        assert!(!editable.restore_into(&mut live));
        assert_eq!(live.nodes.len(), 1, "editable live graph untouched");
    }
}

/// Quit dialog: documents are locked against switching, closing and
/// opening; only Save & Continue may move between tabs.
#[cfg(test)]
mod quit_lock_tests {
    use super::*;
    use crate::document::CustomNodeFile;
    use crate::graph::Graph;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patchwork_quitlock_{tag}_{}_{}",
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

    fn node_file(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("Node.pwnode");
        CustomNodeFile {
            version: 1,
            name: "Node".into(),
            graph: Graph::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
        .save(&path)
        .unwrap();
        path
    }

    #[test]
    fn locked_documents_cannot_be_switched_closed_or_opened() {
        let tmp = TempDir::new("locked");
        let node = node_file(&tmp.0);
        let mut documents = DocumentManager::new();
        let second = documents.new_document("Second");
        let first = documents.active().id;

        documents.set_locked(true);
        assert!(documents.is_locked());
        assert!(!documents.activate(second), "no tab switching");
        assert_eq!(documents.active().id, first, "active tab unchanged");
        assert!(!documents.close_document(second), "no tab closing");
        assert_eq!(documents.documents.len(), 2);
        let count = documents.documents.len();
        assert!(documents.open_custom_node(node).is_err(), "no opening");
        assert_eq!(documents.documents.len(), count, "nothing was created");
        assert_eq!(documents.active().id, first);
    }

    #[test]
    fn save_and_continue_can_still_move_between_tabs() {
        let mut documents = DocumentManager::new();
        let second = documents.new_document("Second");
        documents.set_locked(true);
        assert!(documents.activate_for_quit(second));
        assert_eq!(documents.active().id, second);
        assert!(documents.is_locked(), "still locked for everything else");
        assert!(!documents.activate(1));
        assert!(!documents.activate_for_quit(999), "unknown id");
    }

    #[test]
    fn cancel_restores_normal_tab_interactions() {
        let tmp = TempDir::new("unlock");
        let node = node_file(&tmp.0);
        let mut documents = DocumentManager::new();
        let second = documents.new_document("Second");
        documents.set_locked(true);
        assert!(!documents.activate(second));

        documents.set_locked(false); // Cancel
        assert!(!documents.is_locked());
        assert!(documents.activate(second));
        assert!(documents.activate(1));
        assert!(documents.close_document(second));
        assert!(documents.open_custom_node(node).is_ok());
    }
}
