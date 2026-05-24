use eframe::egui;
use crate::nodes::ScrollAreaExt;
use crate::graph::{NodeId, NodeType};
use crate::node_trait::NodeBehavior;
use crate::icons;
use super::catalog;

pub fn render(
    ui: &mut egui::Ui,
    search: &mut String,
    _node_id: NodeId,
) {
    // When pinned, the node renders under set_zoom_factor(zoom) with inverse-zoom style.
    // Hardcoded sizes (scroll height, card height) need manual compensation.
    let zoom = ui.ctx().zoom_factor();
    let inv = 1.0 / zoom;
    // ── File actions (always accessible) ──
    ui.horizontal(|ui| {
        if ui.button("\u{2795} New").clicked() {
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("file_action_new"), true));
        }
        if ui.button("\u{1f4c2} Open").clicked() {
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("file_action_load"), true));
        }
        if ui.button("\u{1f4be} Save").clicked() {
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("file_action_save"), true));
        }
    });

    ui.separator();

    // Search box
    ui.add(
        egui::TextEdit::singleline(search)
            .hint_text("Search nodes...")
            .desired_width(ui.available_width()),
    );

    ui.add_space(2.0);

    // Category filter pills
    let pill_id = egui::Id::new("palette_cat_filter");
    let mut active_cat: String = ui.ctx().data_mut(|d| d.get_temp(pill_id).unwrap_or_default());
    let accent = ui.visuals().hyperlink_color;
    let categories: &[(&str, &[&str])] = &[
        ("All", &[]),
        ("Math", &["Math", "Logic"]),
        ("Signal", &["Signal", "Input"]),
        ("Visual", &["Image", "Video", "Shader", "ML", "3D"]),
        ("Audio", &["Audio"]),
        ("I/O", &["IO", "Network", "Serial", "OSC", "MIDI", "Hardware"]),
        ("Utility", &["Utility", "Output", "Custom", "System"]),
    ];
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0 * inv;
        for &(label, _) in categories {
            let is_active = if label == "All" { active_cat.is_empty() } else { active_cat == label };
            let text = egui::RichText::new(label).size(10.0 * inv);
            let accent_arr = accent.to_array();
            let btn = if is_active {
                egui::Button::new(text.strong().color(egui::Color32::WHITE))
                    .fill(egui::Color32::from_rgba_unmultiplied(accent_arr[0], accent_arr[1], accent_arr[2], 180))
                    .corner_radius(8.0 * inv)
            } else {
                egui::Button::new(text.color(egui::Color32::GRAY))
                    .fill(egui::Color32::TRANSPARENT)
                    .corner_radius(8.0 * inv)
            };
            if ui.add(btn).clicked() {
                active_cat = if label == "All" { String::new() } else { label.to_string() };
                ui.ctx().data_mut(|d| d.insert_temp(pill_id, active_cat.clone()));
            }
        }
    });

    ui.add_space(2.0);

    let query = search.to_lowercase();
    let cat = catalog();

    // Scrollable list with max height
    egui::ScrollArea::vertical().max_height(400.0 * inv).show_pannable(ui, |ui| {
        let mut last_cat = "";
        let mut any_shown = false;

        for entry in &cat {
            // Category pill filter
            if !active_cat.is_empty() {
                let matched = categories.iter().any(|&(label, cats)| {
                    label == active_cat.as_str() && cats.contains(&entry.category)
                });
                if !matched { continue; }
            }

            if !query.is_empty()
                && !entry.label.to_lowercase().contains(&query)
                && !entry.category.to_lowercase().contains(&query)
            {
                continue;
            }

            // Category header with icon
            if entry.category != last_cat {
                if !last_cat.is_empty() {
                    ui.add_space(4.0);
                }
                let cat_icon = icons::category_icon(entry.category);
                let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
                ui.horizontal(|ui| {
                    ui.label(icons::icon_colored(cat_icon, 12.0 * inv, dim));
                    ui.label(egui::RichText::new(entry.category).small().strong().color(dim));
                });
                ui.add_space(2.0);
                last_cat = entry.category;
            }

            // Node card
            let color_hint = {
                let nt = (entry.factory)();
                let c = nt.color_hint();
                egui::Color32::from_rgb(c[0], c[1], c[2])
            };
            let n_inputs = { let nt = (entry.factory)(); nt.inputs().len() };
            let n_outputs = { let nt = (entry.factory)(); nt.outputs().len() };

            let clicked = render_node_card(ui, entry.label, color_hint, n_inputs, n_outputs, inv);

            if clicked {
                let nt = (entry.factory)();
                ui.memory_mut(|mem| {
                    let mut v: Vec<NodeType> = mem.data.get_temp(egui::Id::new("palette_spawn")).unwrap_or_default();
                    v.push(nt);
                    mem.data.insert_temp(egui::Id::new("palette_spawn"), v);
                });
            }

            any_shown = true;
        }

        if !any_shown {
            ui.add_space(8.0);
            let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
            ui.label(egui::RichText::new("No matches").color(dim).italics());
        }
    }); // end ScrollArea
}

/// Render a mini node card. Returns true if clicked.
fn render_node_card(
    ui: &mut egui::Ui,
    label: &str,
    accent: egui::Color32,
    n_inputs: usize,
    n_outputs: usize,
    inv: f32,
) -> bool {
    let card_width = ui.available_width();
    let card_height = 28.0 * inv;
    let port_r = 3.0 * inv;
    let port_margin = 6.0 * inv;

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(card_width, card_height),
        egui::Sense::click(),
    );

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();

        // Read theme colors from visuals (set by Theme node)
        let vis = ui.visuals();
        let card_bg = if response.hovered() {
            vis.widgets.hovered.bg_fill
        } else {
            vis.widgets.inactive.bg_fill
        };
        let text_color = vis.text_color();
        let port_input_color = vis.widgets.noninteractive.fg_stroke.color;
        let port_output_color = vis.hyperlink_color; // accent-derived

        // Card background
        painter.rect_filled(rect, (4.0 * inv).round().max(1.0), card_bg);

        // Accent bar on left
        let accent_rect = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(3.0 * inv, card_height),
        );
        let cr = (4.0 * inv).round().max(1.0) as u8;
        painter.rect_filled(accent_rect, egui::CornerRadius { nw: cr, sw: cr, ne: 0, se: 0 }, accent);

        // Input port dots (left side)
        if n_inputs > 0 {
            let spacing = card_height / (n_inputs as f32 + 1.0);
            for i in 0..n_inputs {
                let y = rect.top() + spacing * (i as f32 + 1.0);
                painter.circle_filled(
                    egui::pos2(rect.left() + port_margin, y),
                    port_r,
                    port_input_color,
                );
            }
        }

        // Output port dots (right side)
        if n_outputs > 0 {
            let spacing = card_height / (n_outputs as f32 + 1.0);
            for i in 0..n_outputs {
                let y = rect.top() + spacing * (i as f32 + 1.0);
                painter.circle_filled(
                    egui::pos2(rect.right() - port_margin, y),
                    port_r,
                    port_output_color,
                );
            }
        }

        // Icon + label
        let node_icon = icons::node_icon(label);
        let icon_x = rect.left() + 14.0 * inv;
        let text_x = icon_x + 18.0 * inv;
        let cy = rect.center().y;

        painter.text(
            egui::pos2(icon_x, cy),
            egui::Align2::LEFT_CENTER,
            node_icon,
            egui::FontId::proportional(14.0 * inv),
            accent,
        );

        painter.text(
            egui::pos2(text_x, cy),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0 * inv),
            text_color,
        );
    }

    response.clicked()
}
