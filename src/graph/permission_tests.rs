//! P4.3.2D: runtime authorization of package runtimes, through every entry
//! path. These tests switch off the test-only "allow all" default, so any
//! graph that is not given a trust store blocks every package (as in
//! production).

use super::*;
use crate::document::{LOAD_COUNT, read_custom_node_interface_static};
use crate::library::bundle::tests::{child, composite, files_under, leaf, publish};
use crate::library::package::VerifiedPackage;
use crate::library::package::tests::{TempDir, request};
use crate::library::resolve::InstancePackageRef;
use crate::library::trust::authorize::{
    AuthorizedBy, BlockReason, FULL_CHECKS, TEST_DEFAULT_ALLOW, TrustPolicy, authorize_dir,
};
use crate::library::trust::{
    Grant, PackageIdentity, ReviewDecision, StoreLoad, TrustStore, apply_review, identify,
    publish_and_record,
};

/// Enforce permissions on this test thread (restored afterwards).
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

struct Fx {
    tmp: TempDir,
    library: std::path::PathBuf,
    store_path: std::path::PathBuf,
    _enforce: Enforce,
}

fn fx(tag: &str) -> Fx {
    let enforce = Enforce::new();
    let tmp = TempDir::new(tag);
    let library = tmp.0.join("library");
    std::fs::create_dir_all(&library).unwrap();
    let store_path = tmp.0.join("home").join("trust").join("packages.json");
    Fx {
        tmp,
        library,
        store_path,
        _enforce: enforce,
    }
}

impl Fx {
    fn store(&self) -> TrustStore {
        TrustStore::at(&self.store_path)
    }

    fn policy(&self) -> TrustPolicy {
        TrustPolicy::Store(self.store())
    }

    /// A graph configured like the editor's (Library + trust store).
    fn graph(&self) -> Graph {
        let mut graph = Graph::new();
        self.configure(&mut graph, None);
        graph
    }

    fn configure(&self, graph: &mut Graph, project: Option<&std::path::Path>) {
        graph.set_package_environment(project.map(Into::into), Some(self.library.clone()));
        graph.set_trust_policy(self.policy());
    }

    fn source(&self, name: &str, value: f32) -> std::path::PathBuf {
        leaf(&self.tmp.0.join("src").join(name), name, value)
    }

    /// Published without a record (a package from before the trust store,
    /// or copied in by hand).
    fn unrecorded(&self, name: &str, value: f32, version: &str) -> VerifiedPackage {
        let existing = format!("local.{}", name.to_lowercase());
        let existing = self
            .library
            .join("packages")
            .join(&existing)
            .is_dir()
            .then_some(existing);
        publish(
            &self.library,
            &self.source(name, value),
            name,
            existing.as_deref(),
            version,
        )
        .unwrap()
    }

    /// Published by the trusted workflow (`local_publish` provenance).
    fn local(&self, name: &str, value: f32) -> VerifiedPackage {
        let (package, outcome) = publish_and_record(
            &self.store(),
            &self.library,
            &self.source(name, value),
            &request(name, "1.0.0"),
        )
        .unwrap();
        outcome.unwrap();
        package
    }

    fn grant(&self, dir: &std::path::Path) {
        self.store().grant(dir, &identity(dir)).unwrap();
    }

    fn deny(&self, dir: &std::path::Path) {
        self.store().deny(dir, &identity(dir)).unwrap();
    }

    fn edit_record(&self, edit: impl FnOnce(&mut serde_json::Value)) {
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&self.store_path).unwrap()).unwrap();
        edit(&mut value["records"][0]);
        std::fs::write(&self.store_path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
}

fn identity(dir: &std::path::Path) -> PackageIdentity {
    identify(dir, true).unwrap().identity
}

/// The node the palette inserts: package reference plus the statically
/// read interface (nothing constructed).
fn instance(package: &VerifiedPackage) -> NodeType {
    let interface = read_custom_node_interface_static(&package.entry_path).unwrap();
    NodeType::CustomNodeInstance {
        file: package.entry_path.to_string_lossy().into_owned(),
        inputs: interface.inputs,
        outputs: interface.outputs,
        package: Some(InstancePackageRef {
            id: package.manifest.id.clone(),
            version: package.version.to_string(),
            entry: package.manifest.entry.clone(),
        }),
    }
}

/// An instance wired to a downstream node.
fn wired(graph: &mut Graph, package: &VerifiedPackage) -> (NodeId, NodeId) {
    let id = graph.add_node(instance(package), [0.0, 0.0]);
    let sink = graph.add_node(
        NodeType::CustomOutput {
            name: "sink".into(),
            kind: PortKind::Number,
        },
        [200.0, 0.0],
    );
    graph.add_connection(id, 0, sink, 0);
    (id, sink)
}

fn output(graph: &mut Graph, id: NodeId) -> Option<f32> {
    match graph.evaluate(0.0).get(&(id, 0)) {
        Some(PortValue::Float(value)) => Some(*value),
        _ => None,
    }
}

fn loads() -> usize {
    LOAD_COUNT.with(|c| c.get())
}

fn reason(graph: &Graph, id: NodeId) -> Option<BlockReason> {
    graph.custom_blocked(id).map(|b| b.reason.clone())
}

/// Blocked, nothing constructed, and the node itself left intact.
fn assert_blocked(graph: &mut Graph, id: NodeId, what: &str) -> BlockReason {
    let before = loads();
    let node_before = serde_json::to_value(&graph.nodes[&id].node_type).unwrap();
    let wires_before = graph.connections.len();
    assert_eq!(output(graph, id), None, "{what}: no output");
    assert!(!graph.has_custom_runtime(id), "{what}: no runtime");
    assert_eq!(loads(), before, "{what}: nothing constructed");
    assert_eq!(
        serde_json::to_value(&graph.nodes[&id].node_type).unwrap(),
        node_before,
        "{what}: instance, ports and reference kept"
    );
    assert_eq!(graph.connections.len(), wires_before, "{what}: wires kept");
    assert!(
        matches!(
            graph.nodes[&id].node_type,
            NodeType::CustomNodeInstance { .. }
        ),
        "{what}: never replaced by a substitute node"
    );
    reason(graph, id).unwrap_or_else(|| panic!("{what}: no blocked state"))
}

// ── Policy ───────────────────────────────────────────────────────────────

#[test]
fn unknown_imported_and_unconfigured_packages_are_blocked() {
    let f = fx("perm_unknown");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");

    let mut graph = f.graph();
    let (id, sink) = wired(&mut graph, &package);
    assert_eq!(
        assert_blocked(&mut graph, id, "unrecorded"),
        BlockReason::NeedsPermission
    );
    assert!(graph.custom_blocked(id).unwrap().needs_review());
    assert!(
        graph
            .connections
            .iter()
            .any(|c| c.from_node == id && c.to_node == sink)
    );
    let message = crate::node_errors::get(id).unwrap().message;
    assert!(message.starts_with("Needs permission"), "{message}");

    // Accepted in the migration review: still only "imported", no grant.
    apply_review(
        &f.store(),
        &package.dir,
        &identity(&package.dir),
        ReviewDecision::Accept,
    )
    .unwrap();
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(
        assert_blocked(&mut graph, id, "imported"),
        BlockReason::NeedsPermission
    );

    // A graph without a configured store blocks everything.
    let mut bare = Graph::new();
    bare.set_package_environment(None, Some(f.library.clone()));
    let (id, _) = wired(&mut bare, &package);
    assert_eq!(
        assert_blocked(&mut bare, id, "unconfigured"),
        BlockReason::Unconfigured
    );
    assert!(!bare.custom_blocked(id).unwrap().needs_review());

    // A damaged store trusts nothing, not even a local package.
    let local = f.local("Local", 0.5);
    std::fs::write(&f.store_path, "{ damaged").unwrap();
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &local);
    assert!(matches!(
        assert_blocked(&mut graph, id, "damaged store"),
        BlockReason::StoreDamaged(_)
    ));
}

#[test]
fn locally_published_packages_follow_the_local_author_policy() {
    let f = fx("perm_local");
    let package = f.local("Gain", 0.25);
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(output(&mut graph, id), Some(0.25));
    assert!(graph.custom_blocked(id).is_none());
    let authorized = authorize_dir(&f.policy(), &package.dir).unwrap();
    assert_eq!(authorized.by, AuthorizedBy::LocalAuthor);

    // The user can still deny it, and that sticks.
    f.deny(&package.dir);
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(
        assert_blocked(&mut graph, id, "denied local"),
        BlockReason::Denied
    );
    assert!(
        !graph.custom_blocked(id).unwrap().needs_review(),
        "not re-prompted"
    );
}

#[test]
fn grants_and_denials_survive_restarts() {
    let f = fx("perm_restart");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");
    f.grant(&package.dir);
    let StoreLoad::Loaded(store) = f.store().load() else {
        panic!("readable");
    };
    let record = store.record_for("local.gain", &package.version).unwrap();
    assert_eq!(record.grant, Grant::Granted);
    assert_eq!(record.approved, Some(Default::default()));

    // A new graph and store handle, as after restarting Patchwork.
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(output(&mut graph, id), Some(0.25));
    assert_eq!(
        authorize_dir(&f.policy(), &package.dir).unwrap().by,
        AuthorizedBy::Grant
    );

    f.deny(&package.dir);
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(
        assert_blocked(&mut graph, id, "denied"),
        BlockReason::Denied
    );

    // Only an explicit grant reverses it.
    f.grant(&package.dir);
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(output(&mut graph, id), Some(0.25));
}

#[test]
fn a_grant_covers_one_exact_content_version_and_classification() {
    let f = fx("perm_exact");
    let v1 = f.unrecorded("Gain", 0.25, "1.0.0");
    f.grant(&v1.dir);

    // A newer version does not inherit the grant.
    let v2 = f.unrecorded("Gain", 0.5, "1.0.1");
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &v2);
    assert_eq!(
        assert_blocked(&mut graph, id, "newer version"),
        BlockReason::NeedsPermission
    );

    // Changed content (still a valid package) does not inherit it.
    std::fs::write(v1.dir.join("readme.txt"), "changed").unwrap();
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &v1);
    assert_eq!(
        assert_blocked(&mut graph, id, "changed digest"),
        BlockReason::Conflict
    );
    assert!(
        f.store().grant(&v1.dir, &identity(&v1.dir)).is_err(),
        "no silent re-grant"
    );
    std::fs::remove_file(v1.dir.join("readme.txt")).unwrap();
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &v1);
    assert_eq!(
        output(&mut graph, id),
        Some(0.25),
        "the granted bytes again"
    );

    // A grant given under an older classification needs a new decision.
    f.edit_record(|r| r["classification_version"] = 1.into());
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &v1);
    assert!(matches!(
        assert_blocked(&mut graph, id, "reclassified"),
        BlockReason::Stale(_)
    ));
    assert!(graph.custom_blocked(id).unwrap().needs_review());
    // A grant that does not approve the detected capabilities is invalid.
    f.edit_record(|r| {
        r["classification_version"] =
            crate::library::marketplace::scan::CLASSIFICATION_VERSION.into();
        r["detected_capabilities"] = serde_json::json!(["network"]);
    });
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &v1);
    assert!(matches!(
        assert_blocked(&mut graph, id, "unapproved capability"),
        BlockReason::StoreDamaged(_)
    ));
}

#[test]
fn conflicting_copies_never_inherit_a_grant() {
    let f = fx("perm_conflict");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");
    f.grant(&package.dir);
    let project = f.tmp.0.join("project");
    let copy = project.join("packages").join("local.gain").join("1.0.0");
    for rel in files_under(&package.dir) {
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::copy(package.dir.join(&rel), copy.join(&rel)).unwrap();
    }

    // A byte-identical project copy is the same content: it may run.
    let mut graph = Graph::new();
    f.configure(&mut graph, Some(&project));
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(output(&mut graph, id), Some(0.25));

    // A project copy with other bytes and the same id@version is refused,
    // and cannot be granted over the existing record.
    std::fs::write(copy.join("notes.txt"), "different").unwrap();
    let mut graph = Graph::new();
    f.configure(&mut graph, Some(&project));
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(
        assert_blocked(&mut graph, id, "conflict"),
        BlockReason::Conflict
    );
    let before = std::fs::read(&f.store_path).unwrap();
    assert!(f.store().grant(&copy, &identity(&copy)).is_err());
    assert_eq!(
        std::fs::read(&f.store_path).unwrap(),
        before,
        "record unchanged"
    );
}

#[test]
fn nested_dependencies_need_their_own_authorization() {
    let f = fx("perm_nested");
    let leaf_pkg = f.unrecorded("Leaf", 0.75, "1.0.0");
    let chain_src = composite(
        &f.tmp.0.join("src").join("Chain"),
        "Chain",
        &[child(&leaf_pkg)],
    );
    let chain = publish(&f.library, &chain_src, "Chain", None, "1.0.0").unwrap();

    // Allowing the parent does not allow what it bundles.
    f.grant(&chain.dir);
    let mut graph = f.graph();
    let (id, sink) = wired(&mut graph, &chain);
    let BlockReason::Dependency(inner) = assert_blocked(&mut graph, id, "parent of a blocked dep")
    else {
        panic!("blocked by its dependency");
    };
    assert_eq!(inner.reason, BlockReason::NeedsPermission);
    assert_eq!(inner.package.label, "local.leaf@1.0.0");
    assert!(
        inner.package.dir.starts_with(&chain.dir),
        "the bundled copy is what would run"
    );
    assert!(graph.custom_blocked(id).unwrap().needs_review());
    assert!(
        graph
            .connections
            .iter()
            .any(|c| c.from_node == id && c.to_node == sink)
    );

    // The bundled copy has the same bytes as the Library version, so
    // allowing that exact content (anywhere) allows the dependency.
    assert_eq!(identity(&inner.package.dir), identity(&leaf_pkg.dir));
    f.grant(&leaf_pkg.dir);
    assert_eq!(
        output(&mut graph, id),
        Some(0.75),
        "activated without a restart"
    );

    // Denying the dependency blocks the parent again, even while running.
    f.deny(&leaf_pkg.dir);
    let blocked = assert_blocked(&mut graph, id, "dependency denied");
    assert!(
        matches!(blocked, BlockReason::Dependency(inner) if inner.reason == BlockReason::Denied)
    );
}

#[test]
fn granting_activates_the_blocked_node_in_place() {
    let f = fx("perm_activate");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");
    let mut graph = f.graph();
    let (id, sink) = wired(&mut graph, &package);
    assert_blocked(&mut graph, id, "before");
    for _ in 0..5 {
        assert_eq!(output(&mut graph, id), None, "stays blocked");
    }
    f.grant(&package.dir);
    assert_eq!(output(&mut graph, id), Some(0.25), "same graph, next frame");
    assert!(graph.custom_blocked(id).is_none());
    assert!(
        graph
            .connections
            .iter()
            .any(|c| c.from_node == id && c.to_node == sink)
    );
}

#[test]
fn revocation_reaches_running_cloned_and_undo_restored_runtimes() {
    let f = fx("perm_revoke");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");
    f.grant(&package.dir);
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(output(&mut graph, id), Some(0.25));
    assert!(graph.has_custom_runtime(id));

    // Copies that carry the running runtime: a clone, an undo snapshot.
    let mut clone = graph.clone();
    let mut history = crate::app::undo::UndoHistory::new(10);
    history.push(crate::app::undo::UndoSnapshot::new(
        graph.clone(),
        Default::default(),
    ));
    graph.add_node(NodeType::FileMenu, [0.0, 300.0]);
    let mut restored = history
        .undo(crate::app::undo::UndoSnapshot::new(
            graph.clone(),
            Default::default(),
        ))
        .unwrap()
        .graph;
    assert!(
        restored.has_custom_runtime(id),
        "undo restores the cached runtime"
    );
    let mut redone = history
        .redo(crate::app::undo::UndoSnapshot::new(
            restored.clone(),
            Default::default(),
        ))
        .unwrap()
        .graph;

    f.deny(&package.dir);
    for (what, g) in [
        ("live", &mut graph),
        ("clone", &mut clone),
        ("undo", &mut restored),
        ("redo", &mut redone),
    ] {
        assert_eq!(assert_blocked(g, id, what), BlockReason::Denied, "{what}");
    }
}

#[test]
fn a_running_runtime_survives_unrelated_trust_changes() {
    let f = fx("perm_unrelated");
    let a = f.unrecorded("Gain", 0.25, "1.0.0");
    let b = f.unrecorded("Mix", 0.5, "1.0.0");
    f.grant(&a.dir);
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &a);
    assert_eq!(output(&mut graph, id), Some(0.25));
    let loads_before = loads();
    f.grant(&b.dir); // a new trust generation
    assert_eq!(output(&mut graph, id), Some(0.25));
    assert_eq!(loads(), loads_before, "re-authorized, not rebuilt");
}

#[test]
fn blocked_packages_are_not_rehashed_every_frame() {
    let f = fx("perm_cache");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &package);
    let before = FULL_CHECKS.with(|c| c.get());
    // The trust generation is process-wide: tests running in parallel may
    // change it, and each change legitimately decides again once.
    let generation_before = crate::library::trust::generation();
    for _ in 0..30 {
        graph.expire_package_rechecks();
        assert_eq!(output(&mut graph, id), None);
    }
    let checks = FULL_CHECKS.with(|c| c.get()) - before;
    let changes = (crate::library::trust::generation() - generation_before) as usize;
    assert!(
        checks <= 1 + changes,
        "hashed {checks} times in 30 frames ({changes} trust changes)"
    );
    // A change to the package folder is noticed (and checked again).
    std::fs::write(package.dir.join("readme.txt"), "x").unwrap();
    graph.expire_package_rechecks();
    assert_eq!(output(&mut graph, id), None);
    assert!(FULL_CHECKS.with(|c| c.get()) - before > checks);
}

#[test]
fn changed_bytes_are_never_run_under_an_old_authorization() {
    let f = fx("perm_toctou");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");
    f.grant(&package.dir);
    let mut graph = f.graph();
    let (id, _) = wired(&mut graph, &package);
    assert_eq!(output(&mut graph, id), Some(0.25));
    // The entry is replaced by other (consistent) bytes after authorization.
    let other = f.source("Gain", 0.9);
    std::fs::copy(&other, &package.entry_path).unwrap();
    let manifest_path = package.dir.join(crate::library::package::MANIFEST_FILE);
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    manifest["sha256"] = crate::library::package::sha256_file(&package.entry_path)
        .unwrap()
        .into();
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    graph.expire_package_rechecks();
    assert_eq!(
        assert_blocked(&mut graph, id, "replaced bytes"),
        BlockReason::Conflict
    );
}

// ── Entry paths ──────────────────────────────────────────────────────────

#[test]
fn every_entry_path_is_authorized() {
    let f = fx("perm_paths");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");
    let allowed = f.local("Mix", 0.5);

    // Palette insertion: the instance is built from the static interface.
    let before = loads();
    let node = instance(&package);
    assert_eq!(loads(), before, "inserting constructs nothing");
    let mut graph = f.graph();
    let id = graph.add_node(node.clone(), [0.0, 0.0]);
    assert_blocked(&mut graph, id, "palette");

    // Copy and paste, duplicate: the same NodeType in memory.
    let pasted = graph.add_node(node.clone(), [40.0, 0.0]);
    let duplicate = graph.add_node(graph.nodes[&id].node_type.clone(), [80.0, 0.0]);
    assert_blocked(&mut graph, pasted, "paste");
    assert_blocked(&mut graph, duplicate, "duplicate");

    // Project opening and session restoration (both deserialize a graph).
    let project = f.tmp.0.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("project.json"),
        serde_json::to_string(&graph).unwrap(),
    )
    .unwrap();
    let mut opened = crate::app::load_project_graph(&project).unwrap();
    assert_blocked(&mut opened, id, "opened before configuration");
    f.configure(&mut opened, Some(&project));
    assert_blocked(&mut opened, id, "project open");
    let mut session: Graph = serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
    assert_blocked(&mut session, id, "session restore, unconfigured");
    f.configure(&mut session, None);
    assert_blocked(&mut session, id, "session restore");

    // Project-bundled copy (identical bytes, never recorded).
    let copy = project.join("packages").join("local.gain").join("1.0.0");
    std::fs::create_dir_all(&copy).unwrap();
    for rel in files_under(&package.dir) {
        std::fs::copy(package.dir.join(&rel), copy.join(&rel)).unwrap();
    }
    let mut bundled = crate::app::load_project_graph(&project).unwrap();
    f.configure(&mut bundled, Some(&project));
    assert_blocked(&mut bundled, id, "project bundle");
    assert_eq!(
        bundled.custom_blocked(id).unwrap().package.dir,
        copy,
        "the project copy is what was checked"
    );

    // MCP: CreateNode cannot create custom-node instances at all.
    let values = HashMap::new();
    let created = crate::mcp::execute_command(
        crate::mcp::McpCommand::CreateNode {
            type_name: "Custom Node".into(),
            position: [0.0, 0.0],
            properties: None,
        },
        &mut graph,
        &values,
    );
    assert!(matches!(created, crate::mcp::McpResult::Error { .. }));

    // MCP UpdateNode: re-pointing an allowed instance at another package.
    let mut graph = f.graph();
    let running = graph.add_node(instance(&allowed), [0.0, 0.0]);
    assert_eq!(output(&mut graph, running), Some(0.5));
    let NodeType::CustomNodeInstance {
        file,
        package: reference,
        ..
    } = instance(&package)
    else {
        unreachable!()
    };
    crate::mcp::execute_command(
        crate::mcp::McpCommand::UpdateNode {
            node_id: running,
            properties: serde_json::json!({"file": file, "package": reference}),
        },
        &mut graph,
        &values,
    );
    assert_blocked(&mut graph, running, "mcp update");

    // MCP LoadProject: the loaded graph replaces the live one.
    let mut graph = f.graph();
    crate::mcp::execute_command(
        crate::mcp::McpCommand::LoadProject {
            path: project.to_string_lossy().into_owned(),
        },
        &mut graph,
        &values,
    );
    assert!(graph.nodes.contains_key(&id));
    assert_blocked(&mut graph, id, "mcp load, before the editor configures it");
    f.configure(&mut graph, Some(&project));
    assert_blocked(&mut graph, id, "mcp load");

    // Runtime refresh and cache invalidation never skip authorization.
    graph.clear_custom_runtime_failures();
    assert_blocked(&mut graph, id, "refresh");
    graph.set_package_environment(None, Some(f.library.clone()));
    assert_blocked(&mut graph, id, "environment change");
    graph.invalidate_custom_runtimes_for(&package.entry_path);
    assert_blocked(&mut graph, id, "invalidate");
    graph.set_trust_policy(TrustPolicy::AllowAll);
    graph.set_trust_policy(f.policy());
    assert_blocked(&mut graph, id, "policy change");
}

#[test]
fn version_switching_authorizes_the_target_version() {
    let f = fx("perm_switch");
    let v1 = f.unrecorded("Gain", 0.25, "1.0.0");
    let v2 = f.unrecorded("Gain", 0.5, "1.0.1");
    f.grant(&v1.dir);
    let mut graph = f.graph();
    let (id, sink) = wired(&mut graph, &v1);
    assert_eq!(output(&mut graph, id), Some(0.25));

    let before = loads();
    let target = InstancePackageRef {
        id: "local.gain".into(),
        version: "1.0.1".into(),
        entry: v2.manifest.entry.clone(),
    };
    let plan = graph.plan_version_switch(id, &target).unwrap();
    assert_eq!(loads(), before, "planning constructs nothing");
    graph.apply_version_switch(&plan).unwrap();
    assert_eq!(
        assert_blocked(&mut graph, id, "switched"),
        BlockReason::NeedsPermission
    );
    assert!(
        graph
            .connections
            .iter()
            .any(|c| c.from_node == id && c.to_node == sink)
    );
    f.grant(&v2.dir);
    assert_eq!(output(&mut graph, id), Some(0.5));
}

#[test]
fn package_tabs_are_authorized_before_they_are_built() {
    let f = fx("perm_tabs");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");
    let mut documents = crate::document::DocumentManager::new();
    documents.set_trust_policy(f.policy());
    let before = loads();
    let error = documents
        .open_custom_node(package.entry_path.clone())
        .unwrap_err();
    assert!(error.starts_with("Needs permission"), "{error}");
    assert_eq!(loads(), before, "nothing constructed");
    let blocked = documents.take_blocked().unwrap();
    assert_eq!(blocked.reason, BlockReason::NeedsPermission);
    assert_eq!(documents.documents.len(), 1, "no tab opened");

    f.grant(&package.dir);
    let tab = documents
        .open_custom_node(package.entry_path.clone())
        .unwrap();
    let document = documents.documents.iter().find(|d| d.id == tab).unwrap();
    assert!(document.read_only);
    assert!(!document.graph.nodes.is_empty());

    // Plain custom-node files (the user's own, not packages) open as before.
    let own = f.source("Own", 0.1);
    assert!(documents.open_custom_node(own).is_ok());
}

// ── P4.3.2E1: loose custom-node files ───────────────────────────────────

/// An instance of a loose (non-package) custom-node file.
fn loose_instance(path: &std::path::Path) -> NodeType {
    let interface = read_custom_node_interface_static(path).unwrap();
    NodeType::CustomNodeInstance {
        file: path.to_string_lossy().into_owned(),
        inputs: interface.inputs,
        outputs: interface.outputs,
        package: None,
    }
}

fn loose_digest(path: &std::path::Path) -> String {
    crate::library::trust::content::custom_node_digest(&std::fs::read_to_string(path).unwrap())
        .unwrap()
}

#[test]
fn loose_custom_nodes_run_only_when_authored_or_allowed() {
    use crate::library::trust::content::{ContentDecision, ContentKind};
    let f = fx("perm_loose");
    let own = f.source("Own", 0.3);
    let ledger = f.store().content_ledger();

    // Received from elsewhere (no record): blocked, nothing constructed.
    let mut graph = f.graph();
    let id = graph.add_node(loose_instance(&own), [0.0, 0.0]);
    assert_eq!(
        assert_blocked(&mut graph, id, "unrecorded file"),
        BlockReason::NeedsPermission
    );
    let blocked = graph.custom_blocked(id).unwrap();
    assert!(blocked.needs_review());
    assert_eq!(
        blocked.package.loose_digest.as_deref(),
        Some(loose_digest(&own).as_str())
    );

    // Written by Patchwork from authored content: runs, from those bytes.
    ledger
        .record(
            ContentKind::CustomNode,
            &loose_digest(&own),
            ContentDecision::Authored,
            false,
        )
        .unwrap();
    assert_eq!(
        output(&mut graph, id),
        Some(0.3),
        "activated on the trust change"
    );

    // A copy elsewhere is the same content: it runs too.
    let copy = f.tmp.0.join("elsewhere").join("Copy.pwnode");
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&own, &copy).unwrap();
    let mut graph = f.graph();
    let copied = graph.add_node(loose_instance(&copy), [0.0, 0.0]);
    assert_eq!(output(&mut graph, copied), Some(0.3));

    // Changed content is not covered, wherever it is.
    let changed = f.source("Own", 0.9); // rewrites the same file
    assert_eq!(changed, own);
    let mut graph = f.graph();
    let id = graph.add_node(loose_instance(&own), [0.0, 0.0]);
    assert_eq!(
        assert_blocked(&mut graph, id, "changed file"),
        BlockReason::NeedsPermission
    );

    // Deny sticks; an explicit allow of the reviewed content reverses it.
    let digest = loose_digest(&own);
    ledger
        .decide_file(ContentKind::CustomNode, &own, &digest, false)
        .unwrap();
    let mut graph = f.graph();
    let id = graph.add_node(loose_instance(&own), [0.0, 0.0]);
    assert_eq!(
        assert_blocked(&mut graph, id, "denied file"),
        BlockReason::Denied
    );
    assert!(!graph.custom_blocked(id).unwrap().needs_review());
    ledger
        .record(
            ContentKind::CustomNode,
            &digest,
            ContentDecision::Authored,
            false,
        )
        .unwrap();
    assert_eq!(
        assert_blocked(&mut graph, id, "automatic records never undo a denial"),
        BlockReason::Denied
    );
    ledger
        .decide_file(ContentKind::CustomNode, &own, &digest, true)
        .unwrap();
    assert_eq!(output(&mut graph, id), Some(0.9));
}

#[test]
fn project_decisions_never_override_package_or_file_checks() {
    use crate::library::trust::content::ContentState;
    let f = fx("perm_project_scope");
    let package = f.unrecorded("Gain", 0.25, "1.0.0");
    let own = f.source("Own", 0.3);
    let mut graph = f.graph();
    graph.content = ContentState::Allowed(crate::capability::CapSet::ALL); // the user allowed the project
    let packaged = graph.add_node(instance(&package), [0.0, 0.0]);
    let loose = graph.add_node(loose_instance(&own), [0.0, 100.0]);
    assert_eq!(
        assert_blocked(&mut graph, packaged, "package"),
        BlockReason::NeedsPermission
    );
    assert_eq!(
        assert_blocked(&mut graph, loose, "loose file"),
        BlockReason::NeedsPermission
    );
    graph.content = ContentState::Authored;
    assert_eq!(
        assert_blocked(&mut graph, packaged, "authored project"),
        BlockReason::NeedsPermission
    );
}
