//! Art-Net (DMX-over-UDP) transport manager.
//!
//! Mirrors the shape of `crate::osc::OscManager` so the rest of the app
//! handles it identically: nodes push `ArtNetAction` enums during render,
//! `process()` drains them at the end of each frame, and per-node listener
//! threads forward inbound `ReceivedDmx` packets through `mpsc` channels.
//!
//! Wire format is hand-rolled (we only need 4 opcodes — Output, Poll,
//! PollReply, Sync — and the `artnet_protocol` crate would pull proc-macro
//! chains we don't otherwise need).
//!
//! Reference: <https://art-net.org.uk/structure/streaming-packets/artdmx-packet-definition/>

use crate::graph::NodeId;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::mpsc;

const ARTNET_HEADER: &[u8; 8] = b"Art-Net\0";
const OP_OUTPUT: u16 = 0x5000;
const ARTNET_PROTOCOL_VER: u16 = 14;
#[allow(dead_code)]
pub const DEFAULT_ARTNET_PORT: u16 = 6454;

pub enum ArtNetAction {
    /// Send one ArtDmx packet. `data.len()` must be even and ≤ 512.
    Send {
        node_id: NodeId,
        dest: SocketAddr,
        universe: u16,
        sequence_enabled: bool,
        data: Vec<u8>,
    },
    StartListening { node_id: NodeId, port: u16 },
    StopListening { node_id: NodeId },
}

/// One inbound ArtDmx frame. Ports outside the universe filter are dropped
/// at the listener thread level; what reaches the node is already filtered.
#[derive(Clone, Debug)]
pub struct ReceivedDmx {
    pub universe: u16,
    pub data: Vec<u8>,
}

struct Listener {
    _thread: std::thread::JoinHandle<()>,
    rx: mpsc::Receiver<ReceivedDmx>,
}

pub struct ArtNetManager {
    listeners: HashMap<NodeId, Listener>,
    /// Single shared sender socket — Art-Net senders typically use an
    /// ephemeral source port. Bound lazily on first send so we don't open a
    /// socket on app start when no Art-Net node exists.
    send_socket: Option<UdpSocket>,
    /// Per-node sequence counter. ArtDmx specifies a sequence byte that
    /// receivers may use to detect dropped packets; cycles 1..255 (0 = off).
    sequence: HashMap<NodeId, u8>,
    /// Last byte buffer sent per node for change detection — Art-Net is
    /// often sent at 30+ Hz, but if the data hasn't changed there's no
    /// reason to flood the network.
    last_sent: HashMap<NodeId, Vec<u8>>,
}

impl ArtNetManager {
    pub fn new() -> Self {
        Self {
            listeners: HashMap::new(),
            send_socket: None,
            sequence: HashMap::new(),
            last_sent: HashMap::new(),
        }
    }

    fn ensure_send_socket(&mut self) -> Option<&UdpSocket> {
        if self.send_socket.is_none() {
            // Bind to ephemeral port; allow broadcast for the conventional
            // 2.255.255.255 destination address.
            match UdpSocket::bind("0.0.0.0:0") {
                Ok(sock) => {
                    let _ = sock.set_broadcast(true);
                    self.send_socket = Some(sock);
                }
                Err(e) => {
                    crate::system_log::error(format!("Art-Net: bind send socket failed: {}", e));
                    return None;
                }
            }
        }
        self.send_socket.as_ref()
    }

    pub fn process(&mut self, actions: Vec<ArtNetAction>) {
        for action in actions {
            match action {
                ArtNetAction::Send { node_id, dest, universe, sequence_enabled, data } => {
                    // Build the packet outside the borrow scope so we can
                    // mutate `last_sent` afterward without re-borrowing self.
                    let seq = if sequence_enabled {
                        let cur = self.sequence.entry(node_id).or_insert(0);
                        *cur = cur.wrapping_add(1);
                        if *cur == 0 { *cur = 1; }
                        *cur
                    } else {
                        0
                    };
                    let packet = encode_artdmx(seq, universe, &data);

                    // Skip the send if the payload is identical to the
                    // previous frame — keeps idle traffic off the network.
                    // The sequence byte is excluded from the comparison.
                    let payload_for_diff = &packet[18..]; // skip header (18) before length
                    if self.last_sent.get(&node_id).map(|v| v.as_slice()) == Some(payload_for_diff) {
                        continue;
                    }
                    self.last_sent.insert(node_id, payload_for_diff.to_vec());

                    if let Some(sock) = self.ensure_send_socket() {
                        if let Err(e) = sock.send_to(&packet, dest) {
                            crate::system_log::warn(format!("Art-Net send to {} failed: {}", dest, e));
                        }
                    }
                }
                ArtNetAction::StartListening { node_id, port } => {
                    if self.listeners.contains_key(&node_id) {
                        continue;
                    }
                    let bind_addr = format!("0.0.0.0:{}", port);
                    let socket = match UdpSocket::bind(&bind_addr) {
                        Ok(s) => s,
                        Err(e) => {
                            crate::system_log::error(format!("Art-Net bind on port {}: {}", port, e));
                            continue;
                        }
                    };
                    let _ = socket.set_read_timeout(Some(std::time::Duration::from_millis(100)));

                    let (tx, rx) = mpsc::channel();
                    let handle = std::thread::Builder::new()
                        .name(format!("artnet-listen-{}", node_id))
                        .spawn(move || {
                            let mut buf = [0u8; 1024];
                            loop {
                                match socket.recv_from(&mut buf) {
                                    Ok((size, _)) => {
                                        if let Some(frame) = decode_artdmx(&buf[..size]) {
                                            if tx.send(frame).is_err() {
                                                break; // receiver dropped — node went away
                                            }
                                        }
                                    }
                                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock
                                        || e.kind() == std::io::ErrorKind::TimedOut => {
                                        // recv_timeout normal — loop and check again
                                    }
                                    Err(_) => break,
                                }
                            }
                        })
                        .expect("spawn artnet listener");
                    self.listeners.insert(node_id, Listener { _thread: handle, rx });
                }
                ArtNetAction::StopListening { node_id } => {
                    self.listeners.remove(&node_id);
                }
            }
        }
    }

    pub fn poll(&mut self, node_id: NodeId) -> Vec<ReceivedDmx> {
        let mut out = Vec::new();
        if let Some(listener) = self.listeners.get(&node_id) {
            while let Ok(msg) = listener.rx.try_recv() {
                out.push(msg);
            }
        }
        out
    }

    pub fn is_listening(&self, node_id: NodeId) -> bool {
        self.listeners.contains_key(&node_id)
    }

    pub fn cleanup_node(&mut self, node_id: NodeId) {
        self.listeners.remove(&node_id);
        self.sequence.remove(&node_id);
        self.last_sent.remove(&node_id);
    }
}

/// Encode an ArtDmx packet. Returns the full UDP payload.
///
/// Layout (18 bytes header + N data):
///   0..8   "Art-Net\0"
///   8..10  opcode (little-endian) = 0x5000
///   10..12 protocol version (big-endian) = 14
///   12     sequence (0 = off)
///   13     physical port (informational)
///   14..16 universe (little-endian, 15 bits used)
///   16..18 length (big-endian, ≥ 2, even)
///   18..   data (length bytes, 0..255 per channel)
fn encode_artdmx(sequence: u8, universe: u16, data: &[u8]) -> Vec<u8> {
    // Length must be even and ≥ 2; pad an odd-length input with one zero
    // byte so spec-strict receivers don't drop us.
    let mut padded;
    let data_slice: &[u8] = if data.len() % 2 == 1 {
        padded = data.to_vec();
        padded.push(0);
        &padded
    } else {
        data
    };
    let len = data_slice.len().min(512) as u16;

    let mut buf = Vec::with_capacity(18 + len as usize);
    buf.extend_from_slice(ARTNET_HEADER);
    buf.extend_from_slice(&OP_OUTPUT.to_le_bytes());
    buf.extend_from_slice(&ARTNET_PROTOCOL_VER.to_be_bytes());
    buf.push(sequence);
    buf.push(0); // physical port
    buf.extend_from_slice(&universe.to_le_bytes());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&data_slice[..len as usize]);
    buf
}

/// Decode an ArtDmx packet. Returns `None` on any header / opcode mismatch
/// so the listener thread can silently drop unrelated UDP traffic that
/// happens to arrive on the same port.
fn decode_artdmx(buf: &[u8]) -> Option<ReceivedDmx> {
    if buf.len() < 18 { return None; }
    if &buf[0..8] != ARTNET_HEADER { return None; }
    let opcode = u16::from_le_bytes([buf[8], buf[9]]);
    if opcode != OP_OUTPUT { return None; }
    // sequence at buf[12], physical at buf[13] — ignored
    let universe = u16::from_le_bytes([buf[14], buf[15]]);
    let length = u16::from_be_bytes([buf[16], buf[17]]) as usize;
    if buf.len() < 18 + length { return None; }
    let data = buf[18..18 + length.min(512)].to_vec();
    Some(ReceivedDmx { universe, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_artdmx() {
        let data = vec![0u8, 64, 128, 255, 1, 2, 3, 4];
        let packet = encode_artdmx(7, 1234, &data);
        let decoded = decode_artdmx(&packet).expect("decode");
        assert_eq!(decoded.universe, 1234);
        assert_eq!(decoded.data, data);
    }

    #[test]
    fn rejects_non_artnet() {
        assert!(decode_artdmx(b"Not Art-Net at all").is_none());
    }
}
