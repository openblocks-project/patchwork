mod app;
mod artnet;
mod audio;
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
mod serial;
mod settings;
pub mod system_log;
pub mod transport;
mod video_io;

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
        // Box::new(|cc| Ok(Box::new(app::PatchworkApp::new(cc)))),
        Box::new(|cc| {
            cc.egui_ctx.options_mut(|options| {
                options.screen_reader = false;
            });

            Ok(Box::new(app::PatchworkApp::new(cc)))
        }),
    )
}
