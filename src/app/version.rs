//! P4.2 Stage 5: "Version →" for packaged custom-node instances.
//!
//! The confirmation is modal: while it is open the active tab is not
//! editable (`active_is_editable` is false), canvas input and selection are
//! paused, file/system actions are ignored and documents are locked
//! (no tab switching, closing, creating or opening). Only Cancel and
//! Switch Version act.

use std::collections::HashSet;

use eframe::egui;

use super::undo::{UndoHistory, UndoSnapshot};
use crate::graph::version::{PortChange, VersionSwitchPlan};
use crate::graph::{Graph, NodeId};
use crate::library::resolve::InstancePackageRef;
use crate::node_trait::NodeBehavior;

/// A switch waiting for the user's confirmation (it would lose ports or
/// connections). Bound to the document and, through the plan's context, to
/// the exact node object and connections that were previewed.
pub(super) struct PendingVersionSwitch {
    pub document: u64,
    pub plan: VersionSwitchPlan,
}

/// Result of applying a (confirmed or direct) switch.
#[derive(Debug)]
pub(super) enum SwitchOutcome {
    /// Applied; one undo step was recorded.
    Applied { removed: usize },
    /// The effect changed but the node is the same: confirm the new plan.
    Reconfirm(Box<VersionSwitchPlan>),
    /// The previewed node or its context is gone; nothing was changed and
    /// the user has to choose the version again.
    Invalidated(String),
    /// The target can no longer be used; nothing was changed.
    Failed(String),
}

/// Re-plan against the current graph and apply only what was confirmed.
///
/// * Every fallible check (re-verification of the target, node identity,
///   node state, exact connections) happens before anything is recorded.
/// * A different node object (e.g. an id reused after undo) is never
///   switched.
/// * Different removed wires, or any other change in the effect, need a
///   new confirmation; a stale preview that now removes nothing is applied
///   directly only if the node and its connections are unchanged.
/// * The undo snapshot is recorded only after the graph was updated, so a
///   refusal leaves no undo step.
pub(super) fn confirm_version_switch(
    graph: &mut Graph,
    history: &mut UndoHistory,
    pinned: &HashSet<NodeId>,
    confirmed: &VersionSwitchPlan,
) -> SwitchOutcome {
    let fresh = match graph.plan_version_switch(confirmed.instance, &confirmed.to) {
        Ok(fresh) => fresh,
        Err(error) => {
            return match graph.switch_context(confirmed.instance) {
                Some(context) if context.token == confirmed.context.token => {
                    SwitchOutcome::Failed(error)
                }
                _ => SwitchOutcome::Invalidated(
                    "The custom node was removed or replaced while the confirmation was open."
                        .to_string(),
                ),
            };
        }
    };
    if !fresh.same_node(confirmed) {
        return SwitchOutcome::Invalidated(
            "The custom node was replaced while the confirmation was open; choose the version \
             again."
                .to_string(),
        );
    }
    if !fresh.same_effect(confirmed) {
        let context_unchanged = fresh.context == confirmed.context;
        if fresh.needs_confirmation() {
            return SwitchOutcome::Reconfirm(Box::new(fresh));
        }
        if !context_unchanged {
            return SwitchOutcome::Invalidated(
                "The custom node changed while the confirmation was open; choose the version \
                 again."
                    .to_string(),
            );
        }
        // Same node, same connections, and the fresh plan loses nothing:
        // safe to apply directly.
    }
    let snapshot = UndoSnapshot::new(graph.clone(), pinned.clone());
    match graph.apply_version_switch(&fresh) {
        Ok(report) => {
            history.push(snapshot);
            SwitchOutcome::Applied {
                removed: report.removed.len(),
            }
        }
        Err(error) => SwitchOutcome::Failed(error),
    }
}

/// The modal rule: editing, selection, document changes and file actions
/// pause while either confirmation is open.
pub(super) fn editing_paused(quit_dialog_open: bool, version_switch_pending: bool) -> bool {
    quit_dialog_open || version_switch_pending
}

fn node_label(graph: &Graph, id: NodeId) -> String {
    graph
        .nodes
        .get(&id)
        .map(|n| format!("{} #{id}", n.node_type.title()))
        .unwrap_or_else(|| format!("#{id}"))
}

/// Readable preview grouped by affected port: one line per port, then the
/// exact wires that would be removed.
pub(super) fn describe_affected_ports(plan: &VersionSwitchPlan, graph: &Graph) -> Vec<String> {
    let mut lines = Vec::new();
    for port in &plan.affected {
        let side = if port.is_input { "input" } else { "output" };
        let what = match &port.change {
            PortChange::Removed => "removed".to_string(),
            PortChange::TypeChanged { to } => format!("type changes {} → {to}", port.kind),
            PortChange::Unmatched => "cannot be matched".to_string(),
        };
        let wires = match port.connections {
            0 => "no connections".to_string(),
            1 => "1 connection removed".to_string(),
            n => format!("{n} connections removed"),
        };
        lines.push(format!("{side} '{}': {what} ({wires})", port.name));
        for wire in plan
            .removed
            .iter()
            .filter(|w| w.is_input == port.is_input && w.port == port.name)
        {
            let other = if wire.is_input {
                wire.from_node
            } else {
                wire.to_node
            };
            let arrow = if wire.is_input { "from" } else { "to" };
            lines.push(format!("    {arrow} {}", node_label(graph, other)));
        }
    }
    lines
}

impl super::PatchworkApp {
    /// True while any modal confirmation pauses editing (quit dialog or a
    /// pending version switch).
    pub(super) fn modal_lock(&self) -> bool {
        editing_paused(
            self.pending_quit_dialog,
            self.pending_version_switch.is_some(),
        )
    }

    /// Open or close the version confirmation. While open, documents are
    /// locked (shared with the quit dialog's lock) and menus are closed.
    pub(super) fn set_pending_version_switch(&mut self, pending: Option<PendingVersionSwitch>) {
        let open = pending.is_some();
        self.pending_version_switch = pending;
        self.documents.set_locked(open || self.pending_quit_dialog);
        if open {
            self.show_context_menu = false;
            self.show_node_menu = false;
            self.wire_menu_conn = None;
            self.pending_close_tab = None;
            self.pending_edit_custom_node = None;
        }
    }

    /// Inline "Version →" section of the node context menu. Returns true
    /// when a version was chosen (the menu should close).
    pub(super) fn version_menu(&mut self, ui: &mut egui::Ui, node: NodeId) -> bool {
        if !self.active_is_editable() {
            return false;
        }
        let Some(current) = self
            .graph
            .nodes
            .get(&node)
            .and_then(|n| match &n.node_type {
                crate::graph::NodeType::CustomNodeInstance {
                    package: Some(package),
                    ..
                } => Some(package.version.clone()),
                _ => None,
            })
        else {
            return false;
        };
        let mut chosen: Option<InstancePackageRef> = None;
        egui::CollapsingHeader::new(format!("Version → {current}"))
            .id_salt(("version_menu", self.documents.active().id, node))
            .default_open(false)
            .show(ui, |ui| {
                for choice in self.graph.available_versions(node) {
                    if choice.current {
                        ui.add_enabled(
                            false,
                            egui::Button::new(format!("✓ {} (current)", choice.version)),
                        );
                    } else if choice.selectable() {
                        if ui.button(&choice.version).clicked() {
                            chosen = choice.reference.clone();
                        }
                    } else {
                        let reason = choice.status.clone().err().unwrap_or_default();
                        ui.add_enabled(
                            false,
                            egui::Button::new(format!("{} — unavailable", choice.version)),
                        )
                        .on_disabled_hover_text(reason);
                    }
                }
            });
        match chosen {
            Some(target) => {
                self.request_version_switch(node, &target);
                true
            }
            None => false,
        }
    }

    /// Plan a switch; apply it directly when nothing is lost, otherwise ask.
    pub(super) fn request_version_switch(&mut self, node: NodeId, target: &InstancePackageRef) {
        if !self.require_editable("Switch Version") {
            return;
        }
        match self.graph.plan_version_switch(node, target) {
            Ok(plan) if plan.needs_confirmation() => {
                let document = self.documents.active().id;
                self.set_pending_version_switch(Some(PendingVersionSwitch { document, plan }));
            }
            Ok(plan) => self.finish_version_switch(&plan),
            Err(error) => {
                crate::system_log::error(format!("Cannot switch version: {error}"));
                self.toast_error(format!("Cannot switch version: {error}"));
            }
        }
    }

    /// Apply through [`confirm_version_switch`] and report the outcome.
    fn finish_version_switch(&mut self, plan: &VersionSwitchPlan) {
        let pinned = self.pinned_nodes.clone();
        match confirm_version_switch(&mut self.graph, &mut self.undo_history, &pinned, plan) {
            SwitchOutcome::Applied { removed } => {
                self.is_dirty = true;
                let message = format!(
                    "Switched {} from {} to {}{}.",
                    plan.to.id,
                    plan.from.version,
                    plan.to.version,
                    match removed {
                        0 => String::new(),
                        n => format!(" ({n} connection(s) removed)"),
                    }
                );
                crate::system_log::log(message.clone());
                self.toast_info(message);
            }
            SwitchOutcome::Reconfirm(fresh) => {
                let document = self.documents.active().id;
                self.set_pending_version_switch(Some(PendingVersionSwitch {
                    document,
                    plan: *fresh,
                }));
            }
            SwitchOutcome::Invalidated(reason) | SwitchOutcome::Failed(reason) => {
                crate::system_log::warn(format!("Version not switched: {reason}"));
                self.toast_warn(format!("Version not switched: {reason}"));
            }
        }
    }

    /// The modal confirmation window.
    pub(super) fn render_version_switch_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.pending_version_switch else {
            return;
        };
        // Bound to its document; the quit dialog supersedes it.
        if pending.document != self.documents.active().id || self.pending_quit_dialog {
            self.set_pending_version_switch(None);
            return;
        }
        let plan = pending.plan.clone();
        let lines = describe_affected_ports(&plan, &self.graph);
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Switch Custom Node Version")
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "Switch {} from {} to {}?",
                    plan.to.id, plan.from.version, plan.to.version
                ));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Some ports of {} do not exist in {} with the same type. \
                         {} connection(s) will be removed:",
                        plan.from.version, plan.to.version, plan.removed_connections
                    ))
                    .color(egui::Color32::from_rgb(255, 200, 100)),
                );
                for line in &lines {
                    ui.label(format!("• {line}"));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Switch Version").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            self.set_pending_version_switch(None);
        } else if confirm {
            self.set_pending_version_switch(None);
            self.finish_version_switch(&plan);
        }
    }
}
