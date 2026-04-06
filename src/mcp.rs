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
    ListNodeTypes,
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
            "description": "List all available node types with their categories, input ports, and output ports",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_node",
            "description": "Create a new node of the specified type at a canvas position",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "Node type name (e.g., 'Slider', 'Synth', 'Add')" },
                    "position": { "type": "array", "items": { "type": "number" }, "description": "[x, y] canvas position, default [200, 200]" },
                    "properties": { "type": "object", "description": "Initial property values (e.g., {\"value\": 0.5} for Slider)" }
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
            "description": "Update properties of an existing node (e.g., set slider value, change synth frequency)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": { "type": "integer" },
                    "properties": { "type": "object", "description": "Properties to update" }
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
            "name": "create_workflow",
            "description": "Create multiple nodes and connections in one atomic operation. Connections use 0-based indices into the nodes array.",
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
        },
        {
            "name": "trigger_node",
            "description": "Trigger an action on a node. Actions: 'send' (HttpRequest, AiRequest), 'play'/'pause'/'stop' (VideoPlayer, AudioPlayer), 'listen'/'stop_listen' (OscIn), 'activate'/'deactivate' (Speaker)",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "node_id": { "type": "integer", "description": "Node ID" },
                    "action": { "type": "string", "description": "Action name: send, play, pause, stop, listen, stop_listen, activate, deactivate" }
                },
                "required": ["node_id", "action"]
            }
        }
    ])
}

// ── Property Merge ───────────────────────────────────────────────────────────

/// Merge JSON properties into a NodeType using serde serialization round-trip
pub fn apply_properties(node_type: &mut NodeType, properties: Value) {
    if let Value::Object(props) = properties {
        if let Ok(mut current) = serde_json::to_value(&*node_type) {
            // NodeType serializes as {"Slider": {"value": 0.5, ...}}
            if let Value::Object(outer) = &mut current {
                if let Some((_, inner)) = outer.iter_mut().next() {
                    if let Value::Object(inner_map) = inner {
                        for (k, v) in props {
                            inner_map.insert(k, v);
                        }
                    }
                }
            }
            if let Ok(updated) = serde_json::from_value::<NodeType>(current) {
                *node_type = updated;
            }
        }
    }
}

// ── Command Execution (called by app.rs) ─────────────────────────────────────

pub fn execute_command(
    cmd: McpCommand,
    graph: &mut Graph,
    values: &HashMap<(NodeId, usize), PortValue>,
) -> McpResult {
    match cmd {
        McpCommand::ListNodeTypes => {
            let catalog = nodes::catalog();
            let types: Vec<Value> = catalog.iter().map(|e| {
                let nt = (e.factory)();
                // Extract property names and default values from serialized NodeType
                let properties = if let Ok(val) = serde_json::to_value(&nt) {
                    if let Value::Object(outer) = val {
                        if let Some((_, inner)) = outer.into_iter().next() {
                            if let Value::Object(fields) = inner {
                                let schema: serde_json::Map<String, Value> = fields.into_iter()
                                    .filter(|(k, _)| {
                                        // Skip internal/runtime fields
                                        !matches!(k.as_str(),
                                            "response" | "status" | "log" | "last_hash" |
                                            "last_args" | "last_args_text" | "discovered" |
                                            "result" | "error" | "result_text" | "last_input_hash" |
                                            "current_frame" | "duration" | "variables" |
                                            "image_data" | "last_save_hash" | "content" |
                                            "messages" | "detected_devices" | "effects" |
                                            "x" | "y" // mouse tracker runtime values
                                        )
                                    })
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
                            } else { json!({}) }
                        } else { json!({}) }
                    } else { json!({}) }
                } else { json!({}) };
                json!({
                    "name": e.label,
                    "category": e.category,
                    "inputs": nt.inputs().iter().map(|p| p.name.as_ref()).collect::<Vec<_>>(),
                    "outputs": nt.outputs().iter().map(|p| p.name.as_ref()).collect::<Vec<_>>(),
                    "properties": properties,
                })
            }).collect();
            McpResult::Json(json!(types))
        }

        McpCommand::CreateNode { type_name, position, properties } => {
            let catalog = nodes::catalog();
            if let Some(entry) = catalog.iter().find(|e| e.label.eq_ignore_ascii_case(&type_name)) {
                let mut nt = (entry.factory)();
                if let Some(props) = properties {
                    apply_properties(&mut nt, props);
                }
                let id = graph.add_node(nt, position);
                McpResult::Json(json!({ "node_id": id }))
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
                apply_properties(&mut node.node_type, properties);
                McpResult::Json(json!({ "success": true }))
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
                _ => McpResult::Error { error: format!(
                    "Action '{}' not supported for node type '{}'", action, node.node_type.title()
                ) },
            }
        }

        McpCommand::CreateWorkflow { nodes: wf_nodes, connections: wf_conns } => {
            let catalog = nodes::catalog();
            let mut created_ids: Vec<NodeId> = Vec::new();

            for (i, wf_node) in wf_nodes.iter().enumerate() {
                let entry = catalog.iter().find(|e| e.label.eq_ignore_ascii_case(&wf_node.node_type));
                if let Some(entry) = entry {
                    let mut nt = (entry.factory)();
                    if let Some(ref props) = wf_node.properties {
                        apply_properties(&mut nt, props.clone());
                    }
                    let pos = wf_node.position.unwrap_or([200.0 + i as f32 * 250.0, 200.0]);
                    let id = graph.add_node(nt, pos);
                    created_ids.push(id);
                } else {
                    return McpResult::Error {
                        error: format!("Unknown node type at index {}: '{}'", i, wf_node.node_type),
                    };
                }
            }

            // Create connections using the resolved IDs
            for wf_conn in &wf_conns {
                if wf_conn.from_index >= created_ids.len() || wf_conn.to_index >= created_ids.len() {
                    continue; // Skip invalid indices
                }
                let from_id = created_ids[wf_conn.from_index];
                let to_id = created_ids[wf_conn.to_index];
                graph.add_connection(from_id, wf_conn.from_port, to_id, wf_conn.to_port);
            }

            McpResult::Json(json!({ "node_ids": created_ids }))
        }
    }
}

// ── MCP Thread (JSON-RPC over stdio) ─────────────────────────────────────────

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
                write_json(&mut stdout, &response);
            }

            "notifications/initialized" => {
                log_msg(&log, "✓ Client initialized".into());
            }

            "tools/list" => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "tools": tool_definitions() }
                });
                write_json(&mut stdout, &response);
            }

            "tools/call" => {
                let tool_name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                log_msg(&log, format!("→ {} {}", tool_name, serde_json::to_string(&arguments).unwrap_or_default()));

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
                        write_json(&mut stdout, &response);
                        continue;
                    }
                };

                // Send command to GUI thread and wait for result
                let (resp_tx, resp_rx) = mpsc::channel();
                let req = McpRequest { command: cmd, response_tx: resp_tx };
                if command_tx.send(req).is_err() {
                    write_jsonrpc_error(&mut stdout, id, -32603, "App disconnected");
                    break;
                }

                // Wait for GUI to process (blocks until next frame)
                match resp_rx.recv() {
                    Ok(result) => {
                        let text = match &result {
                            McpResult::Json(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
                            McpResult::Error { error } => format!("Error: {}", error),
                        };
                        let is_error = matches!(result, McpResult::Error { .. });
                        let short = if text.len() > 80 { format!("{}...", &text[..80]) } else { text.clone() };
                        log_msg(&log, format!("← {}{}", if is_error { "ERR " } else { "" }, short));
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": text }],
                                "isError": is_error
                            }
                        });
                        write_json(&mut stdout, &response);
                    }
                    Err(_) => {
                        write_jsonrpc_error(&mut stdout, id, -32603, "Response timeout");
                    }
                }
            }

            _ => {
                write_jsonrpc_error(&mut stdout, id, -32601, &format!("Unknown method: {}", method));
            }
        }
    }
}

fn parse_tool_call(name: &str, args: &Value) -> Result<McpCommand, String> {
    match name {
        "list_node_types" => Ok(McpCommand::ListNodeTypes),

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
