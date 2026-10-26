use super::Document;

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
        self.active = self.documents.len() - 1;

        id
    }

    pub fn activate(&mut self, id: u64) -> bool {
        if let Some(index) = self.documents.iter().position(|doc| doc.id == id) {
            self.active = index;
            true
        } else {
            false
        }
    }
}

impl Default for DocumentManager {
    fn default() -> Self {
        Self::new()
    }
}