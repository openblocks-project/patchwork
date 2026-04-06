use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Per-fetch result: optional decoded image + optional local cache file
/// path written for remote URLs (so we can survive URL expiry).
type FetchResult = (Option<Arc<ImageData>>, Option<String>);

/// Async fetch state stored in egui temp data per node.
#[derive(Clone)]
struct ImgFetchState {
    loaded_url: String,
    pending_url: String,
    pending_result: Arc<Mutex<Option<FetchResult>>>,
}

/// Hash a URL into a stable filename stem.
fn hash_url(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Sniff bytes to pick an extension. Falls back to .bin.
fn sniff_ext(bytes: &[u8]) -> &'static str {
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" { return "png"; }
    if bytes.len() >= 3 && &bytes[..3] == b"\xff\xd8\xff" { return "jpg"; }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") { return "gif"; }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" { return "webp"; }
    "bin"
}

/// Resolve `~/.patchwork/image_cache/` (creating it if missing).
fn cache_dir() -> Option<std::path::PathBuf> {
    let dir = dirs::home_dir()?.join(".patchwork").join("image_cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Download a remote URL, decode it, and persist the original bytes to
/// the local image cache. Returns the decoded image and the cache path.
fn fetch_and_cache(url: &str) -> FetchResult {
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(_) => return (None, None),
    };
    let resp = match client.get(url).send() {
        Ok(r) if r.status().is_success() => r,
        _ => return (None, None),
    };
    let bytes = match resp.bytes() {
        Ok(b) => b,
        Err(_) => return (None, None),
    };
    let img = match image::load_from_memory(&bytes) {
        Ok(i) => i,
        Err(_) => return (None, None),
    };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let data = Arc::new(ImageData::new(w, h, rgba.into_raw()));

    // Persist original bytes so the node survives URL expiry.
    let cache_path = cache_dir().and_then(|dir| {
        let ext = sniff_ext(&bytes);
        let file = dir.join(format!("{}.{}", hash_url(url), ext));
        std::fs::write(&file, &bytes).ok()?;
        Some(file.display().to_string())
    });

    (Some(data), cache_path)
}

/// Drive the per-node fetch state machine.
///
/// Returns `Some(new_image)` when a freshly-loaded image should overwrite
/// `image_data`. Local file paths load synchronously (cheap); http(s)
/// URLs spawn a background thread and `ctx.request_repaint()` wakes egui
/// up when the bytes arrive — keeping the render thread responsive.
fn maybe_load_async(
    ctx: &egui::Context,
    node_id: NodeId,
    src: &str,
) -> Option<FetchResult> {
    let id = egui::Id::new(("img_fetch_state", node_id));
    let trimmed = src.trim().to_string();
    if trimmed.is_empty() {
        return None;
    }

    let mut state: ImgFetchState = ctx
        .data_mut(|d| d.get_temp::<ImgFetchState>(id))
        .unwrap_or_else(|| ImgFetchState {
            loaded_url: String::new(),
            pending_url: String::new(),
            pending_result: Arc::new(Mutex::new(None)),
        });

    // 1) Drain any completed background fetch first.
    let drained: Option<FetchResult> = state
        .pending_result
        .lock()
        .ok()
        .and_then(|mut g| g.take());
    if let Some(result) = drained {
        state.loaded_url = state.pending_url.clone();
        state.pending_url.clear();
        ctx.data_mut(|d| d.insert_temp(id, state.clone()));
        // Force one more frame: render runs *after* eval, so the new
        // image_data we're about to hand back won't reach downstream
        // consumers until the *next* eval pass. Without this nudge the
        // UI goes idle and the user has to press Send again to wake it.
        ctx.request_repaint();
        return Some(result);
    }

    // 2) Already loaded this exact URL — nothing to do.
    if state.loaded_url == trimmed && state.pending_url.is_empty() {
        ctx.data_mut(|d| d.insert_temp(id, state));
        return None;
    }

    // 3) Already fetching this same URL — wait.
    if state.pending_url == trimmed {
        ctx.data_mut(|d| d.insert_temp(id, state));
        return None;
    }

    // 4) New URL — kick off load.
    let is_remote = trimmed.starts_with("http://") || trimmed.starts_with("https://");
    if is_remote {
        state.pending_url = trimmed.clone();
        // Reset the slot so any stale value doesn't fool us.
        if let Ok(mut g) = state.pending_result.lock() { *g = None; }
        let slot = state.pending_result.clone();
        let ctx_clone = ctx.clone();
        let url = trimmed.clone();
        std::thread::spawn(move || {
            let result = fetch_and_cache(&url);
            if let Ok(mut g) = slot.lock() {
                *g = Some(result);
            }
            ctx_clone.request_repaint();
        });
        ctx.data_mut(|d| d.insert_temp(id, state));
        None
    } else {
        // Local path / file:// — synchronous, cheap. No caching needed
        // (the file already lives on disk).
        let img = load_image_from_source(&trimmed);
        state.loaded_url = trimmed;
        state.pending_url.clear();
        ctx.data_mut(|d| d.insert_temp(id, state));
        Some((img, None))
    }
}

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
) {
    let (path, save_path, image_data, preview_size, last_save_hash, cached_file) = match node_type {
        NodeType::ImageNode { path, save_path, image_data, preview_size, last_save_hash, cached_file } => (path, save_path, image_data, preview_size, last_save_hash, cached_file),
        _ => return,
    };

    // Cold-load: after deserialize, image_data is None. If we have a
    // local cache file from a previous session (e.g. an expired DALL·E
    // URL), prefer it over the original `path`.
    if image_data.is_none() && !cached_file.is_empty() {
        if let Some(img) = load_image_from_path(cached_file) {
            *image_data = Some(img);
        }
    }

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

    // ── Src input port (text URL or file path) ───────────────────
    // When wired, the upstream text overrides the manually-typed path.
    // Supports http(s):// URLs (one-shot blocking download), file://
    // URLs, and plain filesystem paths.
    let src_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
    if src_wired {
        if let PortValue::Text(s) = Graph::static_input_value(connections, values, node_id, 1) {
            let trimmed = s.trim().to_string();
            if !trimmed.is_empty() {
                if trimmed != *path {
                    *path = trimmed.clone();
                }
                // Only swap on a successful load — a failed/empty fetch
                // must NOT clear the previous frame, otherwise downstream
                // blends flash blank between AI responses.
                if let Some((maybe_img, maybe_cache)) = maybe_load_async(ui.ctx(), node_id, &trimmed) {
                    if let Some(new_img) = maybe_img {
                        *image_data = Some(new_img);
                    }
                    if let Some(cp) = maybe_cache {
                        *cached_file = cp;
                    }
                }
            }
        }
    }

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

    // Editable source path / URL — disabled when wired from a port
    let old_path = path.clone();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Src:").small());
        ui.add_enabled(
            !src_wired,
            egui::TextEdit::singleline(path)
                .desired_width(160.0)
                .font(egui::TextStyle::Small)
                .hint_text(if src_wired { "(wired)" } else { "path or url" }),
        );
    });
    if !src_wired && (*path != old_path || (image_data.is_none() && !path.is_empty())) {
        if !path.is_empty() {
            let p = path.clone();
            // Initial load (image_data == None) may set None; subsequent
            // refreshes keep the old image until a new one decodes.
            match maybe_load_async(ui.ctx(), node_id, &p) {
                Some((Some(new_img), maybe_cache)) => {
                    *image_data = Some(new_img);
                    if let Some(cp) = maybe_cache { *cached_file = cp; }
                }
                Some((None, _)) if image_data.is_none() => *image_data = None,
                _ => {}
            }
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

/// Load an image from any source string the Image node accepts:
/// - `http://...` / `https://...` — blocking download via reqwest
/// - `file://...` — strip the prefix and read as a local file
/// - anything else — treated as a filesystem path
///
/// The blocking download only runs when the Src text actually changes
/// (caller-side guard), so this is one HTTP hit per AI response, not
/// per frame.
pub fn load_image_from_source(src: &str) -> Option<Arc<ImageData>> {
    let s = src.trim();
    if s.is_empty() {
        return None;
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .ok()?;
        let resp = client.get(s).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let bytes = resp.bytes().ok()?;
        let img = image::load_from_memory(&bytes).ok()?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        return Some(Arc::new(ImageData::new(w, h, rgba.into_raw())));
    }
    if let Some(rest) = s.strip_prefix("file://") {
        return load_image_from_path(rest);
    }
    load_image_from_path(s)
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
