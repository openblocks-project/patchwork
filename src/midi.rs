use crate::graph::NodeId;
use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::collections::HashMap;
use std::sync::mpsc;

/// Actions collected during rendering, processed after.
pub enum MidiAction {
    ConnectOutput { node_id: NodeId, port_name: String },
    DisconnectOutput { node_id: NodeId },
    Send { node_id: NodeId, message: [u8; 3] },
    ConnectInput { node_id: NodeId, port_name: String },
    DisconnectInput { node_id: NodeId },
    /// Re-scan available MIDI input/output ports. Triggered from the ↻
    /// button on midi_in / midi_out nodes so the user can pick up newly
    /// enabled devices (e.g. macOS IAC Driver) without restarting Patchwork.
    RescanPorts,
}

pub struct MidiManager {
    outputs: HashMap<NodeId, MidiOutputConnection>,
    last_sent: HashMap<NodeId, [u8; 3]>,
    input_conns: HashMap<NodeId, MidiInputConnection<()>>,
    input_rx: HashMap<NodeId, mpsc::Receiver<Vec<u8>>>,
    // Cached port lists (refreshed on demand)
    pub cached_output_ports: Vec<String>,
    pub cached_input_ports: Vec<String>,
}

impl MidiManager {
    pub fn new() -> Self {
        let mut m = Self {
            outputs: HashMap::new(),
            last_sent: HashMap::new(),
            input_conns: HashMap::new(),
            input_rx: HashMap::new(),
            cached_output_ports: Vec::new(),
            cached_input_ports: Vec::new(),
        };
        m.refresh_ports();
        m
    }

    pub fn refresh_ports(&mut self) {
        self.cached_output_ports = Self::scan_output_ports();
        self.cached_input_ports = Self::scan_input_ports();
    }

    /// Set port lists from background thread (avoids blocking UI)
    pub fn set_port_lists(&mut self, input_ports: Vec<String>, output_ports: Vec<String>) {
        self.cached_input_ports = input_ports;
        self.cached_output_ports = output_ports;
    }

    fn scan_output_ports() -> Vec<String> {
        let Ok(midi_out) = MidiOutput::new("patchwork-scan") else {
            return vec![];
        };
        midi_out
            .ports()
            .iter()
            .filter_map(|p| midi_out.port_name(p).ok())
            .collect()
    }

    fn scan_input_ports() -> Vec<String> {
        let Ok(midi_in) = MidiInput::new("patchwork-scan") else {
            return vec![];
        };
        midi_in
            .ports()
            .iter()
            .filter_map(|p| midi_in.port_name(p).ok())
            .collect()
    }

    pub fn is_output_connected(&self, node_id: NodeId) -> bool {
        self.outputs.contains_key(&node_id)
    }

    pub fn is_input_connected(&self, node_id: NodeId) -> bool {
        self.input_conns.contains_key(&node_id)
    }

    /// Process actions collected during the render pass.
    pub fn process(&mut self, actions: Vec<MidiAction>) {
        for action in actions {
            match action {
                MidiAction::ConnectOutput { node_id, port_name } => {
                    self.connect_output(node_id, &port_name);
                }
                MidiAction::DisconnectOutput { node_id } => {
                    self.outputs.remove(&node_id);
                    self.last_sent.remove(&node_id);
                }
                MidiAction::Send { node_id, message } => {
                    // Only send if changed
                    if self.last_sent.get(&node_id) != Some(&message) {
                        if let Some(conn) = self.outputs.get_mut(&node_id) {
                            let _ = conn.send(&message);
                        }
                        self.last_sent.insert(node_id, message);
                    }
                }
                MidiAction::ConnectInput { node_id, port_name } => {
                    self.connect_input(node_id, &port_name);
                }
                MidiAction::DisconnectInput { node_id } => {
                    self.input_conns.remove(&node_id);
                    self.input_rx.remove(&node_id);
                }
                MidiAction::RescanPorts => {
                    self.refresh_ports();
                }
            }
        }
    }

    fn connect_output(&mut self, node_id: NodeId, port_name: &str) {
        // Disconnect existing first
        self.outputs.remove(&node_id);
        self.last_sent.remove(&node_id);

        let Ok(midi_out) = MidiOutput::new("patchwork") else {
            return;
        };
        let ports = midi_out.ports();
        for port in &ports {
            if let Ok(name) = midi_out.port_name(port) {
                if name == port_name {
                    if let Ok(conn) = midi_out.connect(port, "patchwork-out") {
                        self.outputs.insert(node_id, conn);
                    }
                    return;
                }
            }
        }
    }

    fn connect_input(&mut self, node_id: NodeId, port_name: &str) {
        // Disconnect existing first
        self.input_conns.remove(&node_id);
        self.input_rx.remove(&node_id);

        let Ok(midi_in) = MidiInput::new("patchwork") else {
            return;
        };
        let ports = midi_in.ports();
        for port in &ports {
            if let Ok(name) = midi_in.port_name(port) {
                if name == port_name {
                    let (tx, rx) = mpsc::channel();
                    if let Ok(conn) = midi_in.connect(
                        port,
                        "patchwork-in",
                        move |_stamp, msg, _| {
                            let _ = tx.send(msg.to_vec());
                        },
                        (),
                    ) {
                        self.input_conns.insert(node_id, conn);
                        self.input_rx.insert(node_id, rx);
                    }
                    return;
                }
            }
        }
    }

    /// Drain latest received MIDI message for an input node.
    pub fn poll_input(&self, node_id: NodeId) -> Option<Vec<u8>> {
        let rx = self.input_rx.get(&node_id)?;
        let mut last = None;
        while let Ok(msg) = rx.try_recv() {
            last = Some(msg);
        }
        last
    }

    /// Clean up connections when a node is deleted.
    pub fn cleanup_node(&mut self, node_id: NodeId) {
        self.outputs.remove(&node_id);
        self.last_sent.remove(&node_id);
        self.input_conns.remove(&node_id);
        self.input_rx.remove(&node_id);
    }
}
