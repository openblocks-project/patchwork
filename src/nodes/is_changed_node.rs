// Is Changed — emits a Trigger pulse when the input value changes. Also
// passes the current value through. Use it when a Trigger-port consumer
// (OB Motor, OB Light in "On Trigger" send mode, …) should fire only on
// real upstream change.
//
// Modes (mode dropdown):
//   • Number — fire when |Δ| > threshold (the original behaviour).
//   • Text   — fire on any string inequality.
//   • Image  — work in progress; passes the frame through but never fires.

use crate::graph::{ImageData, PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;
use std::sync::Arc;

fn default_threshold() -> f32 { 0.001 }

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum IsChangedMode {
    #[default]
    Number,
    Text,
    Image,
}

impl IsChangedMode {
    fn label(self) -> &'static str {
        match self {
            Self::Number => "Number",
            Self::Text   => "Text",
            Self::Image  => "Image (WIP)",
        }
    }
    fn port_kind(self) -> PortKind {
        match self {
            Self::Number => PortKind::Number,
            Self::Text   => PortKind::Text,
            Self::Image  => PortKind::Image,
        }
    }
    const ALL: [Self; 3] = [Self::Number, Self::Text, Self::Image];
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsChangedNode {
    #[serde(default)]
    pub mode: IsChangedMode,

    /// Number-mode minimum |delta| that counts as a change. Below this,
    /// no trigger. Ignored in Text / Image modes.
    #[serde(default = "default_threshold")]
    pub threshold: f32,

    // ── Runtime state — not persisted ───────────────────────────────
    #[serde(skip)] pub previous_number: f32,
    #[serde(skip)] pub previous_text: String,
    /// Arc-pointer of the most recent image. Image-mode change detection
    /// would compare new vs. old via `Arc::ptr_eq`. Wired up but unused
    /// while Image mode is WIP.
    #[serde(skip)] pub previous_image: Option<Arc<ImageData>>,
    #[serde(skip)] pub initialized: bool,
    /// When set, render shows a flash up to this Instant (visual feedback).
    #[serde(skip)] pub flash_until: Option<std::time::Instant>,
}

impl Default for IsChangedNode {
    fn default() -> Self {
        Self {
            mode: IsChangedMode::default(),
            threshold: default_threshold(),
            previous_number: 0.0,
            previous_text: String::new(),
            previous_image: None,
            initialized: false,
            flash_until: None,
        }
    }
}

impl IsChangedNode {
    /// Reset change-detection state. Called when the mode changes so the
    /// first eval after the switch seeds the new baseline rather than
    /// firing on a phantom "change" from prior-mode state.
    fn reset_state(&mut self) {
        self.previous_number = 0.0;
        self.previous_text.clear();
        self.previous_image = None;
        self.initialized = false;
        self.flash_until = None;
    }
}

impl NodeBehavior for IsChangedNode {
    fn title(&self) -> &str { "Is Changed" }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("In", self.mode.port_kind())]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Trigger", PortKind::Trigger),
            PortDef::new("Value",   self.mode.port_kind()),
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [120, 220, 200] }
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let input = inputs.first().cloned().unwrap_or(PortValue::None);

        // First evaluation has no previous to compare against — don't
        // fire, just seed the baseline. Otherwise compare per mode.
        let fired = match self.mode {
            IsChangedMode::Number => {
                let val = input.as_float();
                let f = self.initialized && (val - self.previous_number).abs() > self.threshold;
                self.previous_number = val;
                f
            }
            IsChangedMode::Text => {
                let s = match &input {
                    PortValue::Text(t) => t.clone(),
                    _ => String::new(),
                };
                let f = self.initialized && s != self.previous_text;
                self.previous_text = s;
                f
            }
            IsChangedMode::Image => {
                // WIP: capture the current frame for future Arc-ptr / hash
                // comparison, but never report a change yet.
                if let PortValue::Image(img) = &input {
                    self.previous_image = Some(img.clone());
                } else {
                    self.previous_image = None;
                }
                false
            }
        };
        self.initialized = true;

        if fired {
            self.flash_until = Some(
                std::time::Instant::now() + std::time::Duration::from_millis(180),
            );
        }

        // Pass the input through on Value as the same kind we received.
        // PortValue::None is fine when nothing is wired or input is wrong-type.
        let value_out = match self.mode {
            IsChangedMode::Number => PortValue::Float(input.as_float()),
            IsChangedMode::Text   => match input {
                PortValue::Text(t) => PortValue::Text(t),
                _ => PortValue::Text(String::new()),
            },
            IsChangedMode::Image  => match input {
                PortValue::Image(img) => PortValue::Image(img),
                other => other,
            },
        };

        vec![
            (0, PortValue::Float(if fired { 1.0 } else { 0.0 })),
            (1, value_out),
        ]
    }

    fn type_tag(&self) -> &str { "isChanged" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<IsChangedNode>(state.clone()) {
            *self = loaded;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // ── Mode dropdown ───────────────────────────────────────
        // Switching mode changes the input/output port kinds, so any
        // existing wire on the In port is now type-incompatible. Push it
        // to `pending_disconnects` and reset baseline state so the first
        // post-switch eval doesn't spuriously fire.
        let prev_mode = self.mode;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Mode").small().color(dim));
            egui::ComboBox::from_id_salt(egui::Id::new(("ischanged_mode", ctx.node_id)))
                .selected_text(self.mode.label())
                .width(120.0)
                .show_ui(ui, |ui| {
                    for m in IsChangedMode::ALL {
                        ui.selectable_value(&mut self.mode, m, m.label());
                    }
                });
        });
        if self.mode != prev_mode {
            ctx.pending_disconnects.push((ctx.node_id, 0));
            self.reset_state();
        }

        // ── Input row (kind matches mode) ───────────────────────
        let port_kind = self.mode.port_kind();
        let in_wired = ctx.connections.iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let input_val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from,
                ctx.pending_disconnects, port_kind);
            ui.label(egui::RichText::new("In:").small());
            if in_wired {
                let txt = match (&self.mode, &input_val) {
                    (IsChangedMode::Number, _) => format!("{:.3}", input_val.as_float()),
                    (IsChangedMode::Text,   PortValue::Text(s)) => format!("\"{}\"", crate::nodes::truncate_chars(s, 20)),
                    (IsChangedMode::Text,   _) => "—".into(),
                    (IsChangedMode::Image,  PortValue::Image(img)) =>
                        format!("[{}×{}]", img.width, img.height),
                    (IsChangedMode::Image,  PortValue::GpuImage(h)) =>
                        format!("[GPU {}×{}]", h.width, h.height),
                    (IsChangedMode::Image,  _) => "—".into(),
                };
                ui.label(egui::RichText::new(txt)
                    .small().color(ui.visuals().hyperlink_color));
            } else {
                ui.label(egui::RichText::new("—").small().color(dim));
            }
        });

        // ── Threshold (Number only) ─────────────────────────────
        if self.mode == IsChangedMode::Number {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Δ ≥").small().color(dim));
                ui.add(egui::DragValue::new(&mut self.threshold)
                    .speed(0.001).range(0.0..=1_000_000.0));
            });
        }

        // ── Live "fired" indicator + previous value ──────────────
        let flashing = self.flash_until.map_or(false, |t| std::time::Instant::now() < t);
        ui.horizontal(|ui| {
            let dot = if flashing {
                egui::Color32::from_rgb(255, 200, 60)
            } else {
                egui::Color32::from_rgb(60, 80, 60)
            };
            ui.colored_label(dot, "●");
            let prev_label = match self.mode {
                IsChangedMode::Number =>
                    format!("prev {:.3}", self.previous_number),
                IsChangedMode::Text =>
                    format!("prev \"{}\"", crate::nodes::truncate_chars(&self.previous_text, 16)),
                IsChangedMode::Image => match &self.previous_image {
                    Some(img) => format!("prev [{}×{}]", img.width, img.height),
                    None => "prev —".into(),
                },
            };
            ui.label(egui::RichText::new(prev_label)
                .small().monospace().color(dim));
        });

        // Image-mode WIP banner — keeps users from wiring up a node that
        // silently never fires without explanation. Remove this when Image
        // change-detection is implemented (Arc-ptr or hash compare).
        if self.mode == IsChangedMode::Image {
            ui.colored_label(
                egui::Color32::from_rgb(255, 170, 80),
                egui::RichText::new("⚠ Image mode is work in progress — Trigger never fires.").small(),
            );
        }

        // Keep the UI ticking so the flash decays smoothly.
        if flashing {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
        }

        ui.separator();

        // ── Outputs ─────────────────────────────────────────────
        crate::nodes::output_port_row(ui, "Trig",
            if flashing { "1" } else { "0" },
            ctx.node_id, 0, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Trigger);
        let value_label = match self.mode {
            IsChangedMode::Number => format!("{:.3}", input_val.as_float()),
            IsChangedMode::Text   => match &input_val {
                PortValue::Text(s) => format!("\"{}\"", crate::nodes::truncate_chars(s, 12)),
                _ => "—".into(),
            },
            IsChangedMode::Image  => match &input_val {
                PortValue::Image(img)    => format!("[{}×{}]", img.width, img.height),
                PortValue::GpuImage(h)   => format!("[{}×{}]", h.width, h.height),
                _ => "—".into(),
            },
        };
        crate::nodes::output_port_row(ui, "Value",
            &value_label,
            ctx.node_id, 1, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, port_kind);
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("isChanged", |state| {
        if let Ok(node) = serde_json::from_value::<IsChangedNode>(state.clone()) {
            Box::new(node)
        } else {
            Box::new(IsChangedNode::default())
        }
    });
}
