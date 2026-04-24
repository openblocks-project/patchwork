//! wgpu ↔ Metal bridge — extract raw `MTLDevice` / `MTLTexture`
//! handles from wgpu types. macOS only.
//!
//! wgpu doesn't expose its HAL types on the public surface; we reach
//! in via the `as_hal` closure API. The M1 spike at
//! `src/bin/syphon_spike.rs` proved this chain works against wgpu
//! 24.0.5 + wgpu-hal 24.0.4 + metal 0.31.

use eframe::egui_wgpu::wgpu;
use wgpu_hal::metal::Api as MetalApi;

/// Extract the raw `metal::Texture` backing a `wgpu::Texture`.
///
/// Returns `None` if the device isn't on the Metal backend — should
/// never happen on macOS where wgpu has no other backend, but we'd
/// rather degrade gracefully than panic from deep inside a publish
/// loop.
///
/// SAFETY: `wgpu::Texture::as_hal` is unsafe; calling it with the
/// correct `Api` matches what the backend expects. The closure result
/// (`metal::Texture`) is an Arc-like wrapper over the underlying
/// `id<MTLTexture>` so cloning out of the closure is cheap and safe.
pub fn wgpu_texture_to_mtl(tex: &wgpu::Texture) -> Option<metal::Texture> {
    unsafe {
        tex.as_hal::<MetalApi, _, _>(|hal_opt| {
            hal_opt.map(|hal_tex| hal_tex.raw_handle().to_owned())
        })
    }
}

/// Extract the raw `metal::Device` backing a `wgpu::Device`.
///
/// `wgpu_hal::metal::Device::raw_device()` returns a `Mutex` guard
/// because wgpu-hal serialises some command encoding paths internally.
/// We lock it briefly, clone the underlying handle (cheap Arc-like)
/// and drop the guard — callers never see the Mutex.
///
/// Called once at SyphonServer/Client setup; not on the hot path.
pub fn wgpu_device_to_mtl(device: &wgpu::Device) -> Option<metal::Device> {
    unsafe {
        device.as_hal::<MetalApi, _, _>(|hal_opt| {
            hal_opt.map(|hal_dev| hal_dev.raw_device().lock().clone())
        })
    }
}

/// Wrap an existing `metal::Texture` (typically handed to us by a
/// Syphon client's `newFrameImage`) as a `wgpu::Texture`. Needed so
/// incoming Syphon frames can live inside the existing GPU cache /
/// snapshot plumbing without any pixel copies.
///
/// Maps the MTL pixel format to wgpu's `TextureFormat`; if the format
/// isn't one we recognise we fall back to `Bgra8Unorm` (the most
/// common Syphon publish format) and let the caller worry about
/// potential swizzle.
///
/// SAFETY: caller must keep the underlying `mtl_tex` alive for at
/// least as long as the returned `wgpu::Texture` is in use. In M5 we
/// re-construct this per frame and let wgpu drop its reference when
/// the next frame arrives.
pub fn mtl_to_wgpu_texture(
    device: &wgpu::Device,
    mtl_tex: metal::Texture,
) -> Option<wgpu::Texture> {
    use foreign_types_shared::ForeignTypeRef;
    let width = mtl_tex.width() as u32;
    let height = mtl_tex.height() as u32;
    let mtl_format = mtl_tex.pixel_format();
    let wgpu_format = match mtl_format {
        metal::MTLPixelFormat::BGRA8Unorm => wgpu::TextureFormat::Bgra8Unorm,
        metal::MTLPixelFormat::BGRA8Unorm_sRGB => wgpu::TextureFormat::Bgra8UnormSrgb,
        metal::MTLPixelFormat::RGBA8Unorm => wgpu::TextureFormat::Rgba8Unorm,
        metal::MTLPixelFormat::RGBA8Unorm_sRGB => wgpu::TextureFormat::Rgba8UnormSrgb,
        _ => wgpu::TextureFormat::Bgra8Unorm,
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("syphon-imported"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu_format,
        // Syphon textures are published as render attachments; mark
        // them readable for our downstream TEXTURE_BINDING consumers.
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC
             | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    };
    let copy_size = wgpu_hal::CopyExtent { width, height, depth: 1 };
    unsafe {
        let hal_tex = wgpu_hal::metal::Device::texture_from_raw(
            mtl_tex,
            wgpu_format,
            metal::MTLTextureType::D2,
            1, // array layers
            1, // mip levels
            copy_size,
        );
        Some(device.create_texture_from_hal::<MetalApi>(hal_tex, &desc))
    }
}
