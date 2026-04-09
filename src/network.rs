use crate::graph::NodeId;
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::{Endpoint, PublicKey, SecretKey};
use iroh::endpoint::presets;
use iroh_gossip::api::Event;
use iroh_gossip::net::{Gossip, GOSSIP_ALPN};
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::mpsc as std_mpsc;

// ── Wire format ──────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NetMessage {
    pub seq: u64,
    pub sender_short_id: [u8; 8],
    pub payload: Vec<NetPort>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum NetPort {
    None,
    Float(f32),
    Text(String),
    Image { format: u8, data: Vec<u8> },
}

// ── Link format ──────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PatchworkLink {
    pub version: u8,
    pub topic: [u8; 32],
    pub peers: Vec<PeerInfo>,
    pub schema: Vec<PortSchema>,
    pub label: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PeerInfo {
    pub id: [u8; 32],
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PortSchema {
    pub name: String,
    pub kind: PortSchemaKind,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum PortSchemaKind {
    Float,
    Text,
    Image,
}

pub fn encode_link(link: &PatchworkLink) -> String {
    let bytes = postcard::to_stdvec(link).unwrap_or_default();
    format!("patchwork://{}", data_encoding::BASE32_NOPAD.encode(&bytes))
}

pub fn decode_link(s: &str) -> Result<PatchworkLink, String> {
    let data = s.strip_prefix("patchwork://").unwrap_or(s);
    let bytes = data_encoding::BASE32_NOPAD
        .decode(data.to_ascii_uppercase().as_bytes())
        .map_err(|e| format!("Invalid link encoding: {}", e))?;
    postcard::from_bytes(&bytes).map_err(|e| format!("Invalid link data: {}", e))
}

// ── Actions & Events ─────────────────────────────────────────────────────────

pub enum NetworkAction {
    CreateSend {
        node_id: NodeId,
        schema: Vec<PortSchema>,
        secret_key_hex: Option<String>,
        label: String,
    },
    SendValues {
        node_id: NodeId,
        values: Vec<NetPort>,
    },
    CreateReceive {
        node_id: NodeId,
        link: String,
    },
    DestroyNode {
        node_id: NodeId,
    },
}

pub enum NetworkEvent {
    LinkReady {
        node_id: NodeId,
        link: String,
        secret_hex: String,
    },
    SendStatus {
        node_id: NodeId,
        peers: usize,
    },
    Received {
        node_id: NodeId,
        values: Vec<NetPort>,
        sender_short_id: [u8; 8],
    },
    Error {
        node_id: NodeId,
        error: String,
    },
}

// ── Background state ─────────────────────────────────────────────────────────

struct SendState {
    _topic: TopicId,
    sender: iroh_gossip::api::GossipSender,
    seq: u64,
    short_id: [u8; 8],
}

struct ReceiveState {
    _topic: TopicId,
}

// ── NetworkManager ───────────────────────────────────────────────────────────

pub struct NetworkManager {
    action_tx: tokio::sync::mpsc::UnboundedSender<NetworkAction>,
    event_rx: std_mpsc::Receiver<NetworkEvent>,
    _thread: std::thread::JoinHandle<()>,
}

impl NetworkManager {
    pub fn new() -> Self {
        let (action_tx, action_rx) = tokio::sync::mpsc::unbounded_channel::<NetworkAction>();
        let (event_tx, event_rx) = std_mpsc::channel::<NetworkEvent>();

        let thread = std::thread::Builder::new()
            .name("patchwork-network".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("[patchwork-network] tokio runtime failed: {}", e);
                        return;
                    }
                };
                // LocalSet is required for spawn_local to work
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, network_loop(action_rx, event_tx));
            })
            .expect("spawn network thread");

        Self {
            action_tx,
            event_rx,
            _thread: thread,
        }
    }

    pub fn process(&self, actions: Vec<NetworkAction>) {
        for action in actions {
            let _ = self.action_tx.send(action);
        }
    }

    pub fn poll(&self) -> Vec<NetworkEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.event_rx.try_recv() {
            events.push(ev);
        }
        events
    }

    pub fn cleanup_node(&self, node_id: NodeId) {
        let _ = self.action_tx.send(NetworkAction::DestroyNode { node_id });
    }
}

// ── Async event loop ─────────────────────────────────────────────────────────

async fn network_loop(
    mut action_rx: tokio::sync::mpsc::UnboundedReceiver<NetworkAction>,
    event_tx: std_mpsc::Sender<NetworkEvent>,
) {
    let mut send_states: HashMap<NodeId, SendState> = HashMap::new();
    let mut recv_states: HashMap<NodeId, ReceiveState> = HashMap::new();

    // Lazily initialized iroh infrastructure
    let mut endpoint: Option<Endpoint> = None;
    let mut gossip: Option<Gossip> = None;
    let mut _router: Option<iroh::protocol::Router> = None;

    // Non-blocking select loop: wait for actions from the main thread
    // while letting tokio drive iroh's networking and spawned tasks
    while let Some(action) = action_rx.recv().await {
        // Collect all pending actions into a batch
        let mut batch = vec![action];
        while let Ok(action) = action_rx.try_recv() {
            batch.push(action);
        }

        // Deduplicate: keep only the latest SendValues per node_id
        let mut latest_sends: HashMap<NodeId, Vec<NetPort>> = HashMap::new();
        let mut other_actions: Vec<NetworkAction> = Vec::new();
        for action in batch {
            match action {
                NetworkAction::SendValues { node_id, values } => {
                    latest_sends.insert(node_id, values);
                }
                other => other_actions.push(other),
            }
        }

        // Process non-send actions first (CreateSend, CreateReceive, Destroy)
        for action in other_actions {
            handle_action(
                action,
                &mut endpoint,
                &mut gossip,
                &mut _router,
                &mut send_states,
                &mut recv_states,
                &event_tx,
            ).await;
        }

        // Then send the latest value for each node (deduplicated)
        for (node_id, values) in latest_sends {
            handle_action(
                NetworkAction::SendValues { node_id, values },
                &mut endpoint,
                &mut gossip,
                &mut _router,
                &mut send_states,
                &mut recv_states,
                &event_tx,
            ).await;
        }

        // Yield to let spawned receiver tasks process incoming gossip messages
        tokio::task::yield_now().await;
    }
}

async fn ensure_iroh(
    endpoint: &mut Option<Endpoint>,
    gossip: &mut Option<Gossip>,
    router: &mut Option<iroh::protocol::Router>,
) -> Result<(), String> {
    if endpoint.is_some() {
        return Ok(());
    }
    match init_iroh().await {
        Ok((ep, gos, rtr)) => {
            *endpoint = Some(ep);
            *gossip = Some(gos);
            *router = Some(rtr);
            Ok(())
        }
        Err(e) => Err(format!("Failed to init network: {}", e)),
    }
}

async fn handle_action(
    action: NetworkAction,
    endpoint: &mut Option<Endpoint>,
    gossip: &mut Option<Gossip>,
    router: &mut Option<iroh::protocol::Router>,
    send_states: &mut HashMap<NodeId, SendState>,
    recv_states: &mut HashMap<NodeId, ReceiveState>,
    event_tx: &std_mpsc::Sender<NetworkEvent>,
) {
    match action {
        NetworkAction::CreateSend { node_id, schema, secret_key_hex, label } => {
            if let Err(e) = ensure_iroh(endpoint, gossip, router).await {
                let _ = event_tx.send(NetworkEvent::Error { node_id, error: e });
                return;
            }

            // Destroy previous state for this node
            send_states.remove(&node_id);

            let ep = endpoint.as_ref().unwrap();
            let gos = gossip.as_ref().unwrap();

            let _sk = secret_key_hex.as_ref().and_then(|hex| hex.parse::<SecretKey>().ok());

            let topic_bytes: [u8; 32] = rand::random();
            let topic = TopicId::from_bytes(topic_bytes);

            // Use subscribe() not subscribe_and_join() — the sender creates the
            // topic with no peers, so joined() would block forever.
            match gos.subscribe(topic, vec![]).await {
                Ok(gossip_topic) => {
                    let (sender, receiver) = gossip_topic.split();

                    // Build short ID from endpoint public key
                    let pub_key = ep.id();
                    let pub_bytes = pub_key.as_bytes();
                    let mut short_id = [0u8; 8];
                    short_id.copy_from_slice(&pub_bytes[..8]);

                    // Build link
                    let my_addr = ep.addr();
                    let peer_info = PeerInfo {
                        id: *my_addr.id.as_bytes(),
                    };
                    let link = PatchworkLink {
                        version: 1,
                        topic: topic_bytes,
                        peers: vec![peer_info],
                        schema,
                        label,
                    };
                    let link_str = encode_link(&link);

                    let secret_hex = secret_key_hex.unwrap_or_default();

                    let _ = event_tx.send(NetworkEvent::LinkReady {
                        node_id,
                        link: link_str,
                        secret_hex,
                    });

                    send_states.insert(node_id, SendState {
                        _topic: topic,
                        sender,
                        seq: 0,
                        short_id,
                    });

                    // Spawn task to track neighbor count
                    let ev_tx = event_tx.clone();
                    tokio::task::spawn_local(peer_tracker(node_id, receiver, ev_tx));
                }
                Err(e) => {
                    let _ = event_tx.send(NetworkEvent::Error {
                        node_id,
                        error: format!("Failed to subscribe: {}", e),
                    });
                }
            }
        }

        NetworkAction::SendValues { node_id, values } => {
            if let Some(state) = send_states.get_mut(&node_id) {
                state.seq += 1;
                let msg = NetMessage {
                    seq: state.seq,
                    sender_short_id: state.short_id,
                    payload: values,
                };
                if let Ok(bytes) = postcard::to_stdvec(&msg) {
                    if let Err(e) = state.sender.broadcast(Bytes::from(bytes)).await {
                        let _ = event_tx.send(NetworkEvent::Error {
                            node_id,
                            error: format!("Send failed: {}", e),
                        });
                    }
                }
            }
        }

        NetworkAction::CreateReceive { node_id, link } => {
            recv_states.remove(&node_id);

            let parsed = match decode_link(&link) {
                Ok(l) => l,
                Err(e) => {
                    let _ = event_tx.send(NetworkEvent::Error { node_id, error: e });
                    return;
                }
            };

            if let Err(e) = ensure_iroh(endpoint, gossip, router).await {
                let _ = event_tx.send(NetworkEvent::Error { node_id, error: e });
                return;
            }

            let gos = gossip.as_ref().unwrap();
            let topic = TopicId::from_bytes(parsed.topic);

            let bootstrap: Vec<PublicKey> = parsed.peers.iter()
                .filter_map(|p| PublicKey::from_bytes(&p.id).ok())
                .collect();

            match gos.subscribe_and_join(topic, bootstrap).await {
                Ok(gossip_topic) => {
                    let (_sender, receiver) = gossip_topic.split();
                    recv_states.insert(node_id, ReceiveState { _topic: topic });

                    // Spawn receiver task
                    let ev_tx = event_tx.clone();
                    tokio::task::spawn_local(recv_loop(node_id, receiver, ev_tx));
                }
                Err(e) => {
                    let _ = event_tx.send(NetworkEvent::Error {
                        node_id,
                        error: format!("Failed to join topic: {}", e),
                    });
                }
            }
        }

        NetworkAction::DestroyNode { node_id } => {
            send_states.remove(&node_id);
            recv_states.remove(&node_id);
        }
    }
}

/// Track neighbor join/leave on the send side and report peer count.
async fn peer_tracker(
    node_id: NodeId,
    mut receiver: iroh_gossip::api::GossipReceiver,
    event_tx: std_mpsc::Sender<NetworkEvent>,
) {
    let mut peer_count: usize = 0;
    while let Some(Ok(event)) = receiver.next().await {
        match event {
            Event::NeighborUp(_) => {
                peer_count += 1;
                let _ = event_tx.send(NetworkEvent::SendStatus { node_id, peers: peer_count });
            }
            Event::NeighborDown(_) => {
                peer_count = peer_count.saturating_sub(1);
                let _ = event_tx.send(NetworkEvent::SendStatus { node_id, peers: peer_count });
            }
            _ => {}
        }
    }
}

/// Receive gossip messages and forward as NetworkEvents.
async fn recv_loop(
    node_id: NodeId,
    mut receiver: iroh_gossip::api::GossipReceiver,
    event_tx: std_mpsc::Sender<NetworkEvent>,
) {
    while let Some(Ok(event)) = receiver.next().await {
        if let Event::Received(msg) = event {
            if let Ok(net_msg) = postcard::from_bytes::<NetMessage>(&msg.content) {
                let _ = event_tx.send(NetworkEvent::Received {
                    node_id,
                    values: net_msg.payload,
                    sender_short_id: net_msg.sender_short_id,
                });
            }
        }
    }
}

async fn init_iroh() -> Result<(Endpoint, Gossip, iroh::protocol::Router), Box<dyn std::error::Error>> {
    let endpoint = Endpoint::builder(presets::N0)
        .bind()
        .await?;

    let gossip = Gossip::builder().spawn(endpoint.clone());

    let router = iroh::protocol::Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .spawn();

    // Wait for relay connection with a timeout — don't block link generation
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        endpoint.online(),
    ).await;

    Ok((endpoint, gossip, router))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_roundtrip() {
        let link = PatchworkLink {
            version: 1,
            topic: [42u8; 32],
            peers: vec![PeerInfo { id: [7u8; 32] }],
            schema: vec![
                PortSchema { name: "x".into(), kind: PortSchemaKind::Float },
                PortSchema { name: "msg".into(), kind: PortSchemaKind::Text },
                PortSchema { name: "img".into(), kind: PortSchemaKind::Image },
            ],
            label: "test node".into(),
        };

        let encoded = encode_link(&link);
        assert!(encoded.starts_with("patchwork://"));

        let decoded = decode_link(&encoded).expect("decode should succeed");
        assert_eq!(decoded.version, 1);
        assert_eq!(decoded.topic, [42u8; 32]);
        assert_eq!(decoded.peers.len(), 1);
        assert_eq!(decoded.peers[0].id, [7u8; 32]);
        assert_eq!(decoded.schema.len(), 3);
        assert_eq!(decoded.schema[0].name, "x");
        assert_eq!(decoded.schema[0].kind, PortSchemaKind::Float);
        assert_eq!(decoded.schema[1].name, "msg");
        assert_eq!(decoded.schema[1].kind, PortSchemaKind::Text);
        assert_eq!(decoded.schema[2].name, "img");
        assert_eq!(decoded.schema[2].kind, PortSchemaKind::Image);
        assert_eq!(decoded.label, "test node");
    }

    #[test]
    fn link_decode_invalid() {
        assert!(decode_link("garbage").is_err());
        assert!(decode_link("patchwork://NOTVALIDBASE32!!!").is_err());
        assert!(decode_link("").is_err());
    }

    #[test]
    fn link_decode_without_prefix() {
        let link = PatchworkLink {
            version: 1,
            topic: [0u8; 32],
            peers: vec![],
            schema: vec![PortSchema { name: "val".into(), kind: PortSchemaKind::Float }],
            label: String::new(),
        };
        let encoded = encode_link(&link);
        let raw = encoded.strip_prefix("patchwork://").unwrap();
        let decoded = decode_link(raw).expect("decode without prefix should work");
        assert_eq!(decoded.schema[0].name, "val");
    }

    #[test]
    fn wire_format_roundtrip() {
        let msg = NetMessage {
            seq: 42,
            sender_short_id: [1, 2, 3, 4, 5, 6, 7, 8],
            payload: vec![
                NetPort::Float(3.14),
                NetPort::Text("hello".into()),
                NetPort::None,
                NetPort::Image { format: 0, data: vec![0xFF, 0xD8] },
            ],
        };

        let bytes = postcard::to_stdvec(&msg).expect("serialize");
        let decoded: NetMessage = postcard::from_bytes(&bytes).expect("deserialize");

        assert_eq!(decoded.seq, 42);
        assert_eq!(decoded.sender_short_id, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(decoded.payload.len(), 4);

        match &decoded.payload[0] {
            NetPort::Float(f) => assert!((f - 3.14).abs() < 0.001),
            other => panic!("expected Float, got {:?}", other),
        }
        match &decoded.payload[1] {
            NetPort::Text(t) => assert_eq!(t, "hello"),
            other => panic!("expected Text, got {:?}", other),
        }
        assert!(matches!(decoded.payload[2], NetPort::None));
        match &decoded.payload[3] {
            NetPort::Image { format, data } => {
                assert_eq!(*format, 0);
                assert_eq!(data, &[0xFF, 0xD8]);
            }
            other => panic!("expected Image, got {:?}", other),
        }
    }

    #[test]
    fn network_manager_create_send_returns_link() {
        let manager = NetworkManager::new();

        // Send a CreateSend action
        manager.process(vec![NetworkAction::CreateSend {
            node_id: 42,
            schema: vec![PortSchema { name: "x".into(), kind: PortSchemaKind::Float }],
            secret_key_hex: None,
            label: "test".into(),
        }]);

        // Poll for events — iroh needs time to bind and connect
        let mut got_link = false;
        let mut got_error = false;
        let mut error_msg = String::new();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(30) {
            let events = manager.poll();
            for event in events {
                match event {
                    NetworkEvent::LinkReady { node_id, link, .. } => {
                        eprintln!("Got LinkReady for node {}: {}", node_id, &link[..50.min(link.len())]);
                        assert_eq!(node_id, 42);
                        assert!(link.starts_with("patchwork://"));
                        got_link = true;
                    }
                    NetworkEvent::Error { node_id, error } => {
                        eprintln!("Got Error for node {}: {}", node_id, error);
                        error_msg = error;
                        got_error = true;
                    }
                    _ => {}
                }
            }
            if got_link || got_error {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        assert!(got_link, "Should have received LinkReady (got_error={}, msg={})", got_error, error_msg);
    }

    #[test]
    fn two_managers_send_receive() {
        // Sender
        let sender_mgr = NetworkManager::new();
        sender_mgr.process(vec![NetworkAction::CreateSend {
            node_id: 1,
            schema: vec![PortSchema { name: "x".into(), kind: PortSchemaKind::Float }],
            secret_key_hex: None,
            label: "test".into(),
        }]);

        // Wait for link
        let mut link = String::new();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(15) {
            for ev in sender_mgr.poll() {
                if let NetworkEvent::LinkReady { link: l, .. } = ev {
                    link = l;
                }
            }
            if !link.is_empty() { break; }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!link.is_empty(), "Sender should produce a link");
        eprintln!("[test] Got link: {}...{}", &link[..30], &link[link.len()-10..]);

        // Receiver
        let receiver_mgr = NetworkManager::new();
        receiver_mgr.process(vec![NetworkAction::CreateReceive {
            node_id: 2,
            link: link.clone(),
        }]);

        // Wait for connection
        eprintln!("[test] Waiting for receiver to connect...");
        std::thread::sleep(std::time::Duration::from_secs(5));

        // Drain events to see if receiver got anything
        let recv_events: Vec<_> = receiver_mgr.poll().into_iter().collect();
        eprintln!("[test] Receiver events after connect wait: {}", recv_events.len());
        for ev in &recv_events {
            match ev {
                NetworkEvent::Error { error, .. } => eprintln!("[test]   Error: {}", error),
                NetworkEvent::Received { values, .. } => eprintln!("[test]   Received {} values", values.len()),
                NetworkEvent::LinkReady { .. } => eprintln!("[test]   LinkReady"),
                NetworkEvent::SendStatus { peers, .. } => eprintln!("[test]   SendStatus: {} peers", peers),
            }
        }

        // Sender should now see a peer
        let sender_events: Vec<_> = sender_mgr.poll().into_iter().collect();
        eprintln!("[test] Sender events: {}", sender_events.len());
        for ev in &sender_events {
            match ev {
                NetworkEvent::SendStatus { peers, .. } => eprintln!("[test]   SendStatus: {} peers", peers),
                NetworkEvent::Error { error, .. } => eprintln!("[test]   Error: {}", error),
                _ => {}
            }
        }

        // Now send values
        eprintln!("[test] Sending values...");
        for i in 0..10 {
            sender_mgr.process(vec![NetworkAction::SendValues {
                node_id: 1,
                values: vec![NetPort::Float(i as f32 * 0.1 + 0.5)],
            }]);
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        // Wait a moment then check receiver
        std::thread::sleep(std::time::Duration::from_secs(2));

        let mut received_any = false;
        let recv_events: Vec<_> = receiver_mgr.poll().into_iter().collect();
        eprintln!("[test] Receiver events after sends: {}", recv_events.len());
        for ev in &recv_events {
            match ev {
                NetworkEvent::Received { values, .. } => {
                    eprintln!("[test]   Received: {:?}", values);
                    received_any = true;
                }
                NetworkEvent::Error { error, .. } => eprintln!("[test]   Error: {}", error),
                _ => {}
            }
        }

        assert!(received_any, "Receiver should have gotten at least one value");
    }

    #[test]
    fn two_managers_rapid_fire() {
        // Simulates the real app: sender broadcasts at ~60fps, receiver joins mid-stream
        let sender_mgr = NetworkManager::new();
        sender_mgr.process(vec![NetworkAction::CreateSend {
            node_id: 1,
            schema: vec![PortSchema { name: "x".into(), kind: PortSchemaKind::Float }],
            secret_key_hex: None,
            label: "test".into(),
        }]);

        // Wait for link
        let mut link = String::new();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(15) {
            for ev in sender_mgr.poll() {
                if let NetworkEvent::LinkReady { link: l, .. } = ev { link = l; }
            }
            if !link.is_empty() { break; }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(!link.is_empty(), "Need link");

        // Receiver joins
        let receiver_mgr = NetworkManager::new();
        receiver_mgr.process(vec![NetworkAction::CreateReceive {
            node_id: 2,
            link: link.clone(),
        }]);

        // Wait for connection to establish
        std::thread::sleep(std::time::Duration::from_secs(5));

        // Now rapid-fire send at ~60fps for 3 seconds (single thread, like the real app)
        for i in 0..180 {
            sender_mgr.process(vec![NetworkAction::SendValues {
                node_id: 1,
                values: vec![NetPort::Float(i as f32 * 0.01)],
            }]);
            // Also poll sender events to keep it responsive
            let _ = sender_mgr.poll();
            std::thread::sleep(std::time::Duration::from_millis(16));
        }

        // Wait for messages to arrive
        std::thread::sleep(std::time::Duration::from_secs(2));

        let recv_events: Vec<_> = receiver_mgr.poll().into_iter().collect();
        let received: Vec<_> = recv_events.iter().filter_map(|ev| {
            if let NetworkEvent::Received { values, .. } = ev { Some(values) } else { None }
        }).collect();

        eprintln!("[rapid] received {} messages total", received.len());
        if let Some(last) = received.last() {
            eprintln!("[rapid] last value: {:?}", last);
        }

        assert!(!received.is_empty(), "Should receive values during rapid send");
    }
}
