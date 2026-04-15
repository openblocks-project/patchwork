use eframe::egui::{self, RichText};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── WebSocket Node ───────────────────────────────────────────────────────────
//
// Generic WebSocket server that broadcasts Patchwork values to any connected
// client (Unity, Processing, Python, browser, Max/MSP, etc.).
//
// Opens ws://127.0.0.1:{PORT} — clients connect and receive JSON frames
// at ~60fps with all input port values.
//
// Ports:
//   In  0..N: Named numeric inputs (user adds/removes)
//   Out 0: URL (Text — ws:// URL)
//   Out 1: Clients (Number — connected client count)

struct WsServerState {
    port: u16,
    is_running: bool,
    input_values: Vec<f32>,
    input_names: Vec<String>,
    client_count: usize,
}

type SharedWsState = Arc<Mutex<WsServerState>>;

fn start_ws_server(state: SharedWsState) -> u16 {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => { crate::system_log::error(format!("WS server bind failed: {}", e)); return 0; }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    if let Ok(mut s) = state.lock() { s.port = port; s.is_running = true; }

    // Accept connections in a dedicated thread
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let state = state.clone();

            // Each client gets its own thread
            std::thread::spawn(move || {
                // Upgrade HTTP → WebSocket
                let mut ws = match tungstenite::accept(stream) {
                    Ok(ws) => ws,
                    Err(_) => return,
                };

                // Track client
                if let Ok(mut s) = state.lock() { s.client_count += 1; }

                // Send loop: push data at ~60fps
                loop {
                    let json = if let Ok(s) = state.lock() {
                        if !s.is_running { break; }
                        let pairs: Vec<String> = s.input_names.iter().zip(s.input_values.iter())
                            .map(|(name, val)| format!("\"{}\":{:.6}", name, val))
                            .collect();
                        format!("{{{}}}", pairs.join(","))
                    } else { break };

                    match ws.send(tungstenite::Message::Text(json)) {
                        Ok(_) => {}
                        Err(_) => break, // client disconnected
                    }

                    // Also drain any incoming messages (keep connection alive)
                    // Non-blocking read — if no message, continue
                    match ws.read() {
                        Ok(tungstenite::Message::Close(_)) => break,
                        Ok(tungstenite::Message::Ping(d)) => { let _ = ws.send(tungstenite::Message::Pong(d)); }
                        Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => break,
                        _ => {}
                    }

                    std::thread::sleep(std::time::Duration::from_millis(16)); // ~60fps
                }

                // Client disconnected
                if let Ok(mut s) = state.lock() { s.client_count = s.client_count.saturating_sub(1); }
            });
        }
    });
    port
}

// ── Node ──────────────────────────────────────────────────────────────────────

pub struct WebSocketNode {
    pub input_names: Vec<String>,
    server_port: u16,
    server_state: Option<SharedWsState>,
    input_values: Vec<f32>,
}

impl std::fmt::Debug for WebSocketNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSocketNode").field("port", &self.server_port).finish()
    }
}

impl Clone for WebSocketNode {
    fn clone(&self) -> Self {
        Self {
            input_names: self.input_names.clone(),
            server_port: 0,
            server_state: None,
            input_values: self.input_values.clone(),
        }
    }
}

impl Default for WebSocketNode {
    fn default() -> Self {
        Self {
            input_names: vec!["x".into(), "y".into(), "z".into()],
            server_port: 0,
            server_state: None,
            input_values: vec![0.0; 3],
        }
    }
}

impl NodeBehavior for WebSocketNode {
    fn title(&self)      -> &str   { "WebSocket" }
    fn type_tag(&self)   -> &str   { "websocket" }
    fn color_hint(&self) -> [u8;3] { [60, 180, 255] }
    fn min_width(&self)  -> Option<f32> { Some(220.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        self.input_names.iter()
            .map(|name| PortDef::dynamic(name.clone(), PortKind::Number))
            .collect()
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("URL",     PortKind::Text),
            PortDef::new("Clients", PortKind::Number),
        ]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        self.input_values.resize(self.input_names.len(), 0.0);
        // Only override from wired ports — preserve manual values when not connected
        for i in 0..self.input_names.len() {
            if let Some(PortValue::Float(f)) = inputs.get(i) {
                self.input_values[i] = *f;
            }
        }

        // Update shared server state
        if let Some(ref state) = self.server_state {
            if let Ok(mut s) = state.lock() {
                s.input_values = self.input_values.clone();
                s.input_names = self.input_names.clone();
            }
        }

        let client_count = self.server_state.as_ref()
            .and_then(|s| s.lock().ok().map(|s| s.client_count))
            .unwrap_or(0);

        let url = if self.server_port > 0 {
            format!("ws://127.0.0.1:{}", self.server_port)
        } else { String::new() };

        vec![
            (0, PortValue::Text(url)),
            (1, PortValue::Float(client_count as f32)),
        ]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim   = egui::Color32::from_rgb(110, 110, 120);
        let green = egui::Color32::from_rgb(80, 200, 120);
        let blue  = egui::Color32::from_rgb(100, 180, 240);

        // ── Lazy server startup ──────────────────────────────────────────
        if self.server_state.is_none() {
            let state = Arc::new(Mutex::new(WsServerState {
                port: 0,
                is_running: true,
                input_values: self.input_values.clone(),
                input_names: self.input_names.clone(),
                client_count: 0,
            }));
            self.server_port = start_ws_server(state.clone());
            self.server_state = Some(state);
        }

        let client_count = self.server_state.as_ref()
            .and_then(|s| s.lock().ok().map(|s| s.client_count))
            .unwrap_or(0);

        // ── Status ───────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            if self.server_port > 0 {
                ui.label(RichText::new("●").color(green));
                ui.label(RichText::new(format!("ws://127.0.0.1:{}", self.server_port))
                    .small().monospace().color(dim));
            } else {
                ui.label(RichText::new("○ Stopped").small().color(dim));
            }
        });
        ui.horizontal(|ui| {
            let client_color = if client_count > 0 { green } else { dim };
            ui.label(RichText::new(format!("{} client{}", client_count, if client_count == 1 { "" } else { "s" }))
                .small().color(client_color));
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Named input ports ────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Inputs").small().strong().color(dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(RichText::new("+").color(green)).clicked() {
                    let idx = self.input_names.len();
                    self.input_names.push(format!("v{}", idx));
                    self.input_values.push(0.0);
                }
            });
        });

        let mut remove_idx: Option<usize> = None;
        for i in 0..self.input_names.len() {
            let port_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == i);
            ui.horizontal(|ui| {
                if self.input_names.len() > 1 {
                    if ui.small_button(RichText::new("−").color(egui::Color32::from_rgb(220, 80, 80))).clicked() {
                        remove_idx = Some(i);
                    }
                }
                crate::nodes::inline_port_circle(ui, node_id, i, true,
                    ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
                ui.add(egui::TextEdit::singleline(&mut self.input_names[i]).desired_width(60.0)
                    .font(egui::TextStyle::Small));
                let val = self.input_values.get(i).copied().unwrap_or(0.0);
                if port_wired {
                    ui.label(RichText::new(format!("{:.2}", val)).small().monospace().color(blue));
                } else {
                    ui.label(RichText::new(format!("{:.2}", val)).small().monospace().color(dim));
                }
            });
        }

        if let Some(idx) = remove_idx {
            self.input_names.remove(idx);
            if idx < self.input_values.len() { self.input_values.remove(idx); }
            for p in idx..=self.input_names.len() {
                ctx.pending_disconnects.push((node_id, p));
            }
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── JSON preview ─────────────────────────────────────────────────
        let pairs: Vec<String> = self.input_names.iter().zip(self.input_values.iter())
            .map(|(name, val)| format!("\"{}\": {:.2}", name, val))
            .collect();
        let preview = format!("{{ {} }}", pairs.join(", "));
        ui.label(RichText::new(&preview).small().monospace().color(dim));

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports ─────────────────────────────────────────────────
        let url_str = if self.server_port > 0 { format!(":{}", self.server_port) } else { "—".into() };
        crate::nodes::output_port_row(ui, "URL", &url_str, node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Text);
        crate::nodes::output_port_row(ui, "Clients", &format!("{}", client_count), node_id, 1,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);

        if client_count > 0 {
            ui.ctx().request_repaint();
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({ "input_names": self.input_names })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(names) = state.get("input_names").and_then(|v| v.as_array()) {
            self.input_names = names.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            self.input_values = vec![0.0; self.input_names.len()];
        }
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("websocket", |state| {
        let mut n = WebSocketNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
