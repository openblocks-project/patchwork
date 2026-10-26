use std::collections::HashSet;
use std::path::PathBuf;

use crate::egui::Vec2;
use crate::graph::{Graph, NodeId};

pub mod manager;

pub use manager::DocumentManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Project,
    CustomNode,
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
}

impl Document {
    pub fn new(id: u64, title: impl Into<String>) -> Self {
        Self {
            id,
            kind: DocumentKind::Project,
            title: title.into(),
            path: None,

            graph: Graph::new(),

            selected_nodes: HashSet::new(),
            pinned_nodes: HashSet::new(),

            canvas_offset: Vec2::ZERO,
            canvas_zoom: 1.0,
            target_zoom: 1.0,

            is_dirty: false,
        }
    }
}