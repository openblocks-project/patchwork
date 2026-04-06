use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;
use std::sync::Arc;

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
) {
    let (path, save_path, image_data, preview_size, last_save_hash) = match node_type {
        NodeType::ImageNode { path, save_path, image_data, preview_size, last_save_hash } => (path, save_path, image_data, preview_size, last_save_hash),
        _ => return,
    };

    // Check if there's an input image (for receiving processed images).
    // For GpuImage, store a stub Arc<ImageData> with empty pixels — the GPU
    // display callback samples directly from gpu_tex_cache, and save-to-disk
    // is gated below on non-empty pixels.
    let input_val = Graph::static_input_value(connections, values, node_id, 0);
    let input_gpu_source: Option<(NodeId, usize)> = match &input_val {
        PortValue::Image(img) => {
            *image_data = Some(img.clone());
            None
        }
        PortValue::GpuImage(handle) => {
            *image_data = Some(Arc::new(ImageData {
                width: handle.width,
                height: handle.height,
                pixels: Vec::new(),
            }));
            Some((handle.node_id, handle.port))
        }
        _ => None,
    };

    // Open / Save buttons
    ui.horizontal(|ui| {
        if ui.button("Open...").clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp"])
                .pick_file()
            {
                *path = p.display().to_string();
                *image_data = load_image_from_path(&p.display().to_string());
            }
        }
        if ui.button("Save...").clicked() {
            if let Some(img) = image_data.as_ref().filter(|i| !i.pixels.is_empty()) {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("PNG", &["png"])
                    .add_filter("JPEG", &["jpg", "jpeg"])
                    .save_file()
                {
                    let sp = p.display().to_string();
                    save_image(img, &sp);
                    *save_path = sp;
                }
            }
        }
    });

    // Editable source path
    let old_path = path.clone();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Src:").small());
        ui.add(egui::TextEdit::singleline(path).desired_width(160.0).font(egui::TextStyle::Small));
    });
    if *path != old_path || (image_data.is_none() && !path.is_empty()) {
        if !path.is_empty() {
            *image_data = load_image_from_path(path);
        }
    }

    // Auto-save path — when set, saves automatically when image changes
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Save:").small());
        ui.add(egui::TextEdit::singleline(save_path).desired_width(160.0).font(egui::TextStyle::Small).hint_text("auto-save path"));
    });

    // Auto-save: if save_path is set and image exists, save when image changes
    if !save_path.is_empty() {
        if let Some(img) = image_data.as_ref().filter(|i| !i.pixels.is_empty()) {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            img.width.hash(&mut h);
            img.height.hash(&mut h);
            if img.pixels.len() >= 16 {
                img.pixels[..16].hash(&mut h);
                img.pixels[img.pixels.len()-16..].hash(&mut h);
            }
            let hash = h.finish();
            if hash != *last_save_hash {
                save_image(img, save_path);
                *last_save_hash = hash;
                ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "✓ Saved");
            }
        }
    }

    // Preview
    if let Some(img) = image_data.as_ref() {
        ui.label(egui::RichText::new(format!("{}x{}", img.width, img.height)).small());
        if img.pixels.is_empty() || input_gpu_source.is_some() {
            // GPU-resident path: sample from gpu_tex_cache via the paint callback.
            show_image_gpu(ui, node_id, img, *preview_size, input_gpu_source);
        } else {
            show_image_preview(ui, node_id, img, *preview_size);
        }
    } else {
        ui.colored_label(egui::Color32::GRAY, "No image loaded");
    }
}

pub fn load_image_from_path(path: &str) -> Option<Arc<ImageData>> {
    let img = image::open(path).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(Arc::new(ImageData::new(w, h, rgba.into_raw())))
}

fn save_image(img: &ImageData, path: &str) {
    let buf = image::RgbaImage::from_raw(img.width, img.height, img.pixels.clone());
    if let Some(buf) = buf {
        let _ = buf.save(path);
    }
}

/// Display an image thumbnail in the UI. Caches texture per node + image pointer.
/// Render image via wgpu paint callback — bypasses egui texture system.
/// Faster for animated images (no ColorImage copy + load_texture each frame).
pub fn show_image_gpu(ui: &mut egui::Ui, node_id: NodeId, img: &std::sync::Arc<ImageData>, max_size: f32, gpu_source: Option<(NodeId, usize)>) {
    if img.width == 0 || img.height == 0 { return; }

    let aspect = img.height as f32 / img.width as f32;
    let w = max_size.min(ui.available_width());
    let h = w * aspect;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());

    // If pixels are empty AND no GPU source, nothing to render
    if img.pixels.is_empty() && gpu_source.is_none() { return; }

    let target_format = ui.ctx().data_mut(|d| {
        d.get_temp::<eframe::egui_wgpu::wgpu::TextureFormat>(egui::Id::new("wgpu_target_format"))
    }).unwrap_or(eframe::egui_wgpu::wgpu::TextureFormat::Bgra8UnormSrgb);

    ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(
        rect,
        crate::gpu_image::GpuImageDisplayCallback {
            node_id,
            img: img.clone(),
            target_format,
            gpu_source,
        },
    ));
}

pub fn show_image_preview(ui: &mut egui::Ui, node_id: NodeId, img: &ImageData, max_size: f32) {
    let tex_id = egui::Id::new(("img_tex", node_id));
    let hash_id = egui::Id::new(("img_hash", node_id));

    // Fast content hash — same as image eval loop cache
    let img_hash = {
        let mut h: u64 = (img.width as u64).wrapping_mul(31).wrapping_add(img.height as u64);
        let len = img.pixels.len();
        if len > 0 {
            let step = (len / 128).max(1);
            for i in (0..len).step_by(step).take(32) {
                h = h.wrapping_mul(31).wrapping_add(img.pixels[i] as u64);
            }
        }
        h
    };
    let prev_hash: Option<u64> = ui.ctx().data_mut(|d| d.get_temp(hash_id));

    let texture: egui::TextureHandle = if prev_hash == Some(img_hash) {
        // Same content — reuse cached texture
        if let Some(tex) = ui.ctx().data_mut(|d| d.get_temp::<egui::TextureHandle>(tex_id)) {
            tex
        } else {
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [img.width as usize, img.height as usize], &img.pixels,
            );
            ui.ctx().load_texture(format!("img_{}", node_id), color_image, egui::TextureOptions::LINEAR)
        }
    } else {
        // New image content — upload (downsample large images for preview)
        let (tw, th, pix) = if img.width > 512 || img.height > 512 {
            let scale = 512.0 / img.width.max(img.height) as f32;
            let tw = (img.width as f32 * scale) as u32;
            let th = (img.height as f32 * scale) as u32;
            let mut small = vec![0u8; (tw * th * 4) as usize];
            for y in 0..th {
                for x in 0..tw {
                    let sx = (x as f32 / scale) as u32;
                    let sy = (y as f32 / scale) as u32;
                    let si = ((sy * img.width + sx) * 4) as usize;
                    let di = ((y * tw + x) * 4) as usize;
                    if si + 3 < img.pixels.len() && di + 3 < small.len() {
                        small[di..di+4].copy_from_slice(&img.pixels[si..si+4]);
                    }
                }
            }
            (tw as usize, th as usize, small)
        } else {
            (img.width as usize, img.height as usize, img.pixels.to_vec())
        };
        let color_image = egui::ColorImage::from_rgba_unmultiplied([tw, th], &pix);
        ui.ctx().load_texture(format!("img_{}", node_id), color_image, egui::TextureOptions::LINEAR)
    };

    ui.ctx().data_mut(|d| d.insert_temp(hash_id, img_hash));

    // Compute display size maintaining aspect ratio
    let aspect = img.width as f32 / img.height.max(1) as f32;
    let (w, h) = if aspect > 1.0 {
        (max_size, max_size / aspect)
    } else {
        (max_size * aspect, max_size)
    };

    ui.image(egui::load::SizedTexture::new(texture.id(), egui::vec2(w, h)));
    ui.ctx().data_mut(|d| d.insert_temp(tex_id, texture));
}
