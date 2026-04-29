//! DMX-over-USB transport manager.
//!
//! Supports two common adapter families:
//!
//! - **Enttec USB Pro** (framed): the adapter handles DMX timing — we send
//!   `0x7E 0x06 len_lo len_hi data… 0xE7` and the firmware emits the BREAK
//!   and the proper inter-byte timing on the wire. Most reliable on macOS.
//! - **Enttec Open DMX** (raw FTDI): we drive the line directly at 250 kbaud
//!   with `set_break(true) / set_break(false)` for each frame. macOS's
//!   default 16 ms FTDI latency timer can disrupt timing — surface that
//!   caveat in the node tooltip.
//!
//! A dedicated 30–44 Hz sender thread per active node owns the serial port
//! and reads the latest universe state from a shared `Arc<Mutex<[u8; 513]>>`
//! (start code + 512 channels). DMX-In is supported on USB Pro (the
//! framework receives `0x05` packets back from the adapter).

use crate::graph::NodeId;
use serialport::SerialPort;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DmxAdapter {
    /// `0x7E 0x06 …data… 0xE7` framed. Adapter handles timing.
    UsbPro,
    /// FTDI raw. We drive BREAK + 250 kbaud writes directly.
    OpenDmx,
}

impl Default for DmxAdapter {
    fn default() -> Self { DmxAdapter::UsbPro }
}

pub enum DmxAction {
    /// Open a port and start a sender thread, or update an existing
    /// port's adapter / frame rate. Idempotent within a node.
    OpenOutput {
        node_id: NodeId,
        port_name: String,
        adapter: DmxAdapter,
        frame_rate_hz: u8,
    },
    /// Update the universe data for an open port. Lock-free hot path
    /// (writes the shared `[u8; 513]`); the sender thread picks it up.
    SetUniverse {
        node_id: NodeId,
        data: Vec<u8>, // first byte ignored; we always emit start code 0
    },
    /// Close the port and stop its sender thread.
    Close { node_id: NodeId },
    /// USB Pro only — start receiving inbound DMX frames from the adapter.
    StartListening {
        node_id: NodeId,
        port_name: String,
        adapter: DmxAdapter,
    },
    StopListening { node_id: NodeId },
}

/// One inbound DMX frame from a USB Pro adapter (label 5).
#[derive(Clone, Debug)]
pub struct ReceivedDmx {
    pub data: Vec<u8>,
}

/// Sender side: shared universe + a running thread.
struct ActiveOutput {
    /// 513 bytes: index 0 = DMX start code (0x00), 1..=512 = channels.
    universe: Arc<Mutex<[u8; 513]>>,
    /// Set by Close to ask the sender thread to exit at the next tick.
    stop: Arc<AtomicBool>,
    _thread: std::thread::JoinHandle<()>,
}

/// Receiver side: bg thread reads framed packets, forwards to mpsc.
struct ActiveInput {
    stop: Arc<AtomicBool>,
    rx: mpsc::Receiver<ReceivedDmx>,
    _thread: std::thread::JoinHandle<()>,
}

pub struct DmxManager {
    outputs: HashMap<NodeId, ActiveOutput>,
    inputs: HashMap<NodeId, ActiveInput>,
}

impl DmxManager {
    pub fn new() -> Self {
        Self { outputs: HashMap::new(), inputs: HashMap::new() }
    }

    pub fn process(&mut self, actions: Vec<DmxAction>) {
        for action in actions {
            match action {
                DmxAction::OpenOutput { node_id, port_name, adapter, frame_rate_hz } => {
                    // If the port is already open with the same adapter,
                    // skip — universe data is updated via SetUniverse.
                    // If adapter or port changed, close and re-open.
                    if let Some(existing) = self.outputs.get(&node_id) {
                        let _ = existing; // existing ports are always replaced on re-open
                        self.close_output(node_id);
                    }
                    match open_output_thread(port_name.clone(), adapter, frame_rate_hz) {
                        Ok(active) => {
                            self.outputs.insert(node_id, active);
                        }
                        Err(e) => {
                            crate::system_log::error(format!(
                                "DMX open output {} ({:?}): {}", port_name, adapter, e
                            ));
                        }
                    }
                }
                DmxAction::SetUniverse { node_id, data } => {
                    if let Some(active) = self.outputs.get(&node_id) {
                        if let Ok(mut u) = active.universe.lock() {
                            // Index 0 stays as start code 0; channels live
                            // at 1..=512. Cap incoming data to 512 bytes.
                            for (i, &b) in data.iter().take(512).enumerate() {
                                u[i + 1] = b;
                            }
                        }
                    }
                }
                DmxAction::Close { node_id } => {
                    self.close_output(node_id);
                    self.close_input(node_id);
                }
                DmxAction::StartListening { node_id, port_name, adapter } => {
                    if self.inputs.contains_key(&node_id) { continue; }
                    if adapter != DmxAdapter::UsbPro {
                        crate::system_log::warn(
                            "DMX In requires Enttec USB Pro; Open DMX adapters are output-only".to_string()
                        );
                        continue;
                    }
                    match open_input_thread(port_name.clone()) {
                        Ok(active) => { self.inputs.insert(node_id, active); }
                        Err(e) => {
                            crate::system_log::error(format!(
                                "DMX open input {}: {}", port_name, e
                            ));
                        }
                    }
                }
                DmxAction::StopListening { node_id } => { self.close_input(node_id); }
            }
        }
    }

    fn close_output(&mut self, node_id: NodeId) {
        if let Some(active) = self.outputs.remove(&node_id) {
            active.stop.store(true, Ordering::Relaxed);
            // Thread will exit on its next tick; we don't join (avoid
            // blocking the UI thread on a slow serial flush).
            let _ = active;
        }
    }

    fn close_input(&mut self, node_id: NodeId) {
        if let Some(active) = self.inputs.remove(&node_id) {
            active.stop.store(true, Ordering::Relaxed);
            let _ = active;
        }
    }

    pub fn poll_input(&mut self, node_id: NodeId) -> Vec<ReceivedDmx> {
        let mut out = Vec::new();
        if let Some(active) = self.inputs.get(&node_id) {
            while let Ok(msg) = active.rx.try_recv() {
                out.push(msg);
            }
        }
        out
    }

    pub fn is_output_open(&self, node_id: NodeId) -> bool {
        self.outputs.contains_key(&node_id)
    }
    pub fn is_listening(&self, node_id: NodeId) -> bool {
        self.inputs.contains_key(&node_id)
    }

    pub fn cleanup_node(&mut self, node_id: NodeId) {
        self.close_output(node_id);
        self.close_input(node_id);
    }
}

fn open_output_thread(
    port_name: String,
    adapter: DmxAdapter,
    frame_rate_hz: u8,
) -> Result<ActiveOutput, String> {
    let port = serialport::new(&port_name, 250_000)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::Two)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|e| e.to_string())?;

    let universe = Arc::new(Mutex::new([0u8; 513]));
    let stop = Arc::new(AtomicBool::new(false));
    let universe_t = universe.clone();
    let stop_t = stop.clone();
    let frame_dur = Duration::from_millis((1000 / frame_rate_hz.max(1).min(44) as u64).max(20));

    let handle = std::thread::Builder::new()
        .name(format!("dmx-out-{}", port_name))
        .spawn(move || {
            let mut port = port;
            while !stop_t.load(Ordering::Relaxed) {
                let snapshot: [u8; 513] = match universe_t.lock() {
                    Ok(g) => *g,
                    Err(_) => { return; } // poisoned — bail
                };
                let send_result = match adapter {
                    DmxAdapter::UsbPro => send_frame_usb_pro(&mut *port, &snapshot[1..]),
                    DmxAdapter::OpenDmx => send_frame_open_dmx(&mut *port, &snapshot),
                };
                if let Err(e) = send_result {
                    crate::system_log::warn(format!("DMX send error: {}", e));
                    // Don't kill the thread on a transient write error;
                    // the user may unplug/replug. Pause briefly.
                    std::thread::sleep(Duration::from_millis(200));
                }
                std::thread::sleep(frame_dur);
            }
        })
        .map_err(|e| format!("spawn DMX sender thread: {}", e))?;

    Ok(ActiveOutput { universe, stop, _thread: handle })
}

fn open_input_thread(port_name: String) -> Result<ActiveInput, String> {
    let port = serialport::new(&port_name, 250_000)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::Two)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_millis(100))
        .open()
        .map_err(|e| e.to_string())?;

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let stop_t = stop.clone();

    let handle = std::thread::Builder::new()
        .name(format!("dmx-in-{}", port_name))
        .spawn(move || {
            let mut port = port;
            // Tell USB Pro to start streaming inbound frames (label 5
            // = "Receive DMX on Change", set always-send mode = 0).
            // Spec: send a label-5 packet with 1-byte payload = 0.
            let _ = write_usb_pro_packet(&mut *port, 5, &[0]);
            // Read loop: parse framed packets, forward inbound (label 5).
            while !stop_t.load(Ordering::Relaxed) {
                if let Ok(Some(frame)) = read_usb_pro_packet(&mut *port) {
                    if frame.label == 5 && !frame.data.is_empty() {
                        // First byte of a label-5 packet is a start code;
                        // skip it so the receiver gets just channels.
                        let data = if frame.data.len() > 1 {
                            frame.data[1..].to_vec()
                        } else {
                            Vec::new()
                        };
                        if tx.send(ReceivedDmx { data }).is_err() {
                            break;
                        }
                    }
                }
            }
            // Best-effort: stop streaming when we exit.
            let _ = write_usb_pro_packet(&mut *port, 5, &[1]);
        })
        .map_err(|e| format!("spawn DMX listener thread: {}", e))?;

    Ok(ActiveInput { stop, rx, _thread: handle })
}

fn send_frame_usb_pro(port: &mut dyn SerialPort, channels: &[u8]) -> std::io::Result<()> {
    // USB Pro "Output Only Send DMX Packet" = label 6.
    // Payload: start-code byte (0) + channel data.
    let mut payload = Vec::with_capacity(1 + channels.len());
    payload.push(0); // DMX start code
    payload.extend_from_slice(channels);
    write_usb_pro_packet(port, 6, &payload).map(|_| ())
}

fn send_frame_open_dmx(port: &mut dyn SerialPort, full: &[u8; 513]) -> std::io::Result<()> {
    // 1. BREAK ≥ 88 µs, then MAB ≥ 8 µs, then 250 kbaud serial of 513 bytes.
    port.set_break()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    std::thread::sleep(Duration::from_micros(120));
    port.clear_break()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    std::thread::sleep(Duration::from_micros(12));
    port.write_all(full)?;
    Ok(())
}

fn write_usb_pro_packet(port: &mut dyn SerialPort, label: u8, data: &[u8]) -> std::io::Result<usize> {
    let len = data.len() as u16;
    let mut buf = Vec::with_capacity(5 + data.len());
    buf.push(0x7E);              // start of message
    buf.push(label);              // label
    buf.push((len & 0xFF) as u8); // length LSB
    buf.push((len >> 8) as u8);   // length MSB
    buf.extend_from_slice(data);
    buf.push(0xE7);              // end of message
    port.write_all(&buf)?;
    Ok(buf.len())
}

#[derive(Debug)]
struct UsbProFrame {
    label: u8,
    data: Vec<u8>,
}

/// Read one framed packet from a USB Pro port. Returns Ok(None) on
/// timeout / partial read; the caller should retry on the next tick.
fn read_usb_pro_packet(port: &mut dyn SerialPort) -> std::io::Result<Option<UsbProFrame>> {
    let mut byte = [0u8; 1];

    // Hunt for the 0x7E start byte. Drop bytes until we find one or
    // hit a read error / timeout.
    loop {
        match port.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                if byte[0] == 0x7E { break; }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    // Read label + length.
    let mut header = [0u8; 3];
    if read_exact_or_none(port, &mut header)?.is_none() { return Ok(None); }
    let label = header[0];
    let len = u16::from_le_bytes([header[1], header[2]]) as usize;
    if len > 600 { return Ok(None); } // sanity cap
    let mut data = vec![0u8; len];
    if !data.is_empty() {
        if read_exact_or_none(port, &mut data)?.is_none() { return Ok(None); }
    }
    let mut end = [0u8; 1];
    if read_exact_or_none(port, &mut end)?.is_none() { return Ok(None); }
    if end[0] != 0xE7 { return Ok(None); }
    Ok(Some(UsbProFrame { label, data }))
}

fn read_exact_or_none(port: &mut dyn SerialPort, buf: &mut [u8]) -> std::io::Result<Option<()>> {
    let mut total = 0;
    while total < buf.len() {
        match port.read(&mut buf[total..]) {
            Ok(0) => return Ok(None),
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(Some(()))
}
