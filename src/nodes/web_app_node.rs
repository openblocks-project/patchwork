use eframe::egui::{self, RichText};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Web App Node ─────────────────────────────────────────────────────────────
//
// Hosts a local web page with a built-in JavaScript API that receives
// live Patchwork values at ~60fps via /data endpoint polling.
//
// User writes custom HTML/JS. The injected `patchwork.inputs` object
// provides real-time access to all connected input port values.
//
// Ports:
//   In  0: HTML (Text, optional — overrides inline code editor)
//   In  1..N: Named numeric inputs (p0, p1, p2, p3 by default)
//   Out 0: URL (Text — the localhost URL)

struct WebAppServerState {
    html: String,
    html_hash: u64,
    port: u16,
    is_running: bool,
    input_values: Vec<f32>,
    input_names: Vec<String>,
}

type SharedWebAppState = Arc<Mutex<WebAppServerState>>;

fn compute_hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

const PATCHWORK_JS_BRIDGE: &str = r#"
<script>
(function(){
  window.patchwork = { inputs: {}, onUpdate: null, _connected: false, _frameCount: 0 };
  console.log('[Patchwork] Bridge loaded. Use patchwork.inputs or patchwork.onUpdate');
  async function _pwPoll() {
    try {
      const r = await fetch('/data');
      const d = await r.json();
      window.patchwork.inputs = d;
      window.patchwork._connected = true;
      window.patchwork._frameCount++;
      if (window.patchwork._frameCount % 60 === 1) {
        console.log('[Patchwork] inputs:', JSON.stringify(d));
      }
      if (window.patchwork.onUpdate) window.patchwork.onUpdate(d);
    } catch(e) {
      window.patchwork._connected = false;
      if (window.patchwork._frameCount % 300 === 0) {
        console.warn('[Patchwork] Connection lost, retrying...');
      }
    }
    requestAnimationFrame(_pwPoll);
  }
  requestAnimationFrame(_pwPoll);
  let _pwH = '__HASH__';
  async function _pwReload() {
    try {
      const r = await fetch('/hash');
      const h = await r.text();
      if (h !== _pwH) { _pwH = h; console.log('[Patchwork] Content changed, reloading...'); location.reload(); }
    } catch(e) {}
    setTimeout(_pwReload, 1000);
  }
  setTimeout(_pwReload, 2000);
})();
</script>
"#;

const DEFAULT_HTML: &str = r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Patchwork Web App</title>
<style>
  body { margin:0; padding:20px; font-family:monospace; background:#1a1a2e; color:#eee; }
  .val { color:#0ff; font-size:24px; }
  pre { background:#12121a; padding:12px; border-radius:8px; font-size:13px; color:#8f8; }
</style></head>
<body>
<h2>Patchwork Web App</h2>
<div id="vals"></div>
<hr style="border-color:#333">
<p>Use <code>patchwork.onUpdate</code> to receive live data:</p>
<pre>patchwork.onUpdate = (inputs) => {
  // inputs.p0, inputs.p1, ...
  console.log(inputs);
};</pre>
<script>
console.log('[Patchwork] Default template loaded. Waiting for data...');
patchwork.onUpdate = (d) => {
  console.log('[Patchwork] Data received:', d);
  let html = '';
  for (const [k, v] of Object.entries(d)) {
    html += '<div>' + k + ': <span class="val">' + v.toFixed(3) + '</span></div>';
  }
  document.getElementById('vals').innerHTML = html;
};
</script>
</body></html>"#;

fn start_web_app_server(state: SharedWebAppState) -> u16 {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => { crate::system_log::error(format!("Web App server bind failed: {}", e)); return 0; }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    if let Ok(mut s) = state.lock() { s.port = port; s.is_running = true; }

    crate::system_log::log(format!("Web App server started on port {}", port));

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 { continue; }
            let request = String::from_utf8_lossy(&buf[..n]);

            if request.contains("GET /data") {
                // Serve current input values as JSON
                let json = if let Ok(s) = state.lock() {
                    let pairs: Vec<String> = s.input_names.iter().zip(s.input_values.iter())
                        .map(|(name, val)| format!("\"{}\":{:.6}", name, val))
                        .collect();
                    format!("{{{}}}", pairs.join(","))
                } else {
                    "{}".to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}",
                    json.len(), json);
                let _ = stream.write_all(resp.as_bytes());
            } else if request.contains("GET /hash") {
                let hash = state.lock().map(|s| s.html_hash).unwrap_or(0);
                let body = format!("{}", hash);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(), body);
                let _ = stream.write_all(resp.as_bytes());
            } else if request.contains("GET /raw") {
                let html = state.lock().map(|s| s.html.clone()).unwrap_or_default();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}",
                    html.len(), html);
                let _ = stream.write_all(resp.as_bytes());
            } else {
                // Serve full page with injected bridge
                let (html, hash) = state.lock()
                    .map(|s| (s.html.clone(), s.html_hash))
                    .unwrap_or_default();

                let bridge = PATCHWORK_JS_BRIDGE.replace("__HASH__", &hash.to_string());

                let page = if html.is_empty() {
                    // Serve default template with bridge injected before </body>
                    DEFAULT_HTML.replace("</body>", &format!("{}</body>", bridge))
                } else if html.contains("</body>") {
                    // User HTML has </body> — inject bridge before it
                    html.replace("</body>", &format!("{}</body>", bridge))
                } else if html.contains("</html>") {
                    html.replace("</html>", &format!("{}</html>", bridge))
                } else {
                    // Fragment — wrap in full page
                    format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Web App</title>\
                        <style>body{{margin:0;padding:16px;font-family:sans-serif;background:#1a1a2e;color:#eee;}}</style>\
                        </head><body>{}{}</body></html>", html, bridge)
                };

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}",
                    page.len(), page);
                let _ = stream.write_all(resp.as_bytes());
            }
            let _ = stream.flush();
        }
    });
    port
}

// ── Node ──────────────────────────────────────────────────────────────────────

pub struct WebAppNode {
    pub input_names: Vec<String>,
    pub html_code: String,
    server_port: u16,
    server_state: Option<SharedWebAppState>,
    input_values: Vec<f32>,
}

impl std::fmt::Debug for WebAppNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebAppNode")
            .field("input_names", &self.input_names)
            .field("server_port", &self.server_port)
            .finish()
    }
}

impl Clone for WebAppNode {
    fn clone(&self) -> Self {
        Self {
            input_names: self.input_names.clone(),
            html_code: self.html_code.clone(),
            server_port: 0,
            server_state: None, // server is runtime-only, not cloned
            input_values: self.input_values.clone(),
        }
    }
}

impl Default for WebAppNode {
    fn default() -> Self {
        Self {
            input_names: vec!["p0".into(), "p1".into()],
            html_code: String::new(),
            server_port: 0,
            server_state: None,
            input_values: vec![0.0; 2],
        }
    }
}

impl NodeBehavior for WebAppNode {
    fn title(&self)      -> &str   { "Web App" }
    fn type_tag(&self)   -> &str   { "web_app" }
    fn color_hint(&self) -> [u8;3] { [60, 200, 180] }
    fn min_width(&self)  -> Option<f32> { Some(240.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        let mut ports = vec![PortDef::new("HTML", PortKind::Text)];
        for name in &self.input_names {
            ports.push(PortDef::dynamic(name.clone(), PortKind::Number));
        }
        ports
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Resize input_values to match input_names
        self.input_values.resize(self.input_names.len(), 0.0);

        // Read numeric values from ports 1..N — only override if actually wired
        for i in 0..self.input_names.len() {
            let port_idx = i + 1;
            if let Some(PortValue::Float(f)) = inputs.get(port_idx) {
                self.input_values[i] = *f;
            }
            // When not wired, keep the manual DragValue in self.input_values[i]
        }

        // Update shared server state
        if let Some(ref state) = self.server_state {
            if let Ok(mut s) = state.lock() {
                s.input_values = self.input_values.clone();
                s.input_names = self.input_names.clone();

                // HTML from port 0 overrides inline editor
                if let Some(PortValue::Text(html)) = inputs.first() {
                    if !html.is_empty() {
                        let new_hash = compute_hash(html);
                        if new_hash != s.html_hash {
                            s.html = html.clone();
                            s.html_hash = new_hash;
                        }
                    }
                } else if !self.html_code.is_empty() {
                    let new_hash = compute_hash(&self.html_code);
                    if new_hash != s.html_hash {
                        s.html = self.html_code.clone();
                        s.html_hash = new_hash;
                    }
                }
            }
        }

        vec![]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim   = egui::Color32::from_rgb(110, 110, 120);
        let green = egui::Color32::from_rgb(80, 200, 120);

        // ── Lazy server startup ──────────────────────────────────────────
        if self.server_state.is_none() {
            let state = Arc::new(Mutex::new(WebAppServerState {
                html: self.html_code.clone(),
                html_hash: compute_hash(&self.html_code),
                port: 0,
                is_running: false,
                input_values: self.input_values.clone(),
                input_names: self.input_names.clone(),
            }));
            self.server_port = start_web_app_server(state.clone());
            self.server_state = Some(state);
        }

        // ── Status + buttons ─────────────────────────────────────────────
        ui.horizontal(|ui| {
            if self.server_port > 0 {
                ui.label(RichText::new("●").color(green));
                ui.label(RichText::new(format!(":{}", self.server_port)).small().monospace().color(dim));
            } else {
                ui.label(RichText::new("○").color(dim));
                ui.label(RichText::new("Stopped").small().color(dim));
            }
            if ui.small_button("Open Preview").clicked() && self.server_port > 0 {
                let url = format!("http://127.0.0.1:{}", self.server_port);
                #[cfg(target_os = "macos")]
                let _ = std::process::Command::new("open").arg(&url).spawn();
                #[cfg(target_os = "windows")]
                let _ = std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn();
                #[cfg(target_os = "linux")]
                let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
            }
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── HTML input port ──────────────────────────────────────────────
        let html_connected = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Text);
            ui.label(RichText::new(if html_connected { "HTML ✓" } else { "HTML" }).small()
                .color(if html_connected { green } else { dim }));
        });

        // ── Named input ports ────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Inputs").small().strong().color(dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(RichText::new("+").color(green)).clicked() {
                    let idx = self.input_names.len();
                    self.input_names.push(format!("p{}", idx));
                    self.input_values.push(0.0);
                }
            });
        });

        let mut remove_idx: Option<usize> = None;
        for i in 0..self.input_names.len() {
            let port_idx = i + 1; // +1 because port 0 is HTML
            let port_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == port_idx);
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, node_id, port_idx, true,
                    ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
                ui.add(egui::TextEdit::singleline(&mut self.input_names[i]).desired_width(50.0)
                    .font(egui::TextStyle::Small));
                if port_wired {
                    let val = self.input_values.get(i).copied().unwrap_or(0.0);
                    ui.label(RichText::new(format!("{:.2}", val)).small().monospace()
                        .color(egui::Color32::from_rgb(100, 180, 240)));
                } else {
                    // DragValue when not wired
                    if i < self.input_values.len() {
                        ui.add(egui::DragValue::new(&mut self.input_values[i])
                            .speed(0.01).range(0.0..=1.0).max_decimals(2));
                    }
                }
                // Remove button on the right
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.input_names.len() > 1 {
                        if ui.small_button(RichText::new("−").color(egui::Color32::from_rgb(220, 80, 80))).clicked() {
                            remove_idx = Some(i);
                        }
                    }
                });
            });
        }

        if let Some(idx) = remove_idx {
            self.input_names.remove(idx);
            if idx < self.input_values.len() { self.input_values.remove(idx); }
            // Disconnect shifted ports
            let port_idx = idx + 1;
            for p in port_idx..=self.input_names.len() + 1 {
                ctx.pending_disconnects.push((node_id, p));
            }
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Code editor ──────────────────────────────────────────────────
        if !html_connected {
            ui.label(RichText::new("Code").small().strong().color(dim));
            let changed = egui::ScrollArea::vertical()
                .id_salt(egui::Id::new(("webapp_code", node_id)))
                .max_height(150.0)
                .show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut self.html_code)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)).changed()
                }).inner;

            if changed {
                if let Some(ref state) = self.server_state {
                    if let Ok(mut s) = state.lock() {
                        let h = compute_hash(&self.html_code);
                        s.html = self.html_code.clone();
                        s.html_hash = h;
                    }
                }
            }

            if ui.small_button(RichText::new("Open in editor").small()).clicked() {
                let tmp = std::env::temp_dir().join(format!("patchwork_webapp_{}.html", node_id));
                if std::fs::write(&tmp, &self.html_code).is_ok() {
                    #[cfg(target_os = "macos")]
                    let _ = std::process::Command::new("open").arg(&tmp).spawn();
                    #[cfg(target_os = "windows")]
                    let _ = std::process::Command::new("cmd").args(["/C", "start", "", &tmp.display().to_string()]).spawn();
                    #[cfg(target_os = "linux")]
                    let _ = std::process::Command::new("xdg-open").arg(&tmp).spawn();
                }
            }
        } else {
            ui.label(RichText::new("HTML from input ✓").small().color(green));
        }

    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "input_names": self.input_names,
            "html_code": self.html_code,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(names) = state.get("input_names").and_then(|v| v.as_array()) {
            self.input_names = names.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            self.input_values = vec![0.0; self.input_names.len()];
        }
        if let Some(html) = state.get("html_code").and_then(|v| v.as_str()) {
            self.html_code = html.to_string();
        }
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("web_app", |state| {
        let mut n = WebAppNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
