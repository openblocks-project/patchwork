mod app;
mod audio;
mod gpu_image;
mod graph;
mod node_trait;
mod hid;
mod http;
pub mod icons;
mod mcp;
mod midi;
mod ml;
mod node_errors;
mod nodes;
mod ob;
mod network;
mod osc;
mod serial;
mod settings;
pub mod system_log;
pub mod transport;

use eframe::egui;
use std::sync::Arc;

fn main() -> eframe::Result {
    audio::clap_host::init_main_thread();
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/icon.png"))
        .expect("Failed to decode embedded icon PNG");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(Arc::new(icon))
            .with_inner_size([1280.0, 800.0])
            .with_title("PatchWork"),
        renderer: eframe::Renderer::Wgpu,
        vsync: false,
        ..Default::default()
    };

    eframe::run_native(
        "PatchWork",
        options,
        Box::new(|cc| Ok(Box::new(app::PatchworkApp::new(cc)))),
    )
}
