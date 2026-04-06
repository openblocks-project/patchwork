use crate::graph::*;
use eframe::egui;
use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};

/// Background video decoder using ffmpeg subprocess.
/// Uses a bounded channel (capacity 2) to prevent unbounded memory growth
/// when the UI thread can't keep up with frame production.
struct VideoDecoder {
    process: Child,
    frame_rx: mpsc::Receiver<Arc<ImageData>>,
    _width: u32,
    _height: u32,
    frame_changed: bool,
    disconnected: bool,
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        // Kill the ffmpeg process and wait for it to exit so we don't leak zombie processes.
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl VideoDecoder {
    fn open_file(path: &str, width: u32, height: u32, start_time: f32) -> Result<Self, String> {
        let mut args = vec![
            "-hide_banner".to_string(),
            "-loglevel".into(), "error".into(),
        ];
        if start_time > 0.0 {
            args.extend(["-ss".into(), format!("{:.3}", start_time)]);
        }
        args.extend([
            "-i".into(), path.to_string(),
            "-f".into(), "rawvideo".into(),
            "-pix_fmt".into(), "rgba".into(),
            "-s".into(), format!("{}x{}", width, height),
            "-r".into(), "30".into(),  // output at 30fps
            "pipe:1".into(),
        ]);

        let process = Command::new("ffmpeg")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "ffmpeg not found. Install with: brew install ffmpeg".to_string()
                } else {
                    format!("Failed to start ffmpeg: {}", e)
                }
            })?;

        Self::start_reader(process, width, height)
    }

    fn open_camera(device_index: u32, width: u32, height: u32) -> Result<Self, String> {
        let process = Command::new("ffmpeg")
            .args([
                "-hide_banner", "-loglevel", "error",
                // Low-latency flags: skip probing/buffering for real-time capture
                "-fflags", "nobuffer",
                "-flags", "low_delay",
                "-probesize", "32",
                "-analyzeduration", "0",
                "-f", "avfoundation",
                "-framerate", "30",
                "-video_size", &format!("{}x{}", width, height),
                "-i", &format!("{}:none", device_index),
                "-f", "rawvideo",
                "-pix_fmt", "rgba",
                "-r", "30",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to start camera: {}", e))?;

        Self::start_reader(process, width, height)
    }

    fn start_reader(mut process: Child, width: u32, height: u32) -> Result<Self, String> {
        let stdout = process.stdout.take().ok_or("No stdout")?;
        // Bounded channel (capacity 1): minimal latency — at most 1 frame queued.
        // The producer blocks briefly if the UI hasn't consumed the last frame yet.
        let (tx, rx) = mpsc::sync_channel(1);
        let frame_size = (width * height * 4) as usize;

        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::with_capacity(frame_size, stdout);
            let mut buf = vec![0u8; frame_size];
            loop {
                // read_exact blocks until a full frame arrives from ffmpeg — this is
                // the natural pacing.  The bounded sync_channel(1) provides backpressure
                // if the UI hasn't consumed the previous frame yet, so no artificial
                // sleep is needed.  Removing the sleep eliminates up to 33ms of latency
                // per frame.
                match reader.read_exact(&mut buf) {
                    Ok(()) => {
                        // Zero-copy: swap the filled buffer into the frame and replace
                        // with a fresh buffer.  Avoids cloning ~8MB per frame at 1080p.
                        let pixels = std::mem::replace(&mut buf, vec![0u8; frame_size]);
                        let frame = Arc::new(ImageData { width, height, pixels });
                        if tx.send(frame).is_err() {
                            break; // Receiver dropped (node deleted)
                        }
                    }
                    Err(_) => break, // EOF or error
                }
            }
        });

        Ok(Self {
            process,
            frame_rx: rx,
            _width: width,
            _height: height,
            frame_changed: false,
            disconnected: false,
        })
    }

    fn try_recv_frame(&mut self) -> Option<Arc<ImageData>> {
        let mut latest = None;
        loop {
            match self.frame_rx.try_recv() {
                Ok(frame) => { latest = Some(frame); }
                Err(mpsc::TryRecvError::Empty) => break,       // No new frame, keep last
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Reader thread died (camera unplugged, ffmpeg crashed, EOF)
                    self.disconnected = true;
                    return None;
                }
            }
        }
        if latest.is_some() {
            self.frame_changed = true;
        }
        latest
    }
}

// Store decoders outside the graph (not serializable)
// Using thread_local since all access is from the main/GUI thread
use std::cell::RefCell;
thread_local! {
    static VIDEO_DECODERS: RefCell<HashMap<NodeId, VideoDecoder>> = RefCell::new(HashMap::new());
    static VIDEO_TEXTURES: RefCell<HashMap<NodeId, egui::TextureHandle>> = RefCell::new(HashMap::new());
    static CAMERA_LIST_CACHE: RefCell<(std::time::Instant, Vec<(u32, String)>)> = RefCell::new((std::time::Instant::now(), Vec::new()));
}

/// Get video duration using ffprobe
fn get_duration(path: &str) -> Option<f32> {
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration",
               "-of", "default=noprint_wrappers=1:nokey=1", path])
        .output().ok()?;
    let s = String::from_utf8_lossy(&output.stdout);
    s.trim().parse::<f32>().ok()
}

/// List available camera devices (macOS)
pub fn list_cameras() -> Vec<(u32, String)> {
    let output = Command::new("ffmpeg")
        .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .output();

    let mut cameras = Vec::new();
    if let Ok(output) = output {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut in_video = false;
        for line in stderr.lines() {
            if line.contains("AVFoundation video devices") { in_video = true; continue; }
            if line.contains("AVFoundation audio devices") { break; }
            if in_video {
                // Parse: [AVFoundation indev @ 0x...] [0] Device Name
                if let Some(bracket_start) = line.find("] [") {
                    let rest = &line[bracket_start + 3..];
                    if let Some(bracket_end) = rest.find(']') {
                        if let Ok(idx) = rest[..bracket_end].parse::<u32>() {
                            let name = rest[bracket_end + 2..].trim().to_string();
                            cameras.push((idx, name));
                        }
                    }
                }
            }
        }
    }
    cameras
}

// ── Video Player Node ────────────────────────────────────────────────────────

pub fn render_video(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _connections: &[Connection],
) {
    let (path, playing, looping, res_w, res_h, current_frame, duration, speed, status) = match node_type {
        NodeType::VideoPlayer { path, playing, looping, res_w, res_h, current_frame, duration, speed, status } =>
            (path, playing, looping, res_w, res_h, current_frame, duration, speed, status),
        _ => return,
    };

    // Open button
    ui.horizontal(|ui| {
        if ui.button("Open...").clicked() {
            if let Some(p) = rfd::FileDialog::new()
                .add_filter("Video", &["mp4", "mov", "avi", "webm", "mkv", "gif"])
                .pick_file()
            {
                *path = p.display().to_string();
                *duration = get_duration(&path).unwrap_or(0.0);
                *playing = false;
                *status = "Loaded".into();
                // Stop existing decoder, drop the previously decoded frame,
                // and invalidate any cached GPU textures so downstream nodes
                // don't show one stale frame from the previous file.
                VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
                VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
                *current_frame = None;
                crate::gpu_image::request_node_invalidation(node_id);
            }
        }
    });

    if !path.is_empty() {
        let short = if path.len() > 35 { format!("...{}", &path[path.len()-35..]) } else { path.clone() };
        ui.label(egui::RichText::new(short).small().monospace());
    }

    // Resolution
    ui.horizontal(|ui| {
        ui.label("Res:");
        ui.add(egui::DragValue::new(res_w).range(120..=1920).speed(10).prefix("W:"));
        ui.add(egui::DragValue::new(res_h).range(90..=1080).speed(10).prefix("H:"));
    });

    // Play / Pause / Stop
    ui.horizontal(|ui| {
        let play_label = if *playing { "⏸ Pause" } else { "▶ Play" };
        if ui.button(play_label).clicked() && !path.is_empty() {
            if *playing {
                *playing = false;
                VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
                *status = "Paused".into();
            } else {
                *playing = true;
                match VideoDecoder::open_file(path, *res_w, *res_h, 0.0) {
                    Ok(dec) => {
                        VIDEO_DECODERS.with(|d| d.borrow_mut().insert(node_id, dec));
                        *status = "Playing".into();
                    }
                    Err(e) => *status = e,
                }
            }
        }
        if ui.button("⏹ Stop").clicked() {
            *playing = false;
            VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
            VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
            *current_frame = None;
            crate::gpu_image::request_node_invalidation(node_id);
            *status = "Stopped".into();
        }
        ui.checkbox(looping, "Loop");
    });

    // Speed
    ui.horizontal(|ui| {
        ui.label("Speed:");
        ui.add(egui::Slider::new(speed, 0.25..=4.0).step_by(0.25));
    });

    // Duration display
    if *duration > 0.0 {
        ui.label(egui::RichText::new(format!("Duration: {:.1}s", duration)).small());
    }

    // Status
    if !status.is_empty() {
        let color = if status.contains("Error") || status.contains("not found") {
            egui::Color32::from_rgb(255, 100, 100)
        } else if *playing {
            egui::Color32::from_rgb(80, 200, 80)
        } else {
            egui::Color32::from_rgb(150, 150, 150)
        };
        ui.colored_label(color, egui::RichText::new(&*status).small());
    }

    // Receive frame from decoder
    VIDEO_DECODERS.with(|d| {
        if let Some(decoder) = d.borrow_mut().get_mut(&node_id) {
            if let Some(frame) = decoder.try_recv_frame() {
                *current_frame = Some(frame);
            }
            if decoder.disconnected {
                *current_frame = None;
                *status = "Disconnected".to_string();
                *playing = false;
                crate::system_log::warn(format!("Video source disconnected (id:{})", node_id));
                decoder.disconnected = false;
            }
        }
    });

    // Preview — downsample to preview size before texture upload
    if let Some(frame) = current_frame.as_ref() {
        let max_w = ui.available_width().min(300.0);
        let aspect = frame.height as f32 / frame.width as f32;
        let preview_h = max_w * aspect;
        let pw = max_w as u32;
        let ph = preview_h as u32;

        // Fast downsample for preview (skip pixels, not bilinear)
        let preview_pixels = fast_downsample(frame, pw, ph);

        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [pw as usize, ph as usize],
            &preview_pixels,
        );
        VIDEO_TEXTURES.with(|textures| {
            let mut textures = textures.borrow_mut();
            let tex = textures.entry(node_id).or_insert_with(|| {
                ui.ctx().load_texture(format!("video_{}", node_id), color_image.clone(), egui::TextureOptions::LINEAR)
            });
            tex.set(color_image, egui::TextureOptions::LINEAR);
            ui.image(egui::load::SizedTexture::new(tex.id(), egui::vec2(max_w, preview_h)));
        });
    }

    if *playing {
        ui.ctx().request_repaint();
    }
}

/// Get cached camera list — uses background thread to avoid blocking UI.
/// Returns stale data while refresh is in progress.
fn cached_camera_list() -> Vec<(u32, String)> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<(std::time::Instant, Vec<(u32, String)>, bool)>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new((std::time::Instant::now() - std::time::Duration::from_secs(100), Vec::new(), false)));
    let mut guard = match cache.lock() {
        Ok(g) => g,
        Err(_) => return Vec::new(), // Mutex poisoned — return empty list
    };
    let (last_refresh, ref cameras, ref mut refreshing) = *guard;
    if last_refresh.elapsed().as_secs() >= 10 && !*refreshing {
        *refreshing = true;
        let cache_ref = cache;
        std::thread::spawn(move || {
            let result = list_cameras();
            if let Ok(mut g) = cache_ref.lock() {
                g.0 = std::time::Instant::now();
                g.1 = result;
                g.2 = false;
            }
        });
    }
    cameras.clone()
}

// ── Camera Node ──────────────────────────────────────────────────────────────

pub fn render_camera(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _connections: &[Connection],
) {
    let (device_index, res_w, res_h, active, current_frame, status) = match node_type {
        NodeType::Camera { device_index, res_w, res_h, active, current_frame, status } =>
            (device_index, res_w, res_h, active, current_frame, status),
        _ => return,
    };

    // Device selector
    let cameras = cached_camera_list();
    ui.horizontal(|ui| {
        ui.label("Device:");
        egui::ComboBox::from_id_salt(egui::Id::new(("cam_device", node_id)))
            .selected_text(
                cameras.iter().find(|(i, _)| *i == *device_index)
                    .map(|(_, name)| name.as_str())
                    .unwrap_or("Select...")
            )
            .width(150.0)
            .show_ui(ui, |ui| {
                for (idx, name) in &cameras {
                    if ui.selectable_label(*device_index == *idx, name).clicked() && *device_index != *idx {
                        // Switching device — tear down the old decoder, drop
                        // the cached frame so downstream nodes can't see it
                        // for a single frame, and invalidate any GPU textures
                        // keyed by this node id. Restart capture if the
                        // camera was already running.
                        let was_active = *active;
                        VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
                        VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
                        *current_frame = None;
                        crate::gpu_image::request_node_invalidation(node_id);
                        *device_index = *idx;
                        if was_active {
                            match VideoDecoder::open_camera(*device_index, *res_w, *res_h) {
                                Ok(dec) => {
                                    VIDEO_DECODERS.with(|d| d.borrow_mut().insert(node_id, dec));
                                    *active = true;
                                    *status = "Capturing".into();
                                }
                                Err(e) => {
                                    *active = false;
                                    *status = e;
                                }
                            }
                        } else {
                            *status = "Stopped".into();
                        }
                    }
                }
            });
    });

    // Resolution
    ui.horizontal(|ui| {
        ui.label("Res:");
        ui.add(egui::DragValue::new(res_w).range(160..=1920).speed(10).prefix("W:"));
        ui.add(egui::DragValue::new(res_h).range(120..=1080).speed(10).prefix("H:"));
    });

    // Start / Stop
    ui.horizontal(|ui| {
        if *active {
            if ui.button("⏹ Stop").clicked() {
                *active = false;
                VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
                VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
                *current_frame = None;
                crate::gpu_image::request_node_invalidation(node_id);
                *status = "Stopped".into();
            }
            ui.colored_label(egui::Color32::from_rgb(255, 80, 80), "● REC");
        } else {
            if ui.button("▶ Start").clicked() {
                match VideoDecoder::open_camera(*device_index, *res_w, *res_h) {
                    Ok(dec) => {
                        VIDEO_DECODERS.with(|d| d.borrow_mut().insert(node_id, dec));
                        *active = true;
                        *status = "Capturing".into();
                    }
                    Err(e) => *status = e,
                }
            }
        }
    });

    // Status
    if !status.is_empty() {
        let color = if status.contains("Error") || status.contains("Failed") {
            egui::Color32::from_rgb(255, 100, 100)
        } else if *active {
            egui::Color32::from_rgb(80, 200, 80)
        } else {
            egui::Color32::from_rgb(150, 150, 150)
        };
        ui.colored_label(color, egui::RichText::new(&*status).small());
    }

    // Receive frame — detect camera/source disconnect
    VIDEO_DECODERS.with(|d| {
        if let Some(decoder) = d.borrow_mut().get_mut(&node_id) {
            if let Some(frame) = decoder.try_recv_frame() {
                *current_frame = Some(frame);
            }
            if decoder.disconnected {
                *current_frame = None;
                *status = "Disconnected".to_string();
                *active = false;
                crate::system_log::warn(format!("Camera disconnected (id:{})", node_id));
                decoder.disconnected = false; // only log once
            }
        }
    });

    // Preview — downsample for display
    if let Some(frame) = current_frame.as_ref() {
        let max_w = ui.available_width().min(300.0);
        let aspect = frame.height as f32 / frame.width as f32;
        let preview_h = max_w * aspect;
        let pw = max_w as u32;
        let ph = preview_h as u32;

        let preview_pixels = fast_downsample(frame, pw, ph);
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [pw as usize, ph as usize],
            &preview_pixels,
        );
        VIDEO_TEXTURES.with(|textures| {
            let mut textures = textures.borrow_mut();
            let tex = textures.entry(node_id).or_insert_with(|| {
                ui.ctx().load_texture(format!("cam_{}", node_id), color_image.clone(), egui::TextureOptions::LINEAR)
            });
            tex.set(color_image, egui::TextureOptions::LINEAR);
            ui.image(egui::load::SizedTexture::new(tex.id(), egui::vec2(max_w, preview_h)));
        });
    }

    if *active {
        ui.ctx().request_repaint();
    }
}

/// Fast nearest-neighbor downsample for preview display
pub fn fast_downsample(img: &ImageData, target_w: u32, target_h: u32) -> Vec<u8> {
    if target_w == 0 || target_h == 0 { return vec![]; }
    if target_w >= img.width && target_h >= img.height {
        return img.pixels.clone();
    }
    let mut out = vec![0u8; (target_w * target_h * 4) as usize];
    let x_ratio = img.width as f32 / target_w as f32;
    let y_ratio = img.height as f32 / target_h as f32;
    for y in 0..target_h {
        let sy = (y as f32 * y_ratio) as u32;
        for x in 0..target_w {
            let sx = (x as f32 * x_ratio) as u32;
            let si = ((sy * img.width + sx) * 4) as usize;
            let di = ((y * target_w + x) * 4) as usize;
            if si + 3 < img.pixels.len() && di + 3 < out.len() {
                out[di..di+4].copy_from_slice(&img.pixels[si..si+4]);
            }
        }
    }
    out
}

/// Cleanup decoder and texture when node is deleted
pub fn cleanup_node(node_id: NodeId) {
    VIDEO_DECODERS.with(|d| d.borrow_mut().remove(&node_id));
    VIDEO_TEXTURES.with(|t| t.borrow_mut().remove(&node_id));
    // Also drop GPU cache entries for this node so VRAM is released
    // immediately rather than waiting for the frame-LRU sweep.
    crate::gpu_image::request_node_invalidation(node_id);
}
