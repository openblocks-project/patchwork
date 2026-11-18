//! P4.3.2E1: content identity and the content ledger.

use super::*;
use crate::capability::Capability;

const NET: CapSet = CapSet::of(&[Capability::Network]);
use crate::library::package::tests::TempDir;
use serde_json::json;

fn ledger(tmp: &TempDir) -> ContentLedger {
    ContentLedger::at(tmp.0.join("trust").join(CONTENT_FILE))
}

fn project_text(value: f64) -> String {
    serde_json::to_string_pretty(&json!({
        "graph": {"nodes": {"1": {"id": 1, "pos": [0.0, 0.0], "node_type": {"Slider": {"value": value}}}},
                  "connections": [], "next_id": 2},
        "canvas_offset": [0.0, 0.0], "canvas_zoom": 1.0, "pinned_nodes": []
    }))
    .unwrap()
}

#[test]
fn identity_is_the_graph_content_not_its_file() {
    let text = project_text(0.5);
    let digest = project_digest(&text).unwrap();
    assert_eq!(digest.len(), 64);
    // Formatting, key order and project UI fields do not matter.
    let compact: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(project_digest(&compact.to_string()).unwrap(), digest);
    let mut ui_changed = compact.clone();
    ui_changed["canvas_zoom"] = json!(3.0);
    ui_changed["pinned_nodes"] = json!([1]);
    assert_eq!(project_digest(&ui_changed.to_string()).unwrap(), digest);
    // Forged provenance outside the graph is ignored; inside the graph it is
    // simply different content (never a reason to trust it).
    let mut forged = compact.clone();
    forged["provenance"] = json!({"kind": "local_publish", "authored_here": true});
    assert_eq!(project_digest(&forged.to_string()).unwrap(), digest);
    let mut forged_inside = compact.clone();
    forged_inside["graph"]["authored_here"] = json!(true);
    assert_ne!(project_digest(&forged_inside.to_string()).unwrap(), digest);
    // Any change to the graph is different content.
    assert_ne!(project_digest(&project_text(0.6)).unwrap(), digest);
    // A bare graph file and a project wrapping the same graph agree.
    assert_eq!(
        project_digest(&compact["graph"].to_string()).unwrap(),
        digest
    );
    // Kinds are separate: the same JSON as a custom node is other content.
    assert_ne!(
        custom_node_digest(&compact["graph"].to_string()).unwrap(),
        digest
    );
    assert!(project_digest("{ truncated").is_err());
}

#[test]
fn decisions_are_recorded_and_survive_restarts() {
    let tmp = TempDir::new("content_ledger");
    let digest = project_digest(&project_text(0.5)).unwrap();
    assert_eq!(ledger(&tmp).load(), LedgerLoad::Missing);
    assert!(matches!(
        ledger(&tmp).classify(ContentKind::Project, &digest),
        ContentState::Unverified {
            digest: Some(_),
            note: None,
            ..
        }
    ));
    ledger(&tmp)
        .record(
            ContentKind::Project,
            &digest,
            ContentDecision::Authored,
            false,
        )
        .unwrap();
    // A new handle, as after a restart.
    assert_eq!(
        ledger(&tmp).classify(ContentKind::Project, &digest),
        ContentState::Authored
    );
    assert!(matches!(
        ledger(&tmp).classify(ContentKind::CustomNode, &digest),
        ContentState::Unverified { .. }
    ));

    // A denial is never replaced by an automatic record, only explicitly.
    ledger(&tmp)
        .record(ContentKind::Project, &digest, ContentDecision::Denied, true)
        .unwrap();
    ledger(&tmp)
        .record(
            ContentKind::Project,
            &digest,
            ContentDecision::Authored,
            false,
        )
        .unwrap();
    assert!(matches!(
        ledger(&tmp).classify(ContentKind::Project, &digest),
        ContentState::Denied { .. }
    ));
    ledger(&tmp)
        .record(
            ContentKind::Project,
            &digest,
            ContentDecision::Allowed(NET),
            true,
        )
        .unwrap();
    assert_eq!(
        ledger(&tmp).classify(ContentKind::Project, &digest),
        ContentState::Allowed(NET)
    );
    assert!(
        ledger(&tmp)
            .record(
                ContentKind::Project,
                "abc",
                ContentDecision::Allowed(NET),
                true
            )
            .is_err()
    );
}

#[test]
fn a_damaged_ledger_trusts_nothing_and_is_never_overwritten() {
    let tmp = TempDir::new("content_damaged");
    let digest = project_digest(&project_text(0.5)).unwrap();
    ledger(&tmp)
        .record(
            ContentKind::Project,
            &digest,
            ContentDecision::Authored,
            false,
        )
        .unwrap();
    let good = std::fs::read_to_string(ledger(&tmp).path()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&good).unwrap();
    let entry = value["entries"][0].clone();
    for (what, text) in [
        ("truncated", good[..good.len() / 2].to_string()),
        ("garbage", "not json".to_string()),
        ("wrong schema", good.replace(CONTENT_SCHEMA, "other")),
        ("unknown field", {
            let mut v = value.clone();
            v["entries"][0]["trusted"] = json!(true);
            v.to_string()
        }),
        ("duplicate", {
            value["entries"] = json!([entry.clone(), entry.clone()]);
            value.to_string()
        }),
        ("bad digest", {
            let mut v = value.clone();
            v["entries"] = json!([{"kind": "project", "sha256": "xyz", "decision": "authored", "recorded_at": 1}]);
            v.to_string()
        }),
        ("unknown decision", {
            let mut v = value.clone();
            v["entries"] = json!([{"kind": "project", "sha256": digest, "decision": "trusted", "recorded_at": 1}]);
            v.to_string()
        }),
    ] {
        std::fs::write(ledger(&tmp).path(), &text).unwrap();
        assert!(
            matches!(ledger(&tmp).load(), LedgerLoad::Damaged(_)),
            "{what}"
        );
        assert!(
            matches!(
                ledger(&tmp).classify(ContentKind::Project, &digest),
                ContentState::Unverified { note: Some(_), .. }
            ),
            "{what}: nothing is authored"
        );
        assert!(
            ledger(&tmp)
                .record(
                    ContentKind::Project,
                    &digest,
                    ContentDecision::Allowed(NET),
                    true
                )
                .is_err(),
            "{what}"
        );
        assert_eq!(
            std::fs::read_to_string(ledger(&tmp).path()).unwrap(),
            text,
            "{what}"
        );
    }
}

#[test]
fn file_decisions_apply_only_to_the_reviewed_content() {
    let tmp = TempDir::new("content_file");
    let path = tmp.0.join("Mine.pwnode");
    std::fs::write(
        &path,
        r#"{"version":1,"name":"Mine","graph":{"nodes":{},"connections":[],"next_id":1}}"#,
    )
    .unwrap();
    let reviewed = custom_node_digest(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // Changed after review: refused.
    std::fs::write(
        &path,
        r#"{"version":1,"name":"Other","graph":{"nodes":{},"connections":[],"next_id":1}}"#,
    )
    .unwrap();
    assert!(
        ledger(&tmp)
            .decide_file(ContentKind::CustomNode, &path, &reviewed, true)
            .is_err()
    );
    assert_eq!(ledger(&tmp).load(), LedgerLoad::Missing);
    // Unchanged: recorded.
    let current = custom_node_digest(&std::fs::read_to_string(&path).unwrap()).unwrap();
    ledger(&tmp)
        .decide_file(ContentKind::CustomNode, &path, &current, false)
        .unwrap();
    assert!(matches!(
        ledger(&tmp).classify(ContentKind::CustomNode, &current),
        ContentState::Denied { .. }
    ));
}

#[test]
fn only_runnable_content_passes_its_provenance_on() {
    // What a save records, per state of the content written.
    assert_eq!(
        ContentState::Authored.lineage(),
        Some(ContentDecision::Authored)
    );
    assert_eq!(
        ContentState::Allowed(NET).lineage(),
        Some(ContentDecision::Allowed(NET))
    );
    let unverified = ContentState::Unverified {
        kind: ContentKind::Project,
        digest: Some("a".repeat(64)),
        note: None,
    };
    assert_eq!(
        unverified.lineage(),
        None,
        "saving imported content records nothing"
    );
    assert_eq!(
        ContentState::Denied {
            kind: ContentKind::Project,
            digest: "a".repeat(64)
        }
        .lineage(),
        None
    );
    // After a save, unverified content is identified by what was written,
    // and stays unverified.
    let mut state = unverified;
    state.after_write(ContentKind::Project, "b".repeat(64));
    assert_eq!(
        state,
        ContentState::Unverified {
            kind: ContentKind::Project,
            digest: Some("b".repeat(64)),
            note: None
        }
    );
    let mut authored = ContentState::Authored;
    authored.after_write(ContentKind::Project, "c".repeat(64));
    assert_eq!(authored, ContentState::Authored);
}

#[test]
fn p4_3_2e1_ledgers_allow_running_without_capabilities() {
    let tmp = TempDir::new("content_format1");
    let digest = project_digest(&project_text(0.5)).unwrap();
    let other = project_digest(&project_text(0.7)).unwrap();
    std::fs::create_dir_all(tmp.0.join("trust")).unwrap();
    let format1 = json!({
        "schema": CONTENT_SCHEMA, "format": 1,
        "entries": [
            {"kind": "project", "sha256": digest, "decision": "allowed", "recorded_at": 1},
            {"kind": "project", "sha256": other, "decision": "authored", "recorded_at": 1}
        ]
    });
    std::fs::write(ledger(&tmp).path(), format1.to_string()).unwrap();
    assert_eq!(
        ledger(&tmp).classify(ContentKind::Project, &digest),
        ContentState::Allowed(CapSet::EMPTY),
        "still runs, with no privileged capability until approved again"
    );
    assert_eq!(
        ledger(&tmp).classify(ContentKind::Project, &other),
        ContentState::Authored
    );
    // Any write upgrades the file to format 2 with explicit sets.
    ledger(&tmp)
        .record(
            ContentKind::Project,
            &digest,
            ContentDecision::Allowed(NET),
            true,
        )
        .unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(ledger(&tmp).path()).unwrap()).unwrap();
    assert_eq!(written["format"], 2);
    assert_eq!(
        ledger(&tmp).classify(ContentKind::Project, &digest),
        ContentState::Allowed(NET)
    );
    // Format 2 requires the set on allowed entries, and only there.
    for entry in [
        json!({"kind": "project", "sha256": digest, "decision": "allowed", "recorded_at": 1}),
        json!({"kind": "project", "sha256": digest, "decision": "authored",
               "approved": ["network"], "recorded_at": 1}),
        json!({"kind": "project", "sha256": digest, "decision": "allowed",
               "approved": ["teleport"], "recorded_at": 1}),
    ] {
        let file = json!({"schema": CONTENT_SCHEMA, "format": 2, "entries": [entry]});
        std::fs::write(ledger(&tmp).path(), file.to_string()).unwrap();
        assert!(
            matches!(ledger(&tmp).load(), LedgerLoad::Damaged(_)),
            "{file}"
        );
    }
}

#[test]
fn allowing_a_file_approves_exactly_what_it_uses() {
    let tmp = TempDir::new("content_file_caps");
    let path = tmp.0.join("Net.pwnode");
    std::fs::write(
        &path,
        r#"{"version":1,"name":"Net","graph":{"nodes":{"1":{"id":1,"pos":[0.0,0.0],"node_type":{"HttpRequest":{}}}},"connections":[],"next_id":2}}"#,
    )
    .unwrap();
    let digest = custom_node_digest(&std::fs::read_to_string(&path).unwrap()).unwrap();
    ledger(&tmp)
        .decide_file(ContentKind::CustomNode, &path, &digest, true)
        .unwrap();
    assert_eq!(
        ledger(&tmp).classify(ContentKind::CustomNode, &digest),
        ContentState::Allowed(NET)
    );
}

#[test]
fn graphs_from_files_start_unverified_and_new_graphs_are_authored() {
    let graph = crate::graph::Graph::new();
    assert_eq!(graph.content, ContentState::Authored);
    assert!(graph.content.runnable());
    let restored: crate::graph::Graph =
        serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
    assert!(
        !restored.content.runnable(),
        "deserialized graphs never run by default"
    );
    assert!(matches!(
        restored.content,
        ContentState::Unverified { digest: None, .. }
    ));
    assert_eq!(
        graph.clone().content,
        ContentState::Authored,
        "clones keep the state"
    );
    assert!(
        !ContentState::Pending {
            kind: ContentKind::Project,
            digest: "a".repeat(64)
        }
        .runnable()
    );
}
