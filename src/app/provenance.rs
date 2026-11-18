//! P4.3.2E1: provenance of the editor's top-level graph (projects and
//! custom-node tabs) and of the custom-node files Patchwork writes.
//!
//! See `library::trust::content` for the model. In short:
//! * a graph loaded from a file is identified by its content and looked up
//!   in the content ledger (`authored`, `allowed`, `denied` or nothing);
//! * a graph that is not authored or allowed keeps its nodes, wires and
//!   editability, but does not run: the runtime part of the frame and the
//!   audio engine see an empty graph, and node bodies are drawn inert;
//! * whatever Patchwork writes inherits the decision of the content it was
//!   written from, and content that may not run is never recorded.
//!
//! P4.3.2E2: a running document has rights (`crate::capability`). Authored
//! content has every capability (local-author policy); allowed content has
//! exactly the capabilities the user approved, which saved copies inherit.
//! Edits never widen them: a node added to an allowed document, by hand or
//! by MCP, only runs its privileged operations once the user approves the
//! new capabilities. MCP edits that bring a capability into an authored
//! document for the first time withhold it the same way.

use crate::capability::CapSet;
use crate::graph::Graph;
use crate::library::trust::authorize::TrustPolicy;
use crate::library::trust::content::{
    ContentBasis, ContentDecision, ContentKind, ContentLedger, ContentState, digest_value,
    follow_decision, follow_package,
};

/// The content ledger of the editor's trust store.
pub(super) fn editor_ledger() -> Option<ContentLedger> {
    match super::permissions::editor_trust_policy() {
        TrustPolicy::Store(store) => Some(store.content_ledger()),
        _ => None,
    }
}

/// Record what Patchwork just wrote (`digest`) from content in `state`.
pub(super) fn record_written(state: &ContentState, kind: ContentKind, digest: &str) {
    let Some(decision) = state.lineage() else {
        return;
    };
    match editor_ledger() {
        Some(ledger) => {
            if let Err(error) = ledger.record(kind, digest, decision, false) {
                crate::system_log::warn(format!(
                    "The {} was saved, but its provenance could not be recorded: {error}",
                    kind.label()
                ));
            }
        }
        None => crate::system_log::warn("Content provenance is unavailable (no home folder)."),
    }
}

/// Look up content that was just loaded.
pub(super) fn resolve(state: &mut ContentState) {
    if let ContentState::Pending { kind, digest } = state.clone() {
        *state = match editor_ledger() {
            Some(ledger) => ledger.classify(kind, &digest),
            None => ContentState::Unverified {
                kind,
                digest: Some(digest),
                note: Some("permissions are unavailable".to_string()),
            },
        };
    }
}

/// P4.3.2F: identify a loaded top-level graph (see [`resolve`]) and bind
/// its decision to the exact content it was loaded as.
pub(super) fn resolve_graph(graph: &mut Graph) {
    resolve_graph_with(graph, editor_ledger().as_ref());
}

pub(super) fn resolve_graph_with(graph: &mut Graph, ledger: Option<&ContentLedger>) {
    let ContentState::Pending { kind, digest } = graph.content.clone() else {
        return;
    };
    graph.content = match ledger {
        Some(ledger) => ledger.classify(kind, &digest),
        None => ContentState::Unverified {
            kind,
            digest: Some(digest.clone()),
            note: Some("permissions are unavailable".to_string()),
        },
    };
    bind_content(graph, kind, &digest, ledger);
}

/// P4.3.2F: bind an open document's decision to `digest` (what it was just
/// identified, saved or decided as), with the ledger's current decision.
pub(super) fn bind_content(
    graph: &mut Graph,
    kind: ContentKind,
    digest: &str,
    ledger: Option<&ContentLedger>,
) {
    let seen = ledger.and_then(|ledger| ledger.decision(kind, digest).ok().flatten());
    graph.content_basis = Some(ContentBasis::Content {
        kind,
        digest: digest.to_string(),
        seen,
    });
}

/// P4.3.2F: check an open document (shown or not) against what its
/// decision is bound to, after a grant or decision changed. A denial or
/// revocation stops it at once; only an explicit grant for exactly its
/// content lets it run again. Its nodes, wires and editability are kept.
/// Returns whether its state changed.
pub(super) fn recheck_document(
    graph: &mut Graph,
    ledger: Option<&ContentLedger>,
    policy: &TrustPolicy,
) -> bool {
    let Some(mut basis) = graph.content_basis.take() else {
        return false;
    };
    let changed = match &basis {
        ContentBasis::Content { kind, digest, .. } => {
            let (now, note) = match ledger.map(|ledger| ledger.decision(*kind, digest)) {
                Some(Ok(now)) => (now, None),
                Some(Err(why)) => (None, Some(why)),
                None => (None, Some("permissions are unavailable".to_string())),
            };
            follow_decision(&mut graph.content, &mut basis, now, note)
        }
        ContentBasis::Package { entry, .. } => {
            let now = crate::document::authorize_package_tab(policy, entry);
            follow_package(&mut graph.content, &mut basis, now)
        }
    };
    graph.content_basis = Some(basis);
    changed
}

/// P4.3.2E2: the capabilities a graph's own nodes may exercise when it
/// runs (nested custom nodes are authorized separately).
pub(super) fn graph_uses(graph: &Graph) -> CapSet {
    match serde_json::to_value(graph) {
        Ok(value) => crate::library::marketplace::scan::runtime_capabilities_of_graph(&value),
        // Cannot tell: assume it may use anything (never approves less).
        Err(_) => CapSet::ALL,
    }
}

/// A cheap fingerprint of which node types a graph contains (to cache
/// [`graph_uses`]).
pub(super) fn node_types_fingerprint(graph: &Graph) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut entries: Vec<u64> = graph
        .nodes
        .values()
        .map(|node| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            node.token.hash(&mut hasher);
            std::mem::discriminant(&node.node_type).hash(&mut hasher);
            if let crate::graph::NodeType::Dynamic { inner } = &node.node_type {
                inner.node.type_tag().hash(&mut hasher);
            }
            hasher.finish()
        })
        .collect();
    entries.sort_unstable();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entries.hash(&mut hasher);
    hasher.finish()
}

/// P4.3.2E2: after an MCP edit. MCP may act on a document but never
/// widens what it may do: a capability an MCP edit brings into a running
/// document (one its nodes did not use before the edit) is withheld until
/// the user approves it. The document becomes allowed with what it could
/// already do minus that capability, so a later MCP edit cannot use it
/// either. Returns what was withheld.
pub(super) fn after_mcp_edit(state: &mut ContentState, before: CapSet, after: CapSet) -> CapSet {
    let introduced = after.minus(before);
    if introduced.is_empty() || !state.runnable() {
        return CapSet::EMPTY;
    }
    let withheld = state.capabilities().intersect(introduced);
    if !withheld.is_empty() {
        *state = ContentState::Allowed(state.capabilities().minus(introduced));
    }
    withheld
}

/// The project content digest of an in-memory graph, as it would be read
/// back from a saved project (the text round trip matters for floats).
fn project_digest_of(graph: &Graph) -> Option<String> {
    let text = serde_json::to_string(graph).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(digest_value(ContentKind::Project, &value))
}

/// How a document's rights are named in messages.
fn document_label(kind: crate::document::DocumentKind) -> &'static str {
    match kind {
        crate::document::DocumentKind::Project => "this project",
        crate::document::DocumentKind::CustomNode => "this custom node",
    }
}

/// Pasting may never let content run that could not run where it came
/// from: from a document that may not run into one that runs is refused.
pub(super) fn paste_allowed(source_runnable: bool, target_runnable: bool) -> bool {
    source_runnable || !target_runnable
}

/// P4.3.2E2: the capabilities a node copied from a document with
/// `source_rights` would gain by being pasted into one with
/// `target_rights`: those its type uses that the target grants but the
/// source never did (unapproved there, withheld from an MCP edit, or a
/// document that may not run). Such a paste is refused: it would turn
/// imported or unapproved content into content that runs with the
/// target's rights (and is saved as the target's, e.g. as authored).
pub(super) fn paste_gains(
    source_rights: CapSet,
    node_uses: CapSet,
    target_rights: CapSet,
) -> CapSet {
    node_uses.intersect(target_rights).minus(source_rights)
}

/// P4.3.2E2: the capabilities one node's type exercises when it runs.
pub(super) fn node_uses(node_type: &crate::graph::NodeType) -> CapSet {
    let mut graph = Graph::new();
    graph.add_node(node_type.clone(), [0.0, 0.0]);
    graph_uses(&graph)
}

/// P4.3.2E2: why a paste is refused, if it is.
pub(super) fn paste_refusal(
    source_runnable: bool,
    source_rights: CapSet,
    node_type: &crate::graph::NodeType,
    target: &ContentState,
) -> Option<String> {
    if !paste_allowed(source_runnable, target.runnable()) {
        return Some(
            "This node was copied from a document that is not allowed to run, so it \
             cannot be pasted into one that runs. Allow the source document first."
                .to_string(),
        );
    }
    let gains = paste_gains(source_rights, node_uses(node_type), target.capabilities());
    if gains.is_empty() {
        return None;
    }
    Some(format!(
        "This node uses {}, which was not approved for the document it was copied \
         from, so it cannot be pasted into a document that allows it. Approve it for \
         the source document first (Library \u{2192} Permissions).",
        gains.describe()
    ))
}

impl super::PatchworkApp {
    /// The clipboard node to paste, unless that would move content from a
    /// document that may not run into one that runs (which would let it run
    /// without a decision).
    /// P4.3.2E2: nor may it give a node capabilities its source document
    /// did not grant it (see [`paste_gains`]).
    pub(super) fn clipboard_for_paste(&mut self) -> Option<crate::graph::NodeType> {
        let node = self.clipboard.clone()?;
        if let Some(message) = paste_refusal(
            self.clipboard_runnable,
            self.clipboard_rights,
            &node,
            &self.graph.content,
        ) {
            self.toast_warn(message);
            return None;
        }
        Some(node)
    }

    /// Copy a node of the active document, with what its document allows.
    pub(super) fn copy_to_clipboard(&mut self, node_type: crate::graph::NodeType) {
        self.clipboard = Some(node_type);
        // P4.3.2E1: whether the source document may run.
        self.clipboard_runnable = self.graph.content.runnable();
        // P4.3.2E2: and what it may do.
        self.clipboard_rights = self.graph.content.capabilities();
    }

    /// After the active document's content was written as `digest`.
    pub(super) fn after_content_write(&mut self, kind: ContentKind, digest: &str) {
        record_written(&self.graph.content, kind, digest);
        self.graph.content.after_write(kind, digest.to_string());
        // P4.3.2F: its decision now follows the content just written.
        bind_content(&mut self.graph, kind, digest, editor_ledger().as_ref());
    }

    /// The user's decision about the active document's content. The
    /// decision is recorded for the content it was opened (or last saved)
    /// as; without a known digest an allow lasts for this session only.
    /// P4.3.2E2: allowing approves exactly the capabilities its nodes use
    /// (shown to the user before deciding).
    pub(super) fn decide_active_content(&mut self, allow: bool) -> Result<(), String> {
        let (kind, digest) = match &self.graph.content {
            ContentState::Unverified { kind, digest, .. } => (*kind, digest.clone()),
            ContentState::Denied { kind, digest } => (*kind, Some(digest.clone())),
            ContentState::Pending { kind, digest } => (*kind, Some(digest.clone())),
            ContentState::Authored | ContentState::Allowed(_) => {
                return Err("This document is already allowed to run.".to_string());
            }
        };
        let approved = graph_uses(&self.graph);
        if let Some(digest) = &digest {
            let decision = if allow {
                ContentDecision::Allowed(approved)
            } else {
                ContentDecision::Denied
            };
            editor_ledger()
                .ok_or_else(|| "Permissions are unavailable.".to_string())?
                .record(kind, digest, decision, true)?;
        }
        if let Some(digest) = &digest {
            bind_content(&mut self.graph, kind, digest, editor_ledger().as_ref());
        }
        self.graph.content = match (allow, digest) {
            (true, _) => ContentState::Allowed(approved),
            (false, Some(digest)) => ContentState::Denied { kind, digest },
            (false, None) => ContentState::Unverified {
                kind,
                digest: None,
                note: None,
            },
        };
        if allow {
            // Everything that was held back starts now.
            self.graph.audio_topology_dirty = true;
            self.caches_dirty = true;
        }
        Ok(())
    }

    /// P4.3.2E2: what the active document's own nodes use (cached by its
    /// node types).
    pub(super) fn document_uses(&mut self) -> CapSet {
        let key = node_types_fingerprint(&self.graph);
        match self.capability_uses_cache {
            Some((cached, uses)) if cached == key => uses,
            _ => {
                let uses = graph_uses(&self.graph);
                self.capability_uses_cache = Some((key, uses));
                uses
            }
        }
    }

    /// P4.3.2E2: capabilities the running document's nodes use but may not
    /// exercise (empty for authored documents).
    pub(super) fn unapproved_capabilities(&mut self) -> CapSet {
        let ContentState::Allowed(approved) = self.graph.content else {
            return CapSet::EMPTY;
        };
        self.document_uses().minus(approved)
    }

    /// P4.3.2E2: the user approved the capabilities the running document
    /// uses beyond what it was allowed. Recorded for the document's current
    /// content (projects; it applies on restart if the content is saved or
    /// unchanged) and inherited by what is saved from it.
    pub(super) fn approve_active_capabilities(&mut self) -> Result<CapSet, String> {
        let ContentState::Allowed(approved) = self.graph.content else {
            return Err("This document has no capabilities waiting for approval.".to_string());
        };
        let uses = graph_uses(&self.graph);
        let added = uses.minus(approved);
        if added.is_empty() {
            return Ok(added);
        }
        let now = approved.union(uses);
        if self.documents.active().kind == crate::document::DocumentKind::Project {
            if let Some(digest) = project_digest_of(&self.graph) {
                editor_ledger()
                    .ok_or_else(|| "Permissions are unavailable.".to_string())?
                    .record(
                        ContentKind::Project,
                        &digest,
                        ContentDecision::Allowed(now),
                        true,
                    )?;
                bind_content(
                    &mut self.graph,
                    ContentKind::Project,
                    &digest,
                    editor_ledger().as_ref(),
                );
            }
        }
        self.graph.content = ContentState::Allowed(now);
        self.capability_uses_cache = None;
        Ok(added)
    }

    /// P4.3.2F: the active document's decision changed (the Permissions
    /// window, a decision in another tab, a revoked file or package).
    fn document_decision_changed(&mut self, title: &str) {
        let label = self.graph.content.label();
        crate::system_log::warn(format!("Permissions changed for '{title}': {label}."));
        self.capability_uses_cache = None;
        self.graph.audio_topology_dirty = true;
        self.caches_dirty = true;
    }

    /// P4.3.2E2: the rights the active document runs with this frame. The
    /// returned context is entered for the whole frame; nested runtimes
    /// narrow it while they evaluate.
    ///
    /// The context belongs to the open document (not to its current graph
    /// value), so undo, redo and the frame gate's parking keep it, while a
    /// narrower decision, another project loaded into the tab (its content
    /// state changes) or closing the document revokes it.
    pub(super) fn document_rights(&mut self) -> crate::capability::Context {
        // Documents that were closed lose their rights (by identity).
        self.revoke_closed_documents();
        self.apply_trust_changes();
        let granted = self.graph.content.capabilities();
        let document = self.documents.active();
        let (id, label) = (document.id, document_label(document.kind));
        self.document_scopes.rights(id, granted, label)
    }

    /// P4.3.2E2: revoke the context of every document that is no longer
    /// open (by identity, so tabs that never ran do not matter).
    pub(super) fn revoke_closed_documents(&mut self) {
        let open = self.documents.documents.iter().map(|d| d.id);
        self.document_scopes.retain_open(open);
    }

    /// P4.3.2E2: after a grant or decision changed (a new trust
    /// generation), bring every open document in line with it now, not
    /// only the one being shown: custom-node runtimes are authorized again
    /// (narrowed rights revoke their contexts; refused ones are dropped,
    /// with their workers' rights), in the active graph and in every
    /// background tab, and each document's own context is refreshed.
    pub(super) fn apply_trust_changes(&mut self) {
        let generation = crate::library::trust::generation();
        if generation == self.trust_generation_applied {
            return;
        }
        self.trust_generation_applied = generation;
        let active = self.documents.active().id;
        // P4.3.2F: each open document's own decision, by the exact content
        // (or package) it is bound to.
        let ledger = editor_ledger();
        let policy = super::permissions::editor_trust_policy();
        if recheck_document(&mut self.graph, ledger.as_ref(), &policy) {
            let title = self.documents.active().title.clone();
            self.document_decision_changed(&title);
        }
        self.graph.apply_trust_change();
        for document in self.documents.documents.iter_mut() {
            if document.id == active {
                continue;
            }
            if recheck_document(&mut document.graph, ledger.as_ref(), &policy) {
                crate::system_log::warn(format!(
                    "Permissions changed for '{}': {}.",
                    document.title,
                    document.graph.content.label()
                ));
            }
            document.graph.apply_trust_change();
            self.document_scopes.refresh(
                document.id,
                document.graph.content.capabilities(),
                document_label(document.kind),
            );
        }
    }
}
