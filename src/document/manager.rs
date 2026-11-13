use super::{CustomNodeFile, Document, DocumentKind};
use std::path::PathBuf;

pub struct DocumentManager {
    pub documents: Vec<Document>,
    pub active: usize,
    next_id: u64,
}

impl DocumentManager {
    pub fn new() -> Self {
        Self {
            documents: vec![Document::new(1, "Untitled")],
            active: 0,
            next_id: 2,
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
