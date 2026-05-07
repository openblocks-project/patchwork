use eframe::egui::{self, RichText};
use crate::nodes::ScrollAreaExt;
use crate::nodes::web_app_mjpeg::{JpegSlot, MjpegEncoder, PLACEHOLDER_JPEG};
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Web App Node ─────────────────────────────────────────────────────────────
//
// Hosts a local web page with a built-in JavaScript API that receives
// live Patchwork values at ~60fps via /data endpoint polling.
//
// User writes custom HTML/JS. The injected `patchwork.inputs` object
// provides real-time access to all connected input port values.
//
// Ports (indices are fixed so adding/removing numerics doesn't shift
// the Image port's wire onto a different slot):
//   In  0:    HTML (Text, optional — overrides inline code editor)
//   In  1:    Image (RGBA frames; encoded to MJPEG and exposed at
//             GET /stream)
//   In  2..N+1: Named numeric inputs (p0, p1, p2, p3 by default)
//   Out 0..M:  Named numeric outputs (Web → Patchwork)

/// Fixed port index for the Image input. Wired up downstream of the
/// HTML port; numeric inputs are at indices 2..N+1.
const IMAGE_PORT_IDX: usize = 1;

struct WebAppServerState {
    html: String,
    html_hash: u64,
    port: u16,
    is_running: bool,
    input_values: Vec<f32>,
    input_names: Vec<String>,
    /// Sticky values written by the page via POST /set. Keyed by the
    /// name the JS sends (`patchwork.set("o0", 0.42)` lands here as
    /// "o0" → 0.42). The node reads this each evaluate frame and emits
    /// any name matching its `output_names` to the corresponding port.
    output_values: HashMap<String, f32>,
    /// Shared MJPEG slot — server thread spawns per-connection
    /// streamer threads that subscribe via this Arc.
    jpeg_slot: Arc<JpegSlot>,
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
  console.log('[Patchwork] Bridge loaded. patchwork.inputs (read), patchwork.set(name,val) / patchwork.send({...}) (write back)');
  // ── Patchwork → Web ────────────────────────────────────────────────────
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
  // ── Web → Patchwork ────────────────────────────────────────────────────
  // patchwork.set(name, value) — POST a single named value back into the
  // graph. The node's output port with the matching name will emit it.
  window.patchwork.set = function(name, value) {
    return fetch('/set', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({[name]: Number(value)})
    }).catch(() => {});
  };
  // patchwork.send({a: 0.5, b: 1}) — batch multiple values.
  window.patchwork.send = function(obj) {
    return fetch('/set', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify(obj)
    }).catch(() => {});
  };
  // ── Hot-reload on HTML change ─────────────────────────────────────────
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
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>
  body { margin:0; padding:20px; font-family:-apple-system,system-ui,sans-serif; background:#1a1a2e; color:#eee; }
  h2 { margin:0 0 12px; font-size:18px; }
  .section { background:#22223a; padding:12px 16px; border-radius:10px; margin:12px 0; }
  .val { color:#0ff; font-size:22px; font-family:monospace; }
  .row { display:flex; align-items:center; gap:12px; margin:8px 0; }
  .row label { min-width:60px; opacity:.8; }
  input[type=range] { flex:1; accent-color:#0fc; }
  pre { background:#12121a; padding:10px; border-radius:6px; font-size:11px; color:#8f8; overflow:auto; }
  small { opacity:.6; }
</style></head>
<body>
<h2>Patchwork Web App</h2>

<div class="section">
  <small>Stream (Patchwork → Web)</small>
  <img src="/stream" id="pwstream" alt="" style="width:100%;display:block;background:#000;border-radius:8px;margin-top:6px;" />
  <small style="opacity:.6;">Wire any Image-emitting node into the Web App's Image input.</small>
</div>

<div class="section">
  <small>Inputs (Patchwork → Web)</small>
  <div id="vals"></div>
</div>

<div class="section">
  <small>Outputs (Web → Patchwork)</small>
  <div class="row">
    <label for="o0">o0</label>
    <input type="range" id="o0" min="0" max="1" step="0.01" value="0"
           oninput="patchwork.set('o0', this.value); document.getElementById('o0v').textContent = (+this.value).toFixed(2);">
    <span id="o0v" class="val" style="font-size:16px;">0.00</span>
  </div>
  <div class="row">
    <label>button</label>
    <button style="flex:1;padding:10px;border:0;border-radius:6px;background:#0fc;color:#000;font-weight:600;"
            onpointerdown="patchwork.set('button', 1)"
            onpointerup="patchwork.set('button', 0)"
            onpointerleave="patchwork.set('button', 0)">Press me</button>
  </div>
  <small>Add output ports <code>o0</code> / <code>button</code> on the Patchwork node to receive these.</small>
</div>

<div class="section">
  <small>API</small>
  <pre>patchwork.inputs            // {p0: 0.5, ...}
patchwork.onUpdate = (d) => {}
patchwork.set('name', 0.5)  // POST single value back
patchwork.send({a:1, b:2})  // POST batch back</pre>
</div>

<script>
console.log('[Patchwork] Default template loaded.');
patchwork.onUpdate = (d) => {
  let html = '';
  for (const [k, v] of Object.entries(d)) {
    html += '<div class="row"><label>' + k + '</label><span class="val">' + v.toFixed(3) + '</span></div>';
  }
  document.getElementById('vals').innerHTML = html || '<small>(no inputs wired yet)</small>';
};
</script>
</body></html>"#;

fn start_web_app_server(state: SharedWebAppState) -> u16 {
    // Bind on ALL interfaces so phones / other devices on the same Wi-Fi
    // can reach the page via the host's LAN IP. Falls back to loopback if
    // 0.0.0.0 is blocked (locked-down environment / restrictive firewall).
    let listener = match TcpListener::bind("0.0.0.0:0") {
        Ok(l) => l,
        Err(_) => match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => {
                crate::system_log::error(format!("Web App server bind failed: {}", e));
                return 0;
            }
        }
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

            if request.starts_with("OPTIONS ") {
                // CORS preflight. Browsers send this for cross-origin POSTs
                // with a non-simple Content-Type (application/json).
                let resp = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nContent-Length: 0\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                continue;
            } else if request.starts_with("POST /set") {
                // Body sits after "\r\n\r\n". Cap at the read buffer for
                // simplicity — slider/button payloads are tiny (~50 B).
                let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
                let mut accepted = 0usize;
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                    if let Some(obj) = json.as_object() {
                        if let Ok(mut s) = state.lock() {
                            for (k, v) in obj {
                                if let Some(f) = v.as_f64() {
                                    s.output_values.insert(k.clone(), f as f32);
                                    accepted += 1;
                                } else if let Some(b) = v.as_bool() {
                                    s.output_values.insert(k.clone(),
                                        if b { 1.0 } else { 0.0 });
                                    accepted += 1;
                                } else if let Some(i) = v.as_i64() {
                                    s.output_values.insert(k.clone(), i as f32);
                                    accepted += 1;
                                }
                            }
                        }
                    }
                }
                let body_resp = format!(r#"{{"accepted":{}}}"#, accepted);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-cache\r\nContent-Length: {}\r\n\r\n{}",
                    body_resp.len(), body_resp);
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                continue;
            } else if request.contains("GET /data") {
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
            } else if request.contains("GET /stream") {
                // MJPEG live stream. Hand off to a per-connection thread
                // — the multipart response holds the socket open
                // indefinitely, so leaving it inline would block every
                // other route until the phone disconnects.
                let slot = state.lock().ok().map(|s| s.jpeg_slot.clone());
                if let Some(slot) = slot {
                    let _ = stream.set_nodelay(true);
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    std::thread::spawn(move || serve_mjpeg_stream(stream, slot));
                }
                continue;
            } else {
                // Serve full page with injected bridge
                let (html, hash) = state.lock()
                    .map(|s| (s.html.clone(), s.html_hash))
                    .unwrap_or_default();

                let bridge = PATCHWORK_JS_BRIDGE.replace("__HASH__", &hash.to_string());

                // Inject the bridge at the START of <body> rather than
                // before </body>. Eager inline scripts in the page body
                // (including DEFAULT_HTML's `patchwork.onUpdate = ...`)
                // run as the parser hits them, so they need
                // `window.patchwork` to already be defined. End-of-body
                // injection ran scripts BEFORE the bridge, throwing
                // ReferenceError and silently dropping `onUpdate`.
                let page = if html.is_empty() {
                    DEFAULT_HTML.replace("<body>", &format!("<body>{}", bridge))
                } else if html.contains("<body>") {
                    html.replace("<body>", &format!("<body>{}", bridge))
                } else if html.contains("</html>") {
                    html.replace("</html>", &format!("{}</html>", bridge))
                } else {
                    // Fragment — wrap in full page. Bridge first so
                    // any inline scripts in the user fragment can use
                    // `patchwork.*` immediately.
                    format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Web App</title>\
                        <style>body{{margin:0;padding:16px;font-family:sans-serif;background:#1a1a2e;color:#eee;}}</style>\
                        </head><body>{}{}</body></html>", bridge, html)
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

/// One MJPEG client. Sends multipart headers + a placeholder JPEG, then
/// loops on the shared slot's condvar emitting each new frame as a
/// `multipart/x-mixed-replace` part. Exits when the socket dies (phone
/// closed the page, network blip, write timeout).
fn serve_mjpeg_stream(mut stream: std::net::TcpStream, slot: Arc<JpegSlot>) {
    const BOUNDARY: &str = "pwframe";
    let headers = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: multipart/x-mixed-replace; boundary={BOUNDARY}\r\n\
         Cache-Control: no-cache, no-store, must-revalidate\r\n\
         Pragma: no-cache\r\n\
         Connection: close\r\n\
         Access-Control-Allow-Origin: *\r\n\
         \r\n"
    );
    if stream.write_all(headers.as_bytes()).is_err() {
        return;
    }

    // Placeholder so `<img>` paints something before the encoder
    // produces a real frame. Without this Chrome shows broken-image
    // and Safari shows blank until the first encoded JPEG arrives.
    if write_mjpeg_part(&mut stream, BOUNDARY, PLACEHOLDER_JPEG).is_err() {
        return;
    }

    let mut last_seen: u64 = 0;
    loop {
        match slot.wait_next(last_seen, Duration::from_secs(30)) {
            Some((generation, bytes)) => {
                last_seen = generation;
                if write_mjpeg_part(&mut stream, BOUNDARY, &bytes).is_err() {
                    return;
                }
            }
            None => {
                // No new frame in 30 s. Don't break — the phone's <img>
                // would freeze on a closed connection with no recovery.
                // Just keep waiting; encoder may come back when the
                // user wires Image again.
                continue;
            }
        }
    }
}

fn write_mjpeg_part(stream: &mut std::net::TcpStream, boundary: &str, jpeg: &[u8])
    -> std::io::Result<()>
{
    let part_header = format!(
        "--{boundary}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        jpeg.len()
    );
    stream.write_all(part_header.as_bytes())?;
    stream.write_all(jpeg)?;
    stream.write_all(b"\r\n")?;
    stream.flush()
}

fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

// ── LAN IP detection ────────────────────────────────────────────────────────
//
// Standard "open a UDP socket toward a public IP, read its local addr"
// trick — no actual packet is sent, the OS just picks the local interface
// it would route through. Returns None if there's no routable interface
// (e.g. machine is fully offline) — caller falls back to loopback.
fn detect_lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    let ip = s.local_addr().ok()?.ip();
    if ip.is_loopback() || ip.is_unspecified() { None } else { Some(ip) }
}

// ── QR code rendering ───────────────────────────────────────────────────────
//
// Generates a black-on-white QR pixel image for the given URL with a 2-module
// quiet zone and 4× scale per module. The egui::ColorImage is then uploaded
// to a TextureHandle by the caller (cached on the node, regenerated only
// when the URL string changes — typically just at startup).
fn url_qr_image(url: &str) -> Option<egui::ColorImage> {
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    let colors = code.to_colors();
    let side = code.width(); // QR is square, side = sqrt(colors.len())
    if side == 0 { return None; }
    const SCALE: usize = 4;
    const QUIET: usize = 2;
    let pix_side = (side + QUIET * 2) * SCALE;
    let mut pixels = vec![egui::Color32::WHITE; pix_side * pix_side];
    for y in 0..side {
        for x in 0..side {
            if colors[y * side + x] == qrcode::Color::Dark {
                let py0 = (y + QUIET) * SCALE;
                let px0 = (x + QUIET) * SCALE;
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        pixels[(py0 + dy) * pix_side + (px0 + dx)] = egui::Color32::BLACK;
                    }
                }
            }
        }
    }
    Some(egui::ColorImage { size: [pix_side, pix_side], pixels })
}

// ── Node ──────────────────────────────────────────────────────────────────────

pub struct WebAppNode {
    pub input_names: Vec<String>,
    /// Names of values the page can send back via `patchwork.set(name, ...)`.
    /// Each appears as a Number output port; evaluate() reads the latest
    /// posted value from server state and emits it on the matching port.
    pub output_names: Vec<String>,
    pub html_code: String,
    server_port: u16,
    server_state: Option<SharedWebAppState>,
    input_values: Vec<f32>,
    /// Current playhead/output values mirrored from the server thread,
    /// kept on the node so `outputs()` (which only sees `&self`) can
    /// expose them. Refreshed on every `evaluate()`.
    output_cache: Vec<f32>,
    /// QR code texture for the current URL. Regenerated when the URL
    /// changes (which only happens once at first start, unless the LAN
    /// IP shifts). Stored as (url, handle) so we can detect change.
    qr_texture: Option<(String, egui::TextureHandle)>,
    /// MJPEG encoder. `None` when the Image port isn't wired or
    /// hasn't received a frame yet. Spawned on first frame in
    /// `evaluate()`; dropped on unwire in `render_with_context`.
    encoder: Option<MjpegEncoder>,
    /// Dimensions the current encoder is locked to. On a mismatch
    /// the encoder is dropped + respawned.
    encoder_dims: Option<(u32, u32)>,
    /// Shared JPEG slot — written by the encoder's stdout reader,
    /// read by per-`/stream` connection threads. Lives on the node
    /// (not the encoder) so it survives encoder respawns.
    jpeg_slot: Arc<JpegSlot>,
    /// One-line status surfaced under the Image port row. Empty when
    /// the encoder is healthy or the port is unwired.
    image_status: String,
}

impl std::fmt::Debug for WebAppNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebAppNode")
            .field("input_names", &self.input_names)
            .field("output_names", &self.output_names)
            .field("server_port", &self.server_port)
            .finish()
    }
}

impl Clone for WebAppNode {
    fn clone(&self) -> Self {
        Self {
            input_names: self.input_names.clone(),
            output_names: self.output_names.clone(),
            html_code: self.html_code.clone(),
            server_port: 0,
            server_state: None, // server is runtime-only, not cloned
            input_values: self.input_values.clone(),
            output_cache: vec![0.0; self.output_names.len()],
            qr_texture: None,
            encoder: None,
            encoder_dims: None,
            jpeg_slot: Arc::new(JpegSlot::new()),
            image_status: String::new(),
        }
    }
}

impl Default for WebAppNode {
    fn default() -> Self {
        Self {
            input_names: vec!["p0".into(), "p1".into()],
            output_names: vec!["o0".into(), "button".into()],
            html_code: String::new(),
            server_port: 0,
            server_state: None,
            input_values: vec![0.0; 2],
            output_cache: vec![0.0; 2],
            qr_texture: None,
            encoder: None,
            encoder_dims: None,
            jpeg_slot: Arc::new(JpegSlot::new()),
            image_status: String::new(),
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
        // Port indices are pinned so adding/removing named numerics
        // doesn't shift the Image port's wire onto a different slot:
        //   0 = HTML, 1 = Image, 2..N+1 = dynamic numerics.
        // Visually we still render the Image port row at the bottom
        // of the input list (per the "after the named numeric inputs"
        // spec); the inline_port_circle keys positions by port index,
        // not by render order.
        let mut ports = vec![
            PortDef::new("HTML", PortKind::Text),
            PortDef::new("Image", PortKind::Image),
        ];
        for name in &self.input_names {
            ports.push(PortDef::dynamic(name.clone(), PortKind::Number));
        }
        ports
    }

    fn outputs(&self) -> Vec<PortDef> {
        self.output_names.iter()
            .map(|n| PortDef::dynamic(n.clone(), PortKind::Number))
            .collect()
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Resize input_values to match input_names
        self.input_values.resize(self.input_names.len(), 0.0);

        // Read numeric values from ports 2..N+1 (port 0 = HTML,
        // port 1 = Image, then dynamic numerics).
        for i in 0..self.input_names.len() {
            let port_idx = i + 2;
            if let Some(PortValue::Float(f)) = inputs.get(port_idx) {
                self.input_values[i] = *f;
            }
            // When not wired, keep the manual DragValue in self.input_values[i]
        }

        // Image input at fixed port 1. Spawn encoder on first frame,
        // feed RGBA each call. Mismatched dims trigger a respawn
        // (mirrors FileRecorder's lock-on-first-frame, but stream
        // nodes should adapt rather than drop frames). Drop-on-unwire
        // lives in render_with_context (where we have ctx.connections).
        match inputs.get(IMAGE_PORT_IDX) {
            Some(PortValue::Image(img)) => {
                let dims = (img.width, img.height);
                if self.encoder_dims != Some(dims) {
                    self.encoder = None;
                    match MjpegEncoder::new(dims.0, dims.1, self.jpeg_slot.clone()) {
                        Ok(e) => {
                            self.encoder = Some(e);
                            self.encoder_dims = Some(dims);
                            self.image_status.clear();
                        }
                        Err(e) => {
                            self.encoder_dims = None;
                            self.image_status = e;
                        }
                    }
                }
                if let Some(enc) = self.encoder.as_mut() {
                    if let Err(e) = enc.write_frame(&img.pixels, dims.0, dims.1) {
                        self.image_status = e;
                        self.encoder = None;
                        self.encoder_dims = None;
                    }
                }
            }
            Some(PortValue::GpuImage(_)) => {
                if self.image_status.is_empty() {
                    self.image_status = "GPU image not yet supported — \
                        insert an image effect (e.g. Crop, Transform) before Web App".into();
                }
                self.encoder = None;
                self.encoder_dims = None;
            }
            _ => {
                // Unwired or no value yet; render_with_context handles
                // dropping the encoder when truly unwired.
            }
        }

        // Update shared server state + read back posted output values.
        let mut emitted: Vec<(usize, PortValue)> = Vec::new();
        self.output_cache.resize(self.output_names.len(), 0.0);
        if let Some(ref state) = self.server_state {
            if let Ok(mut s) = state.lock() {
                s.input_values = self.input_values.clone();
                s.input_names = self.input_names.clone();

                // Drain output values into our local cache + emit each
                // matched name on its port. Names not present default to 0.
                for (i, name) in self.output_names.iter().enumerate() {
                    let v = s.output_values.get(name).copied().unwrap_or(0.0);
                    self.output_cache[i] = v;
                    emitted.push((i, PortValue::Float(v)));
                }

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

        emitted
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
                output_values: HashMap::new(),
                jpeg_slot: self.jpeg_slot.clone(),
            }));
            self.server_port = start_web_app_server(state.clone());
            self.server_state = Some(state);
        }

        // ── Encoder lifecycle: drop on unwire ───────────────────────────
        // Mirrors AudioPipeDecoder gating in video_in_node.rs:1326 —
        // both spawn-on-demand and drop-on-unwire of an external decoder
        // gated by a port-wiring check that lives in render. Spawning
        // happens in evaluate() (we need dims from the first frame).
        let image_wired = ctx.connections.iter()
            .any(|c| c.to_node == node_id && c.to_port == IMAGE_PORT_IDX);
        if !image_wired && self.encoder.is_some() {
            self.encoder = None;
            self.encoder_dims = None;
            self.image_status.clear();
        }

        // ── Status + URLs ────────────────────────────────────────────────
        // Two URLs are surfaced: the LAN one (primary, what the QR code
        // encodes — phones on the same Wi-Fi can hit it) and the loopback
        // (secondary, for opening on this machine). LAN IP is detected
        // each frame via the UDP-bind-to-public trick; cheap, and makes
        // the URL refresh if the user's LAN address changes mid-session.
        let loopback_url = if self.server_port > 0 {
            format!("http://127.0.0.1:{}", self.server_port)
        } else {
            String::new()
        };
        let lan_url = if self.server_port > 0 {
            detect_lan_ip().map(|ip| format!("http://{}:{}", ip, self.server_port))
        } else {
            None
        };
        let primary_url = lan_url.clone().unwrap_or_else(|| loopback_url.clone());

        ui.horizontal(|ui| {
            if self.server_port > 0 {
                ui.label(RichText::new("●").color(green));
            } else {
                ui.label(RichText::new("○").color(dim));
            }
            if !primary_url.is_empty() {
                let resp = ui.label(
                    RichText::new(&primary_url).small().monospace()
                        .color(egui::Color32::from_rgb(100, 220, 200)),
                );
                resp.on_hover_text("Click ⤴ Open or scan the QR with a phone on the same Wi-Fi.");
            } else {
                ui.label(RichText::new("Stopped").small().color(dim));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("⤴ Open").on_hover_text("Open in default browser").clicked()
                    && !primary_url.is_empty()
                {
                    open_in_browser(&primary_url);
                }
                if ui.small_button("📋").on_hover_text("Copy URL").clicked()
                    && !primary_url.is_empty()
                {
                    ui.ctx().copy_text(primary_url.clone());
                }
            });
        });

        // Loopback URL as a small secondary line, only when LAN is
        // available (otherwise we'd duplicate the primary).
        if lan_url.is_some() && !loopback_url.is_empty() {
            ui.label(RichText::new(&loopback_url).small().monospace().color(dim));
        }

        // QR code — regenerate the texture lazily when the URL changes.
        if !primary_url.is_empty() {
            let cache_hit = self.qr_texture.as_ref()
                .map(|(u, _)| u == &primary_url).unwrap_or(false);
            if !cache_hit {
                if let Some(img) = url_qr_image(&primary_url) {
                    let handle = ui.ctx().load_texture(
                        format!("webapp_qr_{}", node_id),
                        img,
                        egui::TextureOptions::NEAREST,
                    );
                    self.qr_texture = Some((primary_url.clone(), handle));
                }
            }
            if let Some((_, tex)) = &self.qr_texture {
                ui.add_space(2.0);
                ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(120.0, 120.0)))
                    .on_hover_text("Scan to open from a phone on the same Wi-Fi.");
            }
        }

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
                    // Image port is pinned at index 1; adding a numeric
                    // appends at the end of the dynamic range and never
                    // displaces any existing wire.
                    let idx = self.input_names.len();
                    self.input_names.push(format!("p{}", idx));
                    self.input_values.push(0.0);
                }
            });
        });

        let mut remove_idx: Option<usize> = None;
        for i in 0..self.input_names.len() {
            let port_idx = i + 2; // 0=HTML, 1=Image, 2..N+1=numerics
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
            // Disconnect the removed numeric port + every numeric that
            // shifted down. Image (port 1) is pinned and unaffected.
            //   Pre-remove: numerics span ports 2..N+1.
            //   Post-remove: numerics span ports 2..N (one shorter).
            //   Wires on old indices ≥ idx+2 must be torn down or
            //   they'd silently re-target the slot above.
            let port_idx = idx + 2;
            let old_max_numeric = self.input_names.len() + 2; // pre-remove last
            for p in port_idx..=old_max_numeric {
                ctx.pending_disconnects.push((node_id, p));
            }
        }

        // ── Image input port row ─────────────────────────────────────────
        // Logical index is fixed (IMAGE_PORT_IDX = 1) but rendered
        // visually at the end of the input list per the original spec.
        // `inline_port_circle` keys port positions by index, not render
        // order, so the wire connects to wherever this row sits.
        let img_wired = image_wired;
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, IMAGE_PORT_IDX, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from,
                ctx.pending_disconnects, PortKind::Image);
            let label = if !self.image_status.is_empty() {
                "Image ⚠"
            } else if self.encoder.is_some() {
                "Image ✓"
            } else if img_wired {
                "Image …" // wired but no frame yet
            } else {
                "Image"
            };
            let color = if !self.image_status.is_empty() {
                egui::Color32::from_rgb(220, 160, 80)
            } else if self.encoder.is_some() {
                green
            } else {
                dim
            };
            ui.label(RichText::new(label).small().color(color));
            if let Some((w, h)) = self.encoder_dims {
                ui.label(RichText::new(format!("{}×{} → MJPEG", w, h)).small().monospace().color(dim));
            }
        });
        if !self.image_status.is_empty() {
            ui.label(RichText::new(&self.image_status).small()
                .color(egui::Color32::from_rgb(220, 160, 80)));
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports (Web → Patchwork) ───────────────────────────────
        // Each name is something the page sends back via
        // `patchwork.set('name', value)`. The latest received value
        // appears on the matching output port.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Outputs").small().strong().color(dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(RichText::new("+").color(green)).clicked() {
                    let idx = self.output_names.len();
                    self.output_names.push(format!("o{}", idx));
                    self.output_cache.push(0.0);
                }
            });
        });
        let mut remove_out_idx: Option<usize> = None;
        // Output ports are indexed by their position in output_names (0..N).
        for i in 0..self.output_names.len() {
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, node_id, i, false,
                    ctx.connections, ctx.port_positions, ctx.dragging_from,
                    ctx.pending_disconnects, PortKind::Number);
                ui.add(egui::TextEdit::singleline(&mut self.output_names[i])
                    .desired_width(70.0)
                    .font(egui::TextStyle::Small));
                let v = self.output_cache.get(i).copied().unwrap_or(0.0);
                ui.label(RichText::new(format!("{:.2}", v)).small().monospace()
                    .color(egui::Color32::from_rgb(100, 220, 200)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(
                        RichText::new("−").color(egui::Color32::from_rgb(220, 80, 80))
                    ).clicked() {
                        remove_out_idx = Some(i);
                    }
                });
            });
        }
        if let Some(idx) = remove_out_idx {
            self.output_names.remove(idx);
            if idx < self.output_cache.len() { self.output_cache.remove(idx); }
            // Disconnect any wires hanging off this (or shifted) output port.
            for p in idx..self.output_names.len() + 1 {
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
                .show_pannable(ui, |ui| {
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
            "output_names": self.output_names,
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
        if let Some(names) = state.get("output_names").and_then(|v| v.as_array()) {
            self.output_names = names.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            self.output_cache = vec![0.0; self.output_names.len()];
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
