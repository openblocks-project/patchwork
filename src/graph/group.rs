//! P2: Custom Node Grouping.
//!
//! Turns a selection of nodes into a reusable custom node (.pwnode):
//! `Graph::extract_group` builds the internal graph and interface without
//! touching the source graph, and `Graph::apply_group` replaces the selection
//! with a single `CustomNodeInstance` once the file has been written.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use super::{Connection, Graph, NodeId, NodeType, PortKind};
use crate::document::CustomNodePort;
use crate::node_trait::NodeBehavior;

/// `Dynamic` node tags whose `evaluate` is a pure function of their inputs
/// and saved state, so they run identically inside a nested custom-node
/// runtime. Nodes that rely on app-side passes (GPU/image pipeline, audio,
/// hardware, networking, ML, UI-injected values) are deliberately absent.
const NESTED_SAFE_DYNAMIC_TAGS: &[&str] = &[
    "add",
    "multiply",
    "map_range",
    "math",
    "smoother",
    "gate",
    "pack",
    "point_2d",
    "string_format",
    "text_ops",
    "json_extract",
    "color",
    "comment",
    "select",
    "route",
];

/// `Dynamic` node tags that are editor system nodes (spawned by default).
const SYSTEM_DYNAMIC_TAGS: &[&str] = &["file_menu", "zoom_control", "monitor", "settings"];

/// How a node type can take part in a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupSupport {
    Supported,
    /// Editor system node (File, Zoom, Palette, Monitor, Settings).
    System,
    /// Custom Input / Custom Output interface node.
    Interface,
    /// Depends on app-side processing the nested evaluator does not perform.
    Unsupported,
}

/// Classify whether a node type can be evaluated inside a custom node.
pub fn group_support(node_type: &NodeType) -> GroupSupport {
    match node_type {
        NodeType::CustomInput { .. } | NodeType::CustomOutput { .. } => GroupSupport::Interface,
        NodeType::FileMenu | NodeType::ZoomControl { .. } | NodeType::Palette { .. } => {
            GroupSupport::System
        }
        NodeType::Slider { .. }
        | NodeType::Timer { .. }
        | NodeType::SampleHold { .. }
        | NodeType::CustomNodeInstance { .. } => GroupSupport::Supported,
        NodeType::Dynamic { inner } => {
            let tag = inner.node.type_tag();
            if SYSTEM_DYNAMIC_TAGS.contains(&tag) {
                GroupSupport::System
            } else if NESTED_SAFE_DYNAMIC_TAGS.contains(&tag) {
                GroupSupport::Supported
            } else {
                GroupSupport::Unsupported
            }
        }
        _ => GroupSupport::Unsupported,
    }
}

/// Why a selection cannot be grouped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupError {
    EmptySelection,
    MissingNodes(Vec<NodeId>),
    SystemNodes(Vec<String>),
    InterfaceNodes(Vec<String>),
    PinnedNodes(Vec<String>),
    UnsupportedNodes(Vec<String>),
    /// Grouping would create a loop through the new instance.
    BoundaryCycle(Vec<String>),
}

impl std::fmt::Display for GroupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GroupError::EmptySelection => write!(f, "Select one or more nodes to group."),
            GroupError::MissingNodes(ids) => {
                write!(f, "Some selected nodes no longer exist: {ids:?}.")
            }
            GroupError::SystemNodes(names) => write!(
                f,
                "System nodes cannot be grouped: {}.\nDeselect them and try again.",
                names.join(", ")
            ),
            GroupError::InterfaceNodes(names) => write!(
                f,
                "Custom Input/Output nodes define the current custom node's ports \
                 and cannot be grouped: {}.",
                names.join(", ")
            ),
            GroupError::PinnedNodes(names) => write!(
                f,
                "Pinned nodes cannot be grouped: {}.\nUnpin them first.",
                names.join(", ")
            ),
            GroupError::UnsupportedNodes(names) => write!(
                f,
                "These nodes depend on editor features (images/GPU, audio, hardware, \
                 network, ML or UI) that do not run inside a custom node, so the group \
                 would not work correctly:\n{}",
                names.join("\n")
            ),
            GroupError::BoundaryCycle(names) => write!(
                f,
                "Grouping would create a feedback loop through the new custom node: \
                 its output reaches its own input via {}.\nInclude those nodes in the \
                 selection or remove the loop.",
                names.join(" → ")
            ),
        }
    }
}

/// Result of extracting a selection into a custom-node definition.
#[derive(Debug, Clone)]
pub struct GroupExtraction {
    /// Selected node ids (sorted).
    pub selected: Vec<NodeId>,
    /// Internal graph: selected nodes (original ids), internal connections,
    /// and generated `CustomInput` / `CustomOutput` nodes.
    pub graph: Graph,
    /// External interface, ordered like the runtime maps ports.
    pub inputs: Vec<CustomNodePort>,
    pub outputs: Vec<CustomNodePort>,
    /// External source `(node, port, label)` feeding each instance input.
    pub external_inputs: Vec<(NodeId, usize, String)>,
    /// External consumers `(node, port, label)` of each instance output.
    pub external_outputs: Vec<Vec<(NodeId, usize, String)>>,
    /// Suggested canvas position for the new instance.
    pub position: [f32; 2],
}

/// Validate a custom-node name for use as a file name.
pub fn validate_group_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a name for the custom node.".to_string());
    }
    if name.chars().count() > 100 {
        return Err("The name must be 100 characters or fewer.".to_string());
    }
    if name == "." || name == ".." || name.ends_with('.') || name.ends_with(' ') {
        return Err("The name cannot end with a dot or space.".to_string());
    }
    if name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || name.chars().any(char::is_control)
    {
        return Err(
            "The name cannot contain / \\ : * ? \" < > | or control characters.".to_string(),
        );
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err(format!("'{name}' is a reserved file name."));
    }
    Ok(())
}

/// Make a port name unique among `used` by appending " 2", " 3", ...
fn unique_name(base: &str, used: &mut HashSet<String>) -> String {
    let mut name = base.to_string();
    let mut n = 2;
    while used.contains(&name) {
        name = format!("{base} {n}");
        n += 1;
    }
    used.insert(name.clone());
    name
}

/// Ensure a path ends in `.pwnode`.
pub fn with_pwnode_extension(path: PathBuf) -> PathBuf {
    if path.extension().and_then(|e| e.to_str()) == Some("pwnode") {
        path
    } else {
        let mut os = path.into_os_string();
        os.push(".pwnode");
        PathBuf::from(os)
    }
}

/// Resolve a (possibly not yet existing) file path to canonical form by
/// canonicalising its parent directory.
pub fn canonical_target_path(path: &Path) -> Result<PathBuf, String> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Ok(canonical);
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path.file_name().ok_or("Invalid custom node file name.")?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("Cannot resolve folder {}: {e}", parent.display()))?;
    Ok(parent.join(file_name))
}

/// Check that nested custom nodes in `group_graph` do not (directly or
/// indirectly) reference the file being written (`target`) or any of the
/// `forbidden` files (e.g. the custom node currently being edited).
pub fn check_group_dependencies(
    group_graph: &Graph,
    target: &Path,
    forbidden: &[PathBuf],
) -> Result<(), String> {
    let target = canonical_target_path(target)?;
    for node in group_graph.nodes.values() {
        let NodeType::CustomNodeInstance { file, .. } = &node.node_type else {
            continue;
        };
        let mut visited: HashSet<PathBuf> = HashSet::new();
        visited.insert(target.clone());
        for path in forbidden {
            if let Ok(canonical) = std::fs::canonicalize(path) {
                visited.insert(canonical);
            }
        }
        Graph::check_custom_node_dependencies(Path::new(file), &mut visited, 1)?;
    }
    Ok(())
}

fn node_label(graph: &Graph, id: NodeId) -> String {
    graph
        .nodes
        .get(&id)
        .map(|n| format!("{} #{}", n.node_type.title(), id))
        .unwrap_or_else(|| format!("#{id}"))
}

fn port_name(graph: &Graph, id: NodeId, port: usize, is_output: bool) -> String {
    graph
        .nodes
        .get(&id)
        .and_then(|n| {
            let ports = if is_output {
                n.node_type.outputs()
            } else {
                n.node_type.inputs()
            };
            ports.get(port).map(|p| p.name.to_string())
        })
        .unwrap_or_else(|| format!("{port}"))
}

impl Graph {
    /// Build a custom-node definition from `selection` without modifying
    /// this graph. `pinned` lists pinned nodes (stored in screen space),
    /// which cannot be grouped.
    pub fn extract_group(
        &self,
        selection: &HashSet<NodeId>,
        pinned: &HashSet<NodeId>,
    ) -> Result<GroupExtraction, GroupError> {
        if selection.is_empty() {
            return Err(GroupError::EmptySelection);
        }

        let mut selected: Vec<NodeId> = selection.iter().copied().collect();
        selected.sort();

        let missing: Vec<NodeId> = selected
            .iter()
            .copied()
            .filter(|id| !self.nodes.contains_key(id))
            .collect();
        if !missing.is_empty() {
            return Err(GroupError::MissingNodes(missing));
        }

        // ── Validate node types ──────────────────────────────────────────
        let (mut system, mut interface, mut pinned_names, mut unsupported) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for &id in &selected {
            let label = node_label(self, id);
            match group_support(&self.nodes[&id].node_type) {
                GroupSupport::System => system.push(label),
                GroupSupport::Interface => interface.push(label),
                GroupSupport::Unsupported => unsupported.push(label),
                GroupSupport::Supported if pinned.contains(&id) => pinned_names.push(label),
                GroupSupport::Supported => {}
            }
        }
        if !system.is_empty() {
            return Err(GroupError::SystemNodes(system));
        }
        if !interface.is_empty() {
            return Err(GroupError::InterfaceNodes(interface));
        }
        if !pinned_names.is_empty() {
            return Err(GroupError::PinnedNodes(pinned_names));
        }
        if !unsupported.is_empty() {
            return Err(GroupError::UnsupportedNodes(unsupported));
        }

        // ── Classify connections ─────────────────────────────────────────
        let inside = |id: NodeId| selection.contains(&id);
        let mut internal: Vec<Connection> = Vec::new();
        let mut incoming: Vec<&Connection> = Vec::new();
        let mut outgoing: Vec<&Connection> = Vec::new();
        for c in &self.connections {
            match (inside(c.from_node), inside(c.to_node)) {
                (true, true) => internal.push(c.clone()),
                (false, true) => incoming.push(c),
                (true, false) => outgoing.push(c),
                (false, false) => {}
            }
        }

        // Stable ordering: by canvas position (top to bottom, left to right),
        // then by id and port so equal positions stay deterministic.
        let pos = |id: NodeId| self.nodes.get(&id).map(|n| n.pos).unwrap_or([0.0, 0.0]);
        let by_position = |a: (NodeId, usize), b: (NodeId, usize)| {
            let (pa, pb) = (pos(a.0), pos(b.0));
            pa[1]
                .total_cmp(&pb[1])
                .then(pa[0].total_cmp(&pb[0]))
                .then(a.0.cmp(&b.0))
                .then(a.1.cmp(&b.1))
        };
        incoming.sort_by(|a, b| by_position((a.to_node, a.to_port), (b.to_node, b.to_port)));
        outgoing
            .sort_by(|a, b| by_position((a.from_node, a.from_port), (b.from_node, b.from_port)));

        // ── Boundary cycle detection ─────────────────────────────────────
        // A loop through the new instance exists if an outside consumer of a
        // group output can reach an outside source of a group input.
        let sources: HashSet<NodeId> = incoming.iter().map(|c| c.from_node).collect();
        if !sources.is_empty() && !outgoing.is_empty() {
            let mut parent: HashMap<NodeId, Option<NodeId>> = HashMap::new();
            let mut queue: VecDeque<NodeId> = VecDeque::new();
            for c in &outgoing {
                if parent.insert(c.to_node, None).is_none() {
                    queue.push_back(c.to_node);
                }
            }
            while let Some(node) = queue.pop_front() {
                if sources.contains(&node) {
                    let mut path = vec![node_label(self, node)];
                    let mut cur = parent[&node];
                    while let Some(p) = cur {
                        path.push(node_label(self, p));
                        cur = parent[&p];
                    }
                    path.reverse();
                    return Err(GroupError::BoundaryCycle(path));
                }
                for c in &self.connections {
                    if c.from_node == node && !inside(c.to_node) && !parent.contains_key(&c.to_node)
                    {
                        parent.insert(c.to_node, Some(node));
                        queue.push_back(c.to_node);
                    }
                }
            }
        }

        // ── Build the internal graph ─────────────────────────────────────
        let mut graph = Graph::new();
        for &id in &selected {
            graph.nodes.insert(id, self.nodes[&id].clone());
        }
        graph.connections = internal;
        graph.fix_next_id();

        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &id in &selected {
            let p = pos(id);
            min_x = min_x.min(p[0]);
            min_y = min_y.min(p[1]);
            max_x = max_x.max(p[0]);
            max_y = max_y.max(p[1]);
        }

        let mut used_input_names = HashSet::new();
        let mut used_output_names = HashSet::new();

        // One CustomInput per distinct external source. Created in port
        // order so ascending NodeIds match the runtime's port mapping.
        let mut external_inputs: Vec<(NodeId, usize, String)> = Vec::new();
        let mut input_nodes: HashMap<(NodeId, usize), NodeId> = HashMap::new();
        for c in &incoming {
            let key = (c.from_node, c.from_port);
            let input_node = match input_nodes.get(&key) {
                Some(&id) => id,
                None => {
                    let source_kind = self.port_kind(c.from_node, c.from_port, true);
                    let target_kind = self.port_kind(c.to_node, c.to_port, false);
                    let kind = match source_kind {
                        Some(PortKind::Generic) | None => target_kind.unwrap_or(PortKind::Generic),
                        Some(kind) => kind,
                    };
                    let base = format!(
                        "{} {}",
                        self.nodes[&c.to_node].node_type.title(),
                        port_name(self, c.to_node, c.to_port, false)
                    );
                    let name = unique_name(&base, &mut used_input_names);
                    let index = external_inputs.len();
                    let id = graph.add_node(
                        NodeType::CustomInput { name, kind },
                        [min_x - 240.0, min_y + index as f32 * 90.0],
                    );
                    input_nodes.insert(key, id);
                    external_inputs.push((c.from_node, c.from_port, c.label.clone()));
                    id
                }
            };
            graph.connections.push(Connection {
                from_node: input_node,
                from_port: 0,
                to_node: c.to_node,
                to_port: c.to_port,
                label: c.label.clone(),
            });
        }

        // One CustomOutput per distinct internal source feeding outside.
        let mut external_outputs: Vec<Vec<(NodeId, usize, String)>> = Vec::new();
        let mut output_index: HashMap<(NodeId, usize), usize> = HashMap::new();
        for c in &outgoing {
            let key = (c.from_node, c.from_port);
            let index = match output_index.get(&key) {
                Some(&index) => index,
                None => {
                    let kind = self
                        .port_kind(c.from_node, c.from_port, true)
                        .unwrap_or(PortKind::Generic);
                    let base = format!(
                        "{} {}",
                        self.nodes[&c.from_node].node_type.title(),
                        port_name(self, c.from_node, c.from_port, true)
                    );
                    let name = unique_name(&base, &mut used_output_names);
                    let index = external_outputs.len();
                    let id = graph.add_node(
                        NodeType::CustomOutput { name, kind },
                        [max_x + 280.0, min_y + index as f32 * 90.0],
                    );
                    graph.connections.push(Connection {
                        from_node: c.from_node,
                        from_port: c.from_port,
                        to_node: id,
                        to_port: 0,
                        label: c.label.clone(),
                    });
                    output_index.insert(key, index);
                    external_outputs.push(Vec::new());
                    index
                }
            };
            external_outputs[index].push((c.to_node, c.to_port, c.label.clone()));
        }

        let (inputs, outputs) = crate::document::custom_node_interface(&graph);

        Ok(GroupExtraction {
            selected,
            graph,
            inputs,
            outputs,
            external_inputs,
            external_outputs,
            position: [(min_x + max_x) / 2.0, (min_y + max_y) / 2.0],
        })
    }

    /// Replace the extracted selection with one `CustomNodeInstance`
    /// referencing `file`, and reconnect the boundary. Returns the new id.
    ///
    /// The caller is responsible for undo snapshots and for releasing any
    /// per-node editor resources before calling this.
    pub fn apply_group(&mut self, extraction: &GroupExtraction, file: &str) -> NodeId {
        for &id in &extraction.selected {
            self.remove_node(id);
        }

        let instance = self.add_node(
            NodeType::CustomNodeInstance {
                file: file.to_string(),
                inputs: extraction.inputs.clone(),
                outputs: extraction.outputs.clone(),
            },
            extraction.position,
        );

        // `add_connection` creates unlabeled wires; restore boundary labels.
        fn set_label(graph: &mut Graph, to_node: NodeId, to_port: usize, label: &str) {
            if label.is_empty() {
                return;
            }
            if let Some(c) = graph
                .connections
                .iter_mut()
                .rev()
                .find(|c| c.to_node == to_node && c.to_port == to_port)
            {
                c.label = label.to_string();
            }
        }

        for (port, (from_node, from_port, label)) in extraction.external_inputs.iter().enumerate() {
            self.add_connection(*from_node, *from_port, instance, port);
            set_label(self, instance, port, label);
        }
        for (port, consumers) in extraction.external_outputs.iter().enumerate() {
            for (to_node, to_port, label) in consumers {
                self.add_connection(instance, port, *to_node, *to_port);
                set_label(self, *to_node, *to_port, label);
            }
        }

        instance
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::CustomNodeFile;
    use crate::graph::PortValue;

    fn catalog_node(label: &str) -> NodeType {
        let entry = crate::nodes::catalog()
            .into_iter()
            .find(|e| e.label == label)
            .unwrap_or_else(|| panic!("catalog entry {label}"));
        (entry.factory)()
    }

    fn slider(value: f32) -> NodeType {
        NodeType::Slider {
            value,
            min: -1000.0,
            max: 1000.0,
            step: 0.01,
            slider_color: [80, 160, 255],
            label: String::new(),
        }
    }

    fn select(ids: &[NodeId]) -> HashSet<NodeId> {
        ids.iter().copied().collect()
    }

    fn none() -> HashSet<NodeId> {
        HashSet::new()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("patchwork_group_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn empty_pwnode(path: &Path, name: &str) {
        CustomNodeFile {
            version: 1,
            name: name.to_string(),
            graph: Graph::new(),
            inputs: vec![],
            outputs: vec![],
        }
        .save(path)
        .unwrap();
    }

    fn save_extraction(extraction: &GroupExtraction, path: &Path, name: &str) {
        CustomNodeFile {
            version: 1,
            name: name.to_string(),
            graph: extraction.graph.clone(),
            inputs: extraction.inputs.clone(),
            outputs: extraction.outputs.clone(),
        }
        .save(path)
        .unwrap();
    }

    fn float(values: &HashMap<(NodeId, usize), PortValue>, key: (NodeId, usize)) -> f32 {
        match values.get(&key) {
            Some(PortValue::Float(v)) => *v,
            other => panic!("expected float at {key:?}, got {other:?}"),
        }
    }

    fn label_of(graph: &Graph, from: (NodeId, usize), to: (NodeId, usize)) -> Option<String> {
        graph
            .connections
            .iter()
            .find(|c| (c.from_node, c.from_port) == from && (c.to_node, c.to_port) == to)
            .map(|c| c.label.clone())
    }

    // 1. Internal-only grouping.
    #[test]
    fn internal_only_group_has_no_ports() {
        let mut g = Graph::new();
        let a = g.add_node(catalog_node("Add"), [0.0, 0.0]);
        let m = g.add_node(catalog_node("Multiply"), [200.0, 0.0]);
        g.add_connection(a, 0, m, 0);

        let x = g.extract_group(&select(&[a, m]), &none()).unwrap();
        assert!(x.inputs.is_empty() && x.outputs.is_empty());
        assert_eq!(x.graph.nodes.len(), 2);
        assert_eq!(x.graph.connections.len(), 1);
        assert_eq!(x.selected, vec![a, m]);
        assert_eq!(x.position, [100.0, 0.0]);
    }

    // 2. Incoming and outgoing connections.
    #[test]
    fn incoming_and_outgoing_become_ports() {
        let mut g = Graph::new();
        let s = g.add_node(slider(2.0), [0.0, 0.0]);
        let a = g.add_node(catalog_node("Add"), [200.0, 0.0]);
        let m = g.add_node(catalog_node("Multiply"), [400.0, 0.0]);
        g.add_connection(s, 0, a, 0);
        g.add_connection(a, 0, m, 0);

        let x = g.extract_group(&select(&[a]), &none()).unwrap();
        assert_eq!(x.inputs.len(), 1);
        assert_eq!(x.outputs.len(), 1);
        assert_eq!(x.inputs[0].kind, "Number");
        assert_eq!(x.external_inputs, vec![(s, 0, String::new())]);
        assert_eq!(x.external_outputs, vec![vec![(m, 0, String::new())]]);

        let inst = g.apply_group(&x, "group.pwnode");
        assert!(!g.nodes.contains_key(&a));
        assert!(label_of(&g, (s, 0), (inst, 0)).is_some());
        assert!(label_of(&g, (inst, 0), (m, 0)).is_some());
        assert_eq!(g.connections.len(), 2);
    }

    // 3. Shared incoming source.
    #[test]
    fn shared_incoming_source_creates_one_input() {
        let mut g = Graph::new();
        let s = g.add_node(slider(1.0), [0.0, 0.0]);
        let a = g.add_node(catalog_node("Add"), [200.0, 0.0]);
        let m = g.add_node(catalog_node("Multiply"), [200.0, 100.0]);
        g.add_connection(s, 0, a, 0);
        g.add_connection(s, 0, m, 1);

        let x = g.extract_group(&select(&[a, m]), &none()).unwrap();
        assert_eq!(x.inputs.len(), 1);
        let input_node = x
            .graph
            .nodes
            .values()
            .find(|n| matches!(n.node_type, NodeType::CustomInput { .. }))
            .unwrap()
            .id;
        let fan_out: Vec<_> = x
            .graph
            .connections
            .iter()
            .filter(|c| c.from_node == input_node)
            .map(|c| (c.to_node, c.to_port))
            .collect();
        assert_eq!(fan_out, vec![(a, 0), (m, 1)]);

        let inst = g.apply_group(&x, "group.pwnode");
        assert_eq!(g.connections.len(), 1);
        assert!(label_of(&g, (s, 0), (inst, 0)).is_some());
    }

    // 4. Multiple outgoing consumers.
    #[test]
    fn multiple_consumers_share_one_output() {
        let mut g = Graph::new();
        let a = g.add_node(catalog_node("Add"), [0.0, 0.0]);
        let m1 = g.add_node(catalog_node("Multiply"), [200.0, 0.0]);
        let m2 = g.add_node(catalog_node("Multiply"), [200.0, 100.0]);
        g.add_connection(a, 0, m1, 0);
        g.add_connection(a, 0, m2, 1);

        let x = g.extract_group(&select(&[a]), &none()).unwrap();
        assert_eq!(x.outputs.len(), 1);
        assert_eq!(x.external_outputs[0].len(), 2);

        let inst = g.apply_group(&x, "group.pwnode");
        assert!(label_of(&g, (inst, 0), (m1, 0)).is_some());
        assert!(label_of(&g, (inst, 0), (m2, 1)).is_some());
    }

    // 5. Unique port names and stable ordering.
    #[test]
    fn port_names_are_unique_and_ordered_by_position() {
        let mut g = Graph::new();
        let s1 = g.add_node(slider(1.0), [0.0, 0.0]);
        let s2 = g.add_node(slider(2.0), [0.0, 100.0]);
        let lower = g.add_node(catalog_node("Add"), [200.0, 300.0]);
        let upper = g.add_node(catalog_node("Add"), [200.0, 0.0]);
        g.add_connection(s1, 0, lower, 0);
        g.add_connection(s2, 0, upper, 0);

        let x = g.extract_group(&select(&[lower, upper]), &none()).unwrap();
        let names: Vec<_> = x.inputs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Add A", "Add A 2"]);
        // The input feeding the upper node comes first.
        assert_eq!(x.external_inputs[0].0, s2);
        assert_eq!(x.external_inputs[1].0, s1);
        // The saved interface (NodeId order) matches the extraction.
        assert_eq!(
            crate::document::custom_node_interface(&x.graph),
            (x.inputs.clone(), x.outputs.clone())
        );
        // Deterministic.
        let again = g.extract_group(&select(&[upper, lower]), &none()).unwrap();
        assert_eq!(again.inputs, x.inputs);
        assert_eq!(again.external_inputs, x.external_inputs);
    }

    // 6. Unsupported selections.
    #[test]
    fn invalid_selections_are_rejected() {
        let mut g = Graph::new();
        let a = g.add_node(catalog_node("Add"), [0.0, 0.0]);
        let file = g.add_node(NodeType::FileMenu, [0.0, 0.0]);
        let mcp = g.add_node(NodeType::McpServer, [0.0, 0.0]);
        let input = g.add_node(
            NodeType::CustomInput {
                name: "x".into(),
                kind: PortKind::Number,
            },
            [0.0, 0.0],
        );

        assert_eq!(
            g.extract_group(&none(), &none()).unwrap_err(),
            GroupError::EmptySelection
        );
        assert!(matches!(
            g.extract_group(&select(&[a, 999]), &none()),
            Err(GroupError::MissingNodes(ids)) if ids == vec![999]
        ));
        assert!(matches!(
            g.extract_group(&select(&[a, file]), &none()),
            Err(GroupError::SystemNodes(_))
        ));
        assert!(matches!(
            g.extract_group(&select(&[a, input]), &none()),
            Err(GroupError::InterfaceNodes(_))
        ));
        assert!(matches!(
            g.extract_group(&select(&[a]), &select(&[a])),
            Err(GroupError::PinnedNodes(_))
        ));
        let err = g.extract_group(&select(&[a, mcp]), &none()).unwrap_err();
        assert_eq!(
            err,
            GroupError::UnsupportedNodes(vec![format!("MCP Server #{mcp}")])
        );
        assert!(err.to_string().contains("MCP Server"));
    }

    #[test]
    fn group_names_are_validated() {
        assert!(validate_group_name("My Node").is_ok());
        for bad in ["", "   ", "a/b", "a:b", "name.", "CON", "com1", "x\u{7}"] {
            assert!(
                validate_group_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert_eq!(
            with_pwnode_extension(PathBuf::from("a")),
            PathBuf::from("a.pwnode")
        );
        assert_eq!(
            with_pwnode_extension(PathBuf::from("a.pwnode")),
            PathBuf::from("a.pwnode")
        );
    }

    // 7a. Invalid cycles.
    #[test]
    fn boundary_cycle_is_rejected() {
        let mut g = Graph::new();
        let a = g.add_node(catalog_node("Add"), [0.0, 0.0]);
        let x = g.add_node(catalog_node("Multiply"), [200.0, 0.0]);
        let y = g.add_node(catalog_node("Multiply"), [400.0, 0.0]);
        g.add_connection(a, 0, x, 0);
        g.add_connection(x, 0, y, 0);
        g.add_connection(y, 0, a, 1);

        match g.extract_group(&select(&[a]), &none()) {
            Err(GroupError::BoundaryCycle(path)) => assert_eq!(path.len(), 2),
            other => panic!("expected cycle, got {other:?}"),
        }
        // Including the whole loop is fine.
        assert!(g.extract_group(&select(&[a, x, y]), &none()).is_ok());
    }

    // 7b. Recursive dependencies.
    #[test]
    fn recursive_dependencies_are_rejected() {
        let dir = temp_dir("recursive");
        let existing = dir.join("existing.pwnode");
        empty_pwnode(&existing, "existing");

        let mut g = Graph::new();
        let inst = g.add_node(
            NodeType::CustomNodeInstance {
                file: existing.to_string_lossy().into_owned(),
                inputs: vec![],
                outputs: vec![],
            },
            [0.0, 0.0],
        );
        let x = g.extract_group(&select(&[inst]), &none()).unwrap();
        let new_file = dir.join("new.pwnode");

        // Overwriting the referenced file would make it contain itself.
        assert!(check_group_dependencies(&x.graph, &existing, &[]).is_err());
        // A new file is fine.
        assert!(check_group_dependencies(&x.graph, &new_file, &[]).is_ok());
        // Not when the nested node is the custom node being edited.
        assert!(check_group_dependencies(&x.graph, &new_file, &[existing.clone()]).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // 8. Nested custom-node paths and 9. save/load round trip.
    #[test]
    fn nested_paths_and_round_trip() {
        let dir = temp_dir("nested");
        let inner = dir.join("inner.pwnode");
        empty_pwnode(&inner, "inner");

        let mut g = Graph::new();
        let s = g.add_node(slider(1.0), [0.0, 0.0]);
        let a = g.add_node(catalog_node("Add"), [200.0, 0.0]);
        let nested = g.add_node(
            NodeType::CustomNodeInstance {
                file: inner.to_string_lossy().into_owned(),
                inputs: vec![],
                outputs: vec![],
            },
            [200.0, 100.0],
        );
        let m = g.add_node(catalog_node("Multiply"), [400.0, 0.0]);
        g.add_connection(s, 0, a, 0);
        g.add_connection(a, 0, m, 0);

        let x = g.extract_group(&select(&[a, nested]), &none()).unwrap();
        let outer = dir.join("outer.pwnode");
        assert!(check_group_dependencies(&x.graph, &outer, &[]).is_ok());
        save_extraction(&x, &outer, "outer");

        // Stored relative to the .pwnode file.
        let raw = std::fs::read_to_string(&outer).unwrap();
        assert!(raw.contains("\"file\": \"inner.pwnode\""), "{raw}");

        let loaded = CustomNodeFile::load(&outer).unwrap();
        assert_eq!(loaded.name, "outer");
        assert_eq!(loaded.inputs, x.inputs);
        assert_eq!(loaded.outputs, x.outputs);
        assert_eq!(loaded.graph.nodes.len(), x.graph.nodes.len());
        assert_eq!(loaded.graph.connections.len(), x.graph.connections.len());
        let nested_file = loaded
            .graph
            .nodes
            .values()
            .find_map(|n| match &n.node_type {
                NodeType::CustomNodeInstance { file, .. } => Some(PathBuf::from(file)),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            std::fs::canonicalize(nested_file).unwrap(),
            std::fs::canonicalize(&inner).unwrap()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // 10. Evaluation equivalence for a simple mathematical graph.
    #[test]
    fn grouped_graph_evaluates_identically() {
        let dir = temp_dir("eval");
        let mut g = Graph::new();
        let s1 = g.add_node(slider(3.0), [0.0, 0.0]);
        let s2 = g.add_node(slider(4.0), [0.0, 100.0]);
        let s3 = g.add_node(slider(5.0), [0.0, 200.0]);
        let add = g.add_node(catalog_node("Add"), [200.0, 0.0]);
        let mul = g.add_node(catalog_node("Multiply"), [400.0, 0.0]);
        let sink = g.add_node(catalog_node("Add"), [600.0, 0.0]);
        g.add_connection(s1, 0, add, 0);
        g.add_connection(s2, 0, add, 1);
        g.add_connection(add, 0, mul, 0);
        g.add_connection(s3, 0, mul, 1);
        g.add_connection(mul, 0, sink, 0);

        let before = float(&g.clone().evaluate(1.0), (sink, 0));
        assert_eq!(before, (3.0 + 4.0) * 5.0);

        let x = g.extract_group(&select(&[add, mul]), &none()).unwrap();
        assert_eq!(x.inputs.len(), 3);
        assert_eq!(x.outputs.len(), 1);
        let path = dir.join("math.pwnode");
        save_extraction(&x, &path, "math");

        let mut grouped = g.clone();
        let inst = grouped.apply_group(&x, &path.to_string_lossy());
        let values = grouped.evaluate(1.0);
        assert_eq!(float(&values, (inst, 0)), before);
        assert_eq!(float(&values, (sink, 0)), before);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // 12. Bypass state and connection labels.
    #[test]
    fn bypass_and_labels_are_preserved() {
        let mut g = Graph::new();
        let s = g.add_node(slider(1.0), [0.0, 0.0]);
        let a = g.add_node(catalog_node("Add"), [200.0, 0.0]);
        let m = g.add_node(catalog_node("Multiply"), [400.0, 0.0]);
        let out = g.add_node(catalog_node("Add"), [600.0, 0.0]);
        g.add_connection(s, 0, a, 0);
        g.add_connection(a, 0, m, 0);
        g.add_connection(m, 0, out, 0);
        for c in &mut g.connections {
            c.label = format!("{}->{}", c.from_node, c.to_node);
        }
        g.nodes.get_mut(&m).unwrap().bypassed = true;

        let x = g.extract_group(&select(&[a, m]), &none()).unwrap();
        assert!(x.graph.nodes[&m].bypassed);
        assert!(!x.graph.nodes[&a].bypassed);
        assert_eq!(
            label_of(&x.graph, (a, 0), (m, 0)),
            Some(format!("{a}->{m}"))
        );

        let inst = g.apply_group(&x, "group.pwnode");
        assert_eq!(label_of(&g, (s, 0), (inst, 0)), Some(format!("{s}->{a}")));
        assert_eq!(
            label_of(&g, (inst, 0), (out, 0)),
            Some(format!("{m}->{out}"))
        );
    }
}
