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
}

impl DocumentManager {
    pub fn new() -> Self {
        Self {
            documents: vec![Document::new(1, "Untitled")],
            active: 0,
            next_id: 2,
            resolved_paths: HashMap::new(),
        }
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
    pub fn open_custom_node(&mut self, path: PathBuf) -> Result<u64, String> {
        let canonical_path =
            std::fs::canonicalize(&path).map_err(|e| format!("Invalid custom node path: {e}"))?;

        // Reuse an existing tab if this file is already open.
        if let Some(document) = self.documents.iter().find(|doc| {
            doc.kind == DocumentKind::CustomNode && doc.path.as_ref() == Some(&canonical_path)
        }) {
            return Ok(document.id);
        }

        let custom_node = CustomNodeFile::load(&canonical_path)?;

        let id = self.new_custom_node_document(custom_node.name);

        let document = self
            .documents
            .iter_mut()
            .find(|doc| doc.id == id)
            .expect("Newly created document must exist");

        document.graph = custom_node.graph;
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
        if let Some(index) = self.documents.iter().position(|doc| doc.id == id) {
            self.active = index;
            true
        } else {
            false
        }
    }
    pub fn close_document(&mut self, id: u64) -> bool {
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
