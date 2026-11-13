use crate::graph::NodeId;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc;

pub enum SerialAction {
    Connect {
        node_id: NodeId,
        port_name: String,
        baud_rate: u32,
    },
    Disconnect {
        node_id: NodeId,
    },
    Send {
        node_id: NodeId,
        data: String,
    },
}

struct SerialConn {
    writer: Box<dyn serialport::SerialPort>,
    rx: mpsc::Receiver<String>,
    // Keep the thread handle alive (it dies when writer is dropped)
    _handle: std::thread::JoinHandle<()>,
}

pub struct SerialManager {
    connections: HashMap<NodeId, SerialConn>,
    pub cached_ports: Vec<String>,
}

impl SerialManager {
    pub fn new() -> Self {
        let mut m = Self {
            connections: HashMap::new(),
            cached_ports: Vec::new(),
        };
        m.refresh_ports();
        m
    }

    pub fn refresh_ports(&mut self) {
        self.cached_ports = serialport::available_ports()
            .unwrap_or_default()
            .into_iter()
            .map(|p| p.port_name)
            .collect();
    }

    pub fn set_port_list(&mut self, ports: Vec<String>) {
        self.cached_ports = ports;
    }

    pub fn is_connected(&self, node_id: NodeId) -> bool {
        self.connections.contains_key(&node_id)
    }

    pub fn process(&mut self, actions: Vec<SerialAction>) {
        for action in actions {
            match action {
                SerialAction::Connect {
                    node_id,
                    port_name,
                    baud_rate,
                } => {
                    self.connect(node_id, &port_name, baud_rate);
                }
                SerialAction::Disconnect { node_id } => {
                    self.connections.remove(&node_id);
                }
                SerialAction::Send { node_id, data } => {
                    if let Some(conn) = self.connections.get_mut(&node_id) {
                        let _ = conn.writer.write_all(data.as_bytes());
                        let _ = conn.writer.write_all(b"\n");
                        let _ = conn.writer.flush();
                    }
                }
            }
        }
    }

    fn connect(&mut self, node_id: NodeId, port_name: &str, baud_rate: u32) {
        self.connections.remove(&node_id);

        let Ok(port) = serialport::new(port_name, baud_rate)
            .timeout(std::time::Duration::from_millis(10))
            .open()
        else {
            return;
        };

        let Ok(reader_port) = port.try_clone() else {
            return;
        };
        let (tx, rx) = mpsc::channel();

        let handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(reader_port);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim_end().to_string();
                        if tx.send(trimmed).is_err() {
                            break; // Receiver dropped
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                    Err(_) => break,
                }
            }
        });

        self.connections.insert(
            node_id,
            SerialConn {
                writer: port,
                rx,
                _handle: handle,
            },
        );
    }

    /// Drain all received lines for a node.
    pub fn poll(&self, node_id: NodeId) -> Vec<String> {
        let Some(conn) = self.connections.get(&node_id) else {
            return vec![];
        };
        let mut lines = Vec::new();
        while let Ok(line) = conn.rx.try_recv() {
            lines.push(line);
        }
        lines
    }

    pub fn cleanup_node(&mut self, node_id: NodeId) {
        self.connections.remove(&node_id);
    }
}
