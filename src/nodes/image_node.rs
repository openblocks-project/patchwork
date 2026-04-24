use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-fetch result: optional decoded image + optional local cache file
/// path written for remote URLs (so we can survive URL expiry).
pub type FetchResult = (Option<Arc<ImageData>>, Option<String>);

/// Async fetch state stored in egui temp data per node.
///
/// `gen` is a monotonically increasing counter — every spawned fetch
/// captures the current value. When a fetch completes it only writes
/// to the slot if its captured generation still matches; otherwise a
/// newer fetch has superseded it and the result is dropped. This
/// prevents an in-flight slow fetch from overwriting a fresher one
/// (e.g. mashing the AI Send button back-to-back).
#[derive(Clone)]
struct ImgFetchState {
    loaded_url: String,
    pending_url: String,
    generation: Arc<AtomicU64>,
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

/// Returns `(file_count, total_bytes)` for the image cache directory.
pub fn image_cache_stats() -> (usize, u64) {
    let Some(dir) = cache_dir() else { return (0, 0); };
    let mut count = 0usize;
    let mut bytes = 0u64;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if let Ok(meta) = e.metadata() {
                if meta.is_file() {
                    count += 1;
                    bytes += meta.len();
                }
            }
        }
    }
    (count, bytes)
}

/// Evict oldest cache files until both `max_bytes` and `max_files` are
/// respected. Sort key is mtime (newest kept first). Safe to run on app
/// startup, after settings changes, or after each new download.
pub fn evict_image_cache(max_bytes: u64, max_files: usize) {
    let Some(dir) = cache_dir() else { return; };
    let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if let Ok(meta) = e.metadata() {
                if meta.is_file() {
                    let mt = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    entries.push((e.path(), mt, meta.len()));
                }
            }
        }
    }
    // Newest first; iterate keeping until limits are exhausted, delete the rest.
    entries.sort_by(|a, b| b.1.cmp(&a.1));
    let mut total: u64 = 0;
    let mut kept: usize = 0;
    for (path, _mt, len) in &entries {
        let would_overflow = kept >= max_files || total.saturating_add(*len) > max_bytes;
        if would_overflow {
            let _ = std::fs::remove_file(path);
        } else {
            total += *len;
            kept += 1;
        }
    }
}

/// Wipe every file in the image cache. Used by the Settings node's
/// "Clear cache" button.
pub fn clear_image_cache() {
    let Some(dir) = cache_dir() else { return; };
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Download a remote URL, decode it, and persist the original bytes to
/// the local image cache. Returns the decoded image and the cache path.
///
/// Failures are reported to `crate::node_errors` against `node_id` so the
/// caller's canvas badge + Console pick them up. The return type stays
/// `(None, None)` on failure to keep the calling state-machine simple.
fn fetch_and_cache(node_id: NodeId, url: &str) -> FetchResult {
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            crate::node_errors::report_for(node_id, format!("Image fetch: HTTP client init failed: {}", e));
            return (None, None);
        }
    };
    let resp = match client.get(url).send() {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            crate::node_errors::report_for(node_id, format!("Image fetch: HTTP {}", r.status()));
            return (None, None);
        }
        Err(e) => {
            crate::node_errors::report_for(node_id, format!("Image fetch: {}", e));
            return (None, None);
        }
    };
    let bytes = match resp.bytes() {
        Ok(b) => b,
        Err(e) => {
            crate::node_errors::report_for(node_id, format!("Image fetch: body read failed: {}", e));
            return (None, None);
        }
    };
    let img = match image::load_from_memory(&bytes) {
        Ok(i) => i,
        Err(e) => {
            crate::node_errors::report_for(node_id, format!("Image decode failed: {}", e));
            return (None, None);
        }
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

    // Re-enforce the LRU cap mid-session — without this, a long
    // generative session could blow past the limit between startups.
    let s = crate::settings::Settings::load();
    evict_image_cache(
        (s.image_cache_max_mb as u64) * 1_048_576,
        s.image_cache_max_files as usize,
    );

    (Some(data), cache_path)
}

/// Drive the per-node fetch state machine.
///
/// Returns `Some(new_image)` when a freshly-loaded image should overwrite
/// `image_data`. Local file paths load synchronously (cheap); http(s)
/// URLs spawn a background thread and `ctx.request_repaint()` wakes egui
/// up when the bytes arrive — keeping the render thread responsive.
pub fn maybe_load_async(
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
            generation: Arc::new(AtomicU64::new(0)),
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
        // Bump the generation: any in-flight fetch from a previous URL
        // will see a stale gen at completion and discard its result.
        let my_gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
        // Reset the slot so any stale value doesn't fool us.
        if let Ok(mut g) = state.pending_result.lock() { *g = None; }
        let slot = state.pending_result.clone();
        let gen_ref = state.generation.clone();
        let ctx_clone = ctx.clone();
        let url = trimmed.clone();
        std::thread::spawn(move || {
            let result = fetch_and_cache(node_id, &url);
            // Drop result if a newer fetch was started after us.
            if gen_ref.load(Ordering::SeqCst) != my_gen {
                return;
            }
            // Successful decode clears any stale error badge from a
            // previous failed URL on the same node.
            if result.0.is_some() {
                crate::node_errors::clear(node_id);
            }
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

    // ── Bug A: clear stale GPU stub when upstream disconnects ────
    // When `Image In` (port 0) was wired to a GpuImage we stored a
    // 0-pixel stub in `image_data`. If the upstream is then unwired
    // the stub stays around and downstream Effects/Blend see a 0-pixel
    // image (process_gpu_cached bails). Detect that case and reset so
    // the cold-load + path-reload paths below can refill from disk.
    let image_in_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
    let is_stale_stub = matches!(image_data.as_ref(), Some(img) if img.pixels.is_empty());
    if !image_in_wired && is_stale_stub {
        *image_data = None;
    }

    // Cold-load: after deserialize, image_data is None. If we have a
    // local cache file from a previous session (e.g. an expired DALL·E
    // URL), prefer it over the original `path`. This also runs after
    // the stub-clear above so a disconnected GpuImage upstream falls
    // back to the cached image instead of going blank.
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
            // Arc-aware preview so that when upstream (Camera/Transform) hands
            // us the same frame Arc on successive repaints, we skip the CPU
            // downsample + GPU texture upload entirely.
            show_image_preview_arc(ui, node_id, img, *preview_size);
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
    // Two changes vs the previous impl:
    //
    // 1. Cap the upload at 2× the preview's display size (retina headroom),
    //    not 512. A 640×480 camera frame used to upload 512×384 ≈ 786 KB
    //    per frame just to display at 150 px wide — the GPU sampler then
    //    downsampled it back to ~150 px anyway. Uploading closer to the
    //    display size cuts bandwidth roughly 10×.
    //
    // 2. The "same content?" check used a 32-sample sparse hash with an
    //    `insert_temp` every frame. We don't need content hashing here —
    //    the caller already owns an Arc<ImageData>; if the Arc's data
    //    pointer is the same as last frame's, the content is guaranteed
    //    identical. Single pointer compare, no hashing, no temp churn.
    //    (We fall back to uploading if an Arc isn't available, which is
    //    why this function takes `&ImageData` — but the common path gets
    //    the pointer via the caller below.)
    show_image_preview_cached(ui, node_id, img, None, max_size);
}

/// Internal: same as [`show_image_preview`] but accepts an optional
/// identity key (the upstream Arc's data pointer) so repeated renders of
/// the same frame can skip the downsample + GPU upload entirely.
fn show_image_preview_cached(
    ui: &mut egui::Ui,
    node_id: NodeId,
    img: &ImageData,
    arc_ptr: Option<usize>,
    max_size: f32,
) {
    let tex_id = egui::Id::new(("img_tex", node_id));
    let ptr_id = egui::Id::new(("img_ptr", node_id));

    // Target upload size: ≤ 2× the visible preview size. More than that
    // is just wasted bandwidth — the sampler will shrink it again.
    let target_max = (max_size * 2.0).max(64.0);
    let scale_factor = if img.width as f32 > target_max || img.height as f32 > target_max {
        target_max / img.width.max(img.height) as f32
    } else {
        1.0
    };

    let prev_ptr: Option<usize> = ui.ctx().data_mut(|d| d.get_temp(ptr_id));
    let cache_hit = matches!((arc_ptr, prev_ptr), (Some(a), Some(b)) if a == b);

    // Get or create the TextureHandle ONCE per node_id and re-`set` its
    // contents on each cache miss. A fresh `load_texture` on every new
    // frame churns through egui's texture manager and was the second
    // half of the Camera → Transform → Image jitter the user hit.
    let mut texture: egui::TextureHandle = match ui
        .ctx()
        .data_mut(|d| d.get_temp::<egui::TextureHandle>(tex_id))
    {
        Some(t) => t,
        None => {
            // First render — load a placeholder-sized texture, then the
            // miss branch below uploads the real contents via `.set()`.
            let (tw, th, pix) = downsample_for_preview(img, scale_factor);
            let color_image = egui::ColorImage::from_rgba_unmultiplied([tw, th], &pix);
            let t = ui.ctx().load_texture(
                format!("img_{}", node_id),
                color_image,
                egui::TextureOptions::LINEAR,
            );
            ui.ctx().data_mut(|d| d.insert_temp(tex_id, t.clone()));
            if let Some(p) = arc_ptr {
                ui.ctx().data_mut(|d| d.insert_temp(ptr_id, p));
            }
            t
        }
    };

    if !cache_hit {
        // Frame changed — reupload into the existing handle in place.
        let (tw, th, pix) = downsample_for_preview(img, scale_factor);
        let color_image = egui::ColorImage::from_rgba_unmultiplied([tw, th], &pix);
        texture.set(color_image, egui::TextureOptions::LINEAR);
        if let Some(p) = arc_ptr {
            ui.ctx().data_mut(|d| d.insert_temp(ptr_id, p));
        }
    }

    let aspect = img.width as f32 / img.height.max(1) as f32;
    let (w, h) = if aspect > 1.0 {
        (max_size, max_size / aspect)
    } else {
        (max_size * aspect, max_size)
    };
    ui.image(egui::load::SizedTexture::new(texture.id(), egui::vec2(w, h)));
}

fn downsample_for_preview(img: &ImageData, scale: f32) -> (usize, usize, Vec<u8>) {
    if scale < 0.999 {
        let tw = (img.width as f32 * scale).max(1.0) as u32;
        let th = (img.height as f32 * scale).max(1.0) as u32;
        let mut small = vec![0u8; (tw * th * 4) as usize];
        let inv = 1.0 / scale;
        for y in 0..th {
            let sy = (y as f32 * inv) as u32;
            for x in 0..tw {
                let sx = (x as f32 * inv) as u32;
                let si = ((sy * img.width + sx) * 4) as usize;
                let di = ((y * tw + x) * 4) as usize;
                if si + 3 < img.pixels.len() && di + 3 < small.len() {
                    small[di..di + 4].copy_from_slice(&img.pixels[si..si + 4]);
                }
            }
        }
        (tw as usize, th as usize, small)
    } else {
        // No scaling needed. But the ImageData's `pixels` buffer must
        // match `width × height × 4` or `ColorImage::from_rgba_unmultiplied`
        // on the caller side will panic. Pad / truncate defensively
        // rather than propagate the corrupt buffer; the caller reads
        // the returned dims and sizes its ColorImage to match.
        let expected = (img.width as usize) * (img.height as usize) * 4;
        if img.pixels.len() == expected {
            (img.width as usize, img.height as usize, img.pixels.to_vec())
        } else {
            // Pad with zeroes (or truncate) to the declared dims so
            // downstream ColorImage construction doesn't panic. A
            // follow-up ought to find and fix whichever upstream node
            // is producing this mismatched frame — but a corrupt
            // preview is infinitely preferable to a whole-app crash.
            let mut fixed = vec![0u8; expected];
            let n = img.pixels.len().min(expected);
            fixed[..n].copy_from_slice(&img.pixels[..n]);
            (img.width as usize, img.height as usize, fixed)
        }
    }
}

/// Arc-aware variant: same as [`show_image_preview`] but passes the
/// upstream Arc's data pointer so unchanged frames skip the whole
/// downsample + upload pipeline. Call sites that have an `Arc<ImageData>`
/// should prefer this.
pub fn show_image_preview_arc(
    ui: &mut egui::Ui,
    node_id: NodeId,
    img: &std::sync::Arc<ImageData>,
    max_size: f32,
) {
    let ptr = std::sync::Arc::as_ptr(img) as usize;
    show_image_preview_cached(ui, node_id, img, Some(ptr), max_size);
}
