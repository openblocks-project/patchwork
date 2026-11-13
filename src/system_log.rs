//! Global system log — collects messages from all parts of the app.
//! Console nodes read from this to display system events.
//!
//! Usage:
//!   system_log::log("Audio device started");
//!   system_log::warn("Mic permission denied");
//!   system_log::error("WAV export failed: disk full");

use std::sync::Mutex;

static LOG: Mutex<Vec<LogEntry>> = Mutex::new(Vec::new());
const MAX_ENTRIES: usize = 500;

#[derive(Clone)]
pub struct LogEntry {
    pub timestamp_ms: u64,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Clone, Copy, PartialEq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

fn push(level: LogLevel, message: String) {
    if let Ok(mut log) = LOG.lock() {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        log.push(LogEntry {
            timestamp_ms,
            level,
            message,
        });
        if log.len() > MAX_ENTRIES {
            let excess = log.len() - MAX_ENTRIES;
            log.drain(..excess);
        }
    }
}

pub fn log(msg: impl Into<String>) {
    let s = msg.into();
    eprintln!("[info] {}", s);
    push(LogLevel::Info, s);
}

pub fn warn(msg: impl Into<String>) {
    let s = msg.into();
    eprintln!("[warn] {}", s);
    push(LogLevel::Warn, s);
}

pub fn error(msg: impl Into<String>) {
    let s = msg.into();
    eprintln!("[error] {}", s);
    push(LogLevel::Error, s);
}

/// Read all entries since the given index. Returns (new_index, entries).
pub fn read_since(last_index: usize) -> (usize, Vec<LogEntry>) {
    if let Ok(log) = LOG.lock() {
        let total = log.len();
        if last_index >= total {
            (total, Vec::new())
        } else {
            (total, log[last_index..].to_vec())
        }
    } else {
        (last_index, Vec::new())
    }
}
