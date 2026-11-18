use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::NodeBehavior;
use eframe::egui;
use serde::{Deserialize, Serialize};

const ALL_KEYS: &[(egui::Key, &str)] = &[
    (egui::Key::A, "A"),
    (egui::Key::B, "B"),
    (egui::Key::C, "C"),
    (egui::Key::D, "D"),
    (egui::Key::E, "E"),
    (egui::Key::F, "F"),
    (egui::Key::G, "G"),
    (egui::Key::H, "H"),
    (egui::Key::I, "I"),
    (egui::Key::J, "J"),
    (egui::Key::K, "K"),
    (egui::Key::L, "L"),
    (egui::Key::M, "M"),
    (egui::Key::N, "N"),
    (egui::Key::O, "O"),
    (egui::Key::P, "P"),
    (egui::Key::Q, "Q"),
    (egui::Key::R, "R"),
    (egui::Key::S, "S"),
    (egui::Key::T, "T"),
    (egui::Key::U, "U"),
    (egui::Key::V, "V"),
    (egui::Key::W, "W"),
    (egui::Key::X, "X"),
    (egui::Key::Y, "Y"),
    (egui::Key::Z, "Z"),
    (egui::Key::Num0, "0"),
    (egui::Key::Num1, "1"),
    (egui::Key::Num2, "2"),
    (egui::Key::Num3, "3"),
    (egui::Key::Num4, "4"),
    (egui::Key::Num5, "5"),
    (egui::Key::Num6, "6"),
    (egui::Key::Num7, "7"),
    (egui::Key::Num8, "8"),
    (egui::Key::Num9, "9"),
    (egui::Key::Space, "Space"),
    (egui::Key::Enter, "Enter"),
    (egui::Key::Tab, "Tab"),
    (egui::Key::Escape, "Esc"),
    (egui::Key::Backspace, "Bksp"),
    (egui::Key::Delete, "Del"),
    (egui::Key::ArrowUp, "\u{2191}"),
    (egui::Key::ArrowDown, "\u{2193}"),
    (egui::Key::ArrowLeft, "\u{2190}"),
    (egui::Key::ArrowRight, "\u{2192}"),
    (egui::Key::Home, "Home"),
    (egui::Key::End, "End"),
    (egui::Key::PageUp, "PgUp"),
    (egui::Key::PageDown, "PgDn"),
    (egui::Key::F1, "F1"),
    (egui::Key::F2, "F2"),
    (egui::Key::F3, "F3"),
    (egui::Key::F4, "F4"),
    (egui::Key::F5, "F5"),
    (egui::Key::F6, "F6"),
    (egui::Key::F7, "F7"),
    (egui::Key::F8, "F8"),
    (egui::Key::F9, "F9"),
    (egui::Key::F10, "F10"),
    (egui::Key::F11, "F11"),
    (egui::Key::F12, "F12"),
];

pub fn parse_key(name: &str) -> Option<egui::Key> {
    let lower = name.to_lowercase();
    let trimmed = lower.trim();
    ALL_KEYS
        .iter()
        .find(|(_, d)| d.to_lowercase() == trimmed)
        .map(|(k, _)| *k)
        .or_else(|| match trimmed {
            "up" | "arrowup" => Some(egui::Key::ArrowUp),
            "down" | "arrowdown" => Some(egui::Key::ArrowDown),
            "left" | "arrowleft" => Some(egui::Key::ArrowLeft),
            "right" | "arrowright" => Some(egui::Key::ArrowRight),
            "return" => Some(egui::Key::Enter),
            _ => None,
        })
}

fn key_display_name(name: &str) -> &str {
    let lower = name.to_lowercase();
    for (_, display) in ALL_KEYS {
        if display.to_lowercase() == lower {
            return display;
        }
    }
    name.split_whitespace().next().unwrap_or(name)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyInputNode {
    #[serde(default)]
    pub key_name: String,
    #[serde(skip)]
    pub pressed: bool,
    #[serde(default)]
    pub toggle_mode: bool,
    #[serde(skip)]
    pub toggled_on: bool,
}

impl Default for KeyInputNode {
    fn default() -> Self {
        Self {
            key_name: String::new(),
            pressed: false,
            toggle_mode: false,
            toggled_on: false,
        }
    }
}

impl NodeBehavior for KeyInputNode {
    fn title(&self) -> &str {
        "Keyboard Input"
    }
    fn inputs(&self) -> Vec<PortDef> {
        vec![]
    }
    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Trigger", PortKind::Trigger),
            PortDef::new("Held", PortKind::Gate),
            PortDef::new("Toggle", PortKind::Gate),
        ]
    }
    fn color_hint(&self) -> [u8; 3] {
        [220, 180, 60]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![
            (0, PortValue::Float(if self.pressed { 1.0 } else { 0.0 })),
            (1, PortValue::Float(if self.pressed { 1.0 } else { 0.0 })),
            (2, PortValue::Float(if self.toggled_on { 1.0 } else { 0.0 })),
        ]
    }

    fn type_tag(&self) -> &str {
        "key_input"
    }
    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<KeyInputNode>(state.clone()) {
            self.key_name = l.key_name;
            self.toggle_mode = l.toggle_mode;
        }
    }

    fn render_ui(&mut self, ui: &mut egui::Ui) {
        let node_id_hash = ui.id().value();
        let listening_id = egui::Id::new(("key_listening_d", node_id_hash));
        let is_listening = ui
            .ctx()
            .data_mut(|d| d.get_temp::<bool>(listening_id).unwrap_or(false));

        // Key detection (runs every frame regardless of listening mode)
        // P4.3.2E2: keystrokes reach the graph only with the devices
        // capability (it would otherwise see everything typed in Patchwork).
        if !self.key_name.is_empty()
            && crate::capability::still_allowed(crate::capability::Capability::Devices)
        {
            if let Some(key) = parse_key(&self.key_name) {
                let was_pressed = self.pressed;
                self.pressed = ui.ctx().input(|i| i.key_down(key));
                // Toggle on rising edge
                if self.toggle_mode && self.pressed && !was_pressed {
                    self.toggled_on = !self.toggled_on;
                }
            }
        }

        // Key cap display
        let display = if self.key_name.is_empty() {
            "?"
        } else {
            key_display_name(&self.key_name)
        };
        let is_active = self.pressed || (self.toggle_mode && self.toggled_on);
        let key_size = egui::vec2(ui.available_width().min(80.0), 40.0);
        let (rect, response) = ui.allocate_exact_size(key_size, egui::Sense::click());

        let bg = if is_listening {
            egui::Color32::from_rgb(60, 40, 100)
        } else if is_active {
            ui.visuals().hyperlink_color
        } else {
            ui.visuals().widgets.inactive.bg_fill
        };
        let border = if is_listening {
            egui::Color32::from_rgb(160, 100, 255)
        } else if is_active {
            ui.visuals().hyperlink_color
        } else {
            ui.visuals().widgets.inactive.bg_stroke.color
        };

        let painter = ui.painter();
        painter.rect_filled(rect, 8.0, bg);
        painter.rect_stroke(
            rect,
            8.0,
            egui::Stroke::new(2.0, border),
            egui::StrokeKind::Outside,
        );

        let text_color = if is_active {
            egui::Color32::WHITE
        } else {
            ui.visuals().text_color()
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            display,
            egui::FontId::proportional(18.0),
            text_color,
        );

        if is_listening {
            painter.text(
                egui::pos2(rect.center().x, rect.bottom() + 10.0),
                egui::Align2::CENTER_TOP,
                "Press any key...",
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgb(160, 100, 255),
            );
        }

        if response.clicked() {
            ui.ctx()
                .data_mut(|d| d.insert_temp(listening_id, !is_listening));
        }

        // Key capture
        if is_listening {
            let captured = ui.ctx().input(|i| {
                for (key, name) in ALL_KEYS {
                    if i.key_pressed(*key) {
                        return Some(name.to_string());
                    }
                }
                None
            });
            if let Some(name) = captured {
                self.key_name = name;
                ui.ctx().data_mut(|d| d.insert_temp(listening_id, false));
            }
            if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                ui.ctx().data_mut(|d| d.insert_temp(listening_id, false));
            }
        }

        // Toggle mode
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.toggle_mode, "");
            ui.label(egui::RichText::new("Toggle").small().color(dim));
            if self.toggle_mode && self.toggled_on {
                ui.colored_label(ui.visuals().hyperlink_color, "ON");
            }
        });

        if is_active || is_listening {
            ui.ctx().request_repaint();
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("key_input", |state| {
        if let Ok(n) = serde_json::from_value::<KeyInputNode>(state.clone()) {
            Box::new(n)
        } else {
            Box::new(KeyInputNode::default())
        }
    });
}
