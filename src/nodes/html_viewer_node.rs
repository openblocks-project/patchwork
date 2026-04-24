use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

struct ServerState {
    html: String,
    hash: u64,
    port: u16,
    is_running: bool,
}

type SharedState = Arc<Mutex<ServerState>>;

fn compute_hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn start_server(state: SharedState) -> u16 {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => { crate::system_log::error(format!("HTML server bind failed: {}", e)); return 0; }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    if let Ok(mut s) = state.lock() { s.port = port; s.is_running = true; }

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let request = String::from_utf8_lossy(&buf);

            let (html, hash) = if let Ok(s) = state.lock() { (s.html.clone(), s.hash) }
                else { (String::new(), 0) };

            if request.contains("GET /hash") {
                let body = format!("{}", hash);
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}", body.len(), body);
                let _ = stream.write_all(resp.as_bytes());
            } else if request.contains("GET /raw") {
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}", html.len(), html);
                let _ = stream.write_all(resp.as_bytes());
            } else {
                let is_full = html.contains("<!DOCTYPE") || html.contains("<html") || html.contains("<canvas") || html.contains("<script");
                let page = if is_full {
                    format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Preview</title></head><body style=\"margin:0;background:#1a1a2e;\">{}<script>(function(){{let h='{}';async function c(){{try{{const r=await fetch('/hash');const t=await r.text();if(t!==h){{h=t;location.reload();}}}}catch(e){{}}setTimeout(c,1000);}}setTimeout(c,2000);}})();</script></body></html>", html, hash)
                } else {
                    format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Preview</title><style>body{{margin:0;padding:16px;font-family:sans-serif;background:#1a1a2e;color:#eee;}}</style></head><body><div id=\"c\">{}</div><script>let h='{}';async function p(){{try{{const r=await fetch('/hash');const t=await r.text();if(t!==h){{h=t;const s=await fetch('/raw');document.getElementById('c').innerHTML=await s.text();}}}}catch(e){{}}setTimeout(p,500);}}setTimeout(p,1000);</script></body></html>", html, hash)
                };
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}", page.len(), page);
                let _ = stream.write_all(resp.as_bytes());
            }
            let _ = stream.flush();
        }
    });
    port
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HtmlViewerNode;

impl NodeBehavior for HtmlViewerNode {
    fn title(&self) -> &str { "HTML Viewer" }
    fn inputs(&self) -> Vec<PortDef> { vec![PortDef::new("HTML", PortKind::Text)] }
    fn outputs(&self) -> Vec<PortDef> { vec![PortDef::new("URL", PortKind::Text)] }
    fn color_hint(&self) -> [u8; 3] { [60, 180, 220] }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> { vec![] }

    fn type_tag(&self) -> &str { "html_viewer" }
    fn save_state(&self) -> serde_json::Value { serde_json::json!({}) }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        let input_val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
        let html = match &input_val {
            PortValue::Text(s) => s.clone(),
            PortValue::Float(f) => format!("<h1>{}</h1>", f),
            _ => String::new(),
        };

        // Server state
        let state_id = egui::Id::new(("html_server_d", ctx.node_id));
        let state: SharedState = ui.ctx().data_mut(|d| {
            d.get_temp_mut_or_insert_with(state_id, || {
                let s = Arc::new(Mutex::new(ServerState { html: String::new(), hash: 0, port: 0, is_running: false }));
                start_server(s.clone());
                s
            }).clone()
        });

        let new_hash = compute_hash(&html);
        if let Ok(mut s) = state.lock() { s.html = html.clone(); s.hash = new_hash; }

        let (port, is_running) = state.lock().map(|s| (s.port, s.is_running)).unwrap_or((0, false));

        ui.horizontal(|ui| {
            if is_running {
                ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● Live");
                ui.label(egui::RichText::new(format!(":{}", port)).small().monospace().color(dim));
            } else {
                ui.colored_label(egui::Color32::from_rgb(200, 80, 80), "○ Stopped");
            }
        });

        ui.horizontal(|ui| {
            if is_running {
                if ui.button("Open Preview").clicked() {
                    let _ = open::that(format!("http://127.0.0.1:{}", port));
                }
                if ui.button("Refresh").clicked() {
                    if let Ok(mut s) = state.lock() { s.hash = s.hash.wrapping_add(1); }
                }
            }
        });

        ui.separator();

        if html.is_empty() {
            ui.colored_label(dim, "Connect HTML text to input");
        } else {
            let preview = if html.chars().count() > 300 {
                let head: String = html.chars().take(300).collect();
                format!("{}...", head)
            } else { html };
            egui::ScrollArea::vertical().max_height(100.0).show(ui, |ui| {
                let mut p = preview;
                ui.code_editor(&mut p);
            });
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("html_viewer", |_| Box::new(HtmlViewerNode));
}
