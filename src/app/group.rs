//! P2: Custom Node Grouping — editor integration.
//!
//! Ctrl+G / Cmd+G validates the selection, asks for a name, asks where to
//! save the .pwnode, then replaces the selection with a single custom-node
//! instance as one undoable step.

use super::*;
use crate::graph::group::{self, GroupExtraction};

impl PatchworkApp {
    /// Ctrl+G / Cmd+G: validate the selection and open the naming dialog.
    pub(super) fn begin_group_selection(&mut self) {
        if !self.require_editable("Group into Custom Node") {
            return;
        }
        if self.selected_nodes.is_empty() {
            self.toast_info("Select nodes to group into a custom node (Ctrl+G).");
            return;
        }
        match self
            .graph
            .extract_group(&self.selected_nodes, &self.pinned_nodes)
        {
            Ok(_) => {
                self.group_selection = self.selected_nodes.clone();
                self.group_name = "Custom Node".to_string();
                self.group_dialog_open = true;
            }
            Err(error) => show_group_error(&error.to_string()),
        }
    }

    /// Draw the "Group into Custom Node" naming dialog.
    pub(super) fn render_group_dialog(&mut self, ctx: &egui::Context) {
        if !self.group_dialog_open {
            return;
        }
        let mut open = true;
        let mut create = ctx.input(|i| i.key_pressed(egui::Key::Enter));
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        let validation = group::validate_group_name(&self.group_name);

        egui::Modal::new(egui::Id::new("group_custom_node_modal")).show(ctx, |ui| {
            ui.heading("Group into Custom Node");
            ui.label(format!(
                "{} node(s) will be replaced by one reusable custom node.",
                self.group_selection.len()
            ));
            ui.add_space(8.0);
            ui.label("Name:");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.group_name)
                    .desired_width(280.0)
                    .hint_text("My Custom Node"),
            );
            response.request_focus();
            if let Err(message) = &validation {
                ui.label(
                    egui::RichText::new(message)
                        .small()
                        .color(egui::Color32::from_rgb(255, 120, 120)),
                );
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(validation.is_ok(), egui::Button::new("Create…"))
                    .clicked()
                {
                    create = true;
                }
                if ui.button("Cancel").clicked() {
                    open = false;
                }
            });
        });

        if open && create && validation.is_ok() {
            // Closes on success; stays open if the user cancels the file
            // dialog or declines to overwrite.
            open = !self.create_group_from_dialog();
        }

        if !open {
            self.group_dialog_open = false;
            self.group_selection.clear();
        }
    }

    /// Validate, write the .pwnode and replace the selection.
    /// Returns true when the dialog should close (success or hard error).
    fn create_group_from_dialog(&mut self) -> bool {
        if !self.require_editable("Group into Custom Node") {
            return true;
        }
        let name = self.group_name.trim().to_string();
        if let Err(message) = group::validate_group_name(&name) {
            show_group_error(&message);
            return false;
        }

        // Re-validate: the graph may have changed since the dialog opened.
        let extraction = match self
            .graph
            .extract_group(&self.group_selection, &self.pinned_nodes)
        {
            Ok(extraction) => extraction,
            Err(error) => {
                show_group_error(&error.to_string());
                return true;
            }
        };

        // ── Choose the file location ─────────────────────────────────────
        let mut dialog = rfd::FileDialog::new()
            .set_title("Save Custom Node")
            .add_filter("Patchwork Custom Node", &["pwnode"])
            .set_file_name(format!("{name}.pwnode"));
        if let Some(dir) = self.default_custom_node_dir() {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.save_file() else {
            // Cancelled: graph unchanged, keep the dialog open.
            return false;
        };
        let path = group::with_pwnode_extension(path);

        // ── Overwrite protection ─────────────────────────────────────────
        let overwriting = path.exists();
        if overwriting {
            if path.is_dir() {
                show_group_error("A folder with that name already exists.");
                return false;
            }
            let canonical = std::fs::canonicalize(&path).ok();
            let open_in_tab = self.documents.documents.iter().any(|doc| {
                doc.kind == crate::document::DocumentKind::CustomNode
                    && doc.path.is_some()
                    && doc.path.clone().and_then(|p| std::fs::canonicalize(p).ok()) == canonical
            });
            if open_in_tab {
                show_group_error(
                    "That custom node is open in a tab. Close its tab or choose another file.",
                );
                return false;
            }
            let confirmed = rfd::MessageDialog::new()
                .set_title("Replace Custom Node?")
                .set_description(format!(
                    "{} already exists.\n\nReplacing it changes every instance that uses it. \
                     Do you want to replace it?",
                    path.display()
                ))
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            if confirmed != rfd::MessageDialogResult::Yes {
                return false;
            }
        }

        // ── Recursive-dependency check (before writing anything) ─────────
        let mut forbidden = Vec::new();
        if self.documents.active().kind == crate::document::DocumentKind::CustomNode {
            if let Some(active) = &self.documents.active().path {
                forbidden.push(active.clone());
            }
        }
        if let Err(error) = group::check_group_dependencies(&extraction.graph, &path, &forbidden) {
            show_group_error(&format!("Cannot create custom node: {error}"));
            return false;
        }

        // ── Write the file ───────────────────────────────────────────────
        let file = crate::document::CustomNodeFile {
            version: 1,
            name: name.clone(),
            graph: extraction.graph.clone(),
            inputs: extraction.inputs.clone(),
            outputs: extraction.outputs.clone(),
        };
        match file.save_with_digest(&path) {
            // P4.3.2E1: the new custom node inherits the provenance of the
            // document it was grouped from.
            Ok(digest) => super::provenance::record_written(
                &self.graph.content,
                crate::library::trust::content::ContentKind::CustomNode,
                &digest,
            ),
            Err(error) => {
                show_group_error(&format!("Failed to save custom node: {error}"));
                return false;
            }
        }

        // ── Replace the selection (single undo step) ─────────────────────
        let instance = self.apply_group_extraction(&extraction, &path.to_string_lossy());
        crate::system_log::log(format!(
            "Grouped {} node(s) into custom node {} (#{instance})",
            extraction.selected.len(),
            path.display()
        ));

        if !self.project_custom_nodes.contains(&path) {
            self.project_custom_nodes.push(path.clone());
        }
        if overwriting {
            // Existing instances of the replaced file pick up the new interface.
            self.refresh_custom_node_instances(&path, &extraction.inputs, &extraction.outputs);
        } else {
            self.documents.clear_resolved_paths();
        }
        self.toast_info(format!(
            "Created custom node '{name}'. Undo restores the nodes but keeps the .pwnode file."
        ));
        true
    }

    /// Replace the extracted selection with an instance referencing `file`,
    /// as a single undo step. Returns the new instance id.
    pub(super) fn apply_group_extraction(
        &mut self,
        extraction: &GroupExtraction,
        file: &str,
    ) -> NodeId {
        debug_assert!(self.active_is_editable(), "grouping in a read-only tab");
        self.push_undo();
        for &id in &extraction.selected {
            self.release_node_resources(id);
            self.selected_nodes.remove(&id);
        }
        let instance = self.graph.apply_group(extraction, file);
        self.selected_nodes.clear();
        self.selected_nodes.insert(instance);
        self.selected_connection = None;
        self.caches_dirty = true;
        instance
    }

    /// Release editor-side resources held for a node about to be removed
    /// (mirrors the cleanup performed by node deletion).
    fn release_node_resources(&mut self, id: NodeId) {
        self.midi.cleanup_node(id);
        self.serial.cleanup_node(id);
        self.osc.cleanup_node(id);
        self.network.cleanup_node(id);
        self.ob.cleanup_node(id);
        self.audio.cleanup_node(id);
        self.dmx.cleanup_node(id);
        self.artnet.cleanup_node(id);
        crate::nodes::video_player::cleanup_node(id);
        crate::gpu_image::request_node_invalidation(id);
    }

    /// Folder suggested for new .pwnode files: the active project folder,
    /// or the folder of the custom node being edited.
    fn default_custom_node_dir(&self) -> Option<std::path::PathBuf> {
        let active = self.documents.active();
        if active.kind == crate::document::DocumentKind::CustomNode {
            return active.path.as_ref().and_then(|p| p.parent()).map(|p| {
                std::path::PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\"))
            });
        }
        self.project_path
            .as_ref()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
    }

    /// Custom nodes belonging to the current project: files created by
    /// grouping this session plus every .pwnode referenced by the active graph.
    pub(super) fn project_node_paths(&self) -> Vec<std::path::PathBuf> {
        let mut seen = std::collections::HashSet::new();
        let mut paths = Vec::new();
        let referenced = self
            .graph
            .nodes
            .values()
            .filter_map(|n| match &n.node_type {
                NodeType::CustomNodeInstance { file, .. } => Some(std::path::PathBuf::from(file)),
                _ => None,
            });
        for path in self.project_custom_nodes.iter().cloned().chain(referenced) {
            if !path.is_file() {
                continue;
            }
            let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if seen.insert(key) {
                paths.push(path);
            }
        }
        paths.sort_by_key(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        });
        paths
    }
}

fn show_group_error(message: &str) {
    crate::system_log::warn(format!("Group into custom node: {message}"));
    rfd::MessageDialog::new()
        .set_title("Cannot Group Nodes")
        .set_description(message)
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(test)]
mod tests {
    use super::super::undo::{UndoHistory, UndoSnapshot};
    use crate::graph::{Graph, NodeId, NodeType};
    use std::collections::HashSet;

    fn add_node() -> NodeType {
        let entry = crate::nodes::catalog()
            .into_iter()
            .find(|e| e.label == "Add")
            .unwrap();
        (entry.factory)()
    }

    type Shape = (Vec<NodeId>, Vec<(NodeId, usize, NodeId, usize)>);

    fn shape(graph: &Graph) -> Shape {
        let mut nodes: Vec<_> = graph.nodes.keys().copied().collect();
        nodes.sort();
        let mut conns: Vec<_> = graph
            .connections
            .iter()
            .map(|c| (c.from_node, c.from_port, c.to_node, c.to_port))
            .collect();
        conns.sort();
        (nodes, conns)
    }

    // 11. Grouping is one undo step; undo restores, redo re-applies.
    // Mirrors `apply_group_extraction`: one snapshot, then the replacement.
    #[test]
    fn grouping_is_a_single_undoable_step() {
        let mut graph = Graph::new();
        let a = graph.add_node(add_node(), [0.0, 0.0]);
        let b = graph.add_node(add_node(), [200.0, 0.0]);
        let c = graph.add_node(add_node(), [400.0, 0.0]);
        graph.add_connection(a, 0, b, 0);
        graph.add_connection(b, 0, c, 0);
        let pinned = HashSet::new();
        let original = shape(&graph);

        let mut history = UndoHistory::new(50);
        let selection: HashSet<NodeId> = [b].into_iter().collect();
        let extraction = graph.extract_group(&selection, &pinned).unwrap();
        history.push(UndoSnapshot::new(graph.clone(), pinned.clone()));
        let instance = graph.apply_group(&extraction, "group.pwnode");
        let grouped = shape(&graph);
        assert!(graph.nodes.contains_key(&instance));
        assert!(!graph.nodes.contains_key(&b));

        let restored = history
            .undo(UndoSnapshot::new(graph.clone(), pinned.clone()))
            .expect("undo");
        graph = restored.graph;
        assert_eq!(shape(&graph), original);
        assert!(matches!(
            graph.nodes[&b].node_type,
            NodeType::Dynamic { .. }
        ));
        assert!(!history.can_undo());

        let redone = history
            .redo(UndoSnapshot::new(graph.clone(), pinned.clone()))
            .expect("redo");
        graph = redone.graph;
        assert_eq!(shape(&graph), grouped);
    }
}
