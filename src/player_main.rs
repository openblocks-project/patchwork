//! The runtime of exported Patchwork apps (P6 Stage 2.3).
//!
//! The same modules as the editor (`src/main.rs`), built for the Windows GUI
//! subsystem so an exported app opens no console window. It only plays the
//! project named by the `player.json` beside it: no editor, no catalogue
//! tool, arguments ignored (see `app::launch`).
#![cfg_attr(windows, windows_subsystem = "windows")]
// The editor binary reports this crate's warnings; here most editor-only
// code is simply unreachable.
#![allow(dead_code, unused_imports)]

mod app;
mod artnet;
mod audio;
mod capability;
mod display;
mod dmx;
mod document;
mod gpu_image;
mod gpu_mesh;
mod graph;
mod hardware;
mod hid;
mod http;
pub mod icons;
mod library;
mod material;
mod mcp;
mod mesh;
mod midi;
mod ml;
mod network;
mod node_errors;
mod node_trait;
mod nodes;
mod ob;
mod osc;
mod plugin_trust;
// P4.3.2B: hardened Rhai engines for script-bearing nodes.
mod scripting;
// P4.3.2B: argument hygiene for external programs.
mod external_process;
mod serial;
mod settings;
pub mod system_log;
pub mod transport;
mod video_io;

// Modules refer to `crate::egui`.
use eframe::egui;

/// Which executable this is (also recorded in its build information).
const BINARY: app::launch::Binary = app::launch::Binary::Player;

fn main() -> eframe::Result {
    app::launch::run(BINARY)
}
