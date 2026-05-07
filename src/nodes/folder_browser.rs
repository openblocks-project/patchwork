#![allow(dead_code)]
use crate::graph::*;
use crate::icons;
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

pub fn render(
    ui: &mut egui::Ui,
    dir_path: &mut String,
    selected_file: &mut String,
    search: &mut String,
    node_id: NodeId,
) {
    // Path input with folder icon
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(icons::FOLDER_OPEN).size(14.0));
        let _resp = ui.add(
            egui::TextEdit::singleline(dir_path)
                .hint_text("Drop folder or type path...")
                .desired_width(ui.available_width() - 30.0),
        );
        if ui.small_button("📂").on_hover_text("Browse...").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                *dir_path = path.to_string_lossy().to_string();
            }
        }
    });

    if dir_path.is_empty() {
        ui.colored_label(egui::Color32::GRAY, "No folder selected");
        return;
    }

    // Search filter
    ui.add(
        egui::TextEdit::singleline(search)
            .hint_text("Filter files...")
            .desired_width(ui.available_width()),
    );
    ui.add_space(2.0);

    let query = search.to_lowercase();

    // Read directory (cached via egui temp data)
    let cache_id = egui::Id::new(("folder_cache", node_id));
    let dir_hash = dir_path.len() as u64 ^ dir_path.bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64));

    let cached: Option<(u64, Vec<DirEntry>)> = ui.ctx().data_mut(|d| d.get_temp(cache_id));
    let entries = if let Some((prev_hash, entries)) = cached {
        if prev_hash == dir_hash {
            entries
        } else {
            let entries = read_dir(dir_path);
            ui.ctx().data_mut(|d| d.insert_temp(cache_id, (dir_hash, entries.clone())));
            entries
        }
    } else {
        let entries = read_dir(dir_path);
        ui.ctx().data_mut(|d| d.insert_temp(cache_id, (dir_hash, entries.clone())));
        entries
    };

    // Refresh button
    ui.horizontal(|ui| {
        let count = entries.len();
        ui.label(egui::RichText::new(format!("{} items", count)).small().color(egui::Color32::GRAY));
        if ui.small_button("↻").on_hover_text("Refresh").clicked() {
            let entries = read_dir(dir_path);
            ui.ctx().data_mut(|d| d.insert_temp(cache_id, (dir_hash.wrapping_add(1), entries)));
        }
    });

    // File list
    egui::ScrollArea::vertical().max_height(300.0).show_pannable(ui, |ui| {
        // Parent directory
        if let Some(parent) = Path::new(dir_path.as_str()).parent() {
            let resp = render_file_row(ui, "..", true, "", 0, false);
            if resp {
                *dir_path = parent.to_string_lossy().to_string();
                // Force cache refresh
                ui.ctx().data_mut(|d| d.remove::<(u64, Vec<DirEntry>)>(cache_id));
            }
        }

        let mut any_shown = false;
        // Directories first, then files
        for entry in &entries {
            if !query.is_empty() && !entry.name.to_lowercase().contains(&query) {
                continue;
            }

            let is_selected = entry.full_path == *selected_file;
            let clicked = render_file_row(ui, &entry.name, entry.is_dir, &entry.ext, entry.size, is_selected);

            if clicked {
                if entry.is_dir {
                    // Navigate into directory
                    *dir_path = entry.full_path.clone();
                    *selected_file = String::new();
                    ui.ctx().data_mut(|d| d.remove::<(u64, Vec<DirEntry>)>(cache_id));
                } else {
                    // Select file
                    *selected_file = entry.full_path.clone();
                    // Open the file in the OS default app
                    let _ = open::that(&entry.full_path);
                }
            }
            any_shown = true;
        }

        if !any_shown {
            ui.colored_label(egui::Color32::GRAY, if query.is_empty() { "Empty folder" } else { "No matches" });
        }
    });

    // Selected file info
    if !selected_file.is_empty() {
        ui.separator();
        let name = Path::new(selected_file.as_str())
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(icons::CHECK).size(12.0).color(egui::Color32::from_rgb(80, 200, 120)));
            ui.label(egui::RichText::new(&name).small().strong().color(egui::Color32::from_rgb(80, 200, 120)));
        });
    }
}

fn render_file_row(ui: &mut egui::Ui, name: &str, is_dir: bool, ext: &str, size: u64, is_selected: bool) -> bool {
    let row_h = 22.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        egui::Sense::click(),
    );

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();

        let bg = if is_selected {
            egui::Color32::from_rgb(40, 60, 80)
        } else if response.hovered() {
            egui::Color32::from_rgb(45, 45, 55)
        } else {
            egui::Color32::TRANSPARENT
        };
        if bg != egui::Color32::TRANSPARENT {
            painter.rect_filled(rect, 2.0, bg);
        }

        // Icon
        let icon = if is_dir {
            icons::FOLDER
        } else {
            match ext {
                "rs" | "py" | "js" | "ts" | "c" | "cpp" | "h" | "go" | "java" => icons::CODE,
                "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" => icons::IMAGE,
                "mp3" | "wav" | "ogg" | "flac" | "aac" => icons::MUSIC_NOTE,
                "mp4" | "mov" | "avi" | "mkv" | "webm" => icons::FILM_STRIP,
                "json" | "toml" | "yaml" | "yml" | "xml" => icons::FILE_TEXT,
                "wgsl" | "glsl" | "hlsl" => icons::DIAMOND_FOUR,
                "md" | "txt" | "csv" => icons::FILE_TEXT,
                _ => icons::FILE_TEXT,
            }
        };

        let icon_color = if is_dir {
            egui::Color32::from_rgb(255, 200, 80)
        } else {
            egui::Color32::from_rgb(160, 160, 180)
        };

        painter.text(
            egui::pos2(rect.left() + 4.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            icon,
            egui::FontId::new(12.0, egui::FontFamily::Proportional),
            icon_color,
        );

        // Name
        let name_color = if is_dir {
            egui::Color32::from_rgb(200, 200, 220)
        } else if is_selected {
            egui::Color32::from_rgb(80, 200, 120)
        } else {
            egui::Color32::from_rgb(180, 180, 200)
        };
        painter.text(
            egui::pos2(rect.left() + 22.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            name,
            egui::FontId::new(11.0, egui::FontFamily::Proportional),
            name_color,
        );

        // Size (for files)
        if !is_dir && size > 0 {
            let size_str = format_size(size);
            painter.text(
                egui::pos2(rect.right() - 4.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                size_str,
                egui::FontId::new(9.0, egui::FontFamily::Proportional),
                egui::Color32::from_rgb(100, 100, 120),
            );
        }
    }

    response.clicked()
}

fn read_dir(path: &str) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(path) else { return entries; };

    for entry in read_dir.flatten() {
        let Ok(meta) = entry.metadata() else { continue; };
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden files
        if name.starts_with('.') { continue; }

        let ext = Path::new(&name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        entries.push(DirEntry {
            name,
            full_path: entry.path().to_string_lossy().to_string(),
            is_dir: meta.is_dir(),
            size: meta.len(),
            ext,
        });
    }

    // Sort: directories first, then alphabetical
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    entries
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 { format!("{} B", bytes) }
    else if bytes < 1024 * 1024 { format!("{:.1} KB", bytes as f64 / 1024.0) }
    else if bytes < 1024 * 1024 * 1024 { format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)) }
    else { format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0)) }
}
