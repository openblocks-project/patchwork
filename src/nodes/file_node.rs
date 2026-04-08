//! FileNode — universal file loader.
//!
//! Auto-detects file type from extension and outputs the right PortValue:
//! - Text files (.txt, .json, .csv, .wgsl, etc.) → PortValue::Text
//! - Image files (.png, .jpg, .gif, .bmp, .webp) → PortValue::Image
//! - Audio/video files → PortValue::Text(path) for downstream AudioPlayer/VideoPlayer
//!
//! One primary output port. Type adapts to content.

use crate::graph::{PortDef, PortKind, PortValue, ImageData};
use crate::node_trait::NodeBehavior;
use serde::{Serialize, Deserialize};
use eframe::egui;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum FileType {
    #[default]
    Unknown,
    Text,
    Image,
    Audio,
    Video,
    Data,
}

impl FileType {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "txt" | "md" | "json" | "toml" | "yaml" | "yml" | "csv" | "tsv"
            | "xml" | "html" | "css" | "js" | "ts" | "rs" | "py" | "c" | "cpp"
            | "h" | "wgsl" | "glsl" | "hlsl" | "lua" | "sh" | "bat" | "log" => FileType::Text,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tga" | "tiff" => FileType::Image,
            "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a" | "aiff" => FileType::Audio,
            "mp4" | "mov" | "avi" | "webm" | "mkv" => FileType::Video,
            "onnx" | "bin" | "dat" => FileType::Data,
            _ => FileType::Unknown,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            FileType::Text => crate::icons::FILE_TEXT,
            FileType::Image => crate::icons::DIAMOND_FOUR,
            FileType::Audio => crate::icons::FILE_TEXT,  // TODO: add audio icon
            FileType::Video => crate::icons::FILE_TEXT,  // TODO: add video icon
            FileType::Data => crate::icons::CODE,
            FileType::Unknown => crate::icons::FILE_TEXT,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            FileType::Text => "text",
            FileType::Image => "image",
            FileType::Audio => "audio",
            FileType::Video => "video",
            FileType::Data => "data",
            FileType::Unknown => "file",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    pub path: String,
    /// Hot reload: when true (default), every `evaluate()` cheaply stats the
    /// file and re-reads it if mtime advanced. Lets users edit a `.wgsl` file
    /// in VS Code and see the WGSL Viewer update on save without re-clicking
    /// anything in the graph.
    #[serde(default = "default_true")]
    pub auto_reload: bool,
    #[serde(skip)]
    pub last_mtime: Option<std::time::SystemTime>,
    #[serde(skip)]
    pub content: String,
    #[serde(skip)]
    pub image_data: Option<Arc<ImageData>>,
    #[serde(skip)]
    pub file_type: FileType,
    #[serde(skip)]
    pub file_size: u64,
    #[serde(skip)]
    pub loaded: bool,
    /// Last load-time failure, surfaced on every subsequent `evaluate()` call
    /// via `node_errors::report()`. Populated by `load_file` regardless of
    /// whether the trigger was a synchronous click in `render_ui` or the
    /// first-eval auto-load — evaluate() is the only place where the
    /// thread-local "current node" is set, so we re-raise the error from
    /// there on every tick.
    #[serde(skip)]
    pub last_error: Option<String>,
}

fn default_true() -> bool { true }

impl Default for FileNode {
    fn default() -> Self {
        Self {
            path: String::new(),
            auto_reload: true,
            last_mtime: None,
            content: String::new(),
            image_data: None,
            file_type: FileType::Unknown,
            file_size: 0,
            loaded: false,
            last_error: None,
        }
    }
}

impl FileNode {
    pub fn load_file(&mut self) {
        if self.path.is_empty() {
            self.last_error = None;
            return;
        }

        let ext = std::path::Path::new(&self.path)
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();

        self.file_type = FileType::from_extension(&ext);
        // metadata() failing usually means the path is wrong or unreadable —
        // capture that as the error for this load attempt so we don't silently
        // fall back to a size of 0.
        match std::fs::metadata(&self.path) {
            Ok(m) => {
                self.file_size = m.len();
                self.last_mtime = m.modified().ok();
                self.last_error = None;
            }
            Err(e) => {
                self.file_size = 0;
                self.last_mtime = None;
                self.last_error = Some(format!("File stat failed: {}", e));
            }
        }

        match self.file_type {
            FileType::Text | FileType::Unknown => {
                match std::fs::read_to_string(&self.path) {
                    Ok(s) => {
                        self.content = s;
                        self.last_error = None;
                    }
                    Err(e) => {
                        self.content = format!("Error: {}", e);
                        self.last_error = Some(format!("Read failed: {}", e));
                    }
                }
                self.image_data = None;
            }
            FileType::Image => {
                self.content.clear();
                match image::open(&self.path) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        self.image_data = Some(Arc::new(ImageData {
                            width: w, height: h, pixels: rgba.into_raw(),
                        }));
                        self.last_error = None;
                    }
                    Err(e) => {
                        self.content = format!("Image error: {}", e);
                        self.image_data = None;
                        self.last_error = Some(format!("Image decode failed: {}", e));
                    }
                }
            }
            FileType::Audio | FileType::Video | FileType::Data => {
                // Don't load content — output the path for downstream nodes.
                // These types only carry the path forward, so the only way
                // they can fail is the metadata check above.
                self.content.clear();
                self.image_data = None;
            }
        }
        self.loaded = true;
    }
}

impl NodeBehavior for FileNode {
    fn title(&self) -> &str { "File" }
    fn inputs(&self) -> Vec<PortDef> { vec![] }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("File", PortKind::Generic),
            PortDef::new("Path", PortKind::Text),
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [180, 120, 200] }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Auto-load on first evaluate if path is set but not loaded
        if !self.path.is_empty() && !self.loaded {
            self.load_file();
        }

        // Hot reload: cheap mtime poll on every eval. If the file on disk has
        // been touched since we last loaded it, re-read. This is what makes
        // editing a `.wgsl` (or any text file) in VS Code show up live in the
        // downstream WGSL Viewer / consumer the moment you save.
        if self.auto_reload && !self.path.is_empty() {
            if let Ok(meta) = std::fs::metadata(&self.path) {
                if let Ok(m) = meta.modified() {
                    if Some(m) != self.last_mtime {
                        self.load_file();
                    }
                }
            }
        }

        // Re-raise any sticky load error on every eval tick so the canvas
        // badge + Console reflect the current failure state. `evaluate_node`
        // in the graph clears the error before calling us, so a successful
        // load implicitly clears the badge just by not calling report().
        if let Some(msg) = &self.last_error {
            crate::node_errors::report(msg.clone());
        }

        let primary = match self.file_type {
            FileType::Image => {
                if let Some(img) = &self.image_data {
                    PortValue::Image(img.clone())
                } else {
                    PortValue::None
                }
            }
            FileType::Text | FileType::Unknown => {
                if self.content.is_empty() { PortValue::None }
                else { PortValue::Text(self.content.clone()) }
            }
            FileType::Audio | FileType::Video | FileType::Data => {
                PortValue::Text(self.path.clone())
            }
        };

        vec![
            (0, primary),
            (1, PortValue::Text(self.path.clone())),
        ]
    }

    fn type_tag(&self) -> &str { "file" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path,
            "auto_reload": self.auto_reload,
        })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(p) = state.get("path").and_then(|v| v.as_str()) {
            self.path = p.to_string();
            self.loaded = false; // will reload on next evaluate
        }
        if let Some(b) = state.get("auto_reload").and_then(|v| v.as_bool()) {
            self.auto_reload = b;
        }
    }

    fn render_ui(&mut self, ui: &mut egui::Ui) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // File info + icon
        if !self.path.is_empty() {
            let name = std::path::Path::new(&self.path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| self.path.clone());

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(self.file_type.icon()).size(16.0));
                ui.label(egui::RichText::new(&name).strong());
            });

            // Size + type
            let size_str = if self.file_size > 1_048_576 {
                format!("{:.1} MB", self.file_size as f64 / 1_048_576.0)
            } else if self.file_size > 1024 {
                format!("{:.1} KB", self.file_size as f64 / 1024.0)
            } else {
                format!("{} B", self.file_size)
            };
            ui.label(egui::RichText::new(format!("{} · {}", size_str, self.file_type.label())).small().color(dim));
        } else {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::icons::FILE_TEXT).size(16.0).color(dim));
                ui.label(egui::RichText::new("No file").color(dim));
            });
        }

        // Open button + path field
        ui.horizontal(|ui| {
            if ui.button("Open...").clicked() {
                if let Some(fp) = rfd::FileDialog::new()
                    .add_filter("All files", &["*"])
                    .pick_file()
                {
                    self.path = fp.display().to_string();
                    self.load_file();
                }
            }
            if !self.path.is_empty() {
                if ui.small_button("↻").on_hover_text("Reload").clicked() {
                    self.load_file();
                }
                ui.checkbox(&mut self.auto_reload, "Auto")
                    .on_hover_text("Hot reload: re-read file when it changes on disk");
            }
        });

        // Preview based on type
        match self.file_type {
            FileType::Image => {
                if let Some(img) = &self.image_data {
                    ui.label(egui::RichText::new(format!("{}×{}", img.width, img.height)).small().color(dim));
                    // Thumbnail preview
                    let max_w = ui.available_width().min(200.0);
                    let aspect = img.height as f32 / img.width as f32;
                    let preview_h = (max_w * aspect).min(150.0);
                    let pw = max_w as u32;
                    let ph = preview_h as u32;
                    if pw > 0 && ph > 0 {
                        let preview = crate::nodes::video_player::fast_downsample(img, pw, ph);
                        let color_image = egui::ColorImage::from_rgba_unmultiplied([pw as usize, ph as usize], &preview);
                        let tex_id = egui::Id::new(("file_preview", self.path.as_str()));
                        let tex = ui.ctx().load_texture(format!("file_{}", tex_id.value()), color_image, egui::TextureOptions::LINEAR);
                        ui.image(egui::load::SizedTexture::new(tex.id(), egui::vec2(max_w, preview_h)));
                    }
                }
            }
            FileType::Text | FileType::Unknown if !self.content.is_empty() => {
                // Text preview (first ~10 lines)
                let preview: String = self.content.lines().take(10).collect::<Vec<_>>().join("\n");
                let truncated = self.content.lines().count() > 10;
                egui::ScrollArea::vertical().max_height(100.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut preview.as_str())
                        .code_editor().desired_width(f32::INFINITY).interactive(false));
                });
                if truncated {
                    ui.label(egui::RichText::new(format!("... {} more lines", self.content.lines().count() - 10)).small().color(dim));
                }
            }
            FileType::Audio => {
                ui.label(egui::RichText::new("🎵 Connect to Audio Player").small().color(dim));
            }
            FileType::Video => {
                ui.label(egui::RichText::new("🎬 Connect to Video Player").small().color(dim));
            }
            _ => {}
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("file", |state| {
        let mut node = FileNode::default();
        node.load_state(state);
        Box::new(node)
    });
}
