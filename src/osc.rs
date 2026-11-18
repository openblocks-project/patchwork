use crate::graph::NodeId;
use rosc::{OscMessage, OscPacket, OscType};
use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::mpsc;

pub enum OscAction {
    Send {
        node_id: NodeId,
        host: String,
        port: u16,
        address: String,
        args: Vec<f32>,
    },
    StartListening {
        node_id: NodeId,
        port: u16,
    },
    StopListening {
        node_id: NodeId,
    },
}

/// A received OSC message with both float and string representations of args.
#[derive(Clone, Debug)]
pub struct ReceivedOsc {
    pub address: String,
    pub args_float: Vec<f32>,
    pub args_text: Vec<String>,
}

struct Listener {
    _thread: std::thread::JoinHandle<()>,
    rx: mpsc::Receiver<ReceivedOsc>,
}

pub struct OscManager {
    listeners: HashMap<NodeId, Listener>,
    send_socket: Option<UdpSocket>,
    last_sent: HashMap<NodeId, Vec<u8>>,
}

impl OscManager {
    pub fn new() -> Self {
        Self {
            listeners: HashMap::new(),
            send_socket: UdpSocket::bind("0.0.0.0:0").ok(),
            last_sent: HashMap::new(),
        }
    }

    pub fn process(&mut self, actions: Vec<OscAction>) {
        for action in actions {
            match action {
                OscAction::Send {
                    node_id,
                    host,
                    port,
                    address,
                    args,
                } => {
                    let osc_args: Vec<OscType> = args.iter().map(|&v| OscType::Float(v)).collect();
                    let msg = OscMessage {
                        addr: address,
                        args: osc_args,
                    };
                    let packet = OscPacket::Message(msg);
                    // P4.3.2E2: sending is a network operation of the node.
                    if crate::capability::check_for(node_id, crate::capability::Capability::Network)
                        .is_err()
                    {
                        continue;
                    }
                    if let Ok(buf) = rosc::encoder::encode(&packet) {
                        // Change detection
                        if self.last_sent.get(&node_id) == Some(&buf) {
                            continue;
                        }
                        if let Some(sock) = &self.send_socket {
                            let addr = format!("{}:{}", host, port);
                            let _ = sock.send_to(&buf, &addr);
                        }
                        self.last_sent.insert(node_id, buf);
                    }
                }
                OscAction::StartListening { node_id, port } => {
                    if self.listeners.contains_key(&node_id) {
                        continue;
                    }
                    if crate::capability::check_for(node_id, crate::capability::Capability::Network)
                        .is_err()
                    {
                        continue;
                    }
                    let bind_addr = format!("0.0.0.0:{}", port);
                    let socket = match UdpSocket::bind(&bind_addr) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("OSC bind error on port {}: {}", port, e);
                            continue;
                        }
                    };
                    socket.set_nonblocking(false).ok();
                    socket
                        .set_read_timeout(Some(std::time::Duration::from_millis(10)))
                        .ok();

                    let (tx, rx) = mpsc::channel();
                    // P4.3.2E2: the listener keeps the node's rights and
                    // closes its socket once they are revoked.
                    let handle = crate::capability::spawn(move || {
                        let mut buf = [0u8; 4096];
                        loop {
                            if !crate::capability::still_allowed(
                                crate::capability::Capability::Network,
                            ) {
                                break;
                            }
                            match socket.recv_from(&mut buf) {
                                Ok((size, _)) => {
                                    if let Ok((_, packet)) = rosc::decoder::decode_udp(&buf[..size])
                                    {
                                        Self::extract_messages(&packet, &tx);
                                    }
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                                    std::thread::sleep(std::time::Duration::from_millis(1));
                                }
                                Err(_) => break,
                            }
                        }
                    });
                    self.listeners.insert(
                        node_id,
                        Listener {
                            _thread: handle,
                            rx,
                        },
                    );
                }
                OscAction::StopListening { node_id } => {
                    self.listeners.remove(&node_id);
                }
            }
        }
    }

    fn extract_messages(packet: &OscPacket, tx: &mpsc::Sender<ReceivedOsc>) {
        match packet {
            OscPacket::Message(msg) => {
                let mut args_float = Vec::with_capacity(msg.args.len());
                let mut args_text = Vec::with_capacity(msg.args.len());
                for a in &msg.args {
                    match a {
                        OscType::Float(f) => {
                            args_float.push(*f);
                            args_text.push(format!("{:.4}", f));
                        }
                        OscType::Int(i) => {
                            args_float.push(*i as f32);
                            args_text.push(i.to_string());
                        }
                        OscType::Double(d) => {
                            args_float.push(*d as f32);
                            args_text.push(format!("{:.6}", d));
                        }
                        OscType::Long(l) => {
                            args_float.push(*l as f32);
                            args_text.push(l.to_string());
                        }
                        OscType::String(s) => {
                            // Try parsing as number, fall back to 0.0
                            args_float.push(s.parse::<f32>().unwrap_or(0.0));
                            args_text.push(s.clone());
                        }
                        OscType::Bool(b) => {
                            args_float.push(if *b { 1.0 } else { 0.0 });
                            args_text.push(b.to_string());
                        }
                        OscType::Nil => {
                            args_float.push(0.0);
                            args_text.push("nil".into());
                        }
                        OscType::Blob(bytes) => {
                            args_float.push(bytes.len() as f32);
                            args_text.push(format!("<blob {}B>", bytes.len()));
                        }
                        _ => {
                            args_float.push(0.0);
                            args_text.push("?".into());
                        }
                    }
                }
                let _ = tx.send(ReceivedOsc {
                    address: msg.addr.clone(),
                    args_float,
                    args_text,
                });
            }
            OscPacket::Bundle(bundle) => {
                for p in &bundle.content {
                    Self::extract_messages(p, tx);
                }
            }
        }
    }

    pub fn poll(&mut self, node_id: NodeId) -> Vec<ReceivedOsc> {
        // P4.3.2E2: nothing is delivered (and the socket is closed) once
        // the graph may no longer use the network.
        if !crate::capability::still_allowed(crate::capability::Capability::Network) {
            self.cleanup_node(node_id);
            return Vec::new();
        }
        let mut messages = Vec::new();
        if let Some(listener) = self.listeners.get(&node_id) {
            while let Ok(msg) = listener.rx.try_recv() {
                messages.push(msg);
            }
        }
        messages
    }

    pub fn is_listening(&self, node_id: NodeId) -> bool {
        self.listeners.contains_key(&node_id)
    }

    pub fn cleanup_node(&mut self, node_id: NodeId) {
        self.listeners.remove(&node_id);
        self.last_sent.remove(&node_id);
    }
}
