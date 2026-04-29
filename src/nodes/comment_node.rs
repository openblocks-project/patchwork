//! CommentNode — standalone sticky note node with custom rendering.
//!
//! Demonstrates: custom_render, custom_frame, no_title, color picker,
//! font size control, paper fold effect. This is the proof that plugins
//! can have fully custom visuals.

use crate::graph::{PortDef, PortValue};
use crate::node_trait::NodeBehavior;
use serde::{Serialize, Deserialize};
use eframe::egui;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentNode {
    pub text: String,
    pub bg_color: [u8; 3],
    #[serde(default = "default_font_size")]
    pub font_size: f32,
}

fn default_font_size() -> f32 { 14.0 }

impl Default for CommentNode {
    fn default() -> Self {
        Self {
            text: String::new(),
            bg_color: [45, 45, 50],
            font_size: 14.0,
        }
    }
}

impl NodeBehavior for CommentNode {
    fn title(&self) -> &str { "Comment" }
    fn inputs(&self) -> Vec<PortDef> { vec![] }
    fn outputs(&self) -> Vec<PortDef> { vec![] }
    fn color_hint(&self) -> [u8; 3] { self.bg_color }

    fn apply_theme_defaults(&mut self, ctx: &egui::Context) {
        // Only override the catalog default — never a user pick. Default a
        // fresh sticky note to a slight elevation of the canvas bg so it
        // reads as a card on the active theme.
        if self.bg_color == [45, 45, 50] {
            let bg = crate::nodes::theme::current_bg_arr(ctx);
            self.bg_color = crate::nodes::theme::brighten(bg, 25);
        }
    }

    fn custom_render(&self) -> bool { true }
    fn no_title(&self) -> bool { true }
    fn min_width(&self) -> Option<f32> { Some(130.0) }

    fn render_background(&self, _painter: &egui::Painter, _rect: egui::Rect) -> Option<egui::Frame> {
        // The card fill is delegated to the returned Frame so it always
        // covers the *actual* window rect — the framework only knows an
        // approximate 100px height at the time render_background runs, so
        // painting via the bg painter would leave the bottom of any tall
        // note transparent. The paper-fold decoration is drawn separately
        // in render_ui where we have body geometry.
        let bg = egui::Color32::from_rgb(self.bg_color[0], self.bg_color[1], self.bg_color[2]);
        Some(
            egui::Frame::NONE
                .fill(bg)
                .corner_radius(8.0)
                .inner_margin(12.0),
        )
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> { vec![] }

    fn type_tag(&self) -> &str { "comment" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<CommentNode>(state.clone()) {
            *self = loaded;
        }
    }

    fn render_ui(&mut self, ui: &mut egui::Ui) {
        let node_id = ui.id(); // use egui's widget ID as proxy

        let popup_id = egui::Id::new(("comment_popup_d", node_id));
        let popup_time_id = egui::Id::new(("comment_popup_time_d", node_id));
        let editing_id = egui::Id::new(("comment_editing_d", node_id));
        let just_entered_id = egui::Id::new(("comment_just_entered_d", node_id));
        let now = ui.ctx().input(|i| i.time);

        let is_editing = ui.ctx().data_mut(|d| d.get_temp::<bool>(editing_id).unwrap_or(false));

        // ── Width ────────────────────────────────────────────────────
        // Tight to longest line (so empty notes stay compact), capped at
        // max_body_w (so a long single line wraps instead of dragging the
        // node off the canvas).
        let font = egui::FontId::proportional(self.font_size);
        let placeholder = "Write a note...";
        let measure_line = |ui: &egui::Ui, s: &str| -> f32 {
            ui.fonts(|f| f.layout_no_wrap(s.to_string(), font.clone(), egui::Color32::WHITE).size().x)
        };
        let longest = if self.text.is_empty() {
            measure_line(ui, placeholder)
        } else {
            self.text.lines().map(|l| measure_line(ui, l)).fold(0.0_f32, f32::max)
        };
        let min_body_w = if is_editing { 180.0 } else { 120.0 };
        let max_body_w = 360.0;
        let body_w = (longest + 10.0).clamp(min_body_w, max_body_w);
        let avail_w = ui.available_width().min(body_w);

        // ── Height ───────────────────────────────────────────────────
        // Lay the text out at the body width to find how tall the wrapped
        // content actually is (the single-line `painter.text` was the bug:
        // long inactive notes hid every line after the first). Then clamp
        // the body height to a comfortable range — anything above MAX_H
        // gets a vertical scrollbar via the ScrollArea below.
        const MIN_BODY_H: f32 = 80.0;
        const MAX_BODY_H: f32 = 220.0;
        let display_text = if self.text.is_empty() { placeholder } else { self.text.as_str() };
        let display_color = if self.text.is_empty() {
            ui.visuals().widgets.noninteractive.fg_stroke.color
        } else {
            ui.visuals().text_color()
        };
        let inactive_galley = ui.fonts(|f| {
            f.layout(
                display_text.to_string(),
                font.clone(),
                display_color,
                (avail_w - 8.0).max(1.0),
            )
        });
        let content_h = inactive_galley.size().y + 16.0;
        let body_h = content_h.clamp(MIN_BODY_H, MAX_BODY_H);

        let (body_rect, body_response) = ui.allocate_exact_size(
            egui::vec2(avail_w, body_h),
            egui::Sense::click(),
        );
        let text_rect = body_rect.shrink2(egui::vec2(4.0, 4.0));
        let just_entered = ui.ctx().data_mut(|d| d.get_temp::<bool>(just_entered_id).unwrap_or(false));

        // ── Render text in a ScrollArea (kicks in when content > body_h) ──
        let mut start_editing_from_label = false;
        {
            let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
            if is_editing {
                egui::ScrollArea::vertical()
                    .max_height(text_rect.height())
                    .id_salt(("comment_edit_scroll", node_id))
                    .show(&mut child_ui, |ui| {
                        let text_resp = ui.add(
                            egui::TextEdit::multiline(&mut self.text)
                                .desired_width(text_rect.width())
                                .desired_rows(3)
                                .frame(false)
                                .font(font.clone())
                                .hint_text(placeholder),
                        );
                        if just_entered {
                            text_resp.request_focus();
                            ui.ctx().data_mut(|d| d.insert_temp(just_entered_id, false));
                        }
                        if text_resp.lost_focus() && !just_entered {
                            ui.ctx().data_mut(|d| d.insert_temp(editing_id, false));
                        }
                    });
            } else {
                egui::ScrollArea::vertical()
                    .max_height(text_rect.height())
                    .id_salt(("comment_view_scroll", node_id))
                    .show(&mut child_ui, |ui| {
                        // Label sized via RichText so it picks up the same
                        // font/color the inactive_galley uses for measurement,
                        // and wraps inside the ScrollArea's width.
                        let label_resp = ui.add(
                            egui::Label::new(
                                egui::RichText::new(display_text)
                                    .font(font.clone())
                                    .color(display_color),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if label_resp.clicked() {
                            start_editing_from_label = true;
                        }
                    });
            }
        }

        // ── Click body / inactive label → start editing ──
        if (body_response.clicked() || start_editing_from_label) && !is_editing {
            ui.ctx().data_mut(|d| {
                d.insert_temp(editing_id, true);
                d.insert_temp(just_entered_id, true);
            });
            let is_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
            if !is_open {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(popup_id, true);
                    d.insert_temp(popup_time_id, now);
                });
            }
        }

        // ── Foreground overlays: paper fold + "Comment" footer label ──
        // The card fill is now drawn by the Frame returned from
        // render_background, which always covers the actual window rect.
        // The fold is decorative; drawing it via a Foreground layer keeps
        // it visible above the frame fill (Background paints would be
        // covered) without blocking the options popup (Tooltip layer).
        let node_rect = body_rect.expand(12.0);
        let fold_painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new(("comment_fold_d", node_id)),
        ));
        let fold_size = 14.0_f32;
        let corner_r = 8.0_f32;
        let tr = egui::pos2(
            node_rect.right() - corner_r * 0.25,
            node_rect.top() + corner_r * 0.25,
        );
        let shadow = egui::Color32::from_rgb(
            self.bg_color[0].saturating_sub(30),
            self.bg_color[1].saturating_sub(30),
            self.bg_color[2].saturating_sub(30),
        );
        let flap = egui::Color32::from_rgb(
            self.bg_color[0].saturating_add(18),
            self.bg_color[1].saturating_add(18),
            self.bg_color[2].saturating_add(18),
        );
        fold_painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(tr.x - fold_size, tr.y),
                egui::pos2(tr.x, tr.y + fold_size),
                egui::pos2(tr.x - fold_size, tr.y + fold_size),
            ],
            shadow,
            egui::Stroke::NONE,
        ));
        fold_painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(tr.x - fold_size, tr.y),
                egui::pos2(tr.x, tr.y),
                egui::pos2(tr.x, tr.y + fold_size),
            ],
            flap,
            egui::Stroke::NONE,
        ));
        fold_painter.line_segment(
            [
                egui::pos2(tr.x - fold_size, tr.y),
                egui::pos2(tr.x, tr.y + fold_size),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 60)),
        );

        // "Comment" label bottom-left (same Foreground layer, drawn after
        // the fold so neither covers the other in practice — they don't
        // overlap horizontally).
        let label_painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new(("comment_overlay_d", node_id)),
        ));
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        label_painter.text(
            egui::pos2(node_rect.left() + 14.0, node_rect.bottom() - 6.0),
            egui::Align2::LEFT_BOTTOM,
            "Comment",
            egui::FontId::proportional(10.0),
            dim,
        );

        // No local edit-state ring — see the Scope/Slider notes; the
        // framework already draws the selection highlight.
        let popup_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));

        // Options popup
        if popup_open {
            let popup_pos = egui::pos2(node_rect.right() + 6.0, node_rect.top());
            let opened_time = ui.ctx().data_mut(|d| d.get_temp::<f64>(popup_time_id).unwrap_or(0.0));

            let area_resp = egui::Area::new(egui::Id::new(("comment_opts_d", node_id)))
                .fixed_pos(popup_pos)
                .order(egui::Order::Tooltip)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).corner_radius(10.0).inner_margin(8.0).show(ui, |ui| {
                        ui.set_min_width(130.0);
                        ui.horizontal(|ui| {
                            let mut color = egui::Color32::from_rgb(self.bg_color[0], self.bg_color[1], self.bg_color[2]);
                            if ui.color_edit_button_srgba(&mut color).changed() {
                                let a = color.to_array();
                                self.bg_color = [a[0], a[1], a[2]];
                            }
                            ui.label("Color");
                        });
                        ui.horizontal(|ui| {
                            ui.label("Size");
                            for (label, size) in [("S", 11.0), ("M", 14.0), ("L", 18.0)] {
                                let selected = (self.font_size - size).abs() < 0.5;
                                if ui.selectable_label(selected, label).clicked() {
                                    self.font_size = size;
                                }
                            }
                        });
                    });
                });

            let popup_rect = area_resp.response.rect;
            let esc = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
            let click_outside = if now - opened_time > 0.3 {
                ui.ctx().input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
                    && ui.ctx().pointer_latest_pos().map(|p| !popup_rect.contains(p) && !node_rect.contains(p)).unwrap_or(false)
            } else { false };

            if esc || click_outside {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(popup_id, false);
                    d.insert_temp(editing_id, false);
                });
            }
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("comment", |state| {
        if let Ok(node) = serde_json::from_value::<CommentNode>(state.clone()) {
            Box::new(node)
        } else {
            Box::new(CommentNode::default())
        }
    });
}
