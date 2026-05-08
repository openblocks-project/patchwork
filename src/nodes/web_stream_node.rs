//! Web Stream [WIP] node — WebRTC sub-100ms a/v + DataChannel
//! signals to a phone browser. Sibling of (NOT replacement for) the
//! Web App node.
//!
//! Web App is the right tool when a static page driven by HTTP polling
//! suffices (~16ms minimum). Web Stream is for latency-critical
//! cases: a/v in real time, and numeric signals over a single
//! DataChannel.
//!
//! Phase 4 V1 scope:
//! - DOWNSTREAM video: Image input → H.264 → WebRTC video track
//! - DOWNSTREAM audio: Audio input → Opus → WebRTC audio track
//! - DataChannel signals: dynamic numeric inputs pushed each
//!   evaluate(); browser-set values arrive as numeric outputs
//!   (matches Web App's bidirectional value API, just over a
//!   DataChannel instead of HTTP polling)
//! - Single peer at a time. New POST /offer kills the old peer.
//! - LAN only. Host candidates only. No STUN/TURN.
//!
//! Out of scope for V1 (recvonly transceivers ARE in the SDP, the
//! decoder side just isn't implemented yet — adding it is purely
//! additive next pass):
//! - Phone camera → Patchwork (upstream Image)
//! - Phone mic    → Patchwork (upstream Audio)
//!
//! Port layout (pinned indices so adding/removing dynamic numerics
//! never displaces a media wire):
//!   In  0:    HTML (Text)
//!   In  1:    Audio
//!   In  2:    Image
//!   In  3..N+2: dynamic numerics (p0, p1, ...)
//!   Out 0..M-1: dynamic numerics (o0, button, ...)

use eframe::egui::{self, RichText};
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::ScrollAreaExt;
use crate::nodes::web_app_node::{detect_lan_ip, open_in_browser, url_qr_image};
use crate::nodes::web_stream_audio::{AudioTapProcessor, OpusPump, PacketSink};
use crate::nodes::web_stream_h264::{H264Encoder, NalSink};
use crate::nodes::web_stream_rtc::{PeerStatus, WebStreamShared};

// ── Pinned port indices ──────────────────────────────────────────────────────
const HTML_PORT_IDX:  usize = 0;
const AUDIO_PORT_IDX: usize = 1;
const IMAGE_PORT_IDX: usize = 2;
/// Dynamic typed ports start here.
const NUMERIC_BASE_IDX: usize = 3;

// ── Per-port data type ───────────────────────────────────────────────────────
//
// Each dynamic input + output port carries a type that determines:
// - The Patchwork-side `PortKind` (Number / Text / Gate)
// - The browser-side UI control (slider / text input / button)
// - The JSON encoding in the DataChannel push (number / string / bool)
//
// `Bool` is stored as f32 (0.0 / 1.0) on the Patchwork side — same
// wire format as a `Number` port — but the browser renders a button
// and the DC payload is a JSON `true` / `false`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebPortType {
    Float,
    Text,
    Bool,
}

impl WebPortType {
    fn as_letter(self) -> &'static str {
        match self { Self::Float => "f", Self::Text => "t", Self::Bool => "b" }
    }
    fn from_letter(s: &str) -> Self {
        match s { "t" | "text" => Self::Text, "b" | "bool" => Self::Bool, _ => Self::Float }
    }
    fn label(self) -> &'static str {
        match self { Self::Float => "Float", Self::Text => "Text", Self::Bool => "Button" }
    }
    fn port_kind(self) -> PortKind {
        match self {
            Self::Float => PortKind::Number,
            Self::Text  => PortKind::Text,
            Self::Bool  => PortKind::Gate,
        }
    }
}

// ── Default HTML template ────────────────────────────────────────────────────

const PATCHWORK_RTC_BRIDGE: &str = r#"
<script>
(function(){
  // Bridge runs at <body> start so any inline script in the page can
  // reference webstream.* immediately.
  window.webstream = {
    values: {},
    inputs: [],
    outputs: [],
    onUpdate: null,
    onOutputs: null,
    onTrigger: null,
    _connected: false,
    _pc: null,
    _dc: null,
    _outsKey: '',
    _stats: {audioPackets: 0, audioBytes: 0, videoPackets: 0, videoBytes: 0,
             audioMuted: null, audioTrack: null, videoTrack: null},
    set: function(name, value) {
      if (this._dc && this._dc.readyState === 'open') {
        this._dc.send(JSON.stringify({set: {[name]: value}}));
      }
    },
    send: function(obj) {
      if (this._dc && this._dc.readyState === 'open') {
        this._dc.send(JSON.stringify({set: obj}));
      }
    },
    trigger: function(name) {
      if (this._dc && this._dc.readyState === 'open') {
        this._dc.send(JSON.stringify({trigger: name}));
      }
    },
    /** Toggle the <video id="pwOut"> mute state. Pages wire a button
     *  with onclick="webstream.toggleAudio()" — the click counts as a
     *  user gesture, so unmuting is allowed by mobile browsers. The
     *  matching button text (#pwAudio) updates so the user sees state. */
    toggleAudio: function() {
      var v = document.getElementById('pwOut');
      if (!v) return;
      v.muted = !v.muted;
      if (!v.muted) v.play().catch(function(){});
      var b = document.getElementById('pwAudio');
      if (b) b.textContent = v.muted ? '\u{1F507} Audio OFF' : '\u{1F50A} Audio ON';
    },
    /** Programmatic audio control. Same gesture rules — must run from
     *  a user-initiated event for unmuting to take effect on mobile. */
    setAudio: function(on) {
      var v = document.getElementById('pwOut');
      if (!v) return;
      v.muted = !on;
      if (on) v.play().catch(function(){});
      var b = document.getElementById('pwAudio');
      if (b) b.textContent = on ? '\u{1F50A} Audio ON' : '\u{1F507} Audio OFF';
    },
  };

  async function _connect() {
    const pc = new RTCPeerConnection({iceServers: []}); // LAN only
    window.webstream._pc = pc;
    const dc = pc.createDataChannel("signals", {ordered: true});
    window.webstream._dc = dc;
    dc.onopen = () => { console.log("[Patchwork] DataChannel open"); };
    dc.onmessage = (e) => {
      try {
        const msg = JSON.parse(e.data);
        if (Array.isArray(msg.inputs)) {
          window.webstream.inputs = msg.inputs;
          const flat = {};
          for (const p of msg.inputs) flat[p.n] = p.v;
          window.webstream.values = flat;
          if (window.webstream.onUpdate) window.webstream.onUpdate(flat, msg.inputs);
        }
        if (Array.isArray(msg.outputs)) {
          const sig = msg.outputs.map(o => o.n + ":" + o.t).join("|");
          if (sig !== window.webstream._outsKey) {
            window.webstream._outsKey = sig;
            window.webstream.outputs = msg.outputs;
            if (window.webstream.onOutputs) window.webstream.onOutputs(msg.outputs);
          }
        }
        if (msg.trigger && window.webstream.onTrigger) {
          window.webstream.onTrigger(msg.trigger);
        }
      } catch(_) {}
    };

    pc.addTransceiver("audio", {direction: "recvonly"});
    pc.addTransceiver("video", {direction: "recvonly"});
    const remoteStream = new MediaStream();
    pc.ontrack = (e) => {
      remoteStream.addTrack(e.track);
      const kind = e.track.kind;
      console.log("[Patchwork] ontrack:", kind, "muted=" + e.track.muted);
      if (kind === "audio") window.webstream._stats.audioTrack = e.track;
      if (kind === "video") window.webstream._stats.videoTrack = e.track;
      const v = document.getElementById("pwOut");
      if (v) {
        // Re-bind srcObject every time. Some Android Chrome versions
        // need an explicit re-set when a second track is added to the
        // stream after srcObject was already assigned.
        v.srcObject = remoteStream;
        // Don't touch v.muted here. The page's Audio ON/OFF button is
        // the single source of truth for mute state — flipping it
        // mid-stream from this callback would surprise users who
        // already chose. Just (re)kick playback in case the new
        // track-add interrupted the previous play.
        v.play().catch(err => console.warn("[Patchwork] play() failed:", err));
      }
    };

    pc.onconnectionstatechange = () => {
      window.webstream._connected = pc.connectionState === "connected";
      console.log("[Patchwork] PC state:", pc.connectionState);
    };
    pc.oniceconnectionstatechange = () => {
      console.log("[Patchwork] ICE state:", pc.iceConnectionState);
    };
    pc.onsignalingstatechange = () => {
      console.log("[Patchwork] signaling:", pc.signalingState);
    };
    pc.onicecandidateerror = (e) => {
      console.warn("[Patchwork] ICE candidate error:", e.errorText || e);
    };

    const offer = await pc.createOffer();
    await pc.setLocalDescription(offer);
    await new Promise(resolve => {
      if (pc.iceGatheringState === "complete") resolve();
      else pc.addEventListener("icegatheringstatechange", () => {
        if (pc.iceGatheringState === "complete") resolve();
      });
    });
    const r = await fetch("/offer", {
      method: "POST",
      headers: {"Content-Type": "application/sdp"},
      body: pc.localDescription.sdp,
    });
    if (!r.ok) { console.error("[Patchwork] offer failed:", await r.text()); return; }
    const answer = await r.text();
    await pc.setRemoteDescription({type:"answer", sdp: answer});
    console.log("[Patchwork] WebRTC connection established");

    // Periodic receiver stats. Surfaces audio-track health so we can
    // tell at a glance whether silence is "no packets arriving" vs
    // "packets arriving but track muted by autoplay policy".
    setInterval(async () => {
      if (!pc || pc.connectionState !== "connected") return;
      try {
        const stats = await pc.getStats();
        let aPkt = 0, aByt = 0, vPkt = 0, vByt = 0;
        stats.forEach(s => {
          if (s.type === "inbound-rtp") {
            if (s.kind === "audio") { aPkt = s.packetsReceived || 0; aByt = s.bytesReceived || 0; }
            if (s.kind === "video") { vPkt = s.packetsReceived || 0; vByt = s.bytesReceived || 0; }
          }
        });
        const ws = window.webstream._stats;
        ws.audioPackets = aPkt; ws.audioBytes = aByt;
        ws.videoPackets = vPkt; ws.videoBytes = vByt;
        ws.audioMuted = ws.audioTrack ? ws.audioTrack.muted : null;
      } catch(_) {}
    }, 1000);
  }

  // Hot-reload: poll /hash and reload on change.
  let _wsH = "__HASH__";
  async function _reloadPoll() {
    try {
      const r = await fetch("/hash");
      const h = await r.text();
      if (h !== _wsH) { _wsH = h; location.reload(); }
    } catch(_) {}
    setTimeout(_reloadPoll, 1500);
  }

  // Auto-connect on page load. Video plays muted (autoplay policy
  // allows muted-autoplay without a gesture); audio is controlled by
  // the page's Audio ON/OFF toggle. The first toggle tap is the user
  // gesture that lets v.muted = false take effect on mobile.
  _connect().catch(e => console.error("[Patchwork] connect failed:", e));
  setTimeout(_reloadPoll, 2500);
})();
</script>
"#;

const DEFAULT_HTML: &str = r##"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Patchwork Web Stream</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>
  body { margin:0; padding:16px; font-family:-apple-system,system-ui,sans-serif; background:#0e0e1a; color:#eee; }
  h2 { margin:0 0 12px; font-size:18px; }
  .section { background:#1a1a2a; padding:12px 16px; border-radius:10px; margin:12px 0; }
  .val { color:#0ff; font-size:18px; font-family:monospace; }
  .row { display:flex; align-items:center; gap:12px; margin:8px 0; }
  .row label { min-width:60px; opacity:.8; }
  .row input[type=range] { flex:1; accent-color:#0fc; }
  .row input[type=text] { flex:1; padding:6px 8px; border:0; border-radius:6px; background:#0a0a14; color:#fff; font-family:monospace; }
  video { width:100%; display:block; background:#000; border-radius:8px; }
  pre { background:#0a0a14; padding:10px; border-radius:6px; font-size:11px; color:#8f8; overflow:auto; }
  small { opacity:.6; }
  #status { font-size:11px; padding:4px 8px; border-radius:4px; background:#222; display:inline-block; }
  #stats { font-size:11px; opacity:.7; font-family:monospace; margin-top:6px; }
  /* Audio ON/OFF toggle floating on the video. The first tap is the
     user gesture mobile browsers need before audio playback is
     allowed; subsequent taps just flip the mute state. */
  .vidwrap { position:relative; }
  #pwAudio { position:absolute; top:10px; right:10px;
             background:rgba(0,0,0,0.7); color:#fff;
             border:1px solid rgba(255,255,255,0.25);
             border-radius:999px; padding:8px 14px;
             font-size:13px; font-weight:600; cursor:pointer;
             -webkit-tap-highlight-color:transparent; }
  #pwAudio:active { background:rgba(0,0,0,0.85); }
</style></head>
<body>

<h2>Patchwork Web Stream <span id="status">connecting...</span></h2>

<div class="section">
  <small>Live a/v (Patchwork to Phone)</small>
  <div class="vidwrap">
    <video id="pwOut" autoplay playsinline muted></video>
    <button id="pwAudio" type="button" onclick="webstream.toggleAudio()">&#128263; Audio OFF</button>
  </div>
  <div id="stats">audio: -- pkt, video: -- pkt</div>
</div>

<div class="section">
  <small>Inputs (Patchwork to Phone)</small>
  <div id="vals"><small>(no inputs declared yet)</small></div>
</div>

<div class="section">
  <small>Outputs (Phone to Patchwork)</small>
  <div id="outs"><small>(no outputs declared yet)</small></div>
</div>

<div class="section">
  <small>API</small>
  <pre>webstream.values            // {p0:0.5, ...}
webstream.inputs            // [{n,t,v}, ...]
webstream.outputs           // [{n,t}, ...]
webstream.onUpdate = (vals, descriptors) =&gt; {}
webstream.onOutputs = (descriptors) =&gt; {}
webstream.set("name", 0.5)  // number / boolean / string
webstream.send({a:1, b:2})
</pre>
</div>

<script>
webstream.onUpdate = (_vals, descriptors) => {
  const esc = (s) => String(s).replace(/&/g,"&amp;").replace(/"/g,"&quot;")
                      .replace(/</g,"&lt;").replace(/>/g,"&gt;");
  const rows = (descriptors || []).map(p => {
    const n = esc(p.n);
    if (p.t === "b") {
      const lit = p.v ? "on" : "off";
      const col = p.v ? "#0fc" : "#666";
      return "<div class=\"row\"><label>" + n + "</label>"
        + "<span class=\"val\" style=\"color:" + col + "\">" + lit + "</span></div>";
    }
    if (p.t === "t") {
      return "<div class=\"row\"><label>" + n + "</label>"
        + "<span class=\"val\" style=\"font-size:14px\">\"" + esc(p.v) + "\"</span></div>";
    }
    return "<div class=\"row\"><label>" + n + "</label>"
      + "<span class=\"val\">" + (+p.v).toFixed(3) + "</span></div>";
  });
  document.getElementById("vals").innerHTML = rows.join("")
    || "<small>(no inputs declared yet)</small>";
};

webstream.onOutputs = (descriptors) => {
  const esc = (s) => String(s).replace(/&/g,"&amp;").replace(/"/g,"&quot;")
                      .replace(/</g,"&lt;").replace(/>/g,"&gt;");
  const safeId = (s) => s.replace(/[^a-zA-Z0-9_-]/g, "_");
  const rows = (descriptors || []).map(p => {
    const n = esc(p.n);
    const id = safeId(p.n);
    if (p.t === "b") {
      return "<div class=\"row\"><label>" + n + "</label>"
        + "<button style=\"flex:1;padding:10px;border:0;border-radius:6px;background:#0fc;color:#000;font-weight:600;\""
        + " onpointerdown=\"webstream.set(\'" + n + "\', true)\""
        + " onpointerup=\"webstream.set(\'" + n + "\', false)\""
        + " onpointerleave=\"webstream.set(\'" + n + "\', false)\">Press</button></div>";
    }
    if (p.t === "t") {
      return "<div class=\"row\"><label>" + n + "</label>"
        + "<input type=\"text\" placeholder=\"" + n + "\""
        + " oninput=\"webstream.set(\'" + n + "\', this.value)\"></div>";
    }
    return "<div class=\"row\"><label>" + n + "</label>"
      + "<input type=\"range\" min=\"0\" max=\"1\" step=\"0.01\" value=\"0\""
      + " oninput=\"webstream.set(\'" + n + "\', Number(this.value));"
      + " document.getElementById(\'v_" + id + "\').textContent=(+this.value).toFixed(2);\">"
      + "<span id=\"v_" + id + "\" class=\"val\" style=\"font-size:14px;\">0.00</span></div>";
  });
  document.getElementById("outs").innerHTML = rows.join("")
    || "<small>(no outputs declared yet)</small>";
};

setInterval(() => {
  const s = webstream._pc;
  document.getElementById("status").textContent =
    s ? s.connectionState : "connecting...";
  const st = webstream._stats;
  const muted = st.audioMuted === null ? "?" : (st.audioMuted ? "muted" : "live");
  document.getElementById("stats").textContent =
    "audio: " + st.audioPackets + " pkt / " + st.audioBytes + " B (" + muted + ")"
    + ", video: " + st.videoPackets + " pkt / " + st.videoBytes + " B";
}, 500);
</script>
</body></html>"##;

// ── Server state ─────────────────────────────────────────────────────────────

struct WebStreamServerState {
    html: String,
    html_hash: u64,
    port: u16,
    is_running: bool,
    input_values: Vec<f32>,
    input_names: Vec<String>,
    output_names: Vec<String>,
    /// Keyed by name → latest numeric value the page POSTed via
    /// DataChannel. Used for Float and Bool ports (Bool is 0.0/1.0).
    output_values: HashMap<String, f32>,
    /// Keyed by name → latest text value the page POSTed. Separate
    /// from `output_values` because PortValue::Float and PortValue::Text
    /// are different graph types.
    output_text_values: HashMap<String, String>,
    /// Manual reload kick. Incremented when the user clicks Refresh
    /// on the node UI; the /hash endpoint XORs this in so the phone's
    /// hot-reload poll observes a change and triggers `location.reload`.
    kick: u64,
}

type SharedServerState = Arc<Mutex<WebStreamServerState>>;

fn compute_hash(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// JSON string escape — sufficient for port name / value text that
/// flows through the per-tick DataChannel push. Handles `"` and `\`
/// (the only two chars that can break a quoted JSON string for
/// reasonable port names) plus control chars below 0x20.
fn push_json_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn start_http_server(state: SharedServerState, rtc: Arc<WebStreamShared>) -> u16 {
    let listener = match TcpListener::bind("0.0.0.0:0") {
        Ok(l) => l,
        Err(_) => match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => {
                crate::system_log::error(format!("Web Stream server bind failed: {}", e));
                return 0;
            }
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    if let Ok(mut s) = state.lock() { s.port = port; s.is_running = true; }
    crate::system_log::log(format!("Web Stream server started on port {}", port));

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            let n = match stream.read(&mut buf) { Ok(n) => n, Err(_) => continue };
            if n == 0 { continue; }
            let request = String::from_utf8_lossy(&buf[..n]).to_string();

            if request.starts_with("OPTIONS ") {
                let resp = "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nContent-Length: 0\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                continue;
            } else if request.starts_with("POST /offer") {
                // Body after \r\n\r\n. SDP can be larger than 8 KB on
                // very fancy candidates, but our LAN host-only case
                // fits comfortably under 4 KB.
                let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
                match rtc.handle_offer(body) {
                    Ok(answer) => {
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/sdp\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\n\r\n{}",
                            answer.len(), answer);
                        let _ = stream.write_all(resp.as_bytes());
                    }
                    Err(e) => {
                        let body = format!("offer error: {e}");
                        let resp = format!(
                            "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(), body);
                        let _ = stream.write_all(resp.as_bytes());
                    }
                }
                let _ = stream.flush();
                continue;
            } else if request.contains("GET /hash") {
                let hash = state.lock()
                    .map(|s| s.html_hash ^ s.kick.rotate_left(31))
                    .unwrap_or(0);
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
                // Serve full page with bridge injected at <body> start
                // (so eager inline scripts see webstream.* defined —
                // same fix as Web App's Phase 2.5 commit).
                let (html, hash) = state.lock()
                    .map(|s| (s.html.clone(), s.html_hash))
                    .unwrap_or_default();
                let bridge = PATCHWORK_RTC_BRIDGE.replace("__HASH__", &hash.to_string());
                let page = if html.is_empty() {
                    DEFAULT_HTML.replace("<body>", &format!("<body>{}", bridge))
                } else if html.contains("<body>") {
                    html.replace("<body>", &format!("<body>{}", bridge))
                } else if html.contains("</html>") {
                    html.replace("</html>", &format!("{}</html>", bridge))
                } else {
                    format!("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Web Stream</title>\
                        <style>body{{margin:0;padding:16px;font-family:sans-serif;background:#0e0e1a;color:#eee;}}</style>\
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

// ── Node ─────────────────────────────────────────────────────────────────────

pub struct WebStreamNode {
    pub input_names: Vec<String>,
    pub input_types: Vec<WebPortType>,
    pub output_names: Vec<String>,
    pub output_types: Vec<WebPortType>,
    pub html_code: String,
    server_port: u16,
    server_state: Option<SharedServerState>,
    input_values: Vec<f32>,
    /// Parallel to input_names — stores the latest text value per port
    /// (only meaningful when input_types[i] == Text).
    input_text_values: Vec<String>,
    output_cache: Vec<f32>,
    qr_texture: Option<(String, egui::TextureHandle)>,

    // ── Streaming runtime state ──
    rtc: Arc<WebStreamShared>,
    encoder: Option<H264Encoder>,
    encoder_dims: Option<(u32, u32)>,
    image_status: String,
    /// Opus pump — owns the SPSC ring buffer the AudioTapProcessor
    /// writes into. Spawned when the Audio port is wired; dropped on
    /// unwire.
    opus_pump: Option<OpusPump>,
    audio_status: String,
    /// True when we've registered an `AudioTapProcessor` with the
    /// engine. Used to call `remove_processor` on unwire.
    audio_registered: bool,
}

impl std::fmt::Debug for WebStreamNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebStreamNode")
            .field("input_names", &self.input_names)
            .field("output_names", &self.output_names)
            .field("server_port", &self.server_port)
            .finish()
    }
}

impl Clone for WebStreamNode {
    fn clone(&self) -> Self {
        Self {
            input_names: self.input_names.clone(),
            input_types: self.input_types.clone(),
            output_names: self.output_names.clone(),
            output_types: self.output_types.clone(),
            html_code: self.html_code.clone(),
            server_port: 0,
            server_state: None,
            input_values: self.input_values.clone(),
            input_text_values: self.input_text_values.clone(),
            output_cache: vec![0.0; self.output_names.len()],
            qr_texture: None,
            rtc: WebStreamShared::new(),
            encoder: None,
            encoder_dims: None,
            image_status: String::new(),
            opus_pump: None,
            audio_status: String::new(),
            audio_registered: false,
        }
    }
}

impl Default for WebStreamNode {
    fn default() -> Self {
        Self {
            input_names: vec!["p0".into(), "p1".into()],
            input_types: vec![WebPortType::Float, WebPortType::Float],
            output_names: vec!["o0".into(), "button".into()],
            // The default "button" output gets the matching Bool type
            // so the phone shows a button instead of a slider.
            output_types: vec![WebPortType::Float, WebPortType::Bool],
            html_code: String::new(),
            server_port: 0,
            server_state: None,
            input_values: vec![0.0; 2],
            input_text_values: vec![String::new(); 2],
            output_cache: vec![0.0; 2],
            qr_texture: None,
            rtc: WebStreamShared::new(),
            encoder: None,
            encoder_dims: None,
            image_status: String::new(),
            opus_pump: None,
            audio_status: String::new(),
            audio_registered: false,
        }
    }
}

impl NodeBehavior for WebStreamNode {
    fn title(&self)      -> &str   { "Web Stream [WIP]" }
    fn type_tag(&self)   -> &str   { "web_stream" }
    fn color_hint(&self) -> [u8;3] { [220, 110, 90] }
    fn min_width(&self)  -> Option<f32> { Some(260.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        let mut ports = vec![
            PortDef::new("HTML",  PortKind::Text),
            PortDef::new("Audio", PortKind::Audio),
            PortDef::new("Image", PortKind::Image),
        ];
        for (i, name) in self.input_names.iter().enumerate() {
            let ty = self.input_types.get(i).copied().unwrap_or(WebPortType::Float);
            ports.push(PortDef::dynamic(name.clone(), ty.port_kind()));
        }
        ports
    }

    fn outputs(&self) -> Vec<PortDef> {
        self.output_names.iter().enumerate()
            .map(|(i, n)| {
                let ty = self.output_types.get(i).copied().unwrap_or(WebPortType::Float);
                PortDef::dynamic(n.clone(), ty.port_kind())
            })
            .collect()
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        self.input_values.resize(self.input_names.len(), 0.0);
        self.input_text_values.resize(self.input_names.len(), String::new());
        // Dispatch on type. Float / Bool ports both store an f32; Text
        // ports store a String. Unwired ports keep their last value.
        for i in 0..self.input_names.len() {
            let port_idx = NUMERIC_BASE_IDX + i;
            let ty = self.input_types.get(i).copied().unwrap_or(WebPortType::Float);
            match ty {
                WebPortType::Float | WebPortType::Bool => {
                    if let Some(PortValue::Float(f)) = inputs.get(port_idx) {
                        self.input_values[i] = *f;
                    }
                }
                WebPortType::Text => {
                    if let Some(PortValue::Text(s)) = inputs.get(port_idx) {
                        self.input_text_values[i] = s.clone();
                    }
                }
            }
        }

        // Image input → H.264 encoder. Mirrors web_app_node.rs Phase 2.
        match inputs.get(IMAGE_PORT_IDX) {
            Some(PortValue::Image(img)) => {
                let dims = (img.width, img.height);
                if self.encoder_dims != Some(dims) {
                    self.encoder = None;
                    let rtc = self.rtc.clone();
                    let sink: NalSink = Arc::new(move |nal: &[u8]| {
                        rtc.write_video_nal(nal);
                    });
                    match H264Encoder::new(dims.0, dims.1, sink) {
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
                        insert a Crop/Resize/etc. before Web Stream".into();
                }
                self.encoder = None;
                self.encoder_dims = None;
            }
            _ => { /* unwired or no value yet; render handles drop */ }
        }

        // Push every input + the output port descriptor list to the
        // page via DataChannel. Format:
        //   {
        //     "inputs":  [{"n":"vol","t":"f","v":0.5},
        //                 {"n":"on","t":"b","v":true},
        //                 {"n":"label","t":"t","v":"hi"}, ...],
        //     "outputs": [{"n":"o0","t":"f"}, {"n":"btn","t":"b"}, ...]
        //   }
        // Single-letter `n / t / v` keys keep the per-tick payload
        // small. The JS bridge dispatches UI on `t` (type letter).
        if self.rtc.status() == PeerStatus::Connected {
            let mut json = String::from(r#"{"inputs":["#);
            for (i, name) in self.input_names.iter().enumerate() {
                if i > 0 { json.push(','); }
                let ty = self.input_types.get(i).copied().unwrap_or(WebPortType::Float);
                json.push_str(r#"{"n":""#);
                push_json_escaped(&mut json, name);
                json.push_str(r#"","t":""#);
                json.push_str(ty.as_letter());
                json.push_str(r#"","v":"#);
                match ty {
                    WebPortType::Float => {
                        let v = self.input_values.get(i).copied().unwrap_or(0.0);
                        json.push_str(&format!("{:.6}", v));
                    }
                    WebPortType::Bool => {
                        let v = self.input_values.get(i).copied().unwrap_or(0.0);
                        json.push_str(if v > 0.5 { "true" } else { "false" });
                    }
                    WebPortType::Text => {
                        json.push('"');
                        let empty = String::new();
                        let s = self.input_text_values.get(i).unwrap_or(&empty);
                        push_json_escaped(&mut json, s);
                        json.push('"');
                    }
                }
                json.push('}');
            }
            json.push_str(r#"],"outputs":["#);
            for (i, name) in self.output_names.iter().enumerate() {
                if i > 0 { json.push(','); }
                let ty = self.output_types.get(i).copied().unwrap_or(WebPortType::Float);
                json.push_str(r#"{"n":""#);
                push_json_escaped(&mut json, name);
                json.push_str(r#"","t":""#);
                json.push_str(ty.as_letter());
                json.push_str(r#""}"#);
            }
            json.push_str("]}");
            self.rtc.send_signal(json);
        }

        // Drain inbound DC messages → output_values / output_text_values.
        let mut emitted: Vec<(usize, PortValue)> = Vec::new();
        self.output_cache.resize(self.output_names.len(), 0.0);
        if let Some(ref state) = self.server_state {
            while let Ok(msg) = self.rtc.in_rx.try_recv() {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&msg) {
                    if let Some(set) = json.get("set").and_then(|v| v.as_object()) {
                        if let Ok(mut s) = state.lock() {
                            for (k, v) in set {
                                if let Some(f) = v.as_f64() {
                                    s.output_values.insert(k.clone(), f as f32);
                                } else if let Some(b) = v.as_bool() {
                                    s.output_values.insert(k.clone(),
                                        if b { 1.0 } else { 0.0 });
                                } else if let Some(t) = v.as_str() {
                                    s.output_text_values.insert(k.clone(), t.to_string());
                                }
                            }
                        }
                    }
                }
            }
            if let Ok(mut s) = state.lock() {
                s.input_values = self.input_values.clone();
                s.input_names = self.input_names.clone();
                s.output_names = self.output_names.clone();
                for (i, name) in self.output_names.iter().enumerate() {
                    let ty = self.output_types.get(i).copied().unwrap_or(WebPortType::Float);
                    match ty {
                        WebPortType::Float | WebPortType::Bool => {
                            let v = s.output_values.get(name).copied().unwrap_or(0.0);
                            self.output_cache[i] = v;
                            emitted.push((i, PortValue::Float(v)));
                        }
                        WebPortType::Text => {
                            let t = s.output_text_values.get(name).cloned().unwrap_or_default();
                            self.output_cache[i] = 0.0; // unused for text ports
                            emitted.push((i, PortValue::Text(t)));
                        }
                    }
                }

                // HTML port 0 overrides inline editor.
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
        let amber = egui::Color32::from_rgb(220, 160, 80);

        // ── Lazy server startup ──────────────────────────────────────────
        if self.server_state.is_none() {
            let state = Arc::new(Mutex::new(WebStreamServerState {
                html: self.html_code.clone(),
                html_hash: compute_hash(&self.html_code),
                port: 0,
                is_running: false,
                input_values: self.input_values.clone(),
                input_names: self.input_names.clone(),
                output_names: self.output_names.clone(),
                output_values: HashMap::new(),
                output_text_values: HashMap::new(),
                kick: 0,
            }));
            self.server_port = start_http_server(state.clone(), self.rtc.clone());
            self.server_state = Some(state);
        }

        // ── Encoder lifecycle: drop on Image unwire ──────────────────────
        let image_wired = ctx.connections.iter()
            .any(|c| c.to_node == node_id && c.to_port == IMAGE_PORT_IDX);
        if !image_wired && self.encoder.is_some() {
            self.encoder = None;
            self.encoder_dims = None;
            self.image_status.clear();
        }

        // ── Audio port lifecycle: register / unregister tap processor ────
        let audio_wired = ctx.connections.iter()
            .any(|c| c.to_node == node_id && c.to_port == AUDIO_PORT_IDX);
        if audio_wired && !self.audio_registered {
            // Spawn opus pump first — gives us the SPSC ring buffer
            // the processor will write into.
            let rtc = self.rtc.clone();
            let sink: PacketSink = Arc::new(move |pkt: &[u8], dur: Duration| {
                rtc.write_audio_packet(pkt, dur);
            });
            let engine_sr = ctx.audio.engine_sample_rate as u32;
            match OpusPump::start(engine_sr, sink) {
                Ok(pump) => {
                    let processor = Box::new(AudioTapProcessor::new(pump.buffer.clone()));
                    ctx.audio.add_processor(node_id, processor, 0);
                    ctx.audio.pending_connection_sync.insert(node_id);
                    self.opus_pump = Some(pump);
                    self.audio_registered = true;
                    self.audio_status.clear();
                }
                Err(e) => {
                    self.audio_status = e;
                }
            }
        } else if !audio_wired && self.audio_registered {
            ctx.audio.remove_processor(node_id);
            self.opus_pump = None;
            self.audio_registered = false;
            self.audio_status.clear();
        }

        // ── Status + URLs (mirror Web App) ───────────────────────────────
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
            let status = self.rtc.status();
            let (dot_color, label) = match status {
                PeerStatus::Connected   => (green, "● Connected"),
                PeerStatus::Connecting  => (amber, "● Connecting…"),
                PeerStatus::Negotiating => (amber, "● Negotiating…"),
                PeerStatus::Failed      => (egui::Color32::from_rgb(220, 80, 80), "● Failed"),
                PeerStatus::Disconnected=> (dim,   "○ Disconnected"),
                PeerStatus::Idle        => (if self.server_port > 0 { green } else { dim },
                                            if self.server_port > 0 { "● Ready" } else { "○ Stopped" }),
            };
            ui.label(RichText::new(label).small().color(dot_color));
            if !primary_url.is_empty() {
                ui.label(RichText::new(&primary_url).small().monospace()
                    .color(egui::Color32::from_rgb(100, 220, 200)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("⤴").on_hover_text("Open in browser").clicked()
                    && !primary_url.is_empty()
                {
                    open_in_browser(&primary_url);
                }
                if ui.small_button("📋").on_hover_text("Copy URL").clicked()
                    && !primary_url.is_empty()
                {
                    ui.ctx().copy_text(primary_url.clone());
                }
                if ui.small_button("↻")
                    .on_hover_text("Refresh: close peer + reload phone page")
                    .clicked()
                {
                    // Close the WebRTC peer; encoder/pump keep producing
                    // (next phone offer picks up output mid-stream).
                    self.rtc.close_peer_sync();
                    // Bump /hash so the phone's hot-reload poll triggers
                    // a full page reload, which creates a fresh offer.
                    if let Some(ref state) = self.server_state {
                        if let Ok(mut s) = state.lock() {
                            s.kick = s.kick.wrapping_add(1);
                        }
                    }
                }
            });
        });
        if lan_url.is_some() && !loopback_url.is_empty() {
            ui.label(RichText::new(&loopback_url).small().monospace().color(dim));
        }

        // QR
        if !primary_url.is_empty() {
            let cache_hit = self.qr_texture.as_ref()
                .map(|(u, _)| u == &primary_url).unwrap_or(false);
            if !cache_hit {
                if let Some(img) = url_qr_image(&primary_url) {
                    let handle = ui.ctx().load_texture(
                        format!("webstream_qr_{}", node_id),
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

        // ── HTML port row ────────────────────────────────────────────────
        let html_connected = ctx.connections.iter()
            .any(|c| c.to_node == node_id && c.to_port == HTML_PORT_IDX);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, HTML_PORT_IDX, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from,
                ctx.pending_disconnects, PortKind::Text);
            ui.label(RichText::new(if html_connected { "HTML ✓" } else { "HTML" }).small()
                .color(if html_connected { green } else { dim }));
        });

        // ── Audio port row ───────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, AUDIO_PORT_IDX, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from,
                ctx.pending_disconnects, PortKind::Audio);
            let (label, color) = if !self.audio_status.is_empty() {
                ("Audio ⚠", amber)
            } else if self.audio_registered {
                ("Audio ✓ → Opus", green)
            } else {
                ("Audio", dim)
            };
            ui.label(RichText::new(label).small().color(color));
        });
        if !self.audio_status.is_empty() {
            ui.label(RichText::new(&self.audio_status).small().color(amber));
        }

        // ── Image port row ───────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, IMAGE_PORT_IDX, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from,
                ctx.pending_disconnects, PortKind::Image);
            let (label, color) = if !self.image_status.is_empty() {
                ("Image ⚠", amber)
            } else if self.encoder.is_some() {
                ("Image ✓ → H.264", green)
            } else if image_wired {
                ("Image …", dim)
            } else {
                ("Image", dim)
            };
            ui.label(RichText::new(label).small().color(color));
            if let Some((w, h)) = self.encoder_dims {
                ui.label(RichText::new(format!("{}×{}", w, h)).small().monospace().color(dim));
            }
        });
        if !self.image_status.is_empty() {
            ui.label(RichText::new(&self.image_status).small().color(amber));
        }

        // Keep type Vecs in step with name Vecs (load_state may have
        // left them shorter; new names just default to Float).
        while self.input_types.len() < self.input_names.len() {
            self.input_types.push(WebPortType::Float);
        }
        self.input_types.truncate(self.input_names.len());
        while self.input_text_values.len() < self.input_names.len() {
            self.input_text_values.push(String::new());
        }
        self.input_text_values.truncate(self.input_names.len());
        while self.output_types.len() < self.output_names.len() {
            self.output_types.push(WebPortType::Float);
        }
        self.output_types.truncate(self.output_names.len());

        // ── Dynamic input ports ──────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Inputs").small().strong().color(dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(RichText::new("+").color(green)).clicked() {
                    let idx = self.input_names.len();
                    self.input_names.push(format!("p{}", idx));
                    self.input_types.push(WebPortType::Float);
                    self.input_values.push(0.0);
                    self.input_text_values.push(String::new());
                }
            });
        });
        let mut remove_idx: Option<usize> = None;
        let mut type_changed: Option<(usize, WebPortType)> = None;
        for i in 0..self.input_names.len() {
            let port_idx = NUMERIC_BASE_IDX + i;
            let port_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == port_idx);
            let ty = self.input_types.get(i).copied().unwrap_or(WebPortType::Float);
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, node_id, port_idx, true,
                    ctx.connections, ctx.port_positions, ctx.dragging_from,
                    ctx.pending_disconnects, ty.port_kind());
                ui.add(egui::TextEdit::singleline(&mut self.input_names[i])
                    .desired_width(48.0).font(egui::TextStyle::Small));
                let mut new_ty = ty;
                egui::ComboBox::from_id_salt(egui::Id::new(("ws_in_ty", node_id, i)))
                    .selected_text(ty.as_letter())
                    .width(28.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut new_ty, WebPortType::Float, WebPortType::Float.label());
                        ui.selectable_value(&mut new_ty, WebPortType::Text,  WebPortType::Text.label());
                        ui.selectable_value(&mut new_ty, WebPortType::Bool,  WebPortType::Bool.label());
                    });
                if new_ty != ty { type_changed = Some((i, new_ty)); }
                // Inline value preview / manual editor based on type.
                match ty {
                    WebPortType::Float => {
                        if port_wired {
                            let v = self.input_values.get(i).copied().unwrap_or(0.0);
                            ui.label(RichText::new(format!("{:.2}", v)).small().monospace()
                                .color(egui::Color32::from_rgb(100, 180, 240)));
                        } else if i < self.input_values.len() {
                            ui.add(egui::DragValue::new(&mut self.input_values[i])
                                .speed(0.01).range(0.0..=1.0).max_decimals(2));
                        }
                    }
                    WebPortType::Bool => {
                        let mut on = self.input_values.get(i).copied().unwrap_or(0.0) > 0.5;
                        if ui.checkbox(&mut on, "").changed() {
                            if i < self.input_values.len() {
                                self.input_values[i] = if on { 1.0 } else { 0.0 };
                            }
                        }
                    }
                    WebPortType::Text => {
                        if i < self.input_text_values.len() {
                            ui.add(egui::TextEdit::singleline(&mut self.input_text_values[i])
                                .desired_width(60.0).font(egui::TextStyle::Small));
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.input_names.len() > 1
                        && ui.small_button(RichText::new("−")
                            .color(egui::Color32::from_rgb(220, 80, 80))).clicked()
                    {
                        remove_idx = Some(i);
                    }
                });
            });
        }
        if let Some((i, new_ty)) = type_changed {
            self.input_types[i] = new_ty;
            // Port kind changed → drop any wire on this slot so the
            // user doesn't end up with a Number-source on a Text port.
            ctx.pending_disconnects.push((node_id, NUMERIC_BASE_IDX + i));
        }
        if let Some(idx) = remove_idx {
            self.input_names.remove(idx);
            if idx < self.input_values.len() { self.input_values.remove(idx); }
            if idx < self.input_text_values.len() { self.input_text_values.remove(idx); }
            if idx < self.input_types.len() { self.input_types.remove(idx); }
            let port_idx = NUMERIC_BASE_IDX + idx;
            let old_max = NUMERIC_BASE_IDX + self.input_names.len(); // pre-remove last
            for p in port_idx..=old_max {
                ctx.pending_disconnects.push((node_id, p));
            }
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Dynamic output ports ─────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("Outputs").small().strong().color(dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button(RichText::new("+").color(green)).clicked() {
                    let idx = self.output_names.len();
                    self.output_names.push(format!("o{}", idx));
                    self.output_types.push(WebPortType::Float);
                    self.output_cache.push(0.0);
                }
            });
        });
        let mut remove_out_idx: Option<usize> = None;
        let mut out_type_changed: Option<(usize, WebPortType)> = None;
        for i in 0..self.output_names.len() {
            let ty = self.output_types.get(i).copied().unwrap_or(WebPortType::Float);
            ui.horizontal(|ui| {
                crate::nodes::inline_port_circle(ui, node_id, i, false,
                    ctx.connections, ctx.port_positions, ctx.dragging_from,
                    ctx.pending_disconnects, ty.port_kind());
                ui.add(egui::TextEdit::singleline(&mut self.output_names[i])
                    .desired_width(60.0).font(egui::TextStyle::Small));
                let mut new_ty = ty;
                egui::ComboBox::from_id_salt(egui::Id::new(("ws_out_ty", node_id, i)))
                    .selected_text(ty.as_letter())
                    .width(28.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut new_ty, WebPortType::Float, WebPortType::Float.label());
                        ui.selectable_value(&mut new_ty, WebPortType::Text,  WebPortType::Text.label());
                        ui.selectable_value(&mut new_ty, WebPortType::Bool,  WebPortType::Bool.label());
                    });
                if new_ty != ty { out_type_changed = Some((i, new_ty)); }
                // Live value preview from whatever the phone sent.
                match ty {
                    WebPortType::Float | WebPortType::Bool => {
                        let v = self.output_cache.get(i).copied().unwrap_or(0.0);
                        ui.label(RichText::new(format!("{:.2}", v)).small().monospace()
                            .color(egui::Color32::from_rgb(100, 220, 200)));
                    }
                    WebPortType::Text => {
                        let preview = self.server_state.as_ref()
                            .and_then(|s| s.lock().ok()
                                .and_then(|g| g.output_text_values.get(&self.output_names[i]).cloned()))
                            .unwrap_or_default();
                        let trim = if preview.chars().count() > 16 {
                            let head: String = preview.chars().take(16).collect();
                            format!("{}…", head)
                        } else { preview };
                        ui.label(RichText::new(format!("\"{}\"", trim)).small().monospace()
                            .color(egui::Color32::from_rgb(100, 220, 200)));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(RichText::new("−")
                        .color(egui::Color32::from_rgb(220, 80, 80))).clicked()
                    {
                        remove_out_idx = Some(i);
                    }
                });
            });
        }
        if let Some((i, new_ty)) = out_type_changed {
            self.output_types[i] = new_ty;
            // Port kind changed → drop any wire on this output slot.
            ctx.pending_disconnects.push((node_id, i));
        }
        if let Some(idx) = remove_out_idx {
            self.output_names.remove(idx);
            if idx < self.output_cache.len() { self.output_cache.remove(idx); }
            if idx < self.output_types.len() { self.output_types.remove(idx); }
            for p in idx..self.output_names.len() + 1 {
                ctx.pending_disconnects.push((node_id, p));
            }
        }

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── HTML editor ──────────────────────────────────────────────────
        if !html_connected {
            ui.label(RichText::new("HTML").small().strong().color(dim));
            let changed = egui::ScrollArea::vertical()
                .id_salt(egui::Id::new(("webstream_code", node_id)))
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
        } else {
            ui.label(RichText::new("HTML from input ✓").small().color(green));
        }
    }

    fn save_state(&self) -> serde_json::Value {
        let in_types: Vec<&str> = self.input_types.iter().map(|t| t.as_letter()).collect();
        let out_types: Vec<&str> = self.output_types.iter().map(|t| t.as_letter()).collect();
        serde_json::json!({
            "input_names": self.input_names,
            "input_types": in_types,
            "output_names": self.output_names,
            "output_types": out_types,
            "html_code": self.html_code,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(names) = state.get("input_names").and_then(|v| v.as_array()) {
            self.input_names = names.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            self.input_values = vec![0.0; self.input_names.len()];
            self.input_text_values = vec![String::new(); self.input_names.len()];
        }
        // input_types defaults to all Float when missing (old saves
        // from before the type system landed). Only the parallel
        // length is enforced here; render_with_context truncates /
        // pads as the names Vec grows.
        if let Some(types) = state.get("input_types").and_then(|v| v.as_array()) {
            self.input_types = types.iter()
                .filter_map(|v| v.as_str().map(WebPortType::from_letter))
                .collect();
        } else {
            self.input_types = vec![WebPortType::Float; self.input_names.len()];
        }
        if let Some(names) = state.get("output_names").and_then(|v| v.as_array()) {
            self.output_names = names.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            self.output_cache = vec![0.0; self.output_names.len()];
        }
        if let Some(types) = state.get("output_types").and_then(|v| v.as_array()) {
            self.output_types = types.iter()
                .filter_map(|v| v.as_str().map(WebPortType::from_letter))
                .collect();
        } else {
            // Old saves: keep the historical default (last is Bool if
            // it's named exactly "button", everything else Float).
            self.output_types = self.output_names.iter()
                .map(|n| if n == "button" { WebPortType::Bool } else { WebPortType::Float })
                .collect();
        }
        if let Some(html) = state.get("html_code").and_then(|v| v.as_str()) {
            self.html_code = html.to_string();
        }
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("web_stream", |state| {
        let mut n = WebStreamNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
