use super::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Clip a long error body (HTTP response, backend message, …) so the node
/// tooltip and Console line stay readable. Keeps the first ~160 chars.
fn truncate_for_error(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= 160 {
        trimmed.to_string()
    } else {
        let mut end = 160;
        while !trimmed.is_char_boundary(end) && end > 0 { end -= 1; }
        format!("{}…", &trimmed[..end])
    }
}

/// Convert an absolute asset path to relative (relative to project directory).
fn make_relative(abs_path: &str, project_dir: &str) -> String {
    if abs_path.is_empty() { return String::new(); }
    let abs = Path::new(abs_path);
    let dir = Path::new(project_dir);
    if let Ok(rel) = abs.strip_prefix(dir) {
        rel.display().to_string()
    } else {
        // Not under project dir — keep absolute
        abs_path.to_string()
    }
}

/// Convert a relative asset path to absolute (resolved against project directory).
fn make_absolute(rel_path: &str, project_dir: &str) -> String {
    if rel_path.is_empty() { return String::new(); }
    let p = Path::new(rel_path);
    if p.is_absolute() {
        return rel_path.to_string(); // already absolute
    }
    let dir = Path::new(project_dir);
    dir.join(p).display().to_string()
}

/// Convert all asset paths in a graph to relative (for saving).
fn relativize_paths(graph: &mut Graph, project_dir: &str) {
    for node in graph.nodes.values_mut() {
        match &mut node.node_type {
            NodeType::ImageNode { path, save_path, .. } => {
                *path = make_relative(path, project_dir);
                *save_path = make_relative(save_path, project_dir);
            }
            NodeType::AudioPlayer { file_path, .. } => {
                *file_path = make_relative(file_path, project_dir);
            }
            NodeType::VideoPlayer { path, .. } => {
                *path = make_relative(path, project_dir);
            }
            NodeType::ClapPlugin { plugin_path, .. } => {
                *plugin_path = make_relative(plugin_path, project_dir);
            }
            NodeType::MlModel { model_path, labels_path, .. } => {
                *model_path = make_relative(model_path, project_dir);
                *labels_path = make_relative(labels_path, project_dir);
            }
            _ => {}
        }
    }
}

/// Convert all asset paths in a graph to absolute (after loading).
fn absolutize_paths(graph: &mut Graph, project_dir: &str) {
    for node in graph.nodes.values_mut() {
        match &mut node.node_type {
            NodeType::ImageNode { path, save_path, .. } => {
                *path = make_absolute(path, project_dir);
                *save_path = make_absolute(save_path, project_dir);
            }
            NodeType::AudioPlayer { file_path, .. } => {
                *file_path = make_absolute(file_path, project_dir);
            }
            NodeType::VideoPlayer { path, .. } => {
                *path = make_absolute(path, project_dir);
            }
            NodeType::ClapPlugin { plugin_path, .. } => {
                *plugin_path = make_absolute(plugin_path, project_dir);
            }
            NodeType::MlModel { model_path, labels_path, .. } => {
                *model_path = make_absolute(model_path, project_dir);
                *labels_path = make_absolute(labels_path, project_dir);
            }
            _ => {}
        }
    }
}

/// Project file format — includes graph, pinned nodes, and viewport state.
/// Backward-compatible: old project.json files (raw Graph) are detected and loaded.
#[derive(Serialize, Deserialize)]
struct ProjectFile {
    graph: Graph,
    #[serde(default)]
    pinned_nodes: Vec<NodeId>,
    #[serde(default)]
    canvas_offset: [f32; 2],
    #[serde(default = "default_one_f32")]
    canvas_zoom: f32,
}

fn default_one_f32() -> f32 { 1.0 }

/// Minimal session state that gets auto-saved on close and restored on launch.
#[derive(Serialize, Deserialize)]
struct SessionState {
    graph: Graph,
    canvas_offset: [f32; 2],
    canvas_zoom: f32,
    pinned_nodes: Vec<NodeId>,
    #[serde(default)]
    project_path: Option<String>,
    #[serde(default)]
    api_keys: HashMap<String, String>,
    /// Persisted across launches so the user gets the same `*` indicator
    /// on relaunch as they had on quit (e.g. quit-with-Don't-Save).
    #[serde(default)]
    is_dirty: bool,
    #[serde(default)]
    recent_projects: Vec<String>,
}

fn session_path() -> std::path::PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".patchwork");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("last_session.json")
}

impl super::PatchworkApp {
    /// Save current session state to ~/.patchwork/last_session.json
    pub fn save_session(&self) {
        let state = SessionState {
            graph: self.graph.clone(),
            canvas_offset: [self.canvas_offset.x, self.canvas_offset.y],
            canvas_zoom: self.canvas_zoom,
            pinned_nodes: self.pinned_nodes.iter().copied().collect(),
            project_path: self.project_path.clone(),
            api_keys: self.api_keys.clone(),
            is_dirty: self.is_dirty,
            recent_projects: self.recent_projects.clone(),
        };
        if let Ok(json) = serde_json::to_string_pretty(&state) {
            let _ = std::fs::write(session_path(), json);
        }
    }

    /// Try to restore session from ~/.patchwork/last_session.json.
    /// Returns true if session was successfully restored.
    pub fn restore_session(&mut self) -> bool {
        let path = session_path();
        let json = match std::fs::read_to_string(&path) {
            Ok(j) => j,
            Err(_) => return false,
        };
        let state: SessionState = match serde_json::from_str(&json) {
            Ok(s) => s,
            Err(e) => {
                crate::system_log::warn(format!("Failed to restore session: {}", e));
                return false;
            }
        };
        let mut graph = state.graph;
        graph.fix_next_id();
        // Always start with DSP, Camera, Mic off for safety
        for node in graph.nodes.values_mut() {
            match &mut node.node_type {
                NodeType::AudioDevice { enabled, .. } => { *enabled = false; }
                NodeType::Camera { active, .. } => { *active = false; }
                NodeType::AudioInput { active, .. } => { *active = false; }
                NodeType::VideoPlayer { playing, .. } => { *playing = false; }
                _ => {}
            }
        }
        self.graph = graph;
        self.graph.audio_topology_dirty = true;
        self.canvas_offset = egui::Vec2::new(state.canvas_offset[0], state.canvas_offset[1]);
        self.canvas_zoom = state.canvas_zoom;
        self.pinned_nodes = state.pinned_nodes.into_iter().collect();
        self.project_path = state.project_path;
        self.api_keys = state.api_keys;
        self.is_dirty = state.is_dirty;
        self.recent_projects = state.recent_projects;
        self.port_positions.clear();
        self.node_rects.clear();
        self.undo_history.clear();
        // See `load_project` — same node-id-collision concern after
        // a session restore. Cleared on the next frame in update().
        self.caches_dirty = true;
        true
    }

    /// Wipe the GPU texture cache and all egui per-node temp data.
    /// Called from `update()` when `caches_dirty` is set (load,
    /// restore, undo, redo, delete). The new project's nodes get
    /// node ids starting from 1 (`fix_next_id`), and several caches
    /// are keyed by `(node_id, port)` or `Id::new(("xxx", node_id))`,
    /// so without an explicit wipe a fresh node could pick up the
    /// previous project's image / GPU handle / pending HTTP fetch.
    ///
    /// We use the heavy hammer (`d.clear()`) instead of removing each
    /// well-known key by name because every cache stores a different
    /// concrete type, and `egui::IdTypeMap::remove::<T>` is type-erased
    /// — there's no way to enumerate without knowing every type. The
    /// next frame just rebuilds whatever it needs.
    pub(super) fn clear_caches_if_dirty(&mut self, ctx: &egui::Context) {
        if !self.caches_dirty { return; }
        self.caches_dirty = false;
        self.gpu_tex_cache.clear_all(self.wgpu_render_state.as_ref());
        ctx.data_mut(|d| d.clear());
        // Theme settings rely on `theme_accent` in temp data; re-emit
        // it immediately so the very next render doesn't flash a
        // default-color frame before `apply_theme` runs again.
        self.apply_theme(ctx);
    }
    pub(super) fn handle_file_drop(&mut self, ctx: &egui::Context) {
        // Capture pointer position BEFORE processing drops (macOS clears it during drop)
        let drop_pos = ctx.input(|i| {
            // Try hover_pos first (most reliable during drag-over), then pointer
            i.pointer.hover_pos()
                .or_else(|| i.pointer.latest_pos())
        }).unwrap_or(egui::pos2(300.0, 300.0));

        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() { self.push_undo(); }
        let image_exts = ["png", "jpg", "jpeg", "gif", "bmp", "webp"];
        let video_exts = ["mp4", "mov", "avi", "webm", "mkv"];
        let audio_exts = ["mp3", "wav", "ogg", "flac", "aac", "m4a"];
        let mesh_exts  = ["gltf", "glb"];
        let off_e = self.canvas_offset / self.canvas_zoom;
        let mut new_ids: std::collections::HashSet<crate::graph::NodeId> = std::collections::HashSet::new();
        for (idx, path) in dropped.iter().enumerate() {
            // Stack multiple dropped files vertically from the drop point
            let canvas_x = drop_pos.x - off_e.x;
            let canvas_y = drop_pos.y - off_e.y + (idx as f32 * 40.0);

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            let nid = if audio_exts.contains(&ext.as_str()) {
                self.graph.add_node(NodeType::AudioPlayer {
                    file_path: path.display().to_string(),
                    volume: 1.0,
                    looping: false,
                    duration_secs: 0.0,
                }, [canvas_x, canvas_y])
            } else if image_exts.contains(&ext.as_str()) {
                let image_data = crate::nodes::image_node::load_image_from_path(&path.display().to_string());
                self.graph.add_node(NodeType::ImageNode {
                    path: path.display().to_string(),
                    save_path: String::new(),
                    image_data,
                    preview_size: 150.0,
                    last_save_hash: 0,
                    cached_file: String::new(),
                }, [canvas_x, canvas_y])
            } else if video_exts.contains(&ext.as_str()) {
                self.graph.add_node(NodeType::VideoPlayer {
                    path: path.display().to_string(),
                    playing: false, looping: false,
                    res_w: 640, res_h: 480,
                    current_frame: None,
                    duration: 0.0, speed: 1.0,
                    status: "Loaded".into(),
                }, [canvas_x, canvas_y])
            } else if mesh_exts.contains(&ext.as_str()) {
                // If the drop landed on an existing 3D Shape node, update its
                // custom path in place instead of spawning a fresh node — the
                // user wanted to swap the asset, not duplicate it. Falls
                // through to spawn-new on any miss.
                let target_screen = drop_pos + egui::vec2(0.0, idx as f32 * 40.0);
                let hit_id: Option<NodeId> = self.node_rects.iter()
                    .find(|(_, r)| r.contains(target_screen))
                    .map(|(&id, _)| id);
                let mut updated_existing = false;
                if let Some(id) = hit_id {
                    if let Some(node) = self.graph.nodes.get_mut(&id) {
                        if let NodeType::Dynamic { inner } = &mut node.node_type {
                            let any_mut = &mut *inner.node as &mut dyn std::any::Any;
                            if let Some(s) = any_mut.downcast_mut::<crate::nodes::shape_3d::Shape3DNode>() {
                                s.kind = crate::nodes::shape_3d::ShapeKind::Custom;
                                s.custom_path = path.display().to_string();
                                s.cached_custom = None;
                                s.custom_error = None;
                                updated_existing = true;
                            }
                        }
                    }
                }
                if updated_existing {
                    hit_id.unwrap()
                } else {
                    let node = crate::nodes::shape_3d::Shape3DNode::from_gltf_path(path.display().to_string());
                    self.graph.add_node(
                        NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(node) } },
                        [canvas_x, canvas_y],
                    )
                }
            } else {
                let mut file_node = crate::nodes::file_node::FileNode::default();
                file_node.path = path.display().to_string();
                file_node.load_file();
                self.graph.add_node(NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(file_node) } }, [canvas_x, canvas_y])
            };
            new_ids.insert(nid);
        }
        // Select all dropped nodes as a group (mirrors paste/duplicate flow).
        if !new_ids.is_empty() {
            self.selected_nodes = new_ids;
        }
    }

    pub(super) fn poll_midi_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            if let Some(msg) = self.midi.poll_input(nid) {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    if let NodeType::MidiIn { channel, note, velocity, log, .. } = &mut node.node_type {
                        if msg.len() >= 3 {
                            *channel = msg[0] & 0x0F;
                            let status = msg[0] & 0xF0;
                            match status {
                                0x80 | 0x90 | 0xA0 | 0xB0 => { *note = msg[1]; *velocity = msg[2]; }
                                _ => {}
                            }
                        }
                        log.push(nodes::midi_in::format_midi_message(&msg));
                    }
                }
            }
        }
    }

    pub(super) fn poll_serial_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            let lines = self.serial.poll(nid);
            if !lines.is_empty() {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    if let NodeType::Serial { log, last_line, .. } = &mut node.node_type {
                        for line in lines {
                            *last_line = line.clone();
                            log.push(line);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn poll_osc_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        // Auto-start/stop listeners for MCP-triggered OscIn nodes
        for &nid in &node_ids {
            if let Some(node) = self.graph.nodes.get(&nid) {
                if let NodeType::OscIn { listening, port, .. } = &node.node_type {
                    if *listening && !self.osc.is_listening(nid) && *port > 0 {
                        self.osc.process(vec![crate::osc::OscAction::StartListening { node_id: nid, port: *port }]);
                    } else if !*listening && self.osc.is_listening(nid) {
                        self.osc.process(vec![crate::osc::OscAction::StopListening { node_id: nid }]);
                    }
                }
            }
        }
        for nid in node_ids {
            let messages = self.osc.poll(nid);
            if !messages.is_empty() {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    if let NodeType::OscIn { address_filter, arg_count, last_args, last_args_text, log, discovered, .. } = &mut node.node_type {
                        for msg in messages {
                            // Auto-discover: track unique addresses with their arg counts
                            let preview = msg.args_text.join(", ");
                            if let Some(entry) = discovered.iter_mut().find(|(a, _, _)| *a == msg.address) {
                                entry.1 = msg.args_float.len();
                                entry.2 = preview.clone();
                            } else {
                                discovered.push((msg.address.clone(), msg.args_float.len(), preview.clone()));
                            }

                            // Log ALL messages (before filtering)
                            log.push(format!("{} [{}]", msg.address, msg.args_text.join(", ")));
                            if log.len() > 200 { log.remove(0); }

                            // Address filter: skip if doesn't match
                            if !address_filter.is_empty() && !msg.address.contains(address_filter.as_str()) {
                                continue;
                            }

                            // Update last_args (float) and last_args_text
                            for (i, &val) in msg.args_float.iter().enumerate() {
                                if i < *arg_count {
                                    while last_args.len() <= i { last_args.push(0.0); }
                                    last_args[i] = val;
                                }
                            }
                            *last_args_text = msg.args_text.clone();
                        }
                    }
                }
            }
        }
    }

    /// Drain the manager's per-DmxIn-node receiver and stash the latest
    /// universe slice on the node so `evaluate` can surface it as port
    /// outputs. Mirrors `poll_osc_inputs` shape.
    pub(super) fn poll_dmx_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            let frames = self.dmx.poll_input(nid);
            if frames.is_empty() { continue; }
            // Use the most recent frame — DMX is sampled, old frames stale.
            if let Some(latest) = frames.into_iter().last() {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    if let NodeType::DmxIn { last_values, channel_count, start_at, .. } = &mut node.node_type {
                        let start_idx = (*start_at as usize).saturating_sub(1).min(511);
                        last_values.resize(*channel_count, 0);
                        for i in 0..*channel_count {
                            let ch = start_idx + i;
                            last_values[i] = latest.data.get(ch).copied().unwrap_or(0);
                        }
                    }
                }
            }
        }
    }

    /// Same as poll_dmx_inputs, but for Art-Net listener threads. Drops
    /// frames whose universe doesn't match when the filter is on.
    pub(super) fn poll_artnet_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            let frames = self.artnet.poll(nid);
            if frames.is_empty() { continue; }
            if let Some(node) = self.graph.nodes.get_mut(&nid) {
                if let NodeType::ArtNetIn { last_values, channel_count, start_at, universe, universe_filter, .. } = &mut node.node_type {
                    let want_univ = *universe;
                    let filter_on = *universe_filter;
                    // Pick the most recent matching frame.
                    let latest = frames.into_iter()
                        .filter(|f| !filter_on || f.universe == want_univ)
                        .last();
                    if let Some(latest) = latest {
                        let start_idx = (*start_at as usize).saturating_sub(1).min(511);
                        last_values.resize(*channel_count, 0);
                        for i in 0..*channel_count {
                            let ch = start_idx + i;
                            last_values[i] = latest.data.get(ch).copied().unwrap_or(0);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn poll_network_events(&mut self) {
        let events = self.network.poll();
        for event in events {
            match event {
                crate::network::NetworkEvent::LinkReady { node_id, link, secret_hex } => {
                    if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                        if let NodeType::NetworkSend { last_link, persistent_secret, identity_mode, .. } = &mut node.node_type {
                            *last_link = Some(link);
                            if *identity_mode == 1 && !secret_hex.is_empty() {
                                *persistent_secret = Some(secret_hex);
                            }
                        }
                    }
                }
                crate::network::NetworkEvent::SendStatus { node_id, peers } => {
                    if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                        if let NodeType::NetworkSend { last_peer_count, .. } = &mut node.node_type {
                            *last_peer_count = peers;
                        }
                    }
                }
                crate::network::NetworkEvent::Received { node_id, values, sender_short_id } => {
                    if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                        if let NodeType::NetworkReceive { last_values, last_values_text, last_sender, status, connected, imported_schema, .. } = &mut node.node_type {
                            // Update values from received data
                            last_values.clear();
                            last_values_text.clear();
                            for val in values.iter() {
                                match val {
                                    crate::network::NetPort::Float(f) => {
                                        last_values.push(*f);
                                        last_values_text.push(format!("{:.4}", f));
                                    }
                                    crate::network::NetPort::Text(t) => {
                                        last_values.push(0.0);
                                        last_values_text.push(t.clone());
                                    }
                                    crate::network::NetPort::Image { .. } => {
                                        last_values.push(0.0);
                                        last_values_text.push("<image>".into());
                                    }
                                    crate::network::NetPort::None => {
                                        last_values.push(0.0);
                                        last_values_text.push(String::new());
                                    }
                                }
                            }
                            // Format sender short ID
                            *last_sender = sender_short_id.iter().map(|b| format!("{:02x}", b)).collect::<String>();
                            *connected = true;
                            let port_count = imported_schema.len();
                            *status = format!("Connected ({} ports)", port_count);
                        }
                    }
                }
                crate::network::NetworkEvent::Error { node_id, error } => {
                    if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                        match &mut node.node_type {
                            NodeType::NetworkSend { .. } => {
                                eprintln!("Network send error for node {}: {}", node_id, error);
                            }
                            NodeType::NetworkReceive { status, connected, .. } => {
                                *status = format!("Error: {}", error);
                                *connected = false;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// Consume a pending gear-icon click from an AI generator node.
    ///
    /// The generator writes `(requester_node_id, ai_config_output_port)`
    /// into `egui::Memory` under the key `"ai_gear_click"`. This method
    /// drains the key each frame and:
    ///
    /// 1. If the generator's `Config` input is already wired, does nothing
    ///    (the wire already points to an AiConfig — a future iteration
    ///    will pan/flash that config).
    /// 2. Else if any `AiConfigNode` exists on the graph, creates a
    ///    connection from its matching output port → generator's Config input.
    /// 3. Else spawns a fresh `AiConfigNode` next to the generator and
    ///    wires it up.
    pub(super) fn process_ai_gear_click(&mut self, ctx: &egui::Context) {
        let key = egui::Id::new("ai_gear_click");
        let pending: Option<(NodeId, usize)> =
            ctx.data_mut(|d| d.remove_temp::<(NodeId, usize)>(key));
        let Some((requester, out_port)) = pending else { return; };

        let Some(req_node) = self.graph.nodes.get(&requester) else { return; };
        let req_pos = req_node.pos;

        let already_wired = self
            .graph
            .connections
            .iter()
            .any(|c| c.to_node == requester && c.to_port == 0);
        if already_wired {
            // TODO(gear-pan): find the upstream AiConfig and pan canvas to it + flash.
            return;
        }

        // Look for an existing AiConfig on the graph.
        let existing = self.graph.nodes.iter().find_map(|(nid, n)| {
            if let NodeType::Dynamic { inner } = &n.node_type {
                if inner.node.type_tag() == "ai_config" { return Some(*nid); }
            }
            None
        });

        let config_id = if let Some(id) = existing {
            id
        } else {
            let cfg = NodeType::Dynamic {
                inner: crate::graph::DynNode {
                    node: Box::new(crate::nodes::ai_config_node::AiConfigNode::default()),
                },
            };
            // Place to the left so the wire flows left→right into the generator.
            let pos = [req_pos[0] - 320.0, req_pos[1]];
            self.graph.add_node(cfg, pos)
        };

        // Wire AiConfig's matching output port → requester's Config input (port 0).
        self.graph.add_connection(config_id, out_port, requester, 0);
    }

    pub(super) fn poll_http_responses(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            if let Some(resp) = self.http.poll(nid) {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    match &mut node.node_type {
                        NodeType::HttpRequest { response, status, .. } => {
                            *status = format!("{}", resp.status);
                            *response = resp.body.clone();
                            // Surface non-2xx and network errors as node errors.
                            // status == 0 means the request never reached the server
                            // (DNS, connect, TLS, …); body holds the underlying message.
                            if resp.status == 0 {
                                crate::node_errors::report_for(
                                    nid,
                                    format!("HTTP request failed: {}", truncate_for_error(&resp.body)),
                                );
                            } else if !(200..300).contains(&resp.status) {
                                crate::node_errors::report_for(
                                    nid,
                                    format!("HTTP {}: {}", resp.status, truncate_for_error(&resp.body)),
                                );
                            } else {
                                crate::node_errors::clear(nid);
                            }
                        }
                        NodeType::AiRequest { provider, response, status, .. } => {
                            if resp.status >= 200 && resp.status < 300 {
                                // Auto-detect provider from response if not set
                                let prov = if provider.is_empty() {
                                    if resp.body.contains("\"candidates\"") { "google" }
                                    else if resp.body.contains("\"content\":[{\"type\"") { "anthropic" }
                                    else { "openai" }
                                } else {
                                    provider.as_str()
                                };
                                *response = crate::nodes::ai_request::extract_ai_response(prov, &resp.body);
                                *status = "done".into();
                                crate::node_errors::clear(nid);
                            } else {
                                *response = resp.body.clone();
                                *status = format!("error: {}", resp.status);
                                let label = if resp.status == 0 { "AI request failed".to_string() }
                                            else { format!("AI HTTP {}", resp.status) };
                                crate::node_errors::report_for(
                                    nid,
                                    format!("{}: {}", label, truncate_for_error(&resp.body)),
                                );
                            }
                        }
                        NodeType::Dynamic { inner } => {
                            // Trait-based nodes (TextGen, ImageGen, CodeGen, …)
                            // handle their own response extraction via
                            // `apply_http_response`. Non-HTTP trait nodes have
                            // a no-op default impl, so this is safe for all.
                            inner.node.apply_http_response(resp.body.clone(), resp.status);
                            if !(200..300).contains(&resp.status) {
                                let label = if resp.status == 0 { "Request failed".to_string() }
                                            else { format!("HTTP {}", resp.status) };
                                crate::node_errors::report_for(
                                    nid,
                                    format!("{}: {}", label, truncate_for_error(&resp.body)),
                                );
                            } else {
                                crate::node_errors::clear(nid);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Poll ML inference results and dispatch new requests
    pub(super) fn poll_ml_inference(&mut self, ctx: &egui::Context) {
        // Receive completed results
        while let Ok(result) = self.ml_rx.try_recv() {
            let nid = result.node_id;
            // Mark this node as no longer running so next request can be dispatched
            self.ml_running.remove(&nid);
            if let Some(node) = self.graph.nodes.get_mut(&nid) {
                match &mut node.node_type {
                    NodeType::MlModel { result_text, result_json, annotated_frame, status, .. } => {
                        *result_text = result.result_text;
                        *result_json = result.result_json;
                        *annotated_frame = result.annotated_frame;
                        let s = result.status.clone();
                        *status = s;
                        if status.starts_with("Error") || status.starts_with("error") {
                            crate::node_errors::report_for(nid, format!("ML inference: {}", status));
                        } else {
                            crate::node_errors::clear(nid);
                        }
                    }
                    NodeType::Dynamic { inner, .. } => {
                        // Trait-based ML nodes — route result to the correct
                        // egui temp key so each node type reads its own results.
                        let key = match inner.node.type_tag() {
                            "face_detection" => "face_det_result",
                            "pose_detection" => "pose_det_result",
                            _                => "hand_det_result",
                        };
                        let result_id = egui::Id::new((key, nid));
                        ctx.data_mut(|d| d.insert_temp(result_id, result));
                    }
                    _ => {}
                }
            }
        }

        // Check for new inference requests (stored in egui temp data by ml_model::render).
        // Only dispatch if no inference is already running for this node — prevents
        // thread flooding when inference is slower than the frame rate.
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            if self.ml_running.contains(&nid) {
                // Still running — consume and discard any queued request so it
                // doesn't accumulate across frames.
                let inference_id = egui::Id::new(("ml_inference", nid));
                ctx.data_mut(|d| d.remove::<crate::nodes::ml_model::MlInferenceRequest>(inference_id));
                continue;
            }
            let inference_id = egui::Id::new(("ml_inference", nid));
            if let Some(req) = ctx.data_mut(|d| d.get_temp::<crate::nodes::ml_model::MlInferenceRequest>(inference_id)) {
                ctx.data_mut(|d| d.remove::<crate::nodes::ml_model::MlInferenceRequest>(inference_id));
                self.ml_running.insert(nid);
                let tx = self.ml_tx.clone();
                std::thread::spawn(move || {
                    let result = crate::nodes::ml_model::run_inference(&req);
                    let _ = tx.send(result);
                });
            }
        }
    }

    /// Poll TTS synthesis results and dispatch new requests.
    /// Mirrors poll_ml_inference — uses egui temp data for requests, mpsc for results.
    pub(super) fn poll_tts_synthesis(&mut self, ctx: &egui::Context) {
        // Receive completed results
        while let Ok(result) = self.tts_rx.try_recv() {
            let nid = result.node_id;
            self.audio.tts_running.remove(&nid);
            // Write status + done flag back into the node via egui temp data
            // (the node reads these in render_with_context on the next frame)
            ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new(("tts_result", nid)),
                    (result.status, result.success),
                );
            });
        }

        // Check for Play button presses — replay without re-synthesizing
        let node_ids_play: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids_play {
            let play_id = egui::Id::new(("tts_play", nid));
            if ctx.data_mut(|d| d.get_temp::<bool>(play_id)).is_some() {
                ctx.data_mut(|d| d.remove::<bool>(play_id));
                // Reset buffer read head so audio plays from the beginning
                if let Some(buf) = self.audio.tts_buffers.get(&nid) {
                    buf.replay();
                }
                // Re-send connect command so the engine is wired if it was cleaned up
                let outgoing: Vec<(NodeId, NodeId, usize)> = self.graph.connections.iter()
                    .filter(|c| c.from_node == nid && self.audio.node_params.contains_key(&c.to_node))
                    .map(|c| (c.from_node, c.to_node, c.to_port))
                    .collect();
                for (fn_, tn, tp) in outgoing {
                    let engine_port = if matches!(
                        self.graph.nodes.get(&tn).map(|n| &n.node_type),
                        Some(crate::graph::NodeType::AudioMixer { .. })
                    ) { tp / 2 } else { tp };
                    self.audio.connect_audio(fn_, tn, engine_port);
                }
            }
        }

        // Check for new synthesis requests stored by TtsNode::render_with_context
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            if self.audio.tts_running.contains(&nid) {
                // Still running — consume and discard queued request to avoid accumulation
                let req_id = egui::Id::new(("tts_request", nid));
                ctx.data_mut(|d| d.remove::<crate::nodes::tts_node::TtsSynthRequest>(req_id));
                continue;
            }
            let req_id = egui::Id::new(("tts_request", nid));
            if let Some(req) = ctx.data_mut(|d| d.get_temp::<crate::nodes::tts_node::TtsSynthRequest>(req_id)) {
                ctx.data_mut(|d| d.remove::<crate::nodes::tts_node::TtsSynthRequest>(req_id));
                let tts_tx = self.tts_tx.clone();
                self.audio.start_tts_synthesis(nid, &req, &tts_tx);

                // Immediately wire TTS → downstream audio connections.
                // pending_connection_sync fires 1 frame later; doing it here ensures
                // the engine connection is live before the first audio callback reads.
                let outgoing: Vec<(NodeId, usize, NodeId, usize)> = self.graph.connections.iter()
                    .filter(|c| c.from_node == nid && self.audio.node_params.contains_key(&c.to_node))
                    .map(|c| (c.from_node, c.from_port, c.to_node, c.to_port))
                    .collect();
                for (fn_, _fp, tn, tp) in outgoing {
                    let engine_port = if matches!(
                        self.graph.nodes.get(&tn).map(|n| &n.node_type),
                        Some(crate::graph::NodeType::AudioMixer { .. })
                    ) { tp / 2 } else { tp };
                    self.audio.connect_audio(fn_, tn, engine_port);
                }
            }
        }
    }

    /// Process pending MCP commands from the MCP server thread
    pub(super) fn process_mcp_commands(&mut self, values: &HashMap<(NodeId, usize), PortValue>) {
        let rx = match &self.mcp_rx {
            Some(rx) => rx,
            None => return,
        };
        // Drain all pending requests (non-blocking)
        while let Ok(request) = rx.try_recv() {
            let result = crate::mcp::execute_command(request.command, &mut self.graph, values);
            let _ = request.response_tx.send(result);
        }
    }

    /// Save to existing project path (Cmd+S). Falls back to Save As if no path set.
    pub(super) fn save_project_quick(&mut self) {
        if let Some(ref dir_str) = self.project_path.clone() {
            self.save_to_dir(dir_str.clone());
        } else {
            self.save_project(); // No path yet → show dialog
        }
    }

    /// Save As — prompt for project name, then pick parent folder.
    /// Creates a subfolder with the project name inside the chosen folder.
    pub(super) fn save_project(&mut self) {
        // Step 1: Ask for project name via native dialog
        let default_name = self.project_path.as_ref()
            .and_then(|p| Path::new(p).file_name().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or_else(|| "my-project".to_string());

        // Use save dialog which lets user type a name and pick location
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Save Patchwork Project")
            .set_file_name(&default_name)
            .save_file()
        {
            // The user picks a path — we treat it as the project folder
            // (create it if it doesn't exist)
            let project_dir = if path.extension().is_some() {
                // User typed "name.json" → use parent as project dir
                path.parent().map(|p| p.to_path_buf()).unwrap_or(path.clone())
            } else {
                // User typed a folder name → use as project dir
                path
            };
            let dir_str = project_dir.display().to_string();
            if self.save_to_dir(dir_str.clone()) {
                self.project_path = Some(dir_str);
            }
        }
    }

    /// Write project.json + api_keys.json to the given directory.
    /// Asset paths are temporarily converted to relative for portability, then restored.
    /// Returns true on successful write — callers use this to update dirty/recent state.
    fn save_to_dir(&mut self, dir_str: String) -> bool {
        let dir = Path::new(&dir_str);
        if let Err(e) = std::fs::create_dir_all(dir) {
            crate::system_log::error(format!("Failed to create project folder: {}", e));
            return false;
        }
        // Temporarily relativize paths, serialize, then restore absolute paths
        relativize_paths(&mut self.graph, &dir_str);
        let pf = ProjectFile {
            graph: self.graph.clone(),
            pinned_nodes: self.pinned_nodes.iter().copied().collect(),
            canvas_offset: [self.canvas_offset.x, self.canvas_offset.y],
            canvas_zoom: self.canvas_zoom,
        };
        let project_file = dir.join("project.json");
        let mut ok = false;
        match serde_json::to_string_pretty(&pf) {
            Ok(json) => {
                if json.len() < 10 {
                    crate::system_log::error("Save produced empty JSON — skipping write".to_string());
                } else if let Err(e) = std::fs::write(&project_file, &json) {
                    crate::system_log::error(format!("Save failed: {}", e));
                } else {
                    crate::system_log::log(format!("Saved to {}", project_file.display()));
                    ok = true;
                }
            }
            Err(e) => {
                crate::system_log::error(format!("Serialization failed: {}", e));
            }
        }
        // Restore absolute paths so the running app continues working
        absolutize_paths(&mut self.graph, &dir_str);
        if !self.api_keys.is_empty() {
            let keys_file = dir.join("api_keys.json");
            let keys_json = serde_json::to_string_pretty(&self.api_keys).unwrap_or_default();
            let _ = std::fs::write(&keys_file, keys_json);
        }
        if ok {
            self.is_dirty = false;
            self.add_recent(dir_str);
        }
        ok
    }

    pub(super) fn load_project(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Patchwork", &["json"]).pick_file() {
            self.load_project_path(path);
        }
    }

    /// Load a project given a path to either `project.json` or its parent
    /// directory. Used by both the file dialog and the Open Recent submenu.
    pub(super) fn load_project_path(&mut self, path: std::path::PathBuf) {
        // Resolve to the project.json file. Recent entries are stored as
        // dirs; the dialog returns a file. Handle both shapes.
        let json_path = if path.is_dir() {
            path.join("project.json")
        } else {
            path
        };
        let dir = json_path.parent().map(|p| p.to_path_buf());
        let dir_str = dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();

        if !json_path.exists() {
            crate::system_log::error(format!("Project not found: {}", json_path.display()));
            // Drop the broken entry from recents so the user doesn't keep
            // clicking a dead link.
            self.recent_projects.retain(|p| p != &dir_str);
            return;
        }

        let json = match std::fs::read_to_string(&json_path) {
            Ok(j) => j,
            Err(e) => {
                crate::system_log::error(format!("Load failed: {}", e));
                return;
            }
        };
        let pf: ProjectFile = match serde_json::from_str(&json) {
            Ok(pf) => pf,
            Err(e) => {
                crate::system_log::error(format!("Load failed: {}", e));
                return;
            }
        };
        let mut graph = pf.graph;
        graph.fix_next_id();
        absolutize_paths(&mut graph, &dir_str);
        // Always start with DSP, Camera, Mic off for safety —
        // prevents stale "active" state without actual streams running
        for node in graph.nodes.values_mut() {
            match &mut node.node_type {
                NodeType::AudioDevice { enabled, .. } => { *enabled = false; }
                NodeType::Camera { active, .. } => { *active = false; }
                NodeType::AudioInput { active, .. } => { *active = false; }
                NodeType::VideoPlayer { playing, .. } => { *playing = false; }
                _ => {}
            }
        }
        self.audio.stop_output();
        self.graph = graph;
        self.graph.audio_topology_dirty = true;
        // Restore UI state from project file
        self.pinned_nodes = pf.pinned_nodes.into_iter().collect();
        self.canvas_offset = egui::Vec2::new(pf.canvas_offset[0], pf.canvas_offset[1]);
        self.canvas_zoom = pf.canvas_zoom;
        self.target_zoom = pf.canvas_zoom;
        // Clear transient state
        self.port_positions.clear();
        self.node_rects.clear();
        self.undo_history.clear();
        self.selected_nodes.clear();
        self.selected_connection = None;
        self.wire_menu_conn = None;
        self.dragging_from = None;
        self.show_node_menu = false;
        self.show_context_menu = false;
        // The previous project's GPU textures and per-node
        // egui temp data must be wiped before any node from
        // the new project (whose ids restart at 1 after
        // `fix_next_id`) reads them back. Done at the top
        // of the next `update()` where ctx is in scope.
        self.caches_dirty = true;
        crate::system_log::log(format!("Loaded {}", json_path.display()));

        // Load api_keys from the same folder
        if let Some(dir) = &dir {
            let keys_file = dir.join("api_keys.json");
            if let Ok(json) = std::fs::read_to_string(&keys_file) {
                if let Ok(keys) = serde_json::from_str::<HashMap<String, String>>(&json) {
                    self.api_keys = keys;
                }
            }
        }
        self.project_path = Some(dir_str.clone());
        self.is_dirty = false;
        self.add_recent(dir_str);
    }

    /// Push a project directory to the front of the recents list,
    /// dedup, and cap at 10 entries.
    pub(super) fn add_recent(&mut self, dir: String) {
        if dir.is_empty() { return; }
        self.recent_projects.retain(|p| p != &dir);
        self.recent_projects.insert(0, dir);
        self.recent_projects.truncate(10);
    }

    /// Push a `Title(...)` viewport command when the displayed title
    /// changes. Format is "PatchWork — {project} [*]"; the `*` shows
    /// when there are unsaved changes.
    pub(super) fn update_window_title(&mut self, ctx: &egui::Context) {
        let name = self.project_path.as_deref()
            .and_then(|p| Path::new(p).file_name())
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string());
        let title = if self.is_dirty {
            format!("PatchWork — {} *", name)
        } else {
            format!("PatchWork — {}", name)
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    /// If the OS asks the window to close and we have unsaved changes,
    /// cancel the close and pop the confirmation modal. Once the user
    /// has chosen Save / Don't Save, `force_quit` lets the next request
    /// through.
    pub(super) fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) { return; }
        if self.force_quit || !self.is_dirty { return; }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.pending_quit_dialog = true;
    }

    /// Render the "Save changes before quitting?" modal when pending.
    /// Save → save_project_quick (may show a dialog), then close on success.
    /// Don't Save → close, dropping the in-memory edits.
    /// Cancel → just dismiss the modal.
    pub(super) fn render_quit_dialog(&mut self, ctx: &egui::Context) {
        if !self.pending_quit_dialog { return; }
        let mut dismiss = false;
        let mut do_close = false;
        let mut do_save = false;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let name = self.project_path.as_deref()
                    .and_then(|p| Path::new(p).file_name())
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| "this project".to_string());
                ui.label(format!("Save changes to {} before quitting?", name));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() { do_save = true; }
                    if ui.button("Don't Save").clicked() { do_close = true; }
                    if ui.button("Cancel").clicked() { dismiss = true; }
                });
            });
        if do_save {
            self.save_project_quick();
            // save_project_quick may have shown a dialog the user
            // cancelled; only proceed to close if the dirty flag actually
            // cleared.
            if !self.is_dirty {
                self.pending_quit_dialog = false;
                self.force_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else if do_close {
            self.pending_quit_dialog = false;
            self.force_quit = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if dismiss {
            self.pending_quit_dialog = false;
        }
    }

    #[allow(dead_code)]
    pub(super) fn project_dir(&self) -> Option<std::path::PathBuf> {
        self.project_path.as_ref().map(|p| std::path::PathBuf::from(p))
    }

    #[allow(dead_code)]
    pub(super) fn load_api_keys(&mut self) {
        if let Some(dir) = self.project_dir() {
            let keys_file = dir.join("api_keys.json");
            if let Ok(json) = std::fs::read_to_string(&keys_file) {
                if let Ok(keys) = serde_json::from_str::<HashMap<String, String>>(&json) {
                    self.api_keys = keys;
                }
            }
        }
    }

    pub(super) fn sync_console_messages(&mut self) {
        for node in self.graph.nodes.values_mut() {
            if let NodeType::Console { messages } = &mut node.node_type {
                *messages = self.console_messages.clone();
            }
        }
    }

    pub(super) fn apply_theme(&self, ctx: &egui::Context) {
        for node in self.graph.nodes.values() {
            if let NodeType::Theme { dark_mode, accent, font_size, bg_color, text_color, window_bg, window_alpha, grid_color: _, rounding, spacing, .. } = &node.node_type {
                // Publish theme colors to temp data so anywhere with a
                // ctx can read them without crawling the graph. Read via
                // `crate::nodes::theme::current_*(ctx)` helpers.
                ctx.data_mut(|d| {
                    d.insert_temp(egui::Id::new("theme_accent"), *accent);
                    d.insert_temp(egui::Id::new("theme_text"), *text_color);
                    d.insert_temp(egui::Id::new("theme_bg"), *bg_color);
                });
                nodes::theme::apply(ctx, *dark_mode, *accent, *font_size, *bg_color, *text_color, *window_bg, *window_alpha, *rounding, *spacing);
                return;
            }
        }
        // No Theme node found — apply default Patchwork theme so the app
        // looks correct from the first frame without requiring a Theme node.
        // Each session gets a random accent hue for visual variety.
        let accent = self.session_accent;
        let bg_color: [u8; 3] = [20, 20, 20];
        let text_color: [u8; 3] = [220, 220, 220];
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("theme_accent"), accent);
            d.insert_temp(egui::Id::new("theme_text"), text_color);
            d.insert_temp(egui::Id::new("theme_bg"), bg_color);
        });
        nodes::theme::apply(
            ctx,
            true,                   // dark_mode
            accent,                 // accent
            14.0,                   // font_size
            bg_color,               // bg_color
            text_color,             // text_color
            [24, 24, 24],           // window_bg
            240,                    // window_alpha
            16.0,                   // rounding
            4.0,                    // spacing
        );
    }

    #[allow(dead_code)]
    pub(super) fn log_message(&mut self, msg: String) {
        self.console_messages.push(msg);
        if self.console_messages.len() > 200 {
            self.console_messages.remove(0);
        }
    }

    pub(super) fn update_mouse_trackers(&mut self, _ctx: &egui::Context) {
        // MouseTracker is now trait-based — reads pointer position in render_ui()
    }

    pub(super) fn update_key_inputs(&mut self, _ctx: &egui::Context) {
        // KeyInput is now trait-based — reads key state in render_ui()
    }
}
