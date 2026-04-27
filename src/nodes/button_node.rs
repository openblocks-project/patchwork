//! Button — generic push-button input.
//!
//! A large clickable button. Outputs `on_value` while the mouse is pressed
//! over it, `0.0` otherwise (classic momentary push-button semantics).
//! Right-click opens an options popup (label, on-value, color, delete)
//! mirroring the Slider node's pattern.

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ButtonNode {
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_on_value")]
    pub on_value: f32,
    #[serde(default = "default_color")]
    pub color: [u8; 3],

    #[serde(skip)] pressed: bool,
}

fn default_on_value() -> f32 { 1.0 }
fn default_color() -> [u8; 3] { [80, 160, 255] }

impl Default for ButtonNode {
    fn default() -> Self {
        Self {
            label: String::new(),
            on_value: 1.0,
            color: default_color(),
            pressed: false,
        }
    }
}

impl NodeBehavior for ButtonNode {
    fn title(&self)      -> &str    { "Button" }
    fn type_tag(&self)   -> &str    { "button" }
    fn color_hint(&self) -> [u8; 3] { self.color }
    fn min_width(&self)  -> Option<f32> { Some(100.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self)  -> Vec<PortDef> { vec![] }
    fn outputs(&self) -> Vec<PortDef> { vec![PortDef::new("Value", PortKind::Number)] }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let v = if self.pressed { self.on_value } else { 0.0 };
        vec![(0, PortValue::Float(v))]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<ButtonNode>(state.clone()) { *self = loaded; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;

        // ── Big push-button ──────────────────────────────────────────────────
        let display_label = if self.label.is_empty() {
            format!("Button #{}", node_id)
        } else {
            self.label.clone()
        };
        let fill = egui::Color32::from_rgb(self.color[0], self.color[1], self.color[2]);
        let btn = egui::Button::new(RichText::new(display_label).strong())
            .fill(fill.linear_multiply(0.35))
            .min_size(egui::vec2(ui.available_width().max(80.0), 40.0));
        let r = ui.add(btn);

        // Held-while-down semantics.
        self.pressed = r.is_pointer_button_down_on();

        // Push live value into egui temp data so the Dynamic re-eval + output
        // write uses the same frame's state. We don't need an injection
        // override in app/mod.rs because `evaluate()` reads `self.pressed`
        // directly on the next tick.

        // Right-click → toggle options popup.
        let popup_id      = egui::Id::new(("button_popup", node_id));
        let popup_time_id = egui::Id::new(("button_popup_time", node_id));
        let now           = ui.ctx().input(|i| i.time);
        if r.secondary_clicked() {
            let open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
            ui.ctx().data_mut(|d| {
                d.insert_temp(popup_id, !open);
                d.insert_temp(popup_time_id, now);
            });
        }

        ui.add_space(4.0);

        // ── Output port (bottom, centered) ───────────────────────────────────
        ui.vertical_centered(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, false,
                ctx.connections, ctx.port_positions,
                ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Number,
            );
        });

        // Selection feedback is drawn centrally by PatchworkApp; the popup
        // contents themselves indicate "popup is open" — no second ring needed.

        // ── Options popup ────────────────────────────────────────────────────
        let popup_is_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
        if popup_is_open {
            let popup_pos    = egui::pos2(r.rect.right() + 8.0, r.rect.top());
            let opened_time  = ui.ctx().data_mut(|d| d.get_temp::<f64>(popup_time_id).unwrap_or(0.0));
            let area_resp = egui::Area::new(egui::Id::new(("button_opts", node_id)))
                .fixed_pos(popup_pos)
                .order(egui::Order::Tooltip)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).corner_radius(12.0).inner_margin(12.0).show(ui, |ui| {
                        ui.set_min_width(220.0);

                        // Row 1: color | label | delete
                        ui.horizontal(|ui| {
                            let mut color = egui::Color32::from_rgb(self.color[0], self.color[1], self.color[2]);
                            if ui.color_edit_button_srgba(&mut color).changed() {
                                let a = color.to_array();
                                self.color = [a[0], a[1], a[2]];
                            }
                            ui.add(egui::TextEdit::singleline(&mut self.label)
                                .hint_text(format!("Button #{}", node_id))
                                .desired_width(120.0));
                            if ui.small_button(
                                RichText::new(crate::icons::TRASH).color(egui::Color32::from_rgb(200, 80, 80)),
                            ).clicked() {
                                ui.ctx().data_mut(|d| {
                                    d.insert_temp(egui::Id::new(("button_delete_action", node_id)), true);
                                    d.insert_temp(popup_id, false);
                                });
                            }
                        });

                        ui.add_space(6.0);

                        // Value sent on press
                        ui.horizontal(|ui| {
                            ui.label("On value");
                            ui.add(egui::DragValue::new(&mut self.on_value).speed(0.01));
                        });

                        ui.add_space(4.0);
                        ui.label(RichText::new(format!("ID: {}", node_id)).small().color(egui::Color32::from_rgb(80, 80, 80)));
                    });
                });

            // Close on Escape or click-outside (after a short open delay so
            // the right-click that opened the popup doesn't also close it).
            let popup_rect = area_resp.response.rect;
            let esc = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
            let click_outside = if now - opened_time > 0.2 {
                ui.ctx().input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
                    && ui.ctx().pointer_latest_pos()
                        .map(|p| !popup_rect.contains(p) && !r.rect.contains(p))
                        .unwrap_or(false)
            } else { false };

            if esc || click_outside {
                ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
            }
        }
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("button", |state| {
        let mut n = ButtonNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
