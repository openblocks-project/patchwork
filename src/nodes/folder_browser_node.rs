use crate::graph::{PortDef, PortKind, PortValue, NodeType};
use crate::node_trait::NodeBehavior;
use crate::icons;
use serde::{Serialize, Deserialize};
use eframe::egui;
use crate::nodes::ScrollAreaExt;
use std::path::Path;

#[derive(Clone)]
struct DirEntry {
    name: String,
    full_path: String,
    is_dir: bool,
    size: u64,
    ext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderBrowserNode {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub selected_file: String,
    #[serde(default)]
    pub search: String,
    /// Cached file content — only re-read when selected_file changes.
    #[serde(skip)]
    cached_content: String,
    #[serde(skip)]
    cached_content_path: String,
}

impl Default for FolderBrowserNode {
    fn default() -> Self {
        Self { path: String::new(), selected_file: String::new(), search: String::new(), cached_content: String::new(), cached_content_path: String::new() }
    }
}

impl NodeBehavior for FolderBrowserNode {
    fn title(&self) -> &str { "Folder" }
    fn inputs(&self) -> Vec<PortDef> { vec![] }
    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Path", PortKind::Text), PortDef::new("Name", PortKind::Text), PortDef::new("Content", PortKind::Text)]
    }
    fn color_hint(&self) -> [u8; 3] { [140, 160, 200] }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let mut results = vec![(0, PortValue::Text(self.selected_file.clone()))];
        let name = Path::new(&self.selected_file).file_name()
            .map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        results.push((1, PortValue::Text(name)));
        // Cache file content — only re-read when selected file changes
        if !self.selected_file.is_empty() {
            if self.cached_content_path != self.selected_file {
                self.cached_content = std::fs::read_to_string(&self.selected_file).unwrap_or_default();
                self.cached_content_path = self.selected_file.clone();
            }
            results.push((2, PortValue::Text(self.cached_content.clone())));
        }
        results
    }

    fn type_tag(&self) -> &str { "folder_browser" }
    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<FolderBrowserNode>(state.clone()) { *self = l; }
    }

    fn render_ui(&mut self, ui: &mut egui::Ui) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let node_id_hash = ui.id().value();

        // Path input
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(icons::FOLDER_OPEN).size(14.0));
            ui.add(egui::TextEdit::singleline(&mut self.path)
                .hint_text("Drop folder or type path...")
                .desired_width(ui.available_width() - 30.0));
            if ui.small_button("📂").on_hover_text("Browse...").clicked() {
                if let Some(p) = rfd::FileDialog::new().pick_folder() {
                    self.path = p.to_string_lossy().to_string();
                }
            }
        });

        if self.path.is_empty() {
            ui.colored_label(dim, "No folder selected");
            return;
        }

        // Search
        ui.add(egui::TextEdit::singleline(&mut self.search)
            .hint_text("Filter files...").desired_width(ui.available_width()));
        ui.add_space(2.0);

        let query = self.search.to_lowercase();

        // Read directory (cached)
        let cache_id = egui::Id::new(("folder_cache_d", node_id_hash));
        let dir_hash = self.path.len() as u64 ^ self.path.bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));
        let cached: Option<(u64, Vec<DirEntry>)> = ui.ctx().data_mut(|d| d.get_temp(cache_id));
        let entries = if let Some((prev_hash, entries)) = cached {
            if prev_hash == dir_hash { entries } else {
                let e = read_dir(&self.path);
                ui.ctx().data_mut(|d| d.insert_temp(cache_id, (dir_hash, e.clone())));
                e
            }
        } else {
            let e = read_dir(&self.path);
            ui.ctx().data_mut(|d| d.insert_temp(cache_id, (dir_hash, e.clone())));
            e
        };

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{} items", entries.len())).small().color(dim));
            if ui.small_button("↻").on_hover_text("Refresh").clicked() {
                let e = read_dir(&self.path);
                ui.ctx().data_mut(|d| d.insert_temp(cache_id, (dir_hash.wrapping_add(1), e)));
            }
        });

        // File list
        egui::ScrollArea::vertical().max_height(300.0).show_pannable(ui, |ui| {
            if let Some(parent) = Path::new(&self.path).parent() {
                let (clicked, _) = render_file_row(ui, "..", true, "", 0, false);
                if clicked {
                    self.path = parent.to_string_lossy().to_string();
                    ui.ctx().data_mut(|d| d.remove::<(u64, Vec<DirEntry>)>(cache_id));
                }
            }
            let mut any = false;
            for entry in &entries {
                if !query.is_empty() && !entry.name.to_lowercase().contains(&query) { continue; }
                let selected = entry.full_path == self.selected_file;
                let (clicked, double_clicked) = render_file_row(ui, &entry.name, entry.is_dir, &entry.ext, entry.size, selected);
                if clicked || double_clicked {
                    if entry.is_dir {
                        self.path = entry.full_path.clone();
                        self.selected_file.clear();
                        ui.ctx().data_mut(|d| d.remove::<(u64, Vec<DirEntry>)>(cache_id));
                    } else {
                        self.selected_file = entry.full_path.clone();
                        // Double-click → spawn appropriate node for this file type
                        if double_clicked {
                            if let Some(nt) = node_type_for_file(&entry.full_path, &entry.ext) {
                                ui.memory_mut(|mem| {
                                    let mut v: Vec<NodeType> = mem.data.get_temp(egui::Id::new("palette_spawn")).unwrap_or_default();
                                    v.push(nt);
                                    mem.data.insert_temp(egui::Id::new("palette_spawn"), v);
                                });
                            }
                        }
                    }
                }
                any = true;
            }
            if !any { ui.colored_label(dim, if query.is_empty() { "Empty folder" } else { "No matches" }); }
        });

        if !self.selected_file.is_empty() {
            ui.separator();
            let name = Path::new(&self.selected_file).file_name()
                .map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(icons::CHECK).size(12.0).color(egui::Color32::from_rgb(80, 200, 120)));
                ui.label(egui::RichText::new(&name).small().strong().color(egui::Color32::from_rgb(80, 200, 120)));
            });
        }
    }
}

/// Returns (clicked, double_clicked)
fn render_file_row(ui: &mut egui::Ui, name: &str, is_dir: bool, ext: &str, size: u64, is_selected: bool) -> (bool, bool) {
    let row_h = 22.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let vis = ui.visuals();
        let bg = if is_selected { vis.selection.bg_fill }
            else if response.hovered() { vis.widgets.hovered.bg_fill }
            else { egui::Color32::TRANSPARENT };
        if bg != egui::Color32::TRANSPARENT { painter.rect_filled(rect, 2.0, bg); }

        let icon = if is_dir { icons::FOLDER } else {
            match ext {
                "rs"|"py"|"js"|"ts"|"c"|"cpp"|"h"|"go"|"java" => icons::CODE,
                "png"|"jpg"|"jpeg"|"gif"|"svg"|"webp" => icons::IMAGE,
                "json"|"toml"|"yaml"|"yml"|"xml"|"md"|"txt"|"csv" => icons::FILE_TEXT,
                "wgsl"|"glsl"|"hlsl" => icons::DIAMOND_FOUR,
                _ => icons::FILE_TEXT,
            }
        };
        let icon_color = if is_dir { egui::Color32::from_rgb(255, 200, 80) } else { vis.text_color() };
        painter.text(egui::pos2(rect.left() + 4.0, rect.center().y), egui::Align2::LEFT_CENTER,
            icon, egui::FontId::new(12.0, egui::FontFamily::Proportional), icon_color);

        let name_color = if is_selected { vis.hyperlink_color } else { vis.text_color() };
        painter.text(egui::pos2(rect.left() + 22.0, rect.center().y), egui::Align2::LEFT_CENTER,
            name, egui::FontId::new(11.0, egui::FontFamily::Proportional), name_color);

        if !is_dir && size > 0 {
            let dim = vis.widgets.noninteractive.fg_stroke.color;
            let s = if size < 1024 { format!("{} B", size) }
                else if size < 1048576 { format!("{:.1} KB", size as f64 / 1024.0) }
                else { format!("{:.1} MB", size as f64 / 1048576.0) };
            painter.text(egui::pos2(rect.right() - 4.0, rect.center().y), egui::Align2::RIGHT_CENTER,
                s, egui::FontId::new(9.0, egui::FontFamily::Proportional), dim);
        }
    }
    (response.clicked(), response.double_clicked())
}

/// Create the appropriate NodeType for a file based on its extension.
fn node_type_for_file(path: &str, ext: &str) -> Option<NodeType> {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" => {
            let image_data = crate::nodes::image_node::load_image_from_path(path);
            Some(NodeType::ImageNode {
                path: path.to_string(), save_path: String::new(),
                image_data, preview_size: 150.0, last_save_hash: 0,
                cached_file: String::new(),
            })
        }
        "mp4" | "mov" | "avi" | "webm" | "mkv" => {
            Some(NodeType::VideoPlayer {
                path: path.to_string(), playing: false, looping: false,
                res_w: 640, res_h: 480, current_frame: None,
                duration: 0.0, speed: 1.0, status: "Loaded".into(),
            })
        }
        "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a" => {
            Some(NodeType::AudioPlayer {
                file_path: path.to_string(), volume: 1.0,
                looping: false, duration_secs: 0.0,
            })
        }
        "wgsl" | "glsl" | "hlsl" => {
            let code = std::fs::read_to_string(path).unwrap_or_default();
            Some(NodeType::WgslViewer {
                wgsl_code: code, canvas_w: 300.0, canvas_h: 200.0,
                uniform_names: Vec::new(), uniform_values: Vec::new(),
                uniform_types: Vec::new(), uniform_min: Vec::new(),
                uniform_max: Vec::new(), resolution: 512, expanded: false,
                image_a_mode: crate::graph::ImageInputMode::Unused,
                image_b_mode: crate::graph::ImageInputMode::Unused,
                feedback_reset_pending: false,
                last_compile_ok: false,
                pending_shader_hash: 0,
                pending_shader_since_ms: 0,
                image_a_hint_shown: false,
                image_b_hint_shown: false,
                last_auto_reset_ms: 0,
            })
        }
        _ => {
            // Text/code files → File node
            let mut file_node = crate::nodes::file_node::FileNode::default();
            file_node.path = path.to_string();
            file_node.load_file();
            Some(NodeType::Dynamic { inner: crate::graph::DynNode { node: Box::new(file_node) } })
        }
    }
}

fn read_dir(path: &str) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    let Ok(rd) = std::fs::read_dir(path) else { return entries; };
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else { continue; };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') { continue; }
        let ext = Path::new(&name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        entries.push(DirEntry { name, full_path: entry.path().to_string_lossy().to_string(), is_dir: meta.is_dir(), size: meta.len(), ext });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    entries
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("folder_browser", |state| {
        if let Ok(n) = serde_json::from_value::<FolderBrowserNode>(state.clone()) { Box::new(n) }
        else { Box::new(FolderBrowserNode::default()) }
    });
}
