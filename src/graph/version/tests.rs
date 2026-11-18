//! P4.2 Stage 5 tests: version switching at the graph-model level.

use super::*;
use crate::document::CustomNodeFile;
use crate::graph::PortValue;
use crate::library::bundle::tests::{files_under, leaf, publish, stored};
use crate::library::package::VerifiedPackage;
use crate::library::package::tests::TempDir;
use std::path::{Path, PathBuf};

fn port(name: &str, kind: &str) -> CustomNodePort {
    CustomNodePort {
        name: name.into(),
        kind: kind.into(),
    }
}

/// A custom node with just an interface (no evaluation needed).
fn interface_node(
    dir: &Path,
    name: &str,
    inputs: &[(&str, &str)],
    outputs: &[(&str, &str)],
) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{name}.pwnode"));
    CustomNodeFile {
        version: 1,
        name: name.into(),
        graph: Graph::new(),
        inputs: inputs.iter().map(|(n, k)| port(n, k)).collect(),
        outputs: outputs.iter().map(|(n, k)| port(n, k)).collect(),
    }
    .save(&path)
    .unwrap();
    path
}

struct Versions {
    tmp: TempDir,
    library: PathBuf,
    v1: VerifiedPackage,
    v2: VerifiedPackage,
}

/// v1 and v2 of one package from two source files.
fn versions(
    tag: &str,
    v1_src: impl Fn(&Path) -> PathBuf,
    v2_src: impl Fn(&Path) -> PathBuf,
) -> Versions {
    let tmp = TempDir::new(tag);
    let library = tmp.0.join("library");
    let v1 = publish(&library, &v1_src(&tmp.0.join("v1")), "Gain", None, "1.0.0").unwrap();
    let v2 = publish(
        &library,
        &v2_src(&tmp.0.join("v2")),
        "Gain",
        Some(&v1.manifest.id),
        "2.0.0",
    )
    .unwrap();
    Versions {
        tmp,
        library,
        v1,
        v2,
    }
}

fn slider() -> NodeType {
    NodeType::Slider {
        value: 0.5,
        min: 0.0,
        max: 1.0,
        step: 0.01,
        slider_color: [0, 0, 0],
        label: String::new(),
    }
}

/// An instance of `v` with its interface, in a graph whose package
/// locations are `library` (and optionally a project).
fn add_instance(graph: &mut Graph, v: &VerifiedPackage) -> NodeId {
    let interface = CustomNodeFile::load(&v.entry_path).unwrap();
    graph.add_node(
        NodeType::CustomNodeInstance {
            file: v.entry_path.to_string_lossy().into_owned(),
            inputs: interface.inputs,
            outputs: interface.outputs,
            package: Some(stored(v)),
        },
        [0.0, 0.0],
    )
}

fn graph_with(library: &Path) -> Graph {
    let mut graph = Graph::new();
    graph.set_package_environment(None, Some(library.to_path_buf()));
    graph
}

fn value(graph: &mut Graph, id: NodeId) -> Option<f32> {
    match graph.evaluate(0.0).get(&(id, 0)) {
        Some(PortValue::Float(v)) => Some(*v),
        _ => None,
    }
}

fn instance(
    graph: &Graph,
    id: NodeId,
) -> (String, Option<InstancePackageRef>, Vec<String>, Vec<String>) {
    match &graph.nodes[&id].node_type {
        NodeType::CustomNodeInstance {
            file,
            package,
            inputs,
            outputs,
        } => (
            file.clone(),
            package.clone(),
            inputs.iter().map(|p| p.name.clone()).collect(),
            outputs.iter().map(|p| p.name.clone()).collect(),
        ),
        _ => unreachable!(),
    }
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    files_under(dir)
        .into_iter()
        .map(|rel| {
            let bytes = std::fs::read(dir.join(&rel)).unwrap();
            (rel, bytes)
        })
        .collect()
}

#[test]
fn compatible_switch_keeps_connections_and_evaluates_the_selected_version() {
    let vs = versions(
        "s5_compatible",
        |d| leaf(d, "Gain", 0.25),
        |d| leaf(d, "Gain", 0.75),
    );
    let mut graph = graph_with(&vs.library);
    let id = add_instance(&mut graph, &vs.v1);
    let sink = graph.add_node(slider(), [200.0, 0.0]);
    graph.add_connection(id, 0, sink, 0);
    assert_eq!(value(&mut graph, id), Some(0.25));
    let published = snapshot(&vs.library);

    let plan = graph.plan_version_switch(id, &stored(&vs.v2)).unwrap();
    assert!(!plan.needs_confirmation());
    assert_eq!(plan.removed_connections, 0);
    let report = graph.apply_version_switch(&plan).unwrap();
    assert!(report.removed.is_empty());
    assert!(!graph.has_custom_runtime(id), "runtime invalidated");

    let (file, package, _, _) = instance(&graph, id);
    assert_eq!(PathBuf::from(file), vs.v2.entry_path);
    assert_eq!(
        package,
        Some(stored(&vs.v2)),
        "id kept, version and entry switched"
    );
    assert_eq!(graph.connections.len(), 1, "connection kept");
    assert_eq!(value(&mut graph, id), Some(0.75), "evaluates v2");
    assert_eq!(
        snapshot(&vs.library),
        published,
        "published files untouched"
    );
}

#[test]
fn port_order_changes_are_remapped_without_confirmation() {
    let vs = versions(
        "s5_order",
        |d| interface_node(d, "Gain", &[], &[("a", "Number"), ("b", "Number")]),
        |d| interface_node(d, "Gain", &[], &[("b", "Number"), ("a", "Number")]),
    );
    let mut graph = graph_with(&vs.library);
    let id = add_instance(&mut graph, &vs.v1);
    let sink_a = graph.add_node(slider(), [200.0, 0.0]);
    let sink_b = graph.add_node(slider(), [200.0, 100.0]);
    graph.add_connection(id, 0, sink_a, 0);
    graph.add_connection(id, 1, sink_b, 0);

    let plan = graph.plan_version_switch(id, &stored(&vs.v2)).unwrap();
    assert!(!plan.needs_confirmation(), "{:?}", plan.affected);
    graph.apply_version_switch(&plan).unwrap();
    let port_to = |sink| {
        graph
            .connections
            .iter()
            .find(|c| c.from_node == id && c.to_node == sink)
            .map(|c| c.from_port)
    };
    assert_eq!(port_to(sink_a), Some(1), "'a' follows its name");
    assert_eq!(port_to(sink_b), Some(0));
}

#[test]
fn renamed_removed_and_retyped_ports_are_reported_before_switching() {
    let vs = versions(
        "s5_affected",
        |d| {
            interface_node(
                d,
                "Gain",
                &[("x", "Number"), ("y", "Number")],
                &[("o", "Number"), ("gone", "Number")],
            )
        },
        |d| {
            interface_node(
                d,
                "Gain",
                &[("x", "Text"), ("y2", "Number")],
                &[("o", "Number")],
            )
        },
    );
    let mut graph = graph_with(&vs.library);
    let id = add_instance(&mut graph, &vs.v1);
    let source = graph.add_node(slider(), [-200.0, 0.0]);
    let sink = graph.add_node(slider(), [200.0, 0.0]);
    let sink2 = graph.add_node(slider(), [200.0, 100.0]);
    graph.add_connection(source, 0, id, 0); // x (type changes)
    graph.add_connection(source, 0, id, 1); // y (renamed to y2)
    graph.add_connection(id, 0, sink, 0); // o (kept)
    graph.add_connection(id, 1, sink2, 0); // gone (removed)
    let before = serde_json::to_string(&graph).unwrap();

    let plan = graph.plan_version_switch(id, &stored(&vs.v2)).unwrap();
    assert!(plan.needs_confirmation());
    assert_eq!(plan.removed_connections, 3);
    let summary: Vec<(String, bool, PortChange, usize)> = plan
        .affected
        .iter()
        .map(|p| (p.name.clone(), p.is_input, p.change.clone(), p.connections))
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "x".into(),
                true,
                PortChange::TypeChanged { to: "Text".into() },
                1
            ),
            ("y".into(), true, PortChange::Removed, 1),
            ("gone".into(), false, PortChange::Removed, 1),
        ]
    );
    // Planning (the preview) changes nothing; Cancel = not applying.
    assert_eq!(serde_json::to_string(&graph).unwrap(), before);

    let report = graph.apply_version_switch(&plan).unwrap();
    assert_eq!(report.removed.len(), 3, "exactly what was shown");
    assert_eq!(graph.connections.len(), 1);
    assert_eq!(graph.connections[0].from_node, id);
    assert_eq!(graph.connections[0].to_node, sink);
    let (_, _, inputs, outputs) = instance(&graph, id);
    assert_eq!(inputs, vec!["x", "y2"]);
    assert_eq!(outputs, vec!["o"]);
}

#[test]
fn removed_ports_without_connections_still_need_confirmation() {
    let vs = versions(
        "s5_unconnected",
        |d| interface_node(d, "Gain", &[("in", "Number")], &[("out", "Number")]),
        |d| interface_node(d, "Gain", &[], &[("out", "Number")]),
    );
    let mut graph = graph_with(&vs.library);
    let id = add_instance(&mut graph, &vs.v1);
    let plan = graph.plan_version_switch(id, &stored(&vs.v2)).unwrap();
    assert!(plan.needs_confirmation());
    assert_eq!(plan.removed_connections, 0);
}

#[test]
fn switching_v1_v2_v1_round_trips() {
    let vs = versions(
        "s5_roundtrip",
        |d| leaf(d, "Gain", 0.25),
        |d| leaf(d, "Gain", 0.75),
    );
    let mut graph = graph_with(&vs.library);
    let id = add_instance(&mut graph, &vs.v1);
    let original = instance(&graph, id);
    assert_eq!(value(&mut graph, id), Some(0.25));
    let plan = graph.plan_version_switch(id, &stored(&vs.v2)).unwrap();
    graph.apply_version_switch(&plan).unwrap();
    assert_eq!(value(&mut graph, id), Some(0.75));
    let plan = graph.plan_version_switch(id, &stored(&vs.v1)).unwrap();
    graph.apply_version_switch(&plan).unwrap();
    assert_eq!(value(&mut graph, id), Some(0.25));
    assert_eq!(instance(&graph, id), original);
}

#[test]
fn only_the_chosen_instance_switches() {
    let vs = versions(
        "s5_multi",
        |d| leaf(d, "Gain", 0.25),
        |d| leaf(d, "Gain", 0.75),
    );
    let mut graph = graph_with(&vs.library);
    let a = add_instance(&mut graph, &vs.v1);
    let b = add_instance(&mut graph, &vs.v1);
    assert_eq!(
        (value(&mut graph, a), value(&mut graph, b)),
        (Some(0.25), Some(0.25))
    );
    let plan = graph.plan_version_switch(a, &stored(&vs.v2)).unwrap();
    graph.apply_version_switch(&plan).unwrap();
    assert_eq!(
        instance(&graph, b).1,
        Some(stored(&vs.v1)),
        "other instance unchanged"
    );
    let values = graph.evaluate(0.0);
    let get = |id| match values.get(&(id, 0)) {
        Some(PortValue::Float(v)) => Some(*v),
        _ => None,
    };
    assert_eq!((get(a), get(b)), (Some(0.75), Some(0.25)));
}

#[test]
fn damaged_missing_incompatible_or_foreign_targets_are_refused() {
    let vs = versions(
        "s5_refuse",
        |d| leaf(d, "Gain", 0.25),
        |d| leaf(d, "Gain", 0.75),
    );
    let mut graph = graph_with(&vs.library);
    let id = add_instance(&mut graph, &vs.v1);
    assert_eq!(value(&mut graph, id), Some(0.25));
    let before = serde_json::to_string(&graph).unwrap();

    // Damaged v2: listed as unavailable with a reason, and refused.
    std::fs::write(&vs.v2.entry_path, "{ damaged").unwrap();
    let choices = graph.available_versions(id);
    let v2 = choices.iter().find(|c| c.version == "2.0.0").unwrap();
    assert!(!v2.selectable());
    assert!(
        v2.status.as_ref().unwrap_err().contains("damaged"),
        "{:?}",
        v2.status
    );
    let error = graph.plan_version_switch(id, &stored(&vs.v2)).unwrap_err();
    assert!(error.contains("damaged"), "{error}");

    // Not installed.
    let mut missing = stored(&vs.v1);
    missing.version = "9.9.9".into();
    assert!(
        graph
            .plan_version_switch(id, &missing)
            .unwrap_err()
            .contains("not installed")
    );

    // Incompatible.
    let v3 = publish(
        &vs.library,
        &leaf(&vs.tmp.0.join("v3"), "Gain", 0.9),
        "Gain",
        Some(&vs.v1.manifest.id),
        "3.0.0",
    )
    .unwrap();
    let manifest = v3.dir.join(crate::library::package::MANIFEST_FILE);
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    json["format"] = serde_json::json!(99);
    std::fs::write(&manifest, json.to_string()).unwrap();
    assert!(
        graph
            .plan_version_switch(id, &stored(&v3))
            .unwrap_err()
            .contains("not compatible")
    );

    // Another package, an unsafe reference, and the current version.
    let mut other = stored(&vs.v2);
    other.id = "local.other".into();
    assert!(graph.plan_version_switch(id, &other).is_err());
    let mut unsafe_ref = stored(&vs.v2);
    unsafe_ref.entry = "../Gain.pwnode".into();
    assert!(graph.plan_version_switch(id, &unsafe_ref).is_err());
    assert!(
        graph
            .plan_version_switch(id, &stored(&vs.v1))
            .unwrap_err()
            .contains("already")
    );

    // Nothing changed, and the v1 runtime is still running.
    assert_eq!(serde_json::to_string(&graph).unwrap(), before);
    assert_eq!(value(&mut graph, id), Some(0.25));
}

#[test]
fn available_versions_list_current_and_installed_versions_in_order() {
    let vs = versions(
        "s5_list",
        |d| leaf(d, "Gain", 0.25),
        |d| leaf(d, "Gain", 0.75),
    );
    // A version only present in the project.
    let project = vs.tmp.0.join("project");
    let other_library = vs.tmp.0.join("other");
    let v15 = publish(
        &other_library,
        &leaf(&vs.tmp.0.join("v15"), "Gain", 0.5),
        "Gain",
        None,
        "1.5.0",
    )
    .unwrap();
    assert_eq!(v15.manifest.id, vs.v1.manifest.id);
    let target = project
        .join(crate::library::package::PACKAGES_DIR)
        .join(&v15.manifest.id)
        .join("1.5.0");
    for rel in files_under(&v15.dir) {
        let to = target.join(&rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(v15.dir.join(&rel), to).unwrap();
    }
    let mut graph = Graph::new();
    graph.set_package_environment(Some(project.clone()), Some(vs.library.clone()));
    let id = add_instance(&mut graph, &vs.v2);
    let choices = graph.available_versions(id);
    let listed: Vec<(&str, bool, bool)> = choices
        .iter()
        .map(|c| (c.version.as_str(), c.current, c.selectable()))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("1.0.0", false, true),
            ("1.5.0", false, true),
            ("2.0.0", true, false)
        ],
        "nothing is selected automatically; current is not selectable"
    );
    // The project-only version can be selected and evaluates.
    let plan = graph
        .plan_version_switch(id, choices[1].reference.as_ref().unwrap())
        .unwrap();
    graph.apply_version_switch(&plan).unwrap();
    assert_eq!(value(&mut graph, id), Some(0.5));
}

#[test]
fn unpackaged_instances_have_no_versions() {
    let tmp = TempDir::new("s5_plain");
    let file = leaf(&tmp.0.join("src"), "Plain", 0.1);
    let mut graph = Graph::new();
    let id = graph.add_node(
        NodeType::CustomNodeInstance {
            file: file.to_string_lossy().into_owned(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            package: None,
        },
        [0.0, 0.0],
    );
    assert!(graph.available_versions(id).is_empty());
    let target = InstancePackageRef {
        id: "local.plain".into(),
        version: "1.0.0".into(),
        entry: "Plain.pwnode".into(),
    };
    assert!(graph.plan_version_switch(id, &target).is_err());
}
