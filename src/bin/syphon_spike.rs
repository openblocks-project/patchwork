//! Phase 2 M1 / M3 verification — wgpu ↔ Metal ↔ Syphon bridge via the
//! real `video_io` module.
//!
//! After M3 this is a tiny wrapper that drives the production
//! `video_io::syphon::SyphonServer` + `video_io::wgpu_metal::*` helpers
//! — no raw objc2 lives here anymore. If it still renders magenta in
//! TouchDesigner, the module's public API is sound and we can move on
//! to wiring it into `VideoOutNode` (M4).
//!
//! Run: `cargo run --bin syphon_spike`  (macOS only)

// On non-macOS the spike compiles to a stub that exits with a clear
// message — `#![cfg(target_os = "macos")]` at the file level would
// remove `fn main` entirely on Windows / Linux and the bin would fail
// to compile. Per-item gates plus a fallback main keep the bin
// buildable everywhere.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("syphon_spike: macOS only — Syphon is a macOS interprocess texture protocol.");
}

// The `patchwork` package is binary-only (no `src/lib.rs`), so this
// bin can't `use patchwork::video_io::…`. Point at the same source
// files via `#[path]` instead — no duplication, the production node
// code and the spike share identical module content.
#[cfg(target_os = "macos")]
#[path = "../video_io/wgpu_metal.rs"]
mod wgpu_metal;
#[cfg(target_os = "macos")]
#[path = "../video_io/syphon.rs"]
mod syphon;

#[cfg(target_os = "macos")]
use eframe::egui_wgpu::wgpu;
#[cfg(target_os = "macos")]
use foreign_types_shared::ForeignType;
#[cfg(target_os = "macos")]
use pollster::block_on;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};
#[cfg(target_os = "macos")]
use syphon::SyphonServer;

#[cfg(target_os = "macos")]
const TEX_W: u32 = 256;
#[cfg(target_os = "macos")]
const TEX_H: u32 = 256;

#[cfg(target_os = "macos")]
fn main() {
    println!("[spike] starting wgpu→Metal→Syphon bridge test via video_io module");

    // ── wgpu on Metal ───────────────────────────────────────────────
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::METAL,
        ..Default::default()
    });
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .expect("[spike] no Metal adapter");
    println!("[spike] adapter: {:?}", adapter.get_info().name);

    let (device, queue) = block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("syphon-spike"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .expect("[spike] request_device failed");

    // ── Magenta BGRA texture via a clear pass ───────────────────────
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("spike-magenta"),
        size: wgpu::Extent3d { width: TEX_W, height: TEX_H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING
             | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("spike-clear"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("magenta-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 1.0, g: 0.0, b: 1.0, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        queue.submit([encoder.finish()]);
    }
    device.poll(wgpu::Maintain::Wait);

    // ── Bridge via the public video_io helpers ──────────────────────
    let mtl_texture = wgpu_metal::wgpu_texture_to_mtl(&texture)
        .expect("[spike] wgpu_texture_to_mtl returned None");
    let mtl_device = wgpu_metal::wgpu_device_to_mtl(&device)
        .expect("[spike] wgpu_device_to_mtl returned None");
    println!(
        "[spike] bridge OK  tex:{:p}  device:{:p}",
        mtl_texture.as_ptr() as *const u8,
        mtl_device.as_ptr() as *const u8,
    );

    // ── SyphonServer via the public type ────────────────────────────
    let server = SyphonServer::new("PatchWork Spike", &mtl_device)
        .expect("[spike] SyphonServer::new failed");
    println!("[spike] Syphon server '{}' up", server.name());

    // ── Publish loop ────────────────────────────────────────────────
    let start = Instant::now();
    let total_seconds = 600;
    let frame_period = Duration::from_millis(16);
    let mut frame_count = 0u64;

    println!(
        "[spike] publishing for {}s — open TouchDesigner → Syphon Spout In → 'PatchWork Spike'",
        total_seconds
    );
    while start.elapsed() < Duration::from_secs(total_seconds) {
        server.publish(&mtl_texture, TEX_W, TEX_H, false);
        frame_count += 1;
        if frame_count % 60 == 0 {
            println!(
                "[spike] frame {}  hasClients={}  elapsed={:.1}s",
                frame_count,
                server.has_clients(),
                start.elapsed().as_secs_f32()
            );
        }
        std::thread::sleep(frame_period);
    }

    // Drop(server) calls -[SyphonMetalServer stop] for us, so the TD
    // picker drops "PatchWork Spike" cleanly.
    println!("[spike] done. Published {} frames", frame_count);
}
