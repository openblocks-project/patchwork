//! P4.3.2E1: every way the editor loads a top-level graph identifies it by
//! content (so it runs only if authored here or allowed), and every way it
//! writes one reports the content it wrote (so provenance follows it).

use super::io::{read_project_file, save_project_files};
use super::provenance::paste_allowed;
use super::undo::UndoHistory;
use crate::graph::{Graph, NodeType};
use crate::library::package::tests::TempDir;
use crate::library::trust::content::{
    ContentKind, ContentState, custom_node_digest, project_digest,
};

fn slider_graph(value: f32) -> Graph {
    let mut graph = Graph::new();
    graph.add_node(
        NodeType::Slider {
            value,
            min: 0.0,
            max: 1.0,
            step: 0.01,
            slider_color: [0, 0, 0],
            label: String::new(),
        },
        [0.0, 0.0],
    );
    graph
}

fn save(graph: &mut Graph, dir: &std::path::Path) -> String {
    save_project_files(
        graph,
        &mut UndoHistory::new(10),
        Vec::new(),
        [0.0, 0.0],
        1.0,
        dir,
        None,
        None,
    )
    .unwrap()
    .content_digest
    .expect("the written content is identified")
}

fn pending(kind: ContentKind, digest: &str) -> ContentState {
    ContentState::Pending {
        kind,
        digest: digest.to_string(),
    }
}

#[test]
fn projects_are_identified_by_the_content_written() {
    let tmp = TempDir::new("prov_projects");
    let dir = tmp.0.join("Mine");
    let digest = save(&mut slider_graph(0.4), &dir);
    let text = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(project_digest(&text).unwrap(), digest, "what was written");

    // File → Open / recent projects / session reopen-from-disk and MCP
    // LoadProject all identify the graph before it may run.
    let opened = read_project_file(&dir).unwrap().graph;
    assert_eq!(opened.content, pending(ContentKind::Project, &digest));
    let mcp_loaded = crate::app::load_project_graph(&dir).unwrap();
    assert_eq!(mcp_loaded.content, pending(ContentKind::Project, &digest));
    let mut live = Graph::new();
    crate::mcp::execute_command(
        crate::mcp::McpCommand::LoadProject {
            path: dir.to_string_lossy().into_owned(),
        },
        &mut live,
        &Default::default(),
    );
    assert_eq!(live.content, pending(ContentKind::Project, &digest));

    // A copied project folder is the same content.
    let copy = tmp.0.join("Copy");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::copy(dir.join("project.json"), copy.join("project.json")).unwrap();
    assert_eq!(
        read_project_file(&copy).unwrap().graph.content,
        pending(ContentKind::Project, &digest)
    );

    // Forged metadata outside the graph changes nothing (and proves
    // nothing); edited graph content is other content.
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["provenance"] = serde_json::json!("authored on this computer");
    std::fs::write(copy.join("project.json"), value.to_string()).unwrap();
    assert_eq!(
        read_project_file(&copy).unwrap().graph.content,
        pending(ContentKind::Project, &digest)
    );
    std::fs::write(copy.join("project.json"), text.replace("0.4", "0.41")).unwrap();
    assert_ne!(
        read_project_file(&copy).unwrap().graph.content,
        pending(ContentKind::Project, &digest)
    );
}

#[test]
fn saving_reports_new_content_and_keeps_the_runtime_decision() {
    let tmp = TempDir::new("prov_save_as");
    // Save As of an unverified (imported) project: the copy is identified,
    // and the graph stays unverified (nothing would be recorded for it).
    let mut graph = slider_graph(0.4);
    graph.content = ContentState::Unverified {
        kind: ContentKind::Project,
        digest: None,
        note: None,
    };
    let digest = save(&mut graph, &tmp.0.join("SavedAs"));
    assert_eq!(graph.content.lineage(), None, "never recorded as authored");
    graph
        .content
        .after_write(ContentKind::Project, digest.clone());
    assert_eq!(
        graph.content,
        ContentState::Unverified {
            kind: ContentKind::Project,
            digest: Some(digest),
            note: None
        }
    );
    assert!(!graph.content.runnable());
}

#[test]
fn session_graphs_are_identified_without_their_other_fields() {
    // The session file holds the graph plus keys, recent projects and view
    // state; only the graph is the content.
    let graph = slider_graph(0.4);
    let session = serde_json::json!({
        "graph": serde_json::to_value(&graph).unwrap(),
        "canvas_offset": [10.0, 5.0],
        "canvas_zoom": 2.0,
        "pinned_nodes": [],
        "api_keys": {"anthropic": "sk-..."},
        "recent_projects": ["/x"],
    });
    let project = serde_json::json!({"graph": serde_json::to_value(&graph).unwrap()});
    assert_eq!(
        project_digest(&session.to_string()).unwrap(),
        project_digest(&project.to_string()).unwrap()
    );
    // A session graph restored without identification never runs.
    let restored: Graph = serde_json::from_value(session["graph"].clone()).unwrap();
    assert!(!restored.content.runnable());
}

#[test]
fn custom_node_tabs_and_copies_are_identified() {
    let tmp = TempDir::new("prov_custom_nodes");
    let file = crate::document::CustomNodeFile {
        version: 1,
        name: "Mine".into(),
        graph: slider_graph(0.2),
        inputs: Vec::new(),
        outputs: Vec::new(),
    };
    let path = tmp.0.join("Mine.pwnode");
    let written = file.save_with_digest(&path).unwrap();
    assert_eq!(
        custom_node_digest(&std::fs::read_to_string(&path).unwrap()).unwrap(),
        written
    );

    // Opened in a tab: identified, not yet runnable.
    let mut documents = crate::document::DocumentManager::new();
    let id = documents.open_custom_node(path.clone()).unwrap();
    let tab = documents.documents.iter().find(|d| d.id == id).unwrap();
    assert_eq!(
        tab.graph.content,
        pending(ContentKind::CustomNode, &written)
    );

    // Save As and Duplicate for Editing report what they wrote.
    let (_, saved_as) = crate::document::save_custom_node_as_with_digest(
        &slider_graph(0.2),
        &tmp.0.join("Other.pwnode"),
    )
    .unwrap();
    assert_eq!(
        custom_node_digest(&std::fs::read_to_string(tmp.0.join("Other.pwnode")).unwrap()).unwrap(),
        saved_as
    );
    let duplicate = tmp.0.join("Copy.pwnode");
    let (source_digest, copy_digest) =
        crate::library::package::duplicate_for_editing_with_digest(&path, &duplicate, false)
            .unwrap();
    assert_eq!(source_digest, written);
    assert_eq!(
        custom_node_digest(&std::fs::read_to_string(&duplicate).unwrap()).unwrap(),
        copy_digest
    );
}

// ── P4.3.2E2 ─────────────────────────────────────────────────────────────

fn catalog_node(label: &str) -> NodeType {
    let entry = crate::nodes::catalog()
        .into_iter()
        .find(|e| e.label == label)
        .unwrap();
    (entry.factory)()
}

const NET: crate::capability::CapSet =
    crate::capability::CapSet::of(&[crate::capability::Capability::Network]);
const PROC: crate::capability::CapSet =
    crate::capability::CapSet::of(&[crate::capability::Capability::Process]);
const FILES: crate::capability::CapSet =
    crate::capability::CapSet::of(&[crate::capability::Capability::Filesystem]);

/// An MCP command, applied as the editor applies it.
fn mcp(graph: &mut Graph, type_name: &str) -> crate::capability::CapSet {
    use super::provenance::{after_mcp_edit, graph_uses};
    let before = graph_uses(graph);
    crate::mcp::execute_command(
        crate::mcp::McpCommand::CreateNode {
            type_name: type_name.into(),
            position: [0.0, 0.0],
            properties: None,
        },
        graph,
        &Default::default(),
    );
    let after = graph_uses(graph);
    after_mcp_edit(&mut graph.content, before, after)
}

#[test]
fn mcp_edits_never_widen_what_a_document_may_do() {
    use crate::capability::CapSet;
    use crate::library::trust::content::ContentDecision;
    // An authored project: MCP brings in a network node.
    let mut graph = slider_graph(0.4);
    assert_eq!(mcp(&mut graph, "HTTP Request"), NET, "withheld");
    assert_eq!(graph.content, ContentState::Allowed(CapSet::ALL.minus(NET)));
    assert!(graph.content.runnable(), "everything else keeps running");
    // A second edit cannot bring in another capability either.
    assert_eq!(mcp(&mut graph, "Terminal"), PROC);
    let rights = graph.content.capabilities();
    assert!(!rights.contains(crate::capability::Capability::Network));
    assert!(!rights.contains(crate::capability::Capability::Process));
    // Saving records the narrowed rights, never "authored".
    assert_eq!(
        graph.content.lineage(),
        Some(ContentDecision::Allowed(CapSet::ALL.minus(NET).minus(PROC)))
    );

    // A capability the document already used is not new.
    let mut graph = slider_graph(0.4);
    graph.add_node(catalog_node("HTTP Request"), [0.0, 0.0]); // by hand
    assert_eq!(mcp(&mut graph, "HTTP Request"), CapSet::EMPTY);
    assert_eq!(graph.content, ContentState::Authored);

    // A document that may not run is not changed (nothing runs anyway).
    let mut blocked = slider_graph(0.4);
    blocked.content = ContentState::default();
    assert_eq!(mcp(&mut blocked, "Terminal"), CapSet::EMPTY);
    assert!(!blocked.content.runnable());
}

#[test]
fn allowed_documents_keep_their_approval_through_edits_saves_and_restarts() {
    use crate::library::trust::content::ContentLedger;
    let tmp = TempDir::new("prov_allowed_edits");
    let ledger = ContentLedger::at(tmp.0.join("trust").join("content.json"));
    let mut graph = slider_graph(0.4);
    graph.content = ContentState::Allowed(FILES);
    // The user adds a network node by hand: used, never approved by it.
    graph.add_node(catalog_node("HTTP Request"), [0.0, 0.0]);
    assert!(super::provenance::graph_uses(&graph).contains(crate::capability::Capability::Network));
    assert_eq!(graph.content.capabilities(), FILES);

    // Save As: the copy is recorded with exactly the same approval.
    let copy = tmp.0.join("Copy");
    let digest = save(&mut graph, &copy);
    ledger
        .record(
            ContentKind::Project,
            &digest,
            graph.content.lineage().unwrap(),
            false,
        )
        .unwrap();
    graph
        .content
        .after_write(ContentKind::Project, digest.clone());
    assert_eq!(graph.content, ContentState::Allowed(FILES));

    // Restart: reopened from disk, it has the same rights, no more.
    let reopened = read_project_file(&copy).unwrap().graph;
    let ContentState::Pending { kind, digest } = reopened.content else {
        panic!("identified on load");
    };
    assert_eq!(ledger.classify(kind, &digest), ContentState::Allowed(FILES));
}

#[test]
fn pasting_never_lets_blocked_content_run() {
    assert!(paste_allowed(true, true));
    assert!(paste_allowed(true, false));
    assert!(paste_allowed(false, false), "into another blocked document");
    assert!(!paste_allowed(false, true), "from blocked into running");
}

// ── P4.3.2E2: the clipboard never widens what content may do ─────────────

/// Copy `id` from `source` and paste it into `target` as the editor does:
/// `Ok` with the pasted node, or the refusal shown to the user.
fn copy_paste(
    source: &Graph,
    id: crate::graph::NodeId,
    target: &mut Graph,
) -> Result<crate::graph::NodeId, String> {
    use super::provenance::paste_refusal;
    let node_type = source.nodes[&id].node_type.clone();
    let (runnable, rights) = (source.content.runnable(), source.content.capabilities());
    match paste_refusal(runnable, rights, &node_type, &target.content) {
        Some(refusal) => Err(refusal),
        None => Ok(target.add_node(node_type, [40.0, 40.0])),
    }
}

#[test]
fn pasting_never_gives_a_node_capabilities_its_source_did_not_grant() {
    use super::provenance::{node_uses, paste_gains};
    use crate::capability::CapSet;
    use crate::library::trust::content::ContentDecision;

    // Between locally authored projects (other tabs too: the clipboard
    // belongs to the editor) copying works as before.
    let mut mine = slider_graph(0.1);
    let http = mine.add_node(catalog_node("HTTP Request"), [0.0, 0.0]);
    let mut other_tab = slider_graph(0.2);
    let pasted = copy_paste(&mine, http, &mut other_tab).expect("authored to authored");
    assert_eq!(other_tab.content, ContentState::Authored);
    assert!(other_tab.nodes.contains_key(&pasted));
    assert_eq!(node_uses(&mine.nodes[&http].node_type), NET);

    // An imported project allowed for files only: its network node may not
    // be pasted into an authored project (it would gain network access and
    // be saved as authored).
    let mut imported = slider_graph(0.3);
    imported.content = ContentState::Allowed(FILES);
    let restricted = imported.add_node(catalog_node("HTTP Request"), [0.0, 0.0]);
    let before = mine.nodes.len();
    let refusal = copy_paste(&imported, restricted, &mut mine).unwrap_err();
    assert!(refusal.contains("network access"), "{refusal}");
    assert_eq!(mine.nodes.len(), before, "nothing pasted");
    assert_eq!(mine.content, ContentState::Authored);
    assert_eq!(mine.content.lineage(), Some(ContentDecision::Authored));
    // Its nodes that need nothing it lacked still paste.
    let slider = *imported.nodes.keys().min().unwrap();
    assert!(copy_paste(&imported, slider, &mut mine).is_ok());

    // From a document that may not run: refused into any running one.
    let mut blocked = slider_graph(0.4);
    blocked.content = ContentState::default();
    let blocked_node = blocked.add_node(catalog_node("HTTP Request"), [0.0, 0.0]);
    assert!(copy_paste(&blocked, blocked_node, &mut mine).is_err());
    assert!(copy_paste(&blocked, blocked_node, &mut imported).is_err());
    // Into another blocked document it stays inert (allowed, as before).
    let mut blocked_too = slider_graph(0.5);
    blocked_too.content = ContentState::default();
    assert!(copy_paste(&blocked, blocked_node, &mut blocked_too).is_ok());
    assert!(!blocked_too.content.runnable());

    // A node brought in by MCP (its capability withheld) cannot be
    // laundered through the clipboard into an authored project.
    let mut agent_edited = slider_graph(0.6);
    assert_eq!(mcp(&mut agent_edited, "HTTP Request"), NET);
    let introduced = *agent_edited.nodes.keys().max().unwrap();
    assert!(copy_paste(&agent_edited, introduced, &mut mine).is_err());

    // Into a document with the same (or narrower) rights it pastes, and
    // the target's rights are unchanged: it is refused at run time there.
    let mut same_rights = slider_graph(0.7);
    same_rights.content = ContentState::Allowed(FILES);
    assert!(copy_paste(&imported, restricted, &mut same_rights).is_ok());
    assert_eq!(same_rights.content, ContentState::Allowed(FILES));
    assert_eq!(paste_gains(FILES, NET, FILES), CapSet::EMPTY);
    assert_eq!(paste_gains(FILES, NET, CapSet::ALL), NET);
    assert_eq!(paste_gains(CapSet::ALL, NET, CapSet::ALL), CapSet::EMPTY);
}

#[test]
fn undo_and_redo_after_a_paste_keep_the_documents_rights() {
    use super::undo::{UndoSnapshot, restore_keeping_rights};
    let mut imported = slider_graph(0.3);
    imported.content = ContentState::Allowed(FILES);
    let node = imported.add_node(catalog_node("HTTP Request"), [0.0, 0.0]);

    let mut target = slider_graph(0.1);
    target.content = ContentState::Allowed(FILES);
    let mut history = UndoHistory::new(10);
    history.push(UndoSnapshot::new(target.clone(), Default::default()));
    copy_paste(&imported, node, &mut target).unwrap();
    let pasted_nodes = target.nodes.len();

    let previous = history
        .undo(UndoSnapshot::new(target.clone(), Default::default()))
        .unwrap();
    restore_keeping_rights(&mut target, previous.graph);
    assert_eq!(target.content, ContentState::Allowed(FILES), "after undo");
    let next = history
        .redo(UndoSnapshot::new(target.clone(), Default::default()))
        .unwrap();
    restore_keeping_rights(&mut target, next.graph);
    assert_eq!(target.nodes.len(), pasted_nodes);
    assert_eq!(target.content, ContentState::Allowed(FILES), "after redo");
    assert_eq!(target.content.capabilities(), FILES, "never widened");
}
