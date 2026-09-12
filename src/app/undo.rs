use crate::graph::*;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

pub(super) struct UndoSnapshot {
    pub graph: Graph,
    pub pinned_nodes: HashSet<NodeId>,
    /// Content hash of the snapshot, used to skip duplicate states.
    /// 0 means "hashing failed" — such snapshots are never deduped.
    hash: u64,
}

impl UndoSnapshot {
    pub fn new(graph: Graph, pinned_nodes: HashSet<NodeId>) -> Self {
        let hash = Self::content_hash(&graph, &pinned_nodes);
        Self { graph, pinned_nodes, hash }
    }

    fn content_hash(graph: &Graph, pinned: &HashSet<NodeId>) -> u64 {
        // The graph is Serialize (it's how projects are saved), so its
        // serialized bytes are a faithful fingerprint of its state.
        let Ok(bytes) = serde_json::to_vec(graph) else { return 0 };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        let mut pins: Vec<NodeId> = pinned.iter().copied().collect();
        pins.sort_unstable();
        pins.hash(&mut h);
        let out = h.finish();
        // Reserve 0 as the "unhashable" sentinel.
        if out == 0 { 1 } else { out }
    }

    fn same_state(&self, other_hash: u64) -> bool {
        self.hash != 0 && other_hash != 0 && self.hash == other_hash
    }
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
        // Dedup: over-eager callers (per-frame property-edit detection)
        // can push the same state several times per gesture. Identical
        // consecutive snapshots make Ctrl+Z appear to "do nothing", so
        // refuse a push whose content matches the top of the stack.
    //   if let Some(top) = self.undo_stack.last() {
    //         if top.same_state(snap.hash) {
    //             crate::system_log::log("undo: skipped duplicate snapshot".to_string());
    //             return;
    //         }
    //     }
    // A new user action always invalidates the redo branch, even when
        // the snapshot itself is deduped — otherwise Redo can jump to a
        // state that no longer follows from the current history.
        self.redo_stack.clear();
        if let Some(top) = self.undo_stack.last() {
            if top.same_state(snap.hash) {
                // crate::system_log::log("undo: skipped duplicate snapshot".to_string());
                return;
            }
        }
        self.redo_stack.clear();
        self.undo_stack.push(snap);
        if self.undo_stack.len() > self.max {
            self.undo_stack.remove(0);
        }
    }

    pub fn undo(&mut self, current: UndoSnapshot) -> Option<UndoSnapshot> {
        // Skip over snapshots identical to the current state (legacy
        // duplicates or stray pushes) so every undo visibly changes
        // something.
        // while let Some(prev) = self.undo_stack.pop() {
        //     if !prev.same_state(current.hash) {
        //         self.redo_stack.push(current);
        //         return Some(prev);
        //     }
        //     // identical to what's on screen — discard and keep looking
        // }
        // None

        // Skip over snapshots identical to the current state (legacy
        // duplicates or stray pushes) so every undo visibly changes
        // something.
        while let Some(prev) = self.undo_stack.pop() {
            if !prev.same_state(current.hash) {
                self.redo_stack.push(current);
                return Some(prev);
            }
            // identical to what's on screen — discard and keep looking
        }
        None
    }

    pub fn redo(&mut self, current: UndoSnapshot) -> Option<UndoSnapshot> {
    let next = self.redo_stack.pop()?;
        self.undo_stack.push(current);
        Some(next)
    }

    pub fn can_undo(&self) -> bool { !self.undo_stack.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo_stack.is_empty() }
    pub fn clear(&mut self) { self.undo_stack.clear(); self.redo_stack.clear(); }
}

/// Undo/redo impl on PatchworkApp
impl super::PatchworkApp {
    pub(super) fn push_undo(&mut self) {
        let snap = UndoSnapshot::new(self.graph.clone(), self.pinned_nodes.clone());
        self.undo_history.push(snap);
        // push_undo is called before every meaningful user mutation
        // (node add/delete, connect, drag, property edit) — so this is
        // the natural chokepoint to flip the dirty flag.
        self.is_dirty = true;
    }

    pub(super) fn perform_undo(&mut self) {
        let current = UndoSnapshot::new(self.graph.clone(), self.pinned_nodes.clone());
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
            self.is_dirty = true;
        }
    }

    pub(super) fn perform_redo(&mut self) {
        let current = UndoSnapshot::new(self.graph.clone(), self.pinned_nodes.clone());
        if let Some(next) = self.undo_history.redo(current) {
            self.graph = next.graph;
            self.pinned_nodes = next.pinned_nodes;
            self.port_positions.clear();
            self.node_rects.clear();
            self.selected_nodes.clear();
            self.selected_connection = None;
            self.caches_dirty = true;
            self.is_dirty = true;
        }
    }
}
