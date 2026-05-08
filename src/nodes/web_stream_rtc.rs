//! WebRTC peer connection + tokio runtime bridge for the Web Stream
//! node.
//!
//! Architecture:
//! - One process-wide `tokio::Runtime` (multi-thread, lazy-init) hosts
//!   every WebRTC peer. Cheaper than per-peer runtimes; each Web
//!   Stream node still gets its own `WebStreamShared` and its own
//!   peer.
//! - `WebStreamShared` is the cross-thread state hub: held by the
//!   node, by the HTTP server (signaling), by the H264 encoder
//!   thread (video NAL sink), and by the Opus pump (audio packet
//!   sink). It exposes synchronous methods that internally
//!   `runtime.spawn(...)` the actual webrtc calls.
//! - Inbound DataChannel messages are written into a
//!   `crossbeam_channel::Sender` from inside an async handler; the
//!   node drains them synchronously each evaluate() tick via
//!   `try_recv`.
//!
//! Single peer at a time. A new POST /offer (e.g. phone reload)
//! tears down the existing peer cleanly before negotiating the new
//! one. The encoder/pump don't get torn down — they keep producing
//! and the new peer just picks up output from the next sample.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::runtime::Runtime;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::APIBuilder;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;
#[allow(unused_imports)]
use webrtc::rtp_transceiver::RTCRtpTransceiver;

/// Process-wide tokio runtime. Lazy-initialised on first use.
fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        Runtime::new().expect("failed to create tokio runtime for WebRTC")
    })
}

/// Strip `a=candidate:` lines that webrtc-rs 0.17 can't parse — those
/// with fewer than 8 whitespace-separated tokens after the prefix.
/// Standard candidates have ≥ 8 tokens (`foundation component
/// transport priority address port typ <type>`); webrtc-rs rejects
/// the entire SDP if any single candidate trips its parser, even if
/// the rest are well-formed. Chrome on Android empirically emits an
/// occasional 5-token line — we drop it and keep going.
///
/// Returns `(cleaned_sdp, dropped_lines)` so the caller can surface
/// what was discarded for debugging.
pub(crate) fn sanitize_offer_sdp(offer: &str) -> (String, Vec<String>) {
    let mut clean: Vec<String> = Vec::with_capacity(offer.lines().count());
    let mut dropped: Vec<String> = Vec::new();
    for line in offer.lines() {
        // Both `a=candidate:` (in-SDP) and bare `candidate:` lines
        // (rare but legal in some SDP fragments).
        let value = line.strip_prefix("a=candidate:")
            .or_else(|| line.strip_prefix("candidate:"));
        if let Some(rest) = value {
            let tokens: Vec<&str> = rest.split_whitespace().collect();
            if tokens.len() < 8 {
                dropped.push(line.to_string());
                continue;
            }
        }
        clean.push(line.to_string());
    }
    // Preserve CRLF line endings — SDP-over-HTTP commonly uses CRLF.
    let mut joined = clean.join("\r\n");
    joined.push_str("\r\n");
    (joined, dropped)
}

/// Shared cross-thread state for a single Web Stream node. Cloning is
/// `Arc<Self>::clone` — cheap.
pub struct WebStreamShared {
    /// Latest peer connection. `None` when no phone is connected.
    pc: Mutex<Option<Arc<RTCPeerConnection>>>,
    /// Outgoing video track. Same Arc handed to the H.264 encoder.
    /// `None` until the first /offer.
    video_track: Mutex<Option<Arc<TrackLocalStaticSample>>>,
    /// Outgoing audio track. Same Arc handed to the Opus pump.
    audio_track: Mutex<Option<Arc<TrackLocalStaticSample>>>,
    /// DataChannel for outbound signal pushes.
    dc: Mutex<Option<Arc<RTCDataChannel>>>,
    /// Inbound DataChannel messages queued for the node to drain.
    in_tx: crossbeam_channel::Sender<String>,
    pub in_rx: crossbeam_channel::Receiver<String>,
    /// Latest peer-connection state, surfaced to the node UI.
    pub state: Mutex<PeerStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerStatus {
    Idle,
    Negotiating,
    Connecting,
    Connected,
    Failed,
    Disconnected,
}

impl WebStreamShared {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        Arc::new(Self {
            pc: Mutex::new(None),
            video_track: Mutex::new(None),
            audio_track: Mutex::new(None),
            dc: Mutex::new(None),
            in_tx: tx,
            in_rx: rx,
            state: Mutex::new(PeerStatus::Idle),
        })
    }

    /// Handle a SDP offer (POST /offer body). Synchronous wrapper
    /// around the async dance: kill any existing peer, create a new
    /// one with audio + video tracks, set remote desc, generate
    /// answer, wait for ICE gathering, return final SDP.
    pub fn handle_offer(self: &Arc<Self>, offer_sdp: &str) -> Result<String, String> {
        let this = self.clone();
        let offer_str = offer_sdp.to_string();
        runtime().block_on(async move {
            this.handle_offer_async(offer_str).await
        })
    }

    async fn handle_offer_async(self: &Arc<Self>, offer_sdp: String) -> Result<String, String> {
        crate::system_log::log(format!(
            "Web Stream /offer received ({} bytes)", offer_sdp.len()
        ));
        // Tear down old peer first.
        self.close_peer_async().await;
        *self.state.lock().unwrap() = PeerStatus::Negotiating;

        // ── Build API with H264 + Opus codecs ────────────────────
        let mut media_engine = MediaEngine::default();
        // Opus 48k mono.
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "audio/opus".into(),
                    clock_rate: 48000,
                    channels: 2,
                    sdp_fmtp_line: "minptime=10;useinbandfec=1".into(),
                    rtcp_feedback: vec![],
                },
                payload_type: 111,
                ..Default::default()
            },
            RTPCodecType::Audio,
        ).map_err(|e| format!("register opus: {e}"))?;
        // H264 baseline. Browsers all advertise this profile.
        media_engine.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: "video/H264".into(),
                    clock_rate: 90000,
                    channels: 0,
                    sdp_fmtp_line:
                        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                            .into(),
                    rtcp_feedback: vec![],
                },
                payload_type: 102,
                ..Default::default()
            },
            RTPCodecType::Video,
        ).map_err(|e| format!("register h264: {e}"))?;

        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)
            .map_err(|e| format!("register interceptors: {e}"))?;

        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build();

        // ── Configuration. Host candidates only — LAN mode. ──────
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer::default()],
            ..Default::default()
        };

        let pc = Arc::new(api.new_peer_connection(config).await
            .map_err(|e| format!("new_peer_connection: {e}"))?);

        // ── Tracks ────────────────────────────────────────────────
        let video_track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: "video/H264".into(),
                clock_rate: 90000,
                ..Default::default()
            },
            "patchwork-video".into(),
            "patchwork-stream".into(),
        ));
        let audio_track = Arc::new(TrackLocalStaticSample::new(
            RTCRtpCodecCapability {
                mime_type: "audio/opus".into(),
                clock_rate: 48000,
                channels: 2,
                ..Default::default()
            },
            "patchwork-audio".into(),
            "patchwork-stream".into(),
        ));

        let _video_sender = pc.add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
            .await
            .map_err(|e| format!("add_track video: {e}"))?;
        let _audio_sender = pc.add_track(Arc::clone(&audio_track) as Arc<dyn TrackLocal + Send + Sync>)
            .await
            .map_err(|e| format!("add_track audio: {e}"))?;

        // ── DataChannel — listen for the one the browser created ──
        // Closures capture `Arc<Self>` clones; field access via `.dc`
        // etc. avoids needing per-field Arcs.
        let dc_shared = self.clone();
        pc.on_data_channel(Box::new(move |dc: Arc<RTCDataChannel>| {
            let shared_outer = dc_shared.clone();
            Box::pin(async move {
                if let Ok(mut g) = shared_outer.dc.lock() {
                    *g = Some(dc.clone());
                }
                let shared_msg = shared_outer.clone();
                dc.on_message(Box::new(move |msg: DataChannelMessage| {
                    let shared_msg = shared_msg.clone();
                    Box::pin(async move {
                        if let Ok(s) = String::from_utf8(msg.data.to_vec()) {
                            let _ = shared_msg.in_tx.send(s);
                        }
                    })
                }));
            })
        }));

        // ── Connection state → status mutex ──────────────────────
        //
        // CRITICAL: only clear track slots on `Failed` / `Closed`.
        // `Disconnected` is TRANSIENT — WebRTC's ICE / DTLS layer is
        // reporting "we lost packets recently, will probably recover".
        // If we null the tracks here, the encoder + Opus pump go silent
        // and ICE consent-freshness checks have no media to keep alive,
        // so the peer never recovers and bounces between Disconnected
        // and Connecting forever (the symptom the user saw the moment
        // audio was wired and added enough RTP load to trip a single
        // missed heartbeat).
        let state_shared = self.clone();
        pc.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
            let shared = state_shared.clone();
            Box::pin(async move {
                let new_status = match s {
                    RTCPeerConnectionState::New
                    | RTCPeerConnectionState::Connecting => PeerStatus::Connecting,
                    RTCPeerConnectionState::Connected => PeerStatus::Connected,
                    RTCPeerConnectionState::Disconnected => PeerStatus::Disconnected,
                    RTCPeerConnectionState::Failed => PeerStatus::Failed,
                    RTCPeerConnectionState::Closed => PeerStatus::Disconnected,
                    _ => PeerStatus::Idle,
                };
                *shared.state.lock().unwrap() = new_status;
                crate::system_log::log(format!("Web Stream peer state: {:?}", s));
                if matches!(s,
                    RTCPeerConnectionState::Failed
                    | RTCPeerConnectionState::Closed,
                ) {
                    // Terminal — drop tracks so the encoder / pump stop
                    // hammering on a dead peer. Disconnected does NOT
                    // qualify; that's transient.
                    *shared.pc.lock().unwrap() = None;
                    *shared.video_track.lock().unwrap() = None;
                    *shared.audio_track.lock().unwrap() = None;
                    *shared.dc.lock().unwrap() = None;
                }
            })
        }));

        // Surface ICE + signaling state changes to the system log so
        // when the connection misbehaves the user can copy a window
        // out of the log and we can correlate it with what the browser
        // shows.
        pc.on_ice_connection_state_change(Box::new(|s| {
            Box::pin(async move {
                crate::system_log::log(format!("Web Stream ICE state: {:?}", s));
            })
        }));
        pc.on_signaling_state_change(Box::new(|s| {
            Box::pin(async move {
                crate::system_log::log(format!("Web Stream signaling state: {:?}", s));
            })
        }));

        // ── SDP dance ────────────────────────────────────────────
        //
        // Chrome on Android occasionally emits non-standard
        // `a=candidate:` lines that webrtc-rs 0.17 rejects with
        // `ErrAttributeTooShortIceCandidate`, failing the whole
        // `set_remote_description` instead of just ignoring the bad
        // candidate. Pre-strip those so the offer parses; the
        // remaining candidates are still enough to negotiate on LAN.
        let (cleaned_offer, dropped) = sanitize_offer_sdp(&offer_sdp);
        if !dropped.is_empty() {
            crate::system_log::warn(format!(
                "Web Stream: dropped {} malformed ICE candidate(s) from offer:\n  {}",
                dropped.len(),
                dropped.join("\n  ")
            ));
        }
        let offer = RTCSessionDescription::offer(cleaned_offer.clone())
            .map_err(|e| {
                crate::system_log::error(format!(
                    "Web Stream offer parse failed: {e}\n--- offer SDP ---\n{cleaned_offer}\n--- end ---"
                ));
                format!("parse offer: {e}")
            })?;
        pc.set_remote_description(offer).await
            .map_err(|e| {
                crate::system_log::error(format!(
                    "Web Stream set_remote_description failed: {e}\n--- offer SDP ---\n{cleaned_offer}\n--- end ---"
                ));
                format!("set_remote: {e}")
            })?;

        // Log how the sender-side senders ended up after merge so we
        // can correlate with browser-side packet stats if audio or
        // video misbehaves. add_track + set_remote_description should
        // bind both kinds to the matching offer m-lines automatically.
        let txs = pc.get_transceivers().await;
        let mut audio_kinds = 0usize;
        let mut video_kinds = 0usize;
        for tx in txs.iter() {
            match tx.kind() {
                RTPCodecType::Audio => audio_kinds += 1,
                RTPCodecType::Video => video_kinds += 1,
                _ => {}
            }
        }
        crate::system_log::log(format!(
            "Web Stream transceivers — audio: {}, video: {}, total: {}",
            audio_kinds, video_kinds, txs.len()
        ));

        let answer = pc.create_answer(None).await
            .map_err(|e| format!("create_answer: {e}"))?;
        let mut gather_complete = pc.gathering_complete_promise().await;
        pc.set_local_description(answer).await
            .map_err(|e| format!("set_local: {e}"))?;
        let _ = gather_complete.recv().await;
        let final_sdp = pc.local_description().await
            .ok_or_else(|| "no local description after gathering".to_string())?;

        // ── Publish handles for sync writers ─────────────────────
        *self.pc.lock().unwrap() = Some(Arc::clone(&pc));
        *self.video_track.lock().unwrap() = Some(Arc::clone(&video_track));
        *self.audio_track.lock().unwrap() = Some(Arc::clone(&audio_track));

        Ok(final_sdp.sdp)
    }

    async fn close_peer_async(&self) {
        let pc_arc = self.pc.lock().unwrap().take();
        *self.video_track.lock().unwrap() = None;
        *self.audio_track.lock().unwrap() = None;
        *self.dc.lock().unwrap() = None;
        *self.state.lock().unwrap() = PeerStatus::Idle;
        if let Some(pc) = pc_arc {
            let _ = pc.close().await;
        }
    }

    /// Synchronously close the peer. Called on node drop.
    pub fn close_peer_sync(&self) {
        runtime().block_on(self.close_peer_async());
    }

    /// Push one H.264 NAL to the active video track. Fire-and-forget;
    /// errors (no peer, write fail) are silently dropped — encoder
    /// keeps emitting and a fresh peer picks up at the next sample.
    pub fn write_video_nal(&self, nal: &[u8]) {
        let track = match self.video_track.lock().unwrap().as_ref() {
            Some(t) => t.clone(),
            None => return,
        };
        let bytes = bytes::Bytes::copy_from_slice(nal);
        runtime().spawn(async move {
            // ~33 ms per frame at 30fps; webrtc-rs handles RTP
            // packetisation + timestamping internally.
            let _ = track.write_sample(&Sample {
                data: bytes,
                duration: Duration::from_millis(33),
                ..Default::default()
            }).await;
        });
    }

    /// Push one Opus packet (20 ms frame) to the active audio track.
    pub fn write_audio_packet(&self, packet: &[u8], duration: Duration) {
        let track = match self.audio_track.lock().unwrap().as_ref() {
            Some(t) => t.clone(),
            None => return,
        };
        let bytes = bytes::Bytes::copy_from_slice(packet);
        runtime().spawn(async move {
            let _ = track.write_sample(&Sample {
                data: bytes,
                duration,
                ..Default::default()
            }).await;
        });
    }

    /// Send a JSON message over the DataChannel. Drops the message
    /// silently if the channel isn't open yet.
    pub fn send_signal(&self, json: String) {
        let dc = match self.dc.lock().unwrap().as_ref() {
            Some(c) => c.clone(),
            None => return,
        };
        runtime().spawn(async move {
            let _ = dc.send_text(json).await;
        });
    }

    pub fn status(&self) -> PeerStatus {
        *self.state.lock().unwrap()
    }
}

impl Drop for WebStreamShared {
    fn drop(&mut self) {
        // Best-effort close on node drop. Peer's own Drop also calls
        // close, but doing it here while we still have a runtime
        // reference avoids the "block_on inside drop with no runtime"
        // edge case.
        self.close_peer_sync();
    }
}
