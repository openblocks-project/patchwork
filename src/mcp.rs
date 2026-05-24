// MCP (Model Context Protocol) Server for Patchwork
// Enables AI assistants to programmatically create nodes, connect them, and build workflows.
// Runs as a background thread, communicates with GUI via mpsc channels.
// Protocol: JSON-RPC over stdin/stdout.

use crate::graph::*;
use crate::node_trait::NodeBehavior;
use crate::nodes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{mpsc, Arc, Mutex};

/// Shared MCP log accessible from both MCP thread and GUI
pub type McpLog = Arc<Mutex<Vec<String>>>;

pub fn new_log() -> McpLog {
    Arc::new(Mutex::new(Vec::new()))
}

fn log_msg(log: &McpLog, msg: String) {
    if let Ok(mut l) = log.lock() {
        l.push(msg);
        if l.len() > 200 { l.drain(0..100); }
    }
}

// ── Commands & Results ───────────────────────────────────────────────────────

pub enum McpCommand {
    /// List node types. Defaults to summary mode (name/category/in_count/out_count
    /// only). Pass `full: true` for the legacy verbose schema.
    ListNodeTypes {
        category: Option<String>,
        name_filter: Option<String>,
        full: bool,
    },
    /// Full schema for one node type (ports with index+kind, properties with defaults).
    DescribeNodeType { name: String },
    CreateNode { type_name: String, position: [f32; 2], properties: Option<Value> },
    DeleteNode { node_id: NodeId },
    ListNodes,
    GetNode { node_id: NodeId },
    UpdateNode { node_id: NodeId, properties: Value },
    Connect { from_node: NodeId, from_port: usize, to_node: NodeId, to_port: usize },
    Disconnect { from_node: NodeId, from_port: usize, to_node: NodeId, to_port: usize },
    ListConnections,
    GetPortValues { node_id: Option<NodeId> },
    SaveProject { path: String },
    LoadProject { path: String },
    #[allow(dead_code)]
    GetGraph,
    CreateWorkflow { nodes: Vec<WorkflowNode>, connections: Vec<WorkflowConn> },
    /// Trigger an action on a node (send, play, listen, etc.)
    TriggerNode { node_id: NodeId, action: String },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum McpResult {
    Json(Value),
    Error { error: String },
}

pub struct McpRequest {
    pub command: McpCommand,
    pub response_tx: mpsc::Sender<McpResult>,
}

#[derive(Deserialize)]
pub struct WorkflowNode {
    #[serde(rename = "type")]
    pub node_type: String,
    pub position: Option<[f32; 2]>,
    pub properties: Option<Value>,
}

#[derive(Deserialize)]
pub struct WorkflowConn {
    pub from_index: usize,
    pub from_port: usize,
    pub to_index: usize,
    pub to_port: usize,
}

// ── Tool Schemas ─────────────────────────────────────────────────────────────

fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_node_types",
            "description": "List available node types. Defaults to summary mode (name, category, in_count, out_count, wip) — typically <5KB. Use 'category' or 'name_filter' to narrow further. Pass 'full: true' for the legacy verbose schema (large; prefer describe_node_type for one node).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "category": { "type": "string", "description": "Filter by category (case-insensitive). Examples: Audio, Image, Math, Input, Signal, IO, MIDI, OSC, AI, ML, Hardware, Network, Shader, Utility, Output." },
                    "name_filter": { "type": "string", "description": "Substring match against node name (case-insensitive)." },
                    "full": { "type": "boolean", "description": "If true, include full per-node schema (ports with index+kind, properties with defaults). Default false (summary)." }
                }
            }
        },
        {
            "name": "describe_node_type",
            "description": "Full schema for one node type: ports as [{index, name, kind}] and properties as {key: {type, default}}. Use this after list_node_types narrows down a candidate.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Node type name (case-insensitive). Examples: 'Synth', 'Reverb', 'WGSL Viewer'." }
                },
                "required": ["name"]
            }
        },
        {
            "name": "create_node",
            "description": "Create a new node. Returns {node_id, applied, [ignored_unknown], [readonly_rejected], [errors]}. Unknown property keys land in 'ignored_unknown'; runtime fields (status, response, log, etc.) are 'readonly_rejected'.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "Node type name (e.g., 'Slider', 'Synth', 'Add')" },
                    "position": { "type": "array", "items": { "type": "number" }, "description": "[x, y] canvas position, default [200, 200]" },
                    "properties": { "type": "object", "description": "Initial property values (e.g., {\"value\": 0.5} for Slider). See describe_node_type for valid keys." }
                },
                "required": ["type"]
            }
        },
        {
            "name": "delete_node",
            "description": "Delete a node by its ID (also removes all connections)",
            "inputSchema": {
                "type": "object",
                "properties": { "node_id": { "type": "integer", "description": "Node ID" } },
                "required": ["node_id"]
            }
        },
        {
            "name": "list_nodes",
            "description": "List all nodes in the current graph with their IDs, types, and positions",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_node",
            "description": "Get full state of a node by ID including all properties",
            "inputSchema": {
                "type": "object",
                "properties": { "node_id": { "type": "integer" } },
                "required": ["node_id"]
            }
        },
        {
            "name": "update_node",
            "description": "Update properties of an existing node. Returns {success, applied, [ignored_unknown], [readonly_rejected], [errors]}. Audio Manager 'enabled', OscIn 'listening', and MidiIn/Serial 'port_name' are wired to side effects: setting them takes effect on the next render frame (no separate trigger required).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": { "type": "integer" },
                    "properties": { "type": "object", "description": "Properties to update. Use describe_node_type to see valid keys." }
                },
                "required": ["node_id", "properties"]
            }
        },
        {
            "name": "connect",
            "description": "Connect an output port of one node to an input port of another",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_node": { "type": "integer", "description": "Source node ID" },
                    "from_port": { "type": "integer", "description": "Source output port index (0-based)" },
                    "to_node": { "type": "integer", "description": "Target node ID" },
                    "to_port": { "type": "integer", "description": "Target input port index (0-based)" }
                },
                "required": ["from_node", "from_port", "to_node", "to_port"]
            }
        },
        {
            "name": "disconnect",
            "description": "Remove a connection between two ports",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_node": { "type": "integer" },
                    "from_port": { "type": "integer" },
                    "to_node": { "type": "integer" },
                    "to_port": { "type": "integer" }
                },
                "required": ["from_node", "from_port", "to_node", "to_port"]
            }
        },
        {
            "name": "list_connections",
            "description": "List all connections in the graph",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_port_values",
            "description": "Get current evaluated output values. Optionally filter by node_id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": { "type": "integer", "description": "Optional: filter to a specific node" }
                }
            }
        },
        {
            "name": "save_project",
            "description": "Save the current graph to a project folder",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Folder path to save project" } },
                "required": ["path"]
            }
        },
        {
            "name": "load_project",
            "description": "Load a graph from a project folder",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Path to project.json file" } },
                "required": ["path"]
            }
        },
        {
            "name": "trigger_node",
            "description": "Trigger an action on a node. Actions by node type: HttpRequest/AiRequest='send'; VideoPlayer='play|pause|stop'; AudioPlayer='play'; Speaker/Synth/AudioInput='activate|deactivate' (Synth also accepts play/stop); OscIn='listen|stop_listen'; MidiIn/Serial='connect|disconnect' (port_name must be set first via update_node).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": { "type": "integer", "description": "Node ID" },
                    "action": { "type": "string", "description": "Action verb (see node-type list in description)." }
                },
                "required": ["node_id", "action"]
            }
        },
        {
            "name": "create_workflow",
            "description": "Create multiple nodes and connections in one atomic operation. Connections use 0-based indices into the nodes array. Returns {node_ids, [property_warnings], [connection_errors]} — property_warnings entries include {index, node_id, ignored_unknown, readonly_rejected, errors}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "nodes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "type": { "type": "string" },
                                "position": { "type": "array", "items": { "type": "number" } },
                                "properties": { "type": "object" }
                            },
                            "required": ["type"]
                        }
                    },
                    "connections": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from_index": { "type": "integer", "description": "Index into nodes array" },
                                "from_port": { "type": "integer" },
                                "to_index": { "type": "integer", "description": "Index into nodes array" },
                                "to_port": { "type": "integer" }
                            },
                            "required": ["from_index", "from_port", "to_index", "to_port"]
                        }
                    }
                },
                "required": ["nodes"]
            }
        }
    ])
}

// ── Property Merge ───────────────────────────────────────────────────────────

/// Fields that are runtime/auto-managed state, not user-controllable.
/// `apply_properties` rejects writes to these and `extract_property_schema`
/// hides them from `list_node_types` / `describe_node_type` output.
pub const READONLY_FIELDS: &[&str] = &[
    "response", "status", "log", "last_hash",
    "last_args", "last_args_text", "discovered",
    "result", "error", "result_text", "last_input_hash",
    "current_frame", "duration", "variables",
    "image_data", "last_save_hash", "content",
    "messages", "detected_devices", "effects",
    "x", "y", // mouse tracker runtime values
];

/// Diagnostics returned by `apply_properties` so callers can tell which keys
/// were honored, which were silently dropped, and why.
#[derive(Default, Debug)]
pub struct PropResult {
    /// Keys that matched a known property and were applied successfully.
    pub applied: Vec<String>,
    /// Keys that don't exist on this node type (likely typos or wrapped-state keys).
    pub ignored_unknown: Vec<String>,
    /// Keys rejected because they target runtime/auto-managed state.
    pub readonly_rejected: Vec<String>,
    /// Hard errors — usually a deserialize failure (wrong type for the field).
    pub errors: Vec<String>,
}

impl PropResult {
    /// True iff every key was applied without warnings or errors.
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
            && self.readonly_rejected.is_empty()
            && self.ignored_unknown.is_empty()
    }

    /// Attach the non-empty diagnostic fields onto an existing JSON response.
    pub fn merge_into(&self, resp: &mut Value) {
        if let Value::Object(map) = resp {
            map.insert("applied".into(), json!(self.applied));
            if !self.ignored_unknown.is_empty() {
                map.insert("ignored_unknown".into(), json!(self.ignored_unknown));
            }
            if !self.readonly_rejected.is_empty() {
                map.insert("readonly_rejected".into(), json!(self.readonly_rejected));
            }
            if !self.errors.is_empty() {
                map.insert("errors".into(), json!(self.errors));
            }
        }
    }
}

/// Merge JSON properties into a NodeType, returning per-key diagnostics.
///
/// Keys are validated against the node's serde shape:
/// - matching keys → applied (overwrite previous value)
/// - keys in `READONLY_FIELDS` → rejected with `readonly_rejected`
/// - unknown keys → reported via `ignored_unknown`
/// - whole-payload deserialize failure → `errors`
pub fn apply_properties(node_type: &mut NodeType, properties: Value) -> PropResult {
    let mut result = PropResult::default();

    let Value::Object(props) = properties else {
        result.errors.push("'properties' must be a JSON object".into());
        return result;
    };

    let current = match serde_json::to_value(&*node_type) {
        Ok(v) => v,
        Err(e) => {
            result.errors.push(format!("serialize current state: {}", e));
            return result;
        }
    };

    let mut outer_obj = match current {
        Value::Object(m) => m,
        _ => {
            result.errors.push("unexpected NodeType serialization shape".into());
            return result;
        }
    };

    let tag = match outer_obj.keys().next().cloned() {
        Some(k) => k,
        None => {
            result.errors.push("empty NodeType serialization".into());
            return result;
        }
    };

    let inner_value = outer_obj.get_mut(&tag).expect("tag was just read above");
    let inner_map = match inner_value {
        Value::Object(m) => m,
        _ => {
            result.errors.push("NodeType inner is not an object".into());
            return result;
        }
    };

    let known_keys: std::collections::HashSet<String> = inner_map.keys().cloned().collect();

    for (k, v) in props {
        if READONLY_FIELDS.contains(&k.as_str()) {
            result.readonly_rejected.push(k);
            continue;
        }
        if !known_keys.contains(&k) {
            result.ignored_unknown.push(k);
            continue;
        }
        inner_map.insert(k.clone(), v);
        result.applied.push(k);
    }

    // Re-deserialize from the modified outer object
    match serde_json::from_value::<NodeType>(Value::Object(outer_obj)) {
        Ok(updated) => *node_type = updated,
        Err(e) => result.errors.push(format!("deserialize after merge: {}", e)),
    }

    result
}

// ── Node-Type Description Helpers ────────────────────────────────────────────

fn port_kind_str(k: PortKind) -> &'static str {
    match k {
        PortKind::Number => "Number",
        PortKind::Normalized => "Normalized",
        PortKind::Trigger => "Trigger",
        PortKind::Gate => "Gate",
        PortKind::Text => "Text",
        PortKind::Image => "Image",
        PortKind::Audio => "Audio",
        PortKind::Color => "Color",
        PortKind::Mesh => "Mesh",
        PortKind::Generic => "Generic",
    }
}

/// Build the `properties` schema dict for a node type, hiding runtime fields.
fn extract_property_schema(nt: &NodeType) -> Value {
    let val = match serde_json::to_value(nt) {
        Ok(v) => v,
        Err(_) => return json!({}),
    };
    let outer = match val {
        Value::Object(m) => m,
        _ => return json!({}),
    };
    let inner = match outer.into_iter().next() {
        Some((_, v)) => v,
        None => return json!({}),
    };
    let fields = match inner {
        Value::Object(m) => m,
        _ => return json!({}),
    };
    let schema: serde_json::Map<String, Value> = fields.into_iter()
        .filter(|(k, _)| !READONLY_FIELDS.contains(&k.as_str()))
        .map(|(k, v)| {
            let type_str = match &v {
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
                Value::Null => "null",
            };
            (k, json!({ "type": type_str, "default": v }))
        })
        .collect();
    Value::Object(schema)
}

/// Full per-node description used by `describe_node_type` and `list_node_types({full: true})`.
fn describe_node_type_value(entry: &nodes::NodeCatalogEntry, nt: &NodeType) -> Value {
    let inputs: Vec<Value> = nt.inputs().iter().enumerate().map(|(i, p)| json!({
        "index": i,
        "name": p.name.as_ref(),
        "kind": port_kind_str(p.kind),
    })).collect();
    let outputs: Vec<Value> = nt.outputs().iter().enumerate().map(|(i, p)| json!({
        "index": i,
        "name": p.name.as_ref(),
        "kind": port_kind_str(p.kind),
    })).collect();
    json!({
        "name": entry.label,
        "category": entry.category,
        "wip": entry.wip,
        "inputs": inputs,
        "outputs": outputs,
        "properties": extract_property_schema(nt),
    })
}

// ── Command Execution (called by app.rs) ─────────────────────────────────────

pub fn execute_command(
    cmd: McpCommand,
    graph: &mut Graph,
    values: &HashMap<(NodeId, usize), PortValue>,
) -> McpResult {
    match cmd {
        McpCommand::ListNodeTypes { category, name_filter, full } => {
            let catalog = nodes::catalog();
            let cat_lower = category.as_ref().map(|c| c.to_lowercase());
            let name_lower = name_filter.as_ref().map(|n| n.to_lowercase());
            let types: Vec<Value> = catalog.iter()
                .filter(|e| cat_lower.as_ref()
                    .map_or(true, |c| e.category.to_lowercase() == *c))
                .filter(|e| name_lower.as_ref()
                    .map_or(true, |n| e.label.to_lowercase().contains(n)))
                .map(|e| {
                    let nt = (e.factory)();
                    if full {
                        describe_node_type_value(e, &nt)
                    } else {
                        json!({
                            "name": e.label,
                            "category": e.category,
                            "in_count": nt.inputs().len(),
                            "out_count": nt.outputs().len(),
                            "wip": e.wip,
                        })
                    }
                })
                .collect();
            McpResult::Json(json!(types))
        }

        McpCommand::DescribeNodeType { name } => {
            let catalog = nodes::catalog();
            match catalog.iter().find(|e| e.label.eq_ignore_ascii_case(&name)) {
                Some(entry) => {
                    let nt = (entry.factory)();
                    McpResult::Json(describe_node_type_value(entry, &nt))
                }
                None => McpResult::Error {
                    error: format!("Unknown node type: '{}'. Use list_node_types to discover names.", name),
                },
            }
        }

        McpCommand::CreateNode { type_name, position, properties } => {
            let catalog = nodes::catalog();
            if let Some(entry) = catalog.iter().find(|e| e.label.eq_ignore_ascii_case(&type_name)) {
                let mut nt = (entry.factory)();
                let prop_result = properties
                    .map(|props| apply_properties(&mut nt, props))
                    .unwrap_or_default();
                let id = graph.add_node(nt, position);
                let mut resp = json!({ "node_id": id });
                prop_result.merge_into(&mut resp);
                McpResult::Json(resp)
            } else {
                McpResult::Error { error: format!("Unknown node type: '{}'. Use list_node_types to see available types.", type_name) }
            }
        }

        McpCommand::DeleteNode { node_id } => {
            if graph.nodes.contains_key(&node_id) {
                graph.remove_node(node_id);
                McpResult::Json(json!({ "success": true }))
            } else {
                McpResult::Error { error: format!("Node {} not found", node_id) }
            }
        }

        McpCommand::ListNodes => {
            let nodes: Vec<Value> = graph.nodes.iter().map(|(&id, node)| {
                json!({
                    "id": id,
                    "type": node.node_type.title(),
                    "position": node.pos,
                    "inputs": node.node_type.inputs().iter().map(|p| p.name.as_ref()).collect::<Vec<_>>(),
                    "outputs": node.node_type.outputs().iter().map(|p| p.name.as_ref()).collect::<Vec<_>>(),
                })
            }).collect();
            McpResult::Json(json!(nodes))
        }

        McpCommand::GetNode { node_id } => {
            if let Some(node) = graph.nodes.get(&node_id) {
                let node_json = serde_json::to_value(&node.node_type).unwrap_or(json!(null));
                McpResult::Json(json!({
                    "id": node_id,
                    "type": node.node_type.title(),
                    "position": node.pos,
                    "state": node_json,
                    "inputs": node.node_type.inputs().iter().map(|p| p.name.as_ref()).collect::<Vec<_>>(),
                    "outputs": node.node_type.outputs().iter().map(|p| p.name.as_ref()).collect::<Vec<_>>(),
                }))
            } else {
                McpResult::Error { error: format!("Node {} not found", node_id) }
            }
        }

        McpCommand::UpdateNode { node_id, properties } => {
            if let Some(node) = graph.nodes.get_mut(&node_id) {
                let prop_result = apply_properties(&mut node.node_type, properties);
                let mut resp = json!({ "success": prop_result.errors.is_empty() });
                prop_result.merge_into(&mut resp);
                McpResult::Json(resp)
            } else {
                McpResult::Error { error: format!("Node {} not found", node_id) }
            }
        }

        McpCommand::Connect { from_node, from_port, to_node, to_port } => {
            let src_node = match graph.nodes.get(&from_node) {
                Some(n) => n,
                None => return McpResult::Error { error: format!("Source node {} not found", from_node) },
            };
            let dst_node = match graph.nodes.get(&to_node) {
                Some(n) => n,
                None => return McpResult::Error { error: format!("Target node {} not found", to_node) },
            };
            // Validate port bounds
            let src_outputs = src_node.node_type.outputs();
            let dst_inputs = dst_node.node_type.inputs();
            if from_port >= src_outputs.len() {
                return McpResult::Error { error: format!("Source port {} out of range (node has {} outputs)", from_port, src_outputs.len()) };
            }
            if to_port >= dst_inputs.len() {
                return McpResult::Error { error: format!("Target port {} out of range (node has {} inputs)", to_port, dst_inputs.len()) };
            }
            // Validate port type compatibility
            let src_kind = src_outputs[from_port].kind;
            let dst_kind = dst_inputs[to_port].kind;
            if !PortKind::compatible(src_kind, dst_kind) {
                return McpResult::Error { error: format!("Incompatible ports: {:?} → {:?}", src_kind, dst_kind) };
            }
            graph.add_connection(from_node, from_port, to_node, to_port);
            McpResult::Json(json!({ "success": true }))
        }

        McpCommand::Disconnect { from_node, from_port, to_node, to_port } => {
            let before = graph.connections.len();
            graph.connections.retain(|c| {
                !(c.from_node == from_node && c.from_port == from_port &&
                  c.to_node == to_node && c.to_port == to_port)
            });
            let removed = before - graph.connections.len();
            McpResult::Json(json!({ "success": true, "removed": removed }))
        }

        McpCommand::ListConnections => {
            let conns: Vec<Value> = graph.connections.iter().map(|c| {
                let from_name = graph.nodes.get(&c.from_node)
                    .and_then(|n| n.node_type.outputs().get(c.from_port).map(|p| p.name.to_string()))
                    .unwrap_or_else(|| "?".to_string());
                let to_name = graph.nodes.get(&c.to_node)
                    .and_then(|n| n.node_type.inputs().get(c.to_port).map(|p| p.name.to_string()))
                    .unwrap_or_else(|| "?".to_string());
                json!({
                    "from_node": c.from_node, "from_port": c.from_port, "from_port_name": from_name,
                    "to_node": c.to_node, "to_port": c.to_port, "to_port_name": to_name,
                })
            }).collect();
            McpResult::Json(json!(conns))
        }

        McpCommand::GetPortValues { node_id } => {
            let mut result: HashMap<String, Value> = HashMap::new();
            for (&(nid, port), val) in values {
                if node_id.is_none() || node_id == Some(nid) {
                    let key = format!("{}:{}", nid, port);
                    let v = match val {
                        PortValue::Float(f) => json!(f),
                        PortValue::Text(s) => json!(s),
                        PortValue::Image(img) => json!(format!("[Image {}x{}]", img.width, img.height)),
                        PortValue::GpuImage(h) => json!(format!("[GpuImage {}x{} @ node {}:{}]", h.width, h.height, h.node_id, h.port)),
                        PortValue::Mesh(p) => json!(format!("[Mesh {}v/{}i]", p.mesh.vertices.len(), p.mesh.indices.len())),
                        PortValue::GpuMesh(h) => json!(format!("[GpuMesh {}v/{}i @ node {}:{}]", h.vertex_count, h.index_count, h.node_id, h.port)),
                        PortValue::None => json!(null),
                    };
                    result.insert(key, v);
                }
            }
            McpResult::Json(json!(result))
        }

        McpCommand::SaveProject { path } => {
            let dir = std::path::Path::new(&path);
            if let Err(e) = std::fs::create_dir_all(dir) {
                return McpResult::Error { error: format!("mkdir: {}", e) };
            }
            let json = serde_json::to_string_pretty(graph).unwrap_or_default();
            match std::fs::write(dir.join("project.json"), json) {
                Ok(()) => McpResult::Json(json!({ "success": true, "path": path })),
                Err(e) => McpResult::Error { error: format!("write: {}", e) },
            }
        }

        McpCommand::LoadProject { path } => {
            let p = std::path::Path::new(&path);
            let json_path = if p.is_file() { p.to_path_buf() } else { p.join("project.json") };
            match std::fs::read_to_string(&json_path) {
                Ok(content) => match serde_json::from_str::<Graph>(&content) {
                    Ok(loaded) => {
                        *graph = loaded;
                        McpResult::Json(json!({ "success": true, "nodes": graph.nodes.len() }))
                    }
                    Err(e) => McpResult::Error { error: format!("parse: {}", e) },
                },
                Err(e) => McpResult::Error { error: format!("read: {}", e) },
            }
        }

        McpCommand::GetGraph => {
            let json = serde_json::to_value(graph).unwrap_or(json!(null));
            McpResult::Json(json)
        }

        McpCommand::TriggerNode { node_id, action } => {
            let node = match graph.nodes.get_mut(&node_id) {
                Some(n) => n,
                None => return McpResult::Error { error: format!("Node {} not found", node_id) },
            };
            match (&mut node.node_type, action.as_str()) {
                // HttpRequest: mark for send by setting auto_send + resetting hash
                (NodeType::HttpRequest { auto_send, last_hash, .. }, "send") => {
                    *auto_send = true;
                    *last_hash = 0; // Force hash mismatch → triggers send on next frame
                    McpResult::Json(json!({ "success": true, "triggered": "send" }))
                }
                // AiRequest: set a pending flag via status
                (NodeType::AiRequest { status, .. }, "send") => {
                    *status = "mcp_trigger".into();
                    McpResult::Json(json!({ "success": true, "triggered": "send" }))
                }
                // VideoPlayer
                (NodeType::VideoPlayer { playing, status, .. }, "play") => {
                    *playing = true; *status = "Playing".into();
                    McpResult::Json(json!({ "success": true, "triggered": "play" }))
                }
                (NodeType::VideoPlayer { playing, status, .. }, "pause" | "stop") => {
                    *playing = false; *status = "Stopped".into();
                    McpResult::Json(json!({ "success": true, "triggered": action }))
                }
                // AudioPlayer
                (NodeType::AudioPlayer { volume, .. }, "play") => {
                    if *volume <= 0.0 { *volume = 1.0; }
                    McpResult::Json(json!({ "success": true, "triggered": "play" }))
                }
                // Speaker
                (NodeType::Speaker { active, .. }, "activate") => {
                    *active = true;
                    McpResult::Json(json!({ "success": true, "triggered": "activate" }))
                }
                (NodeType::Speaker { active, .. }, "deactivate") => {
                    *active = false;
                    McpResult::Json(json!({ "success": true, "triggered": "deactivate" }))
                }
                // OscIn
                (NodeType::OscIn { listening, port, .. }, "listen") => {
                    *listening = true;
                    // Actual listener start happens via mcp_trigger temp data
                    let p = *port;
                    // Store trigger request for app layer to process
                    McpResult::Json(json!({ "success": true, "triggered": "listen", "port": p }))
                }
                (NodeType::OscIn { listening, .. }, "stop_listen") => {
                    *listening = false;
                    McpResult::Json(json!({ "success": true, "triggered": "stop_listen" }))
                }
                // Synth active
                (NodeType::Synth { active, .. }, "play" | "activate") => {
                    *active = true;
                    McpResult::Json(json!({ "success": true, "triggered": "activate" }))
                }
                (NodeType::Synth { active, .. }, "stop" | "deactivate") => {
                    *active = false;
                    McpResult::Json(json!({ "success": true, "triggered": "deactivate" }))
                }
                // AudioInput (mic)
                (NodeType::AudioInput { active, .. }, "activate" | "listen") => {
                    *active = true;
                    McpResult::Json(json!({ "success": true, "triggered": "activate" }))
                }
                (NodeType::AudioInput { active, .. }, "deactivate" | "stop") => {
                    *active = false;
                    McpResult::Json(json!({ "success": true, "triggered": "deactivate" }))
                }
                // MidiIn — port_name drives subscription via the reconcile loop in
                // app/io.rs::poll_midi_inputs. Setting port_name with update_node is
                // the canonical way to bind; this trigger is mainly for explicit
                // disconnect or to reaffirm an existing binding.
                (NodeType::MidiIn { port_name, .. }, "connect" | "listen") => {
                    let pn = port_name.clone();
                    if pn.is_empty() {
                        McpResult::Json(json!({
                            "success": false,
                            "error": "MidiIn.port_name is empty; set it via update_node first.",
                        }))
                    } else {
                        McpResult::Json(json!({
                            "success": true,
                            "triggered": "connect",
                            "port_name": pn,
                            "note": "Reconcile loop will subscribe on the next frame.",
                        }))
                    }
                }
                (NodeType::MidiIn { port_name, .. }, "disconnect" | "stop_listen") => {
                    port_name.clear();
                    McpResult::Json(json!({ "success": true, "triggered": "disconnect" }))
                }
                // Serial — same shape as MidiIn. port_name + baud_rate set via
                // update_node; reconcile loop opens the port on the next frame.
                (NodeType::Serial { port_name, .. }, "connect" | "listen") => {
                    let pn = port_name.clone();
                    if pn.is_empty() {
                        McpResult::Json(json!({
                            "success": false,
                            "error": "Serial.port_name is empty; set it via update_node first.",
                        }))
                    } else {
                        McpResult::Json(json!({
                            "success": true,
                            "triggered": "connect",
                            "port_name": pn,
                            "note": "Reconcile loop will open the port on the next frame.",
                        }))
                    }
                }
                (NodeType::Serial { port_name, .. }, "disconnect" | "stop_listen") => {
                    port_name.clear();
                    McpResult::Json(json!({ "success": true, "triggered": "disconnect" }))
                }
                _ => McpResult::Error { error: format!(
                    "Action '{}' not supported for node type '{}'", action, node.node_type.title()
                ) },
            }
        }

        McpCommand::CreateWorkflow { nodes: wf_nodes, connections: wf_conns } => {
            let catalog = nodes::catalog();
            let mut created_ids: Vec<NodeId> = Vec::new();
            let mut prop_warnings: Vec<Value> = Vec::new();

            for (i, wf_node) in wf_nodes.iter().enumerate() {
                let entry = catalog.iter().find(|e| e.label.eq_ignore_ascii_case(&wf_node.node_type));
                if let Some(entry) = entry {
                    let mut nt = (entry.factory)();
                    let prop_result = if let Some(ref props) = wf_node.properties {
                        apply_properties(&mut nt, props.clone())
                    } else {
                        PropResult::default()
                    };
                    let pos = wf_node.position.unwrap_or([200.0 + i as f32 * 250.0, 200.0]);
                    let id = graph.add_node(nt, pos);
                    created_ids.push(id);

                    // Only emit a warning entry if something wasn't perfectly applied
                    if !prop_result.ok() {
                        let mut entry = json!({ "index": i, "node_id": id });
                        prop_result.merge_into(&mut entry);
                        prop_warnings.push(entry);
                    }
                } else {
                    return McpResult::Error {
                        error: format!("Unknown node type at index {}: '{}'", i, wf_node.node_type),
                    };
                }
            }

            // Create connections using the resolved IDs
            let mut connection_errors: Vec<Value> = Vec::new();
            for (ci, wf_conn) in wf_conns.iter().enumerate() {
                if wf_conn.from_index >= created_ids.len() || wf_conn.to_index >= created_ids.len() {
                    connection_errors.push(json!({
                        "connection_index": ci,
                        "error": "from_index or to_index out of range",
                    }));
                    continue;
                }
                let from_id = created_ids[wf_conn.from_index];
                let to_id = created_ids[wf_conn.to_index];
                graph.add_connection(from_id, wf_conn.from_port, to_id, wf_conn.to_port);
            }

            let mut resp = json!({ "node_ids": created_ids });
            if !prop_warnings.is_empty() {
                resp["property_warnings"] = json!(prop_warnings);
            }
            if !connection_errors.is_empty() {
                resp["connection_errors"] = json!(connection_errors);
            }
            McpResult::Json(resp)
        }
    }
}

// ── MCP Thread (JSON-RPC over stdio) ─────────────────────────────────────────

/// Format a `catch_unwind` payload as a human-readable string for logs / error
/// responses. The payload is the `Box<dyn Any + Send>` returned by
/// `catch_unwind(...).unwrap_err()`; in practice it's almost always a `&str` or
/// `String` from `panic!()`.
fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Dispatch a single parsed JSON-RPC request. Returns `true` if the MCP loop
/// should break (e.g., the GUI thread has disconnected — app shutting down).
/// Any panic inside is caught at the call site so the MCP thread survives
/// individual malformed requests.
fn handle_mcp_request(
    request: Value,
    stdout: &mut std::io::Stdout,
    command_tx: &mpsc::Sender<McpRequest>,
    log: &McpLog,
) -> bool {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!({}));

    match method {
        "initialize" => {
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "patchwork", "version": "0.0.1" }
                }
            });
            write_json(stdout, &response);
        }

        "notifications/initialized" => {
            log_msg(log, "✓ Client initialized".into());
        }

        "tools/list" => {
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tool_definitions() }
            });
            write_json(stdout, &response);
        }

        "tools/call" => {
            let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            log_msg(log, format!("→ {} {}", tool_name, serde_json::to_string(&arguments).unwrap_or_default()));

            let cmd = match parse_tool_call(tool_name, &arguments) {
                Ok(cmd) => cmd,
                Err(e) => {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": format!("Error: {}", e) }],
                            "isError": true
                        }
                    });
                    write_json(stdout, &response);
                    return false;
                }
            };

            // Send command to GUI thread and wait for result
            let (resp_tx, resp_rx) = mpsc::channel();
            let req = McpRequest { command: cmd, response_tx: resp_tx };
            if command_tx.send(req).is_err() {
                write_jsonrpc_error(stdout, id, -32603, "App disconnected");
                return true;
            }

            // Wait for GUI to process (blocks until next frame)
            match resp_rx.recv() {
                Ok(result) => {
                    let text = match &result {
                        McpResult::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
                        McpResult::Error { error } => format!("Error: {}", error),
                    };
                    let is_error = matches!(result, McpResult::Error { .. });
                    let short = if text.chars().count() > 80 {
                        let head: String = text.chars().take(80).collect();
                        format!("{}...", head)
                    } else { text.clone() };
                    log_msg(log, format!("← {}{}", if is_error { "ERR " } else { "" }, short));
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": text }],
                            "isError": is_error
                        }
                    });
                    write_json(stdout, &response);
                }
                Err(_) => {
                    write_jsonrpc_error(stdout, id, -32603, "Response timeout");
                }
            }
        }

        _ => {
            write_jsonrpc_error(stdout, id, -32601, &format!("Unknown method: {}", method));
        }
    }

    false
}

pub fn run_mcp_thread(command_tx: mpsc::Sender<McpRequest>, log: McpLog) {
    // Check if stdin is a pipe (Claude Desktop) or terminal (normal launch)
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let stdin_fd = std::io::stdin().as_raw_fd();
        let is_tty = unsafe { libc::isatty(stdin_fd) } != 0;
        if is_tty {
            log_msg(&log, "MCP: stdin is terminal, waiting for pipe connection...".into());
            // Block on stdin — if someone pipes data later, we'll process it
            // If the app exits, this thread exits too
        }
    }

    log_msg(&log, "MCP: Server thread started, listening on stdin".into());

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = std::io::BufReader::new(stdin.lock());

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => {
                log_msg(&log, "MCP: stdin closed".into());
                break;
            }
        };
        if line.trim().is_empty() { continue; }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                write_jsonrpc_error(&mut stdout, Value::Null, -32700, "Parse error");
                continue;
            }
        };

        // Preserve the request id in case the handler panics — we still need
        // to send a JSON-RPC error response with the right id.
        let id_for_panic = request.get("id").cloned().unwrap_or(Value::Null);

        // Top-level panic recovery: a malformed request, internal `unwrap`,
        // or tool-implementation bug must not kill the MCP thread (it would
        // silently break the Claude Desktop integration). Catch panics, send
        // a JSON-RPC error response, log the panic, and continue serving the
        // next request.
        let handler_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_mcp_request(request, &mut stdout, &command_tx, &log)
        }));

        let should_break = match handler_result {
            Ok(brk) => brk,
            Err(payload) => {
                let msg = panic_payload_to_string(&*payload);
                log_msg(&log, format!("MCP: handler panicked, recovering — {}", msg));
                write_jsonrpc_error(
                    &mut stdout,
                    id_for_panic,
                    -32603,
                    &format!("Internal error: handler panic ({})", msg),
                );
                false
            }
        };

        if should_break { break; }
    }
}

fn parse_tool_call(name: &str, args: &Value) -> Result<McpCommand, String> {
    match name {
        "list_node_types" => {
            let category = args.get("category").and_then(|v| v.as_str()).map(String::from);
            let name_filter = args.get("name_filter").and_then(|v| v.as_str()).map(String::from);
            let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(McpCommand::ListNodeTypes { category, name_filter, full })
        }

        "describe_node_type" => {
            let name = args.get("name").and_then(|v| v.as_str())
                .ok_or("Missing 'name' parameter")?.to_string();
            Ok(McpCommand::DescribeNodeType { name })
        }

        "create_node" => {
            let type_name = args.get("type").and_then(|v| v.as_str())
                .ok_or("Missing 'type' parameter")?.to_string();
            let position = args.get("position")
                .and_then(|v| v.as_array())
                .map(|a| [
                    a.first().and_then(|v| v.as_f64()).unwrap_or(200.0) as f32,
                    a.get(1).and_then(|v| v.as_f64()).unwrap_or(200.0) as f32,
                ])
                .unwrap_or([200.0, 200.0]);
            let properties = args.get("properties").cloned();
            Ok(McpCommand::CreateNode { type_name, position, properties })
        }

        "delete_node" => {
            let node_id = args.get("node_id").and_then(|v| v.as_u64())
                .ok_or("Missing 'node_id'")?;
            Ok(McpCommand::DeleteNode { node_id })
        }

        "list_nodes" => Ok(McpCommand::ListNodes),

        "get_node" => {
            let node_id = args.get("node_id").and_then(|v| v.as_u64())
                .ok_or("Missing 'node_id'")?;
            Ok(McpCommand::GetNode { node_id })
        }

        "update_node" => {
            let node_id = args.get("node_id").and_then(|v| v.as_u64())
                .ok_or("Missing 'node_id'")?;
            let properties = args.get("properties").cloned()
                .ok_or("Missing 'properties'")?;
            Ok(McpCommand::UpdateNode { node_id, properties })
        }

        "connect" => {
            let from_node = args.get("from_node").and_then(|v| v.as_u64()).ok_or("Missing 'from_node'")?;
            let from_port = args.get("from_port").and_then(|v| v.as_u64()).ok_or("Missing 'from_port'")? as usize;
            let to_node = args.get("to_node").and_then(|v| v.as_u64()).ok_or("Missing 'to_node'")?;
            let to_port = args.get("to_port").and_then(|v| v.as_u64()).ok_or("Missing 'to_port'")? as usize;
            Ok(McpCommand::Connect { from_node, from_port, to_node, to_port })
        }

        "disconnect" => {
            let from_node = args.get("from_node").and_then(|v| v.as_u64()).ok_or("Missing 'from_node'")?;
            let from_port = args.get("from_port").and_then(|v| v.as_u64()).ok_or("Missing 'from_port'")? as usize;
            let to_node = args.get("to_node").and_then(|v| v.as_u64()).ok_or("Missing 'to_node'")?;
            let to_port = args.get("to_port").and_then(|v| v.as_u64()).ok_or("Missing 'to_port'")? as usize;
            Ok(McpCommand::Disconnect { from_node, from_port, to_node, to_port })
        }

        "list_connections" => Ok(McpCommand::ListConnections),

        "get_port_values" => {
            let node_id = args.get("node_id").and_then(|v| v.as_u64());
            Ok(McpCommand::GetPortValues { node_id })
        }

        "save_project" => {
            let path = args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path'")?.to_string();
            Ok(McpCommand::SaveProject { path })
        }

        "load_project" => {
            let path = args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path'")?.to_string();
            Ok(McpCommand::LoadProject { path })
        }

        "create_workflow" => {
            let nodes: Vec<WorkflowNode> = args.get("nodes")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .ok_or("Missing or invalid 'nodes' array")?;
            let connections: Vec<WorkflowConn> = args.get("connections")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            Ok(McpCommand::CreateWorkflow { nodes, connections })
        }

        "trigger_node" => {
            let node_id = args.get("node_id").and_then(|v| v.as_u64())
                .ok_or("Missing 'node_id'")?;
            let action = args.get("action").and_then(|v| v.as_str())
                .ok_or("Missing 'action'")?.to_string();
            Ok(McpCommand::TriggerNode { node_id, action })
        }

        _ => Err(format!("Unknown tool: {}", name)),
    }
}

// ── JSON-RPC Helpers ─────────────────────────────────────────────────────────

fn write_json(stdout: &mut std::io::Stdout, value: &Value) {
    let s = serde_json::to_string(value).unwrap_or_default();
    let _ = writeln!(stdout, "{}", s);
    let _ = stdout.flush();
}

fn write_jsonrpc_error(stdout: &mut std::io::Stdout, id: Value, code: i32, message: &str) {
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    });
    write_json(stdout, &response);
}
