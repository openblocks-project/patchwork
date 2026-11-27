// P6 Stage 3.12: installer builds (`--features windows-gui-editor`) are GUI
// programs, without a console window; every other build stays a console
// program. Never for the test harness, whose output is the console.
#![cfg_attr(
    all(windows, feature = "windows-gui-editor", not(test)),
    windows_subsystem = "windows"
)]
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

// P6 Stage 2.3: start-up lives in `app::launch`, shared with the exported
// apps' runtime (`src/player_main.rs`, a GUI-subsystem build of the same
// modules).
/// Which executable this is (also recorded in its build information).
const BINARY: app::launch::Binary = app::launch::Binary::Editor;

fn main() -> eframe::Result {
    // `patchwork catalogue …` is the headless tool the marketplace catalogue
    // repository's CI runs (P4.3.4); `patchwork --player <project>` runs a
    // project without the editor (P6); a `player.json` beside the
    // executable makes it an exported app (P6 Stage 2.1).
    app::launch::run(BINARY)
}
