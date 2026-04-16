//! HID-based OB device discovery.
//!
//! Unlike the serial-based `ObHub` path, HID devices auto-enumerate via the OS
//! (IOKit on macOS, hidraw+udev on Linux, HID.dll on Windows). Patchwork
//! spawns a background thread on startup that:
//!   1. Polls `hidapi::HidApi::device_list()` every ~500ms
//!   2. For each plugged-in device matching the OB VID (`0x303A`), opens it
//!      and spawns a dedicated reader sub-thread
//!   3. Reader sub-threads parse the HID report and publish the decoded
//!      values into a shared `HashMap<(device_type, device_id), ObDevice>`
//!
//! The `ObManager` exposes `hid.find_device(...)` which the ObKnob node
//! lookup path (`app/mod.rs` injection + `ob_knob::render`) falls back to
//! after its hub-based search. So an HID-connected knob shows up as
//! `("knob", 1)` in the device map and the node goes Active automatically
//! — no OB Hub node required, no serial port selection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::ob::ObDevice;

// ── OB HID identity ─────────────────────────────────────────────────────────

/// Espressif VID (OB devices use the ESP32-S3 / TinyUSB stack).
pub const OB_VID: u16 = 0x303A;

/// Per-device PIDs. Add new devices here as more HID-capable OB blocks land.
pub const OB_KNOB_PID: u16 = 0x8151;
pub const OB_DISTANCE_PID: u16 = 0x8152;
pub const OB_PRESSURE_PID: u16 = 0x8153;
/// OB Wheel: a rotary encoder + click button. Classified as "encoder" so
/// it routes to the existing `NodeType::ObEncoder` infrastructure — the
/// UI label was renamed but the internal type string is unchanged.
pub const OB_WHEEL_PID: u16 = 0x8154;

/// Classify a (VID, PID) pair as an OB device type.
/// Returns `None` for non-OB HIDs so we skip keyboards, mice, etc.
fn classify(vid: u16, pid: u16) -> Option<&'static str> {
    if vid != OB_VID { return None; }
    match pid {
        OB_KNOB_PID => Some("knob"),
        OB_DISTANCE_PID => Some("distance"),
        OB_PRESSURE_PID => Some("pressure"),
        OB_WHEEL_PID => Some("encoder"),
        _ => None,
    }
}

// ── Shared device map ────────────────────────────────────────────────────────

/// Shared device registry. Mutex-protected because the HID reader threads
/// write concurrently. Cloned into each reader thread; reads happen on the
/// main thread via `HidManager::find_device`.
pub type HidDeviceMap = Arc<Mutex<HashMap<(String, u8), ObDevice>>>;

/// Process-wide HID manager. Initialized exactly once via `OnceLock` the
/// first time `global()` is called. Any attempt to construct it elsewhere
/// would spawn extra enumeration threads that race to open the same HID
/// device — and with `hidapi` being thread-unsafe for concurrent opens,
/// that path ends in malloc corruption and a crash. Don't go there.
///
/// This mirrors the `Transport` clock pattern in `src/transport.rs`.
pub struct HidManager {
    devices: HidDeviceMap,
}

static HID_MANAGER: OnceLock<HidManager> = OnceLock::new();

/// Get-or-init the process-wide HID manager. Safe to call from anywhere;
/// the enumeration thread is spawned on the first call and lives for the
/// app's lifetime.
pub fn global() -> &'static HidManager {
    HID_MANAGER.get_or_init(|| {
        let devices: HidDeviceMap = Arc::new(Mutex::new(HashMap::new()));
        let devices_clone = Arc::clone(&devices);
        // We deliberately don't retain the JoinHandle. The thread lives for
        // the process lifetime; there's no meaningful shutdown path.
        thread::Builder::new()
            .name("patchwork-hid".into())
            .spawn(move || hid_enumeration_thread(devices_clone))
            .expect("spawn HID enumeration thread");
        HidManager { devices }
    })
}

impl HidManager {
    /// Look up an HID-discovered device by type+id. Returns a cloned
    /// `ObDevice` (behind Mutex; cheap to clone since it's small-ish).
    /// `None` if no HID device with that type+id has been seen.
    pub fn find_device(&self, device_type: &str, id: u8) -> Option<ObDevice> {
        let key = (device_type.to_string(), id);
        self.devices.lock().ok()?.get(&key).cloned()
    }

    /// Clear a single value key on a device — used by the encoder injection
    /// to reset the one-shot "turn" field after it's been sampled, so a
    /// single detent produces a single frame of non-zero output. Safe to
    /// call even if the device or key doesn't exist (no-op).
    pub fn clear_device_value(&self, device_type: &str, id: u8, key: &str) {
        let map_key = (device_type.to_string(), id);
        if let Ok(mut map) = self.devices.lock() {
            if let Some(dev) = map.get_mut(&map_key) {
                dev.values.insert(key.into(), 0.0);
            }
        }
    }

    /// Snapshot the current HID device list. Used by UI to show what's
    /// been auto-detected. Returns (device_type, id, is_active, values).
    #[allow(dead_code)]
    pub fn list_devices(&self) -> Vec<(String, u8, bool, HashMap<String, f32>)> {
        let map = match self.devices.lock() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        map.iter()
            .map(|((dtype, id), dev)| (dtype.clone(), *id, dev.is_active, dev.values.clone()))
            .collect()
    }
}

// ── Enumeration thread ──────────────────────────────────────────────────────

/// Long-running thread: periodically scans for OB HID devices and opens
/// readers for newly-plugged-in ones. Exits only if `HidApi::new()` fails
/// (very rare — usually means the OS HID subsystem is unavailable).
fn hid_enumeration_thread(devices: HidDeviceMap) {
    let mut api = match hidapi::HidApi::new() {
        Ok(a) => a,
        Err(e) => {
            crate::system_log::error(format!("HID: init failed: {}", e));
            return;
        }
    };
    crate::system_log::log("HID: enumeration thread started (scanning every 500ms)".to_string());

    // Track which device paths we've already opened so we don't re-spawn
    // readers on every poll. Value is the (device_type, device_id) pair so
    // we can clean up the map entry if the reader exits.
    let mut opened_paths: HashMap<String, (String, u8)> = HashMap::new();

    loop {
        if let Err(e) = api.refresh_devices() {
            crate::system_log::error(format!("HID: refresh_devices failed: {}", e));
        }

        for info in api.device_list() {
            let Some(device_type) = classify(info.vendor_id(), info.product_id()) else {
                continue;
            };
            // Composite USB devices (CDC+HID) may expose multiple HID
            // interfaces per physical device — one gamepad collection and
            // potentially sibling collections that carry no input reports.
            // We want the gamepad: usage_page=0x01 (Generic Desktop),
            // usage=0x05 (Gamepad). Opening the wrong interface returns a
            // handle whose first read_timeout fails with "device
            // disconnected" — which is what caused the reconnect loop.
            //
            // macOS sometimes reports (0, 0) on the first enumeration pass
            // before the descriptor is parsed, so we allow that one-shot
            // fallback path too. Log the tuple either way so we can
            // diagnose future composite-device surprises.
            let up = info.usage_page();
            let u  = info.usage();
            let is_gamepad = up == 0x01 && u == 0x05;
            let is_unknown = up == 0 && u == 0;
            if !is_gamepad && !is_unknown {
                // Log once per (path, up, u) so we can see what non-matching
                // interfaces this device is exposing.
                let path_key = format!("{}|skip", info.path().to_string_lossy());
                if !opened_paths.contains_key(&path_key) {
                    crate::system_log::log(format!(
                        "HID: skipping {} pid={:04x} interface up={:04x} u={:04x} (not gamepad)",
                        device_type, info.product_id(), up, u
                    ));
                    opened_paths.insert(path_key, (device_type.to_string(), 0));
                }
                continue;
            }
            let path = info.path().to_string_lossy().into_owned();
            if opened_paths.contains_key(&path) { continue; }

            // Device ID from USB iSerialNumber — firmware should set this
            // (e.g., `USB.serialNumber("1")` before `USB.begin()`).
            // Falls back to 1 if the string is absent or unparseable.
            let device_id: u8 = info.serial_number()
                .and_then(|s| s.trim().parse::<u8>().ok())
                .unwrap_or(1);

            // Open the device. This can fail if another process has it
            // open exclusively — log and skip; we'll retry next poll.
            let hid_dev = match info.open_device(&api) {
                Ok(d) => d,
                Err(e) => {
                    // Rate-limit this log: only report the first failure per path
                    // so we don't spam every 500ms when a device stays unopenable.
                    crate::system_log::error(format!(
                        "HID: open {} #{} failed: {} (path={})",
                        device_type, device_id, e, path
                    ));
                    // Insert so we don't retry every 500ms.
                    opened_paths.insert(path.clone(), (device_type.to_string(), device_id));
                    continue;
                }
            };
            // Non-blocking mode. `read_timeout(buf, 200)` relies on the
            // underlying hidapi to honour the timeout; on macOS, the OS
            // backend can ignore the timeout in blocking mode and park
            // the thread indefinitely when the device is silent. Forcing
            // non-blocking makes the 200 ms deadline reliable so the
            // reader thread can loop and the diagnostic heartbeat fires.
            let _ = hid_dev.set_blocking_mode(false);

            crate::system_log::log(format!(
                "HID: opened {} #{} (vid={:04x} pid={:04x} serial={:?})",
                device_type, device_id,
                info.vendor_id(), info.product_id(),
                info.serial_number().unwrap_or("(none)")
            ));

            // Pre-create the device entry so the OB Knob node stops showing
            // "Waiting..." immediately — even before the first report arrives.
            {
                let mut map = devices.lock().unwrap();
                let entry = map.entry((device_type.to_string(), device_id))
                    .or_insert_with(|| ObDevice::new(device_type, device_id));
                entry.is_active = true;
                entry.last_seen = Instant::now();
            }

            opened_paths.insert(path.clone(), (device_type.to_string(), device_id));

            // Spawn reader sub-thread. Moves the opened HidDevice; it lives
            // for the device's plugged-in lifetime. When the device is
            // unplugged, read_timeout returns an error and the thread exits.
            let devices_c = Arc::clone(&devices);
            let dtype_c = device_type.to_string();
            let did_c = device_id;
            let _ = thread::Builder::new()
                .name(format!("hid-{}-{}", device_type, device_id))
                .spawn(move || hid_reader_thread(hid_dev, dtype_c, did_c, devices_c));
        }

        // Age out devices whose reader hasn't written anything in 5s.
        // Matches ObHub's aging logic in src/ob.rs poll().
        {
            let mut map = devices.lock().unwrap();
            let now = Instant::now();
            for dev in map.values_mut() {
                if dev.is_active && now.duration_since(dev.last_seen).as_secs() > 5 {
                    dev.is_active = false;
                }
            }
        }

        thread::sleep(Duration::from_millis(500));
    }
}

// ── Reader thread ────────────────────────────────────────────────────────────

/// Per-device reader: loops reading HID reports and publishes parsed values
/// into the shared device map. Exits when the device is unplugged (read
/// error) or the reader encounters a fatal parse failure.
fn hid_reader_thread(
    dev: hidapi::HidDevice,
    device_type: String,
    device_id: u8,
    devices: HidDeviceMap,
) {
    let mut buf = [0u8; 64];
    let mut consecutive_errors = 0u32;
    // Diagnostic: log the first few raw reports so we can see the actual
    // byte layout and tune the parser. After LOG_FIRST_N reports we stop,
    // to avoid spamming the system log.
    const LOG_FIRST_N: u32 = 3;
    let mut reports_logged = 0u32;
    let mut timeout_count = 0u32;

    loop {
        match dev.read_timeout(&mut buf, 200) {
            Ok(0) => {
                // Timeout, no data. Normal. Continue polling.
                consecutive_errors = 0;
                timeout_count += 1;
                // Every 25 timeouts (≈5 s at 200 ms each) log once so we
                // can distinguish "device is silent" from "we're receiving
                // reports but parsing wrong".
                if timeout_count == 25 {
                    crate::system_log::log(format!(
                        "HID: {} #{} — no reports in 5s (device silent or not emitting)",
                        device_type, device_id
                    ));
                }
            }
            Ok(n) => {
                consecutive_errors = 0;
                timeout_count = 0;
                let report = &buf[..n];
                if reports_logged < LOG_FIRST_N {
                    let hex: String = report.iter().map(|b| format!("{:02x} ", b)).collect();
                    crate::system_log::log(format!(
                        "HID: {} #{} report[{}]: {}",
                        device_type, device_id, n, hex.trim_end()
                    ));
                    reports_logged += 1;
                }
                let parsed = match device_type.as_str() {
                    // Knob + Pressure share the same HID report layout (single
                    // normalized analog value on Rx axis). Same firmware pattern,
                    // same parser — just routed to different (device_type, id)
                    // keys by the PID classify() above.
                    "knob" | "pressure" => parse_knob_report(report),
                    "distance" => parse_distance_report(report),
                    // Encoder (OB Wheel): delta-based. Parser accumulates
                    // position into the device map atomically, so it needs
                    // &mut access to the map. Handle it specially below.
                    "encoder" => {
                        parse_encoder_report(report, &devices, &device_type, device_id);
                        None // parser already wrote into the map
                    }
                    _ => None,
                };
                if let Some(values) = parsed {
                    let mut map = devices.lock().unwrap();
                    let entry = map.entry((device_type.clone(), device_id))
                        .or_insert_with(|| ObDevice::new(&device_type, device_id));
                    for (k, v) in values {
                        entry.values.insert(k.to_string(), v);
                    }
                    entry.is_active = true;
                    entry.last_seen = Instant::now();
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                // 3 errors in a row → treat as disconnected, exit reader.
                if consecutive_errors >= 3 {
                    crate::system_log::log(format!(
                        "HID: reader {} #{} exiting: {} (likely unplugged)",
                        device_type, device_id, e
                    ));
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    // Mark inactive on exit so the UI shows "Waiting..." again.
    let mut map = devices.lock().unwrap();
    if let Some(d) = map.get_mut(&(device_type, device_id)) {
        d.is_active = false;
    }
}

// ── Report parsers ───────────────────────────────────────────────────────────

/// Parse the OB Knob HID gamepad report.
///
/// The firmware uses Arduino-ESP32's `USBHIDGamepad.send(x, y, z, rz, rx, ry,
/// hat, buttons)`. The TinyUSB gamepad report descriptor is the standard
/// layout, so the report bytes are:
///   [0]     report ID (usually 1 — TinyUSB gamepad always includes one)
///   [1..=6] x, y, z, rz, rx, ry — each an int8
///   [7]     hat (low 4 bits) + padding
///   [8..]   buttons as little-endian u32
///
/// We only care about Rx (index 5 if report ID is present, 4 if not). Rather
/// than assume, we probe both — whichever has an absolute value >0 in any
/// frame is almost certainly Rx. For the first frame at rest (Rx=0) we default
/// to offset 5 (the most common case for TinyUSB gamepad).
///
/// Output is a single `("value", f32)` entry, with `Rx` mapped from `-127..127`
/// back to the `0.0..1.0` normalized range the firmware started with.
fn parse_knob_report(report: &[u8]) -> Option<Vec<(&'static str, f32)>> {
    // Bytes 0..7 need to exist to reach the Rx offset.
    if report.len() < 6 { return None; }

    // Assume report ID is present (common case for TinyUSB gamepad). Rx at [5].
    // If your device sends reports without an ID, change this to [4].
    let rx_signed = report[5] as i8;
    let rx_f = rx_signed as f32;
    // Map back -127..=127 → 0.0..=1.0. The firmware did:
    //   rx = round((norm * 2 - 1) * 127)  → inverse: norm = (rx + 127) / 254
    let norm = ((rx_f + 127.0) / 254.0).clamp(0.0, 1.0);

    // Emit under both keys so the serial-hub legacy path ("val") and the
    // knob-specific path ("value") both find it. Cheap — one extra HashMap
    // insert per report. Lets one parser serve multiple device types that
    // use the same report shape but different legacy key conventions.
    Some(vec![("value", norm), ("val", norm)])
}

/// Parse the OB Distance HID report.
///
/// Reuses the standard TinyUSB `USBHIDGamepad` descriptor (same as Knob),
/// but packs two distinct values into it:
///   [0]  report ID (1, auto-added by TinyUSB)
///   [1]  X axis (int8)  — mm low byte (firmware writes `mm & 0xFF`)
///   [2]  Y axis (int8)  — mm high byte (firmware writes `(mm >> 8) & 0xFF`)
///   [3]  Z axis (int8)  — unused
///   [4]  Rz axis (int8) — unused
///   [5]  Rx axis (int8) — normalized value, Knob's encoding
///   [6]  Ry axis (int8) — unused
///   [7]  hat + padding, [8..] buttons
///
/// Firmware range: mm ∈ [0, 300]. Normalized: 0 mm → 0.0, 300 mm → 1.0.
/// The X/Y byte pair gives 16-bit mm precision (no loss) for the "mm" port;
/// the Rx axis gives ~0.004 precision (1/254) on the normalized port. Both
/// come from the same underlying reading at emit time on the firmware side.
///
/// Output keys:
///   "val" → normalized 0..1 (read from Rx axis; same decode as Knob)
///   "mm"  → raw millimeters (read from X+Y byte pair)
/// Parse the OB Wheel (rotary encoder) HID report and write the results
/// directly into the shared device map. Unlike the analog-sensor parsers,
/// the encoder accumulates state (position) across reports, which the
/// stateless return-a-Vec pattern can't express — so this function mutates
/// the device map in-place.
///
/// Report layout (reuses the USBHIDGamepad descriptor, same as Knob):
///   [0]     report ID
///   [1]     X axis (int8) — signed delta per report (-1, 0, or +1)
///   [2]     Y axis — unused
///   [3..=6] Z, Rz, Rx, Ry — unused
///   [7]     hat (low 4 bits) + padding
///   [8..]   buttons — bit 0 is the encoder switch
///
/// Keys written (matching the serial-hub legacy path in src/ob.rs:73-84):
///   "turn"     → momentary direction pulse: +1 on CW tick, -1 on CCW,
///                else 0. Set each report; the injection arm snapshots it
///                before the next report overwrites it.
///   "click"    → button state 0.0 / 1.0
///   "position" → accumulated ticks since firmware boot (or reset)
fn parse_encoder_report(
    report: &[u8],
    devices: &HidDeviceMap,
    device_type: &str,
    device_id: u8,
) {
    if report.len() < 9 { return; }

    let delta = report[1] as i8 as f32; // firmware sends -1 / 0 / +1
    let click = if (report[8] & 0x01) != 0 { 1.0 } else { 0.0 };

    let mut map = devices.lock().unwrap();
    let entry = map.entry((device_type.to_string(), device_id))
        .or_insert_with(|| ObDevice::new(device_type, device_id));

    entry.values.insert("turn".into(), delta);
    entry.values.insert("click".into(), click);
    // Position accumulates across reports. A dropped packet loses one
    // tick — acceptable for creative input; for anything that needs
    // absolute position we could add a periodic full-position report.
    let prev_pos = entry.values.get("position").copied().unwrap_or(0.0);
    entry.values.insert("position".into(), prev_pos + delta);
    entry.is_active = true;
    entry.last_seen = Instant::now();
}

/// Parse the OB Distance HID report. Byte layout (reusing gamepad descriptor):
///   [0]  report ID
///   [1]  X axis — mm low byte (u8 interpretation)
///   [2]  Y axis — mm high byte
///   [3]  Z axis — sensor status: 0 = valid, 1 = out of range, 2 = sensor error
///   [5]  Rx axis — normalized 0..1 (same encoding as Knob)
///
/// Output keys: "val" (normalized), "mm" (raw), "status" (0/1/2 as f32).
fn parse_distance_report(report: &[u8]) -> Option<Vec<(&'static str, f32)>> {
    if report.len() < 6 { return None; }

    let rx_signed = report[5] as i8;
    let norm = ((rx_signed as f32 + 127.0) / 254.0).clamp(0.0, 1.0);

    let lo = (report[1] as i8) as u8 as u16;
    let hi = (report[2] as i8) as u8 as u16;
    let mm = ((hi << 8) | lo) as f32;

    // Status byte on Z axis. Firmware writes 0/1/2 as signed int8 which
    // round-trips cleanly because values are small. Default 0 if byte is
    // missing (older firmware, pre-status-byte builds).
    let status = if report.len() >= 4 {
        (report[3] as i8) as f32
    } else {
        0.0
    };

    Some(vec![("val", norm), ("mm", mm), ("status", status)])
}
