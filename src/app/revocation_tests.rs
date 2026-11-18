//! P4.3.2F: open documents follow the decision for the exact content they
//! are bound to: denial and revocation stop them at once (shown or not),
//! and only an explicit grant lets them run again.

use super::provenance::{recheck_document, resolve_graph_with};
use super::undo::{UndoHistory, UndoSnapshot, restore_keeping_rights};
use crate::capability::{CapSet, Capability, Context, DocumentScopes};
use crate::graph::Graph;
use crate::library::package::tests::TempDir;
use crate::library::trust::authorize::{TEST_DEFAULT_ALLOW, TrustPolicy};
use crate::library::trust::content::{
    ContentBasis, ContentDecision, ContentKind, ContentLedger, ContentState, custom_node_digest,
    follow_decision,
};

const FS: CapSet = CapSet::of(&[Capability::Filesystem]);
const NET: CapSet = CapSet::of(&[Capability::Network]);

/// No implicit allow on this thread (as in production).
struct Enforce;

impl Enforce {
    fn new() -> Self {
        TEST_DEFAULT_ALLOW.with(|allow| allow.set(false));
        Enforce
    }
}

impl Drop for Enforce {
    fn drop(&mut self) {
        TEST_DEFAULT_ALLOW.with(|allow| allow.set(true));
    }
}

struct Fx {
    tmp: TempDir,
    ledger: ContentLedger,
    _enforce: Enforce,
}

fn fx(tag: &str) -> Fx {
    let tmp = TempDir::new(tag);
    let ledger = ContentLedger::at(tmp.0.join("home").join("trust").join("content.json"));
    Fx {
        tmp,
        ledger,
        _enforce: Enforce::new(),
    }
}

/// A loose custom node that reads a file (uses the filesystem).
const READER: &str = r#"{"version":1,"name":"Reader","graph":{"nodes":{"1":{"id":1,"pos":[0.0,0.0],"node_type":{"FolderBrowser":{"path":"","selected_file":"secret.txt","search":"","cached_content":"","cached_content_path":""}}},"2":{"id":2,"pos":[200.0,0.0],"node_type":{"Slider":{"value":0.5,"min":0.0,"max":1.0,"step":0.01,"slider_color":[0,0,0],"label":""}}}},"connections":[],"next_id":3}}"#;

impl Fx {
    fn file(&self) -> std::path::PathBuf {
        let path = self.tmp.0.join("Reader.pwnode");
        std::fs::write(&path, READER).unwrap();
        path
    }

    fn digest(&self) -> String {
        custom_node_digest(READER).unwrap()
    }

    /// Opened in a tab exactly as the editor does (identified, then looked
    /// up in the ledger, which binds the decision to its content).
    fn open_tab(&self, documents: &mut crate::document::DocumentManager) -> (u64, Graph) {
        let id = documents.open_custom_node(self.file()).unwrap();
        let mut graph = documents
            .documents
            .iter()
            .find(|d| d.id == id)
            .unwrap()
            .graph
            .clone();
        resolve_graph_with(&mut graph, Some(&self.ledger));
        (id, graph)
    }

    fn recheck(&self, graph: &mut Graph) -> bool {
        recheck_document(graph, Some(&self.ledger), &TrustPolicy::AllowAll)
    }
}

/// A worker started under `context`, reporting whether it may still read
/// files each time it is asked.
fn worker(context: &Context) -> (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<bool>) {
    let (ask_tx, ask_rx) = std::sync::mpsc::channel::<()>();
    let (answer_tx, answer_rx) = std::sync::mpsc::channel();
    let _entered = context.enter();
    crate::capability::spawn(move || {
        while ask_rx.recv().is_ok() {
            let _ = answer_tx.send(crate::capability::check(Capability::Filesystem).is_ok());
        }
    });
    (ask_tx, answer_rx)
}

fn ask(worker: &(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<bool>)) -> bool {
    worker.0.send(()).unwrap();
    worker.1.recv().unwrap()
}

#[test]
fn an_open_custom_node_tab_stops_when_its_file_is_denied() {
    let f = fx("revoke_open_tab");
    let mut documents = crate::document::DocumentManager::new();
    f.ledger
        .record(
            ContentKind::CustomNode,
            &f.digest(),
            ContentDecision::Allowed(FS),
            true,
        )
        .unwrap();
    let (tab, mut active) = f.open_tab(&mut documents);
    assert_eq!(active.content, ContentState::Allowed(FS));
    assert_eq!(
        active.content_basis,
        Some(ContentBasis::Content {
            kind: ContentKind::CustomNode,
            digest: f.digest(),
            seen: Some(ContentDecision::Allowed(FS)),
        }),
        "bound to the exact content"
    );
    // A second window on the same content, in a background tab.
    let mut background = active.clone();
    let mut scopes = DocumentScopes::default();
    let context = scopes.rights(tab, active.content.capabilities(), "this custom node");
    let background_context =
        scopes.rights(99, background.content.capabilities(), "this custom node");
    let running = worker(&context);
    let background_worker = worker(&background_context);
    assert!(ask(&running) && ask(&background_worker));
    let nodes = active.nodes.len();
    let connections = active.connections.len();

    // An edit, with its undo snapshot, from before the denial.
    let mut history = UndoHistory::new(10);
    history.push(UndoSnapshot::new(active.clone(), Default::default()));
    active.add_node(crate::graph::NodeType::FileMenu, [0.0, 300.0]);
    let nodes = nodes + 1;

    // The user denies the file (elsewhere: its instance, the Permissions
    // window, another tab).
    f.ledger
        .decide_file(ContentKind::CustomNode, &f.file(), &f.digest(), false)
        .unwrap();
    // Before P4.3.2F the open tab kept its decision.
    assert!(active.content.runnable(), "not yet re-checked");

    // The editor re-checks every open document on the trust change.
    assert!(f.recheck(&mut active));
    assert!(f.recheck(&mut background));
    for graph in [&active, &background] {
        assert_eq!(
            graph.content,
            ContentState::Denied {
                kind: ContentKind::CustomNode,
                digest: f.digest()
            }
        );
    }
    // Their contexts (and so their workers' rights) end now.
    scopes.rights(tab, active.content.capabilities(), "this custom node");
    scopes.refresh(99, background.content.capabilities(), "this custom node");
    assert!(context.is_revoked() && background_context.is_revoked());
    assert!(!ask(&running), "the running tab's worker stops");
    assert!(
        !ask(&background_worker),
        "the background tab's worker stops"
    );
    // Nodes, wires and editability are kept.
    assert_eq!(active.nodes.len(), nodes);
    assert_eq!(active.connections.len(), connections);
    assert!(
        documents
            .documents
            .iter()
            .any(|d| d.id == tab && d.is_editable())
    );

    // Undo/redo never brings the old decision back.
    let current = UndoSnapshot::new(active.clone(), Default::default());
    let previous = history.undo(current).unwrap();
    restore_keeping_rights(&mut active, previous.graph);
    assert!(!active.content.runnable(), "after undo");
    assert!(!f.recheck(&mut active), "nothing changed");

    // Session restoration: the same content, read again, is denied.
    let mut restored: Graph = serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(READER).unwrap()["graph"].clone(),
    )
    .unwrap();
    restored.content = ContentState::loaded(ContentKind::CustomNode, READER);
    resolve_graph_with(&mut restored, Some(&f.ledger));
    assert!(matches!(restored.content, ContentState::Denied { .. }));

    // Only an explicit grant lets it run again. Saving allowed content
    // with the same digest elsewhere (a lineage record) never does.
    f.ledger
        .record(
            ContentKind::CustomNode,
            &f.digest(),
            ContentDecision::Allowed(FS),
            false,
        )
        .unwrap();
    assert!(!f.recheck(&mut active));
    assert!(!active.content.runnable());
    assert!(
        f.ledger
            .set_approved(ContentKind::CustomNode, &f.digest(), FS, false)
            .is_err(),
        "allowing denied content needs an explicit acknowledgement"
    );
    f.ledger
        .set_approved(ContentKind::CustomNode, &f.digest(), FS, true)
        .unwrap();
    assert!(f.recheck(&mut active));
    assert_eq!(active.content, ContentState::Allowed(FS));
}

#[test]
fn revoking_or_narrowing_an_approval_reaches_open_documents() {
    let f = fx("revoke_narrow");
    let both = FS.union(NET);
    f.ledger
        .record(
            ContentKind::CustomNode,
            &f.digest(),
            ContentDecision::Allowed(both),
            true,
        )
        .unwrap();
    let mut documents = crate::document::DocumentManager::new();
    let (_, mut graph) = f.open_tab(&mut documents);
    assert_eq!(graph.content.capabilities(), both);

    // Narrowed: exactly what was withdrawn is removed.
    f.ledger
        .set_approved(ContentKind::CustomNode, &f.digest(), NET, false)
        .unwrap();
    assert!(f.recheck(&mut graph));
    assert_eq!(graph.content, ContentState::Allowed(NET));

    // Revoked: it needs a decision again (stops).
    f.ledger
        .revoke(ContentKind::CustomNode, &f.digest())
        .unwrap();
    assert!(f.recheck(&mut graph));
    assert!(!graph.content.runnable());
    assert!(matches!(graph.content, ContentState::Unverified { .. }));
    // An unrelated change leaves it as it is.
    assert!(!f.recheck(&mut graph));
}

#[test]
fn following_a_decision_never_widens_what_the_document_may_do() {
    let basis = |seen| ContentBasis::Content {
        kind: ContentKind::Project,
        digest: "a".repeat(64),
        seen,
    };
    // Held back from an MCP edit (network): a narrowing elsewhere removes
    // only what was withdrawn, and never restores what was held back.
    let mut state = ContentState::Allowed(CapSet::ALL.minus(NET));
    let mut bound = basis(Some(ContentDecision::Authored));
    assert!(follow_decision(
        &mut state,
        &mut bound,
        Some(ContentDecision::Allowed(CapSet::ALL.minus(FS))),
        None
    ));
    assert_eq!(
        state,
        ContentState::Allowed(CapSet::ALL.minus(NET).minus(FS))
    );

    // Content authored in this session keeps running when its record is
    // gone or unreadable (its lineage is this editor's)...
    let mut state = ContentState::Authored;
    let mut bound = basis(Some(ContentDecision::Authored));
    assert!(!follow_decision(
        &mut state,
        &mut bound,
        None,
        Some("damaged".into())
    ));
    assert_eq!(state, ContentState::Authored);
    // ...but allowed content stops, with the reason.
    let mut state = ContentState::Allowed(FS);
    let mut bound = basis(Some(ContentDecision::Allowed(FS)));
    assert!(follow_decision(
        &mut state,
        &mut bound,
        None,
        Some("damaged".into())
    ));
    assert!(matches!(
        state,
        ContentState::Unverified { note: Some(ref note), .. } if note == "damaged"
    ));
    // An authored record never restores a stopped document by itself.
    assert!(!follow_decision(
        &mut state,
        &mut bound,
        Some(ContentDecision::Authored),
        None
    ));
    assert!(!state.runnable());
}

#[test]
fn a_package_open_in_a_tab_stops_when_its_grant_is_revoked() {
    use crate::library::trust::content::follow_package;
    let mut state = ContentState::Allowed(FS);
    let mut basis = ContentBasis::Package {
        entry: "pkg/Gain.pwnode".into(),
        seen: Some(FS),
    };
    assert!(follow_package(
        &mut state,
        &mut basis,
        Err("Needs permission".to_string())
    ));
    assert!(!state.runnable());
    assert!(!follow_package(
        &mut state,
        &mut basis,
        Err("Needs permission".to_string())
    ));
    // Allowed again (an explicit grant in the package review).
    assert!(follow_package(&mut state, &mut basis, Ok(FS)));
    assert_eq!(state, ContentState::Allowed(FS));
}

#[test]
fn a_damaged_ledger_is_never_trusted_or_overwritten_and_recovery_starts_empty() {
    let f = fx("revoke_damaged_ledger");
    f.ledger
        .record(
            ContentKind::CustomNode,
            &f.digest(),
            ContentDecision::Allowed(FS),
            true,
        )
        .unwrap();
    let mut documents = crate::document::DocumentManager::new();
    let (_, mut graph) = f.open_tab(&mut documents);
    assert!(graph.content.runnable());

    // The ledger is damaged (tampered, truncated).
    std::fs::write(f.ledger.path(), b"{ not json").unwrap();
    let damaged = std::fs::read(f.ledger.path()).unwrap();
    assert!(f.ledger.entries().is_err());
    assert!(
        f.ledger
            .record(
                ContentKind::Project,
                &"b".repeat(64),
                ContentDecision::Authored,
                false
            )
            .is_err()
    );
    assert!(
        f.ledger
            .revoke(ContentKind::CustomNode, &f.digest())
            .is_err()
    );
    assert_eq!(
        std::fs::read(f.ledger.path()).unwrap(),
        damaged,
        "never written over"
    );
    // Open allowed content stops; newly opened content is unverified.
    assert!(f.recheck(&mut graph));
    assert!(!graph.content.runnable());
    let (_, fresh) = {
        let mut documents = crate::document::DocumentManager::new();
        f.open_tab(&mut documents)
    };
    assert!(!fresh.content.runnable());

    // Recovery moves it aside and starts empty: nothing it covered is
    // trusted again by itself.
    let aside = f.ledger.move_damaged_aside().unwrap();
    assert_eq!(
        std::fs::read(&aside).unwrap(),
        damaged,
        "kept for inspection"
    );
    assert!(f.ledger.entries().unwrap().is_empty());
    assert!(!f.recheck(&mut graph));
    assert!(!graph.content.runnable());
    assert!(f.ledger.move_damaged_aside().is_err(), "only when damaged");
    // The user's file was not touched.
    assert_eq!(std::fs::read_to_string(f.file()).unwrap(), READER);
}

// ── The Permissions window's actions (P4.3.2F) ───────────────────────────

#[test]
fn permission_management_actions_never_grant_more_than_chosen() {
    use super::permissions_manage::{ManageAction, apply, snapshot};
    let f = fx("revoke_manage");
    let store = crate::library::trust::TrustStore::at(
        f.tmp.0.join("home").join("trust").join("packages.json"),
    );
    let ledger = store.content_ledger();
    let digest = f.digest();
    ledger
        .record(
            ContentKind::CustomNode,
            &digest,
            ContentDecision::Allowed(FS),
            true,
        )
        .unwrap();
    let listed = snapshot(Some(&store), None, None);
    let entries = listed.content.as_ref().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].digest, digest, "exact content identity shown");

    // Widening without the acknowledgement is refused and changes nothing.
    let widen = ManageAction::SetContent {
        kind: ContentKind::CustomNode,
        digest: digest.clone(),
        approved: FS.union(NET),
        acknowledged: false,
    };
    assert!(apply(&widen, &store, None).is_err());
    assert_eq!(
        ledger.decision(ContentKind::CustomNode, &digest),
        Ok(Some(ContentDecision::Allowed(FS)))
    );
    // Narrowing needs nothing more; the listing is then outdated.
    let narrow = ManageAction::SetContent {
        kind: ContentKind::CustomNode,
        digest: digest.clone(),
        approved: CapSet::EMPTY,
        acknowledged: false,
    };
    apply(&narrow, &store, None).unwrap();
    assert!(listed.is_outdated());
    assert_eq!(
        ledger.decision(ContentKind::CustomNode, &digest),
        Ok(Some(ContentDecision::Allowed(CapSet::EMPTY)))
    );
    apply(
        &ManageAction::RevokeContent(ContentKind::CustomNode, digest.clone()),
        &store,
        None,
    )
    .unwrap();
    assert_eq!(ledger.decision(ContentKind::CustomNode, &digest), Ok(None));

    // Clean-up never touches anything but installation leftovers.
    let library = f.tmp.0.join("library");
    std::fs::create_dir_all(library.join("packages")).unwrap();
    let project = f.tmp.0.join(".staging-my-project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("project.json"), "{}").unwrap();
    assert!(
        apply(
            &ManageAction::RemoveStaging(project.clone()),
            &store,
            Some(&library)
        )
        .is_err()
    );
    assert!(
        project.join("project.json").exists(),
        "a user folder is never removed"
    );
    assert!(
        apply(&ManageAction::MoveLedgerAside, &store, None).is_err(),
        "not damaged"
    );
}
