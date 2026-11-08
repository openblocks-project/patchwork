
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

use crate::egui::Vec2;
use crate::graph::{Graph, NodeId};
use crate::app::undo::UndoHistory;

pub mod manager;

pub use manager::DocumentManager;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Project,
    CustomNode,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomNodePort {
    pub name: String,
    pub kind: String,
}

impl CustomNodeFile {
  pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
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
        if let crate::graph::NodeType::CustomNodeInstance { file, .. } =
            &mut node.node_type
        {
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

    std::fs::write(path, json)
        .map_err(|e| format!("Failed to save custom node: {e}"))?;

    Ok(())
}
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read custom node: {e}"))?;

    let mut custom_node: Self = serde_json::from_str(&json)
        .map_err(|e| format!("Failed to parse custom node: {e}"))?;

    if custom_node.version != 1 {
        return Err(format!(
            "Unsupported custom node version: {}",
            custom_node.version
        ));
    }

  custom_node.graph.fix_next_id();

// Resolve nested custom-node references relative to this .pwnode file.
let parent = path.parent().unwrap_or(std::path::Path::new("."));

for node in custom_node.graph.nodes.values_mut() {
    if let crate::graph::NodeType::CustomNodeInstance { file, .. } =
        &mut node.node_type
    {
        let child = std::path::Path::new(file);

        if !child.is_absolute() {
            *file = parent.join(child).to_string_lossy().into_owned();
        }
    }
}

Ok(custom_node)
}
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
        }
    }
}