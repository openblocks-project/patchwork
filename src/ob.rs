// OB Hardware Node Control System — Protocol Manager
// Parses /type/id/action serial protocol from ESP32 Hub + direct USB nodes.
// Maintains device registry with live values.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Instant;

use crate::graph::NodeId;

// ── Protocol Messages ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ObMessage {
    /// Sensor data: /type/id/action val0 val1 ...
    Data {
        device_type: String,
        id: u8,
        action: String,
        values: Vec<f32>,
    },
    /// System: /sys/connected type id
    Connected { device_type: String, id: u8 },
    /// System: /sys/disconnected type id
    Disconnected { device_type: String, id: u8 },
    /// System: /sys/ready type id
    Ready { device_type: String, id: u8 },
    /// System: /sys/status type id mode=xxx
    Status { device_type: String, id: u8, mode: String },
    /// System: other /sys/ messages (detecting_mode, hub_found, etc.)
    SysInfo { message: String },
    /// Raw unparsed line
    Raw(String),
}

// ── Device State ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ObDevice {
    pub device_type: String,
    pub _id: u8,
    pub is_active: bool,
    pub last_seen: Instant,
    /// Action-keyed values. For joystick: {"x": -0.2, "y": 0.5, "btn": 0.0}
    /// For encoder: {"turn": 1.0, "click": 0.0, "position": 5.0}
    pub values: HashMap<String, f32>,
}

impl ObDevice {
    pub fn new(device_type: &str, id: u8) -> Self {
        Self {
            device_type: device_type.to_string(),
            _id: id,
            is_active: true,
            last_seen: Instant::now(),
            values: HashMap::new(),
        }
    }

    /// Apply a data message to this device
    pub fn apply_data(&mut self, action: &str, values: &[f32]) {
        self.is_active = true;
        self.last_seen = Instant::now();

        match (self.device_type.as_str(), action) {
            ("joystick", "xybtn") => {
                if values.len() >= 3 {
                    self.values.insert("x".into(), values[0]);
                    self.values.insert("y".into(), values[1]);
                    self.values.insert("btn".into(), values[2]);
                }
            }
            ("encoder", "turn") => {
                if let Some(&dir) = values.first() {
                    self.values.insert("turn".into(), dir);
                    // Accumulate position
                    let pos = self.values.get("position").copied().unwrap_or(0.0);
                    self.values.insert("position".into(), pos + dir);
                }
            }
            ("encoder", "click") => {
                if let Some(&v) = values.first() {
                    self.values.insert("click".into(), v);
                }
            }
            ("knob", "value") => {
                if let Some(&v) = values.first() {
                    self.values.insert("value".into(), v);
                }
            }
            ("move", "accel") => {
                if values.len() >= 3 {
                    self.values.insert("ax".into(), values[0]);
                    self.values.insert("ay".into(), values[1]);
                    self.values.insert("az".into(), values[2]);
                }
            }
            ("move", "gyro") => {
                if values.len() >= 3 {
                    self.values.insert("gx".into(), values[0]);
                    self.values.insert("gy".into(), values[1]);
                    self.values.insert("gz".into(), values[2]);
                }
            }
            ("bend", "val") => {
                if let Some(&v) = values.first() {
                    self.values.insert("val".into(), v.clamp(0.0, 1.0));
                }
            }
            ("pressure", "val") => {
                if let Some(&v) = values.first() {
                    self.values.insert("val".into(), v.clamp(0.0, 1.0));
                }
            }
            ("distance", "val") => {
                if let Some(&v) = values.first() {
                    self.values.insert("val".into(), v.clamp(0.0, 1.0));
                }
            }
            ("distance", "mm") => {
                if let Some(&v) = values.first() {
                    // Normalize: 50mm=0.0 (close), 200mm=1.0 (far), clamped
                    let norm = ((v - 50.0) / (200.0 - 50.0)).clamp(0.0, 1.0);
                    self.values.insert("val".into(), norm);
                }
            }
            ("orb", act) if act == "accel" || act == "gyro" || act == "imu" => {
                match act {
                    "accel" => {
                        if values.len() >= 3 {
                            self.values.insert("ax".into(), values[0]);
                            self.values.insert("ay".into(), values[1]);
                            self.values.insert("az".into(), values[2]);
                        }
                    }
                    "gyro" => {
                        if values.len() >= 3 {
                            self.values.insert("gx".into(), values[0]);
                            self.values.insert("gy".into(), values[1]);
                            self.values.insert("gz".into(), values[2]);
                        }
                    }
                    "imu" => {
                        if values.len() >= 6 {
                            self.values.insert("ax".into(), values[0]);
                            self.values.insert("ay".into(), values[1]);
                            self.values.insert("az".into(), values[2]);
                            self.values.insert("gx".into(), values[3]);
                            self.values.insert("gy".into(), values[4]);
                            self.values.insert("gz".into(), values[5]);
                        }
                    }
                    _ => {}
                }
            }
            // Generic fallback: store all values by index
            (_, _) => {
                for (i, &v) in values.iter().enumerate() {
                    self.values.insert(format!("{}_{}", action, i), v);
                }
            }
        }
    }
}

// ── Hub Instance ─────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct ObHub {
    pub port_name: String,
    pub is_connected: bool,
    pub devices: HashMap<(String, u8), ObDevice>,
    pub log: Vec<String>,
    /// Warning if writer clone failed (read-only mode)
    pub write_warning: Option<String>,
    rx: mpsc::Receiver<ObMessage>,
    thread: Option<std::thread::JoinHandle<()>>,
    stop_signal: std::sync::Arc<std::sync::atomic::AtomicBool>,
    writer: Option<Box<dyn std::io::Write + Send>>,
}

impl Drop for ObHub {
    fn drop(&mut self) {
        // Signal the reader thread to stop
        self.stop_signal.store(true, std::sync::atomic::Ordering::Relaxed);
        // Drop the writer first to close our end of the port
        self.writer.take();
        // Wait for the thread to finish (with timeout)
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl ObHub {
    /// Start a new hub connection on the given serial port
    pub fn connect(port_name: &str) -> Result<Self, String> {
        let port = serialport::new(port_name, 115200)
            .timeout(std::time::Duration::from_millis(10))
            .open()
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("Resource busy") || msg.contains("EBUSY") {
                    format!("Port busy — close Serial Monitor, Arduino IDE, or other apps using {}", port_name)
                } else if msg.contains("No such file") || msg.contains("not found") {
                    format!("Port not found — device may be disconnected: {}", port_name)
                } else if msg.contains("Permission denied") {
                    format!("Permission denied on {} — check user permissions", port_name)
                } else {
                    format!("{}: {}", port_name, msg)
                }
            })?;

        let (writer, write_warning) = match port.try_clone() {
            Ok(p) => (Some(Box::new(p) as Box<dyn std::io::Write + Send>), None),
            Err(e) => (None, Some(format!("⚠ Cannot send commands: port clone failed ({}). Read-only mode.", e))),
        };

        let (tx, rx) = mpsc::channel();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();

        let thread = std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(port);
            let mut line = String::new();
            loop {
                if stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                line.clear();
                match std::io::BufRead::read_line(&mut reader, &mut line) {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim().to_string();
                        if !trimmed.is_empty() {
                            let msg = parse_ob_line(&trimmed);
                            if tx.send(msg).is_err() {
                                break; // Receiver dropped
                            }
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut => {
                        // Normal timeout — check stop signal and loop
                    }
                    Err(_) => break,
                }
            }
            // port is dropped here, releasing the serial lock
        });

        // Send ACK to trigger device identification
        let mut hub = Self {
            port_name: port_name.to_string(),
            is_connected: true,
            devices: HashMap::new(),
            log: Vec::new(),
            write_warning: write_warning.clone(),
            rx,
            thread: Some(thread),
            stop_signal: stop,
            writer,
        };
        if let Some(ref warn) = write_warning {
            hub.log_push(warn.clone());
        }
        hub.send_command("ACK");
        Ok(hub)
    }

    /// Send a command string to the serial port
    pub fn send_command(&mut self, cmd: &str) {
        if let Some(ref mut w) = self.writer {
            if let Err(e) = write!(w, "{}\n", cmd) {
                self.log_push(format!("⚠ Write failed: {}", e));
            }
        }
        // No writer = read-only mode (warning already shown in UI)
    }

    /// Poll for new messages, update device states
    pub fn poll(&mut self) -> Vec<ObMessage> {
        let mut messages = Vec::new();
        let now = Instant::now();

        while let Ok(msg) = self.rx.try_recv() {
            match &msg {
                ObMessage::Data { device_type, id, action, values } => {
                    let key = (device_type.clone(), *id);
                    let device = self.devices.entry(key).or_insert_with(|| ObDevice::new(device_type, *id));
                    device.apply_data(action, values);
                    self.log_push(format!("/{}/{}/{} {:?}", device_type, id, action, values));
                }
                ObMessage::Connected { device_type, id } => {
                    let key = (device_type.clone(), *id);
                    let device = self.devices.entry(key).or_insert_with(|| ObDevice::new(device_type, *id));
                    device.is_active = true;
                    device.last_seen = now;
                    self.log_push(format!("+ {} #{} connected", device_type, id));
                }
                ObMessage::Disconnected { device_type, id } => {
                    let key = (device_type.clone(), *id);
                    if let Some(device) = self.devices.get_mut(&key) {
                        device.is_active = false;
                    }
                    self.log_push(format!("- {} #{} disconnected", device_type, id));
                }
                ObMessage::Ready { device_type, id } => {
                    let key = (device_type.clone(), *id);
                    let device = self.devices.entry(key).or_insert_with(|| ObDevice::new(device_type, *id));
                    device.is_active = true;
                    device.last_seen = now;
                    self.log_push(format!("✓ {} #{} ready", device_type, id));
                }
                ObMessage::Status { device_type, id, mode } => {
                    self.log_push(format!("i {} #{} mode={}", device_type, id, mode));
                }
                ObMessage::SysInfo { message } => {
                    self.log_push(format!("sys: {}", message));
                }
                ObMessage::Raw(line) => {
                    self.log_push(format!("> {}", line));
                }
            }
            messages.push(msg);
        }

        // Timeout check: mark devices inactive after 5 seconds of no data
        for device in self.devices.values_mut() {
            if device.is_active && now.duration_since(device.last_seen).as_secs() > 5 {
                device.is_active = false;
            }
        }

        messages
    }

    /// Get a device's value by type, id, and key
    pub fn get_value(&self, device_type: &str, id: u8, key: &str) -> f32 {
        self.devices
            .get(&(device_type.to_string(), id))
            .and_then(|d| d.values.get(key))
            .copied()
            .unwrap_or(0.0)
    }

    /// Get a device by type and id
    pub fn get_device(&self, device_type: &str, id: u8) -> Option<&ObDevice> {
        self.devices.get(&(device_type.to_string(), id))
    }

    /// List all devices of a given type
    #[allow(dead_code)]
    pub fn devices_of_type(&self, device_type: &str) -> Vec<&ObDevice> {
        self.devices
            .iter()
            .filter(|((t, _), _)| t == device_type)
            .map(|(_, d)| d)
            .collect()
    }

    fn log_push(&mut self, msg: String) {
        self.log.push(msg);
        if self.log.len() > 200 {
            self.log.drain(0..100);
        }
    }
}

// ── Manager (multi-hub) ─────────────────────────────────────────────────────

pub struct ObManager {
    pub hubs: HashMap<NodeId, ObHub>,
}

impl ObManager {
    pub fn new() -> Self {
        // NOTE: HID discovery is handled by a process-wide singleton
        // (`crate::hid::global()`), NOT owned by ObManager. If we owned it
        // here, every `std::mem::replace(&mut self.ob, ObManager::new())`
        // in app/mod.rs would spawn another enumeration thread, causing
        // hidapi concurrent-access crashes. Access HID via `hid::global()`.
        Self {
            hubs: HashMap::new(),
        }
    }

    /// Connect a hub for a given node
    pub fn connect_hub(&mut self, node_id: NodeId, port_name: &str) -> Result<(), String> {
        // Disconnect existing if any
        self.hubs.remove(&node_id);
        let hub = ObHub::connect(port_name)?;
        self.hubs.insert(node_id, hub);
        Ok(())
    }

    /// Disconnect hub for a node
    pub fn disconnect_hub(&mut self, node_id: NodeId) {
        self.hubs.remove(&node_id);
    }

    /// Poll all hubs
    pub fn poll_all(&mut self) {
        for hub in self.hubs.values_mut() {
            hub.poll();
        }
    }

    /// Get a hub by node ID
    pub fn get_hub(&self, node_id: NodeId) -> Option<&ObHub> {
        self.hubs.get(&node_id)
    }

    /// Get mutable hub
    pub fn get_hub_mut(&mut self, node_id: NodeId) -> Option<&mut ObHub> {
        self.hubs.get_mut(&node_id)
    }

    /// Get mutable reference to any connected hub (for sending commands when hub_node_id is 0)
    pub fn find_any_hub_mut(&mut self) -> Option<&mut ObHub> {
        self.hubs.values_mut().find(|h| h.is_connected)
    }

    /// Find any hub that has a device of the given type/id
    pub fn find_device(&self, device_type: &str, id: u8) -> Option<(NodeId, &ObDevice)> {
        for (&hub_id, hub) in &self.hubs {
            if let Some(device) = hub.get_device(device_type, id) {
                return Some((hub_id, device));
            }
        }
        None
    }

    /// Get device value from a specific hub
    #[allow(dead_code)]
    pub fn get_device_value(&self, hub_node_id: NodeId, device_type: &str, id: u8, key: &str) -> f32 {
        self.hubs
            .get(&hub_node_id)
            .map(|h| h.get_value(device_type, id, key))
            .unwrap_or(0.0)
    }

    /// Cleanup when a node is deleted
    pub fn cleanup_node(&mut self, node_id: NodeId) {
        self.hubs.remove(&node_id);
    }
}

// ── Protocol Parser ──────────────────────────────────────────────────────────

fn parse_ob_line(line: &str) -> ObMessage {
    let line = line.trim();

    // System messages: /sys/...
    if line.starts_with("/sys/") {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let path_parts: Vec<&str> = parts[0].split('/').collect();
        // path_parts = ["", "sys", "msg_type"]
        if path_parts.len() >= 3 {
            let msg_type = path_parts[2];
            match msg_type {
                "connected" if parts.len() >= 3 => {
                    let device_type = parts[1].to_string();
                    let id = parts[2].parse::<u8>().unwrap_or(0);
                    return ObMessage::Connected { device_type, id };
                }
                "disconnected" if parts.len() >= 3 => {
                    let device_type = parts[1].to_string();
                    let id = parts[2].parse::<u8>().unwrap_or(0);
                    return ObMessage::Disconnected { device_type, id };
                }
                "ready" if parts.len() >= 3 => {
                    let device_type = parts[1].to_string();
                    let id = parts[2].parse::<u8>().unwrap_or(0);
                    return ObMessage::Ready { device_type, id };
                }
                "status" if parts.len() >= 4 => {
                    let device_type = parts[1].to_string();
                    let id = parts[2].parse::<u8>().unwrap_or(0);
                    let mode = parts[3..].join(" ");
                    return ObMessage::Status { device_type, id, mode };
                }
                _ => {
                    return ObMessage::SysInfo { message: line.to_string() };
                }
            }
        }
        return ObMessage::SysInfo { message: line.to_string() };
    }

    // Data messages: /type/id/action val0 val1 ...
    // Also handle lines with prefix noise (e.g., "RECV from MAC /orb/1/accel ...")
    let data_start = line.find('/').unwrap_or(0);
    let data_line = &line[data_start..];
    if data_line.starts_with('/') && !data_line.starts_with("/sys/") {
        let parts: Vec<&str> = data_line.split_whitespace().collect();
        if !parts.is_empty() {
            let path_parts: Vec<&str> = parts[0].split('/').collect();
            // path_parts = ["", "joystick", "1", "xybtn"]
            if path_parts.len() >= 4 {
                let device_type = path_parts[1].to_string();
                if let Ok(id) = path_parts[2].parse::<u8>() {
                    let action = path_parts[3].to_string();
                    let values: Vec<f32> = parts[1..]
                        .iter()
                        .filter_map(|v| v.parse().ok())
                        .collect();
                    return ObMessage::Data { device_type, id, action, values };
                }
            }
        }
    }

    ObMessage::Raw(line.to_string())
}

// ── Utility: list available serial ports ─────────────────────────────────────

pub fn available_ports() -> Vec<String> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.port_name)
        .collect()
}
