//! P4.3.2A: unknown dynamic nodes are preserved, inert and recoverable.

use super::*;
use crate::document::CustomNodeFile;
use crate::graph::{Graph, NodeId, NodeType};
use crate::library::package::tests::TempDir;
use crate::node_trait::CONSTRUCTED;
use serde_json::json;

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

const ID: NodeId = 50;

/// The node's `inner` object as written in a file.
fn inner(tag: &str) -> Value {
    json!({
        "type_tag": tag,
        "state": {"gain": 0.7, "modes": ["a", "b"], "nested": {"x": 1}},
        "extra": {"kept": true}
    })
}

/// slider A → unknown.in3, unknown.out2 → slider B.
fn graph_json(inner: Value) -> (Value, NodeId, NodeId) {
    let mut graph = Graph::new();
    let a = graph.add_node(slider(), [0.0, 0.0]);
    let b = graph.add_node(slider(), [400.0, 0.0]);
    let mut value = serde_json::to_value(&graph).unwrap();
    value["nodes"][ID.to_string()] = json!({
        "id": ID,
        "node_type": {"Dynamic": {"inner": inner}},
        "pos": [10.0, 20.0]
    });
    value["connections"] = json!([
        {"from_node": a, "from_port": 0, "to_node": ID, "to_port": 2, "label": "in"},
        {"from_node": ID, "from_port": 1, "to_node": b, "to_port": 0, "label": ""}
    ]);
    (value, a, b)
}

fn load(value: Value) -> Graph {
    let mut graph: Graph = serde_json::from_value(value).unwrap();
    graph.fix_next_id();
    graph
}

fn unknown(graph: &Graph, id: NodeId) -> Option<&UnknownNode> {
    match &graph.nodes.get(&id)?.node_type {
        NodeType::Dynamic { inner } => inner.node.as_unknown(),
        _ => None,
    }
}

/// The serialized `inner` object of node `id`.
fn saved_inner(graph: &Graph, id: NodeId) -> Value {
    let value = serde_json::to_value(graph).unwrap();
    value["nodes"][id.to_string()]["node_type"]["Dynamic"]["inner"].clone()
}

#[test]
fn unknown_types_keep_their_data_and_connections_and_construct_nothing() {
    let tag = "p432a_future_node";
    let (value, a, b) = graph_json(inner(tag));
    let before = CONSTRUCTED.with(|c| c.get());
    let graph = load(value);
    let node = unknown(&graph, ID).expect("kept as an unknown node, not replaced");
    assert_eq!(
        CONSTRUCTED.with(|c| c.get()) - before,
        0,
        "no factory ran for the unknown node"
    );
    assert_eq!(node.type_tag(), tag);
    assert_eq!(node.raw(), &inner(tag));
    assert_eq!(node.title(), format!("Missing node: {tag}"));
    assert!(node.missing_message().contains(tag));
    // Ports exist for every attached wire; the wires are untouched.
    assert!(node.inputs().len() >= 3 && node.outputs().len() >= 2);
    assert_eq!(graph.connections.len(), 2);
    assert!(
        graph
            .connections
            .iter()
            .any(|c| c.from_node == a && c.to_node == ID && c.to_port == 2 && c.label == "in")
    );
    assert!(
        graph
            .connections
            .iter()
            .any(|c| c.from_node == ID && c.from_port == 1 && c.to_node == b)
    );
    assert_eq!(graph.nodes[&ID].pos, [10.0, 20.0]);
}

#[test]
fn unknown_nodes_never_execute() {
    let (value, _, _) = graph_json(inner("p432a_never_runs"));
    let mut graph = load(value);
    let mut placeholder = unknown(&graph, ID).unwrap().clone();
    assert!(
        placeholder
            .evaluate(&[crate::graph::PortValue::Float(1.0)])
            .is_empty()
    );
    let values = graph.evaluate(0.0);
    assert!(
        values.keys().all(|(node, _)| *node != ID),
        "an unknown node produces no values"
    );
    // Applying state does nothing either.
    placeholder.load_state(&json!({"gain": 99}));
    assert_eq!(placeholder.raw(), &inner("p432a_never_runs"));
}

#[test]
fn unknown_nodes_survive_clone_undo_save_and_reopen() {
    let tag = "p432a_round_trip";
    let (value, _, _) = graph_json(inner(tag));
    let graph = load(value);

    // Clones (undo snapshots, tab copies) keep the placeholder.
    let before = CONSTRUCTED.with(|c| c.get());
    let copy = graph.clone();
    assert_eq!(CONSTRUCTED.with(|c| c.get()) - before, 0);
    assert_eq!(unknown(&copy, ID).unwrap().raw(), &inner(tag));
    assert_eq!(saved_inner(&copy, ID), inner(tag), "written back verbatim");

    // Save as a custom node file and reopen.
    let tmp = TempDir::new("unknown_round_trip");
    let path = tmp.0.join("Future.pwnode");
    CustomNodeFile {
        version: 1,
        name: "Future".into(),
        graph: graph.clone(),
        inputs: Vec::new(),
        outputs: Vec::new(),
    }
    .save(&path)
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let on_disk: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        on_disk["graph"]["nodes"][ID.to_string()]["node_type"]["Dynamic"]["inner"],
        inner(tag)
    );
    let reopened = CustomNodeFile::load(&path).unwrap().graph;
    assert_eq!(unknown(&reopened, ID).unwrap().raw(), &inner(tag));
    assert_eq!(
        serde_json::to_value(&reopened.connections).unwrap(),
        serde_json::to_value(&graph.connections).unwrap()
    );
    // Saving the reopened graph again changes nothing.
    assert_eq!(saved_inner(&reopened, ID), inner(tag));
}

/// A node type that becomes available later (registered by the test).
struct Recovered {
    tag: &'static str,
    state: Value,
}

impl NodeBehavior for Recovered {
    fn title(&self) -> &str {
        "Recovered"
    }
    fn inputs(&self) -> Vec<PortDef> {
        Vec::new()
    }
    fn outputs(&self) -> Vec<PortDef> {
        Vec::new()
    }
    fn color_hint(&self) -> [u8; 3] {
        [0, 0, 0]
    }
    fn type_tag(&self) -> &str {
        self.tag
    }
    fn save_state(&self) -> Value {
        self.state.clone()
    }
    fn load_state(&mut self, state: &Value) {
        self.state = state.clone();
    }
}

#[test]
fn a_formerly_unknown_node_recovers_its_configuration() {
    let tag: &'static str = "p432a_recoverable";
    let (value, _, _) = graph_json(inner(tag));
    let graph = load(value);
    assert!(unknown(&graph, ID).is_some());
    let saved = serde_json::to_value(&graph).unwrap();

    // The implementation becomes available (a newer Patchwork).
    crate::node_trait::NODE_REGISTRY
        .lock()
        .unwrap()
        .register(tag, move |state| {
            let mut node = Recovered {
                tag,
                state: Value::Null,
            };
            node.load_state(state);
            Box::new(node)
        });
    let recovered = load(saved);
    let NodeType::Dynamic { inner: node } = &recovered.nodes[&ID].node_type else {
        panic!("not dynamic");
    };
    assert!(node.node.as_unknown().is_none(), "now a working node");
    assert_eq!(node.node.type_tag(), tag);
    assert_eq!(
        node.node.save_state(),
        inner(tag)["state"],
        "configuration recovered"
    );
    assert_eq!(recovered.connections.len(), 2, "connections kept");
}

#[test]
fn a_node_without_a_type_is_kept_not_turned_into_add() {
    let raw = json!({"state": {"x": 1}});
    let (value, _, _) = graph_json(raw.clone());
    let graph = load(value);
    let node = unknown(&graph, ID).expect("no silent AddNode");
    assert_eq!(node.title(), "Missing node (no type)");
    assert_eq!(saved_inner(&graph, ID), raw);
}

#[test]
fn known_dynamic_nodes_are_unchanged() {
    let (value, _, _) = graph_json(json!({"type_tag": "comment", "state": {"text": "hello"}}));
    let graph = load(value);
    let NodeType::Dynamic { inner: node } = &graph.nodes[&ID].node_type else {
        panic!("not dynamic");
    };
    assert!(node.node.as_unknown().is_none());
    assert_eq!(node.node.type_tag(), "comment");
    // Exactly what the real node makes of that state.
    let mut real = crate::nodes::comment_node::CommentNode::default();
    real.load_state(&json!({"text": "hello"}));
    assert_eq!(saved_inner(&graph, ID)["state"], real.save_state());
    // Known nodes are not padded with placeholder ports.
    assert_eq!(
        node.node.inputs().len(),
        crate::nodes::comment_node::CommentNode::default()
            .inputs()
            .len()
    );
}

#[test]
fn placeholder_ports_take_the_type_of_their_wires() {
    let (value, a, _) = graph_json(inner("p432b_typed"));
    let graph = load(value);
    let node = unknown(&graph, ID).unwrap();
    let source_kind = graph.nodes[&a].node_type.outputs()[0].kind;
    assert_eq!(node.inputs()[2].kind, source_kind, "learned from the wire");
    assert_eq!(
        node.inputs()[0].kind,
        PortKind::Generic,
        "unwired ports stay generic"
    );
}
