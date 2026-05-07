#![allow(dead_code)]
use crate::graph::*;
use eframe::egui;
use crate::nodes::ScrollAreaExt;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// Shared state between the node and the HTTP server thread
struct HtmlServerState {
    html: String,
    hash: u64,
    port: u16,
    is_running: bool,
}

type SharedHtmlState = Arc<Mutex<HtmlServerState>>;

fn compute_hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Start a tiny HTTP server on localhost
fn start_server(state: SharedHtmlState) -> u16 {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => { crate::system_log::error(format!("HTML server bind failed: {}", e)); return 0; }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);

    if let Ok(mut s) = state.lock() {
        s.port = port;
        s.is_running = true;
    }

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let request = String::from_utf8_lossy(&buf);

            let (html_content, content_hash) = if let Ok(s) = state.lock() {
                (s.html.clone(), s.hash)
            } else {
                (String::new(), 0)
            };

            if request.contains("GET /hash") {
                // Return just the hash — browser polls this to detect changes
                let body = format!("{}", content_hash);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(), body
                );
                let _ = stream.write_all(response.as_bytes());
            } else if request.contains("GET /raw") {
                // Return raw HTML content (for full-page mode reload)
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}",
                    html_content.len(), html_content
                );
                let _ = stream.write_all(response.as_bytes());
            } else {
                // Main page: detect if content is a full HTML document or a fragment
                let is_full_page = html_content.contains("<!DOCTYPE") ||
                    html_content.contains("<html") ||
                    html_content.contains("<canvas") ||
                    html_content.contains("<script");

                let page = if is_full_page {
                    // Full page mode: serve content directly with a tiny reload checker
                    // The checker only reloads the WHOLE page when hash changes
                    format!(r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Patchwork Preview</title>
<style>
  #_pw_status {{ position: fixed; top: 4px; right: 8px; font-size: 10px; opacity: 0.3; color: #888; z-index: 99999; pointer-events: none; }}
</style>
</head>
<body style="margin:0; background:#1a1a2e;">
<div id="_pw_status">live</div>
{}
<script>
// Patchwork live-reload: only reload when content hash changes
(function() {{
  let _pwHash = '{}';
  async function _pwCheck() {{
    try {{
      const r = await fetch('/hash');
      const h = await r.text();
      if (h !== _pwHash) {{
        _pwHash = h;
        location.reload();
      }}
      document.getElementById('_pw_status').textContent = 'live';
    }} catch(e) {{
      document.getElementById('_pw_status').textContent = 'disconnected';
    }}
    setTimeout(_pwCheck, 1000);
  }}
  setTimeout(_pwCheck, 2000);
}})();
</script>
</body>
</html>"#, html_content, content_hash)
                } else {
                    // Fragment mode: safe to use innerHTML swap
                    format!(r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>Patchwork Preview</title>
<style>
  body {{ margin: 0; padding: 16px; font-family: -apple-system, BlinkMacSystemFont, sans-serif; background: #1a1a2e; color: #eee; }}
  #_pw_status {{ position: fixed; top: 4px; right: 8px; font-size: 10px; opacity: 0.3; color: #888; }}
</style>
</head>
<body>
<div id="content">{}</div>
<div id="_pw_status">live</div>
<script>
let _pwHash = '{}';
async function _pwPoll() {{
  try {{
    const r = await fetch('/hash');
    const h = await r.text();
    if (h !== _pwHash) {{
      _pwHash = h;
      const res = await fetch('/raw');
      document.getElementById('content').innerHTML = await res.text();
      document.getElementById('_pw_status').textContent = 'updated ' + new Date().toLocaleTimeString();
    }}
  }} catch(e) {{
    document.getElementById('_pw_status').textContent = 'disconnected';
  }}
  setTimeout(_pwPoll, 500);
}}
setTimeout(_pwPoll, 1000);
</script>
</body>
</html>"#, html_content, content_hash)
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}",
                    page.len(), page
                );
                let _ = stream.write_all(response.as_bytes());
            }
            let _ = stream.flush();
        }
    });

    port
}

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    _node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
) {
    // Get HTML from input port 0
    let input_val = Graph::static_input_value(connections, values, node_id, 0);
    let html = match &input_val {
        PortValue::Text(s) => s.clone(),
        PortValue::Float(f) => format!("<h1>{}</h1>", f),
        _ => String::new(),
    };

    // Server state (persisted via egui temp storage)
    let state_id = egui::Id::new(("html_server", node_id));
    let state: SharedHtmlState = ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_insert_with(state_id, || {
            let s = Arc::new(Mutex::new(HtmlServerState {
                html: String::new(),
                hash: 0,
                port: 0,
                is_running: false,
            }));
            start_server(s.clone());
            s
        }).clone()
    });

    // Update HTML content + hash in server
    let new_hash = compute_hash(&html);
    if let Ok(mut s) = state.lock() {
        s.html = html.clone();
        s.hash = new_hash;
    }

    let (port, is_running) = if let Ok(s) = state.lock() {
        (s.port, s.is_running)
    } else {
        (0, false)
    };

    // Status
    ui.horizontal(|ui| {
        if is_running {
            ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● Live");
            ui.label(egui::RichText::new(format!(":{}", port)).small().monospace().color(egui::Color32::GRAY));
        } else {
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), "○ Stopped");
        }
    });

    // Buttons
    ui.horizontal(|ui| {
        if is_running {
            if ui.button("Open Preview").clicked() {
                let url = format!("http://127.0.0.1:{}", port);
                let _ = open::that(&url);
            }
            if ui.button("Refresh").clicked() {
                // Force hash change to trigger browser reload
                if let Ok(mut s) = state.lock() {
                    s.hash = s.hash.wrapping_add(1);
                }
            }
        }
    });

    ui.separator();

    // Show HTML preview (truncated)
    if html.is_empty() {
        ui.colored_label(egui::Color32::GRAY, "Connect HTML text to input");
    } else {
        let preview = if html.chars().count() > 300 {
            let head: String = html.chars().take(300).collect();
            format!("{}...", head)
        } else {
            html
        };
        egui::ScrollArea::vertical().max_height(100.0).show_pannable(ui, |ui| {
            let mut p = preview;
            ui.code_editor(&mut p);
        });
    }
}
