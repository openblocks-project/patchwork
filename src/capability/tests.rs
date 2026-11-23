//! P4.3.2E2: capability sets, contexts, propagation and revocation.

use super::*;
use crate::library::trust::authorize::TEST_DEFAULT_ALLOW;

/// No implicit allow on this thread (as in production).
struct Enforce;

impl Enforce {
    fn new() -> Self {
        TEST_DEFAULT_ALLOW.with(|allow| allow.set(false));
        Self
    }
}

impl Drop for Enforce {
    fn drop(&mut self) {
        TEST_DEFAULT_ALLOW.with(|allow| allow.set(true));
    }
}

const FS: Capability = Capability::Filesystem;
const NET: Capability = Capability::Network;

#[test]
fn capability_sets_round_trip_and_combine() {
    assert_eq!(CapSet::ALL.iter().count(), Capability::ALL.len());
    for capability in Capability::ALL {
        let set = CapSet::EMPTY.with(capability);
        assert!(set.contains(capability));
        assert_eq!(CapSet::parse_names(&set.names()), Ok(set));
    }
    let both = CapSet::of(&[FS, NET]);
    assert_eq!(both.intersect(CapSet::of(&[NET])), CapSet::of(&[NET]));
    assert_eq!(both.minus(CapSet::of(&[NET])), CapSet::of(&[FS]));
    assert!(CapSet::of(&[FS]).is_subset(both));
    assert!(!both.is_subset(CapSet::of(&[FS])));
    assert_eq!(CapSet::parse_names(&CapSet::ALL.names()), Ok(CapSet::ALL));
    assert!(CapSet::parse_names(&["teleport".to_string()]).is_err());
    assert_eq!(CapSet::EMPTY.describe(), "none");
}

#[test]
fn checks_follow_the_context_in_force_and_fail_closed_without_one() {
    let _enforce = Enforce::new();
    // No context: nothing is allowed.
    assert!(current().is_none());
    assert!(check(FS).is_err());
    assert_eq!(effective(), CapSet::EMPTY);

    let outer = Context::new(CapSet::of(&[FS, NET]), "outer");
    let inner = Context::new(CapSet::of(&[NET]), "inner");
    {
        let _outer = outer.enter();
        assert!(check(FS).is_ok());
        {
            let _inner = inner.enter();
            let denied = check(FS).unwrap_err();
            assert_eq!(denied.capability, FS);
            assert!(denied.to_string().starts_with("Not permitted"));
            assert!(check(NET).is_ok());
        }
        // Leaving the inner context restores the outer one.
        assert!(check(FS).is_ok());
    }
    assert!(current().is_none());
    assert_eq!(outer.allowed_count(FS), 2);
    assert_eq!(inner.denied_count(FS), 1);
    assert_eq!(inner.allowed_count(NET), 1);

    // Node construction never has rights.
    let _outer = outer.enter();
    assert!(without_rights(|| check(FS)).is_err());
    assert!(check(FS).is_ok());
}

#[test]
fn workers_keep_their_context_and_stop_when_it_is_revoked() {
    let _enforce = Enforce::new();
    let context = Context::new(CapSet::of(&[FS]), "worker test");
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let worker = {
        let _entered = context.enter();
        spawn(move || {
            result_tx.send(check(FS).is_ok()).unwrap();
            go_rx.recv().unwrap();
            result_tx.send(check(FS).is_ok()).unwrap();
            result_tx.send(still_allowed(FS)).unwrap();
        })
    };
    assert!(
        result_rx.recv().unwrap(),
        "the worker has its starter's rights"
    );
    context.revoke();
    go_tx.send(()).unwrap();
    assert!(!result_rx.recv().unwrap(), "revoked: refused");
    assert!(!result_rx.recv().unwrap());
    worker.join().unwrap();
    assert_eq!(context.effective(), CapSet::EMPTY);

    // A worker started without a context has no rights at all.
    let unbound = std::thread::spawn(|| {
        TEST_DEFAULT_ALLOW.with(|allow| allow.set(false));
        check(FS).is_ok()
    });
    assert!(!unbound.join().unwrap());
}

#[test]
fn a_graph_scope_revokes_on_narrowing_and_on_drop_but_not_on_clone() {
    let mut scope = Scope::default();
    let wide = scope.ensure(CapSet::ALL, "doc");
    assert!(wide.same(&scope.ensure(CapSet::ALL, "doc")), "reused");
    let narrow = scope.ensure(CapSet::of(&[FS]), "doc");
    assert!(wide.is_revoked(), "narrowed: the old rights are withdrawn");
    assert!(!narrow.is_revoked());
    let widened = scope.ensure(CapSet::of(&[FS, NET]), "doc");
    assert!(!narrow.is_revoked(), "growth keeps what was allowed valid");
    assert!(!widened.is_revoked());

    let clone = scope.clone();
    assert!(clone.context().is_none(), "a copy of a graph has no rights");
    drop(clone);
    assert!(!widened.is_revoked(), "dropping a copy revokes nothing");

    drop(scope);
    assert!(
        widened.is_revoked(),
        "dropping the graph revokes its rights"
    );
}

#[test]
fn denials_are_reported_once_and_listed() {
    let _enforce = Enforce::new();
    let context = Context::new(CapSet::EMPTY, "listed scope");
    let _entered = context.enter();
    for _ in 0..3 {
        assert!(check(Capability::SystemInfo).is_err());
    }
    let listed = recent_denials()
        .into_iter()
        .filter(|d| d.scope == "listed scope")
        .count();
    // At most once (other tests running in parallel may have pushed it out
    // of the bounded list already).
    assert!(listed <= 1, "distinct denials are listed once");
    assert_eq!(context.denied_count(Capability::SystemInfo), 3);
}

#[test]
fn closing_a_document_revokes_its_rights_by_identity() {
    let _enforce = Enforce::new();
    let mut documents = DocumentScopes::default();
    // Three open tabs; only 1 and 3 were ever shown (2 never ran).
    let first = documents.rights(1, CapSet::ALL, "this project");
    let third = documents.rights(3, CapSet::of(&[NET]), "this project");
    assert!(documents.context(2).is_none());

    // A worker of document 3 (e.g. a server its node started).
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let worker = {
        let _entered = third.enter();
        spawn(move || {
            go_rx.recv().unwrap();
            check(NET).is_ok()
        })
    };

    // Document 3 is closed: two contexts for two open documents, which a
    // count-based cleanup would have kept.
    assert_eq!(documents.retain_open([1, 2]), 1);
    assert!(third.is_revoked(), "closed: its rights end at once");
    assert!(!first.is_revoked(), "other open documents keep theirs");
    go_tx.send(()).unwrap();
    assert!(!worker.join().unwrap(), "its workers can no longer act");
    assert!(documents.context(3).is_none());

    // Switching back and forth keeps an open document's rights.
    assert!(first.same(&documents.rights(1, CapSet::ALL, "this project")));
    // A narrower decision for a background document revokes its old
    // rights without showing it; documents that never ran are untouched.
    documents.refresh(1, CapSet::of(&[FS]), "this project");
    assert!(first.is_revoked());
    documents.refresh(2, CapSet::ALL, "this project");
    assert!(documents.context(2).is_none());
}

/// Plain `std::thread::spawn` / `thread::Builder` threads start without a
/// context, so every gated operation they reach is refused (or, before
/// P4.3.2E2, silently ran with no check). Each such site is listed here
/// with why it needs none; a new one must either use
/// `capability::spawn` / `spawn_named` / `bind` or be added with a reason
/// after review.
const PLAIN_THREADS: &[(&str, usize, &str)] = &[
    (
        "app/export.rs",
        1,
        "P6 Stage 2.3: the editor's own export job (file copies; runs no graph content)",
    ),
    (
        "app/marketplace.rs",
        2,
        "editor's own Marketplace jobs (not graph content)",
    ),
    (
        "app/mod.rs",
        2,
        "MIDI port scan and the MCP stdio service (editor's own)",
    ),
    (
        "app/trust.rs",
        1,
        "start-up trust store check (editor's own)",
    ),
    (
        "capability.rs",
        2,
        "the propagating spawn helpers themselves",
    ),
    ("capability/tests.rs", 2, "tests of context-less threads"),
    (
        "hardware.rs",
        2,
        "firmware flashing run by the user from a menu",
    ),
    (
        "hid.rs",
        2,
        "HID enumeration and readers; delivery is gated in the manager",
    ),
    ("library/marketplace/fetch.rs", 2, "test HTTP server"),
    (
        "network.rs",
        1,
        "manager thread; sends and listens are gated per node on the caller",
    ),
    ("nodes/clock_node.rs", 1, "clock ticks only"),
    (
        "nodes/video_out_node.rs",
        1,
        "waits for an already-started recorder to stop",
    ),
    (
        "nodes/video_player.rs",
        4,
        "ffmpeg stderr drains and device enumeration (not gated)",
    ),
    (
        "nodes/web_app_mjpeg.rs",
        2,
        "ffmpeg stderr drain; stdout into a frame slot (no gated call)",
    ),
    (
        "nodes/web_stream_audio.rs",
        1,
        "Opus pump into the peer opened by a gated offer",
    ),
    (
        "nodes/web_stream_h264.rs",
        1,
        "ffmpeg stderr drain (the NAL reader keeps its context)",
    ),
    ("nodes/web_stream_node.rs", 1, "test of the pre-fix shape"),
    ("transport.rs", 1, "transport clock"),
    (
        "video_io/file_recorder.rs",
        2,
        "ffmpeg stderr drain and exit wait",
    ),
    ("video_io/ndi.rs", 1, "test sender"),
];

#[test]
fn every_thread_without_a_permission_context_is_reviewed() {
    fn visit(dir: &std::path::Path, root: &std::path::Path, found: &mut Vec<(String, usize)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, root, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let count = text
                    .lines()
                    .filter(|line| {
                        let code = line.split("//").next().unwrap_or("");
                        (code.contains("thread::spawn(") || code.contains("thread::Builder::new()"))
                            && !code.contains("capability::spawn")
                    })
                    .count();
                if count > 0 {
                    let relative = path.strip_prefix(root).unwrap();
                    let name = relative
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/");
                    found.push((name, count));
                }
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    visit(&root, &root, &mut found);
    found.sort();
    let mut listed: Vec<(String, usize)> = PLAIN_THREADS
        .iter()
        .map(|(file, count, _)| (file.to_string(), *count))
        .collect();
    listed.sort();
    assert_eq!(
        found, listed,
        "a thread started without `capability::spawn` was added or removed: \
         propagate the context, or review it and update PLAIN_THREADS"
    );
}
