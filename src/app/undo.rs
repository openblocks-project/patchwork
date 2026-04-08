use crate::graph::*;
use std::collections::HashSet;

pub(super) struct UndoSnapshot {
    pub graph: Graph,
    pub pinned_nodes: HashSet<NodeId>,
}

pub(super) struct UndoHistory {
    undo_stack: Vec<UndoSnapshot>,
    redo_stack: Vec<UndoSnapshot>,
    max: usize,
}

impl UndoHistory {
    pub fn new(max: usize) -> Self {
        Self { undo_stack: Vec::new(), redo_stack: Vec::new(), max }
    }

    pub fn push(&mut self, snap: UndoSnapshot) {
        self.redo_stack.clear();
        self.undo_stack.push(snap);
        if self.undo_stack.len() > self.max {
            self.undo_stack.remove(0);
        }
    }

    pub fn undo(&mut self, current: UndoSnapshot) -> Option<UndoSnapshot> {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(current);
            Some(prev)
        } else {
            None
        }
    }

    pub fn redo(&mut self, current: UndoSnapshot) -> Option<UndoSnapshot> {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(current);
            Some(next)
        } else {
            None
        }
    }

    pub fn can_undo(&self) -> bool { !self.undo_stack.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo_stack.is_empty() }
    pub fn clear(&mut self) { self.undo_stack.clear(); self.redo_stack.clear(); }
}

/// Undo/redo impl on PatchworkApp
impl super::PatchworkApp {
    pub(super) fn push_undo(&mut self) {
        let snap = UndoSnapshot {
            graph: self.graph.clone(),
            pinned_nodes: self.pinned_nodes.clone(),
        };
        self.undo_history.push(snap);
    }

    pub(super) fn perform_undo(&mut self) {
        let current = UndoSnapshot {
            graph: self.graph.clone(),
            pinned_nodes: self.pinned_nodes.clone(),
        };
        if let Some(prev) = self.undo_history.undo(current) {
            self.graph = prev.graph;
            self.pinned_nodes = prev.pinned_nodes;
            self.port_positions.clear();
            self.node_rects.clear();
            self.selected_nodes.clear();
            self.selected_connection = None;
            // Restoring a previous snapshot reuses the same node ids,
            // so any per-(node_id, port) GPU texture or egui temp data
            // belonging to the *current* state would silently shadow
            // the restored node until the next interaction. Wipe both
            // at the top of the next frame.
            self.caches_dirty = true;
        }
    }

    pub(super) fn perform_redo(&mut self) {
        let current = UndoSnapshot {
            graph: self.graph.clone(),
            pinned_nodes: self.pinned_nodes.clone(),
        };
        if let Some(next) = self.undo_history.redo(current) {
            self.graph = next.graph;
            self.pinned_nodes = next.pinned_nodes;
            self.port_positions.clear();
            self.node_rects.clear();
            self.selected_nodes.clear();
            self.selected_connection = None;
            self.caches_dirty = true;
        }
    }
}
