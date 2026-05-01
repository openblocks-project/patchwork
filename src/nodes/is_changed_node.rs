// Is Changed — emits a Trigger pulse when the input value changes by more
// than a threshold. Also passes the current value through. Use it when you
// want a Trigger-port consumer (like OB Motor or OB Light in "On Trigger"
// send mode) to fire only when something actually changes upstream.

use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;

fn default_threshold() -> f32 { 0.001 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsChangedNode {
    /// Minimum |delta| that counts as a change. Below this, no trigger.
    #[serde(default = "default_threshold")]
    pub threshold: f32,

    // Runtime state — not persisted.
    #[serde(skip)]
    pub previous: f32,
    #[serde(skip)]
    pub initialized: bool,
    /// When set, render shows a flash up to this Instant (for visual feedback).
    #[serde(skip)]
    pub flash_until: Option<std::time::Instant>,
}

impl Default for IsChangedNode {
    fn default() -> Self {
        Self {
            threshold: default_threshold(),
            previous: 0.0,
            initialized: false,
            flash_until: None,
        }
    }
}

impl NodeBehavior for IsChangedNode {
    fn title(&self) -> &str { "Is Changed" }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("In", PortKind::Number)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Trigger", PortKind::Trigger),
            PortDef::new("Value",   PortKind::Number),
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [120, 220, 200] }
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let val = inputs.first().map(|v| v.as_float()).unwrap_or(0.0);

        // First evaluation has no previous to compare against — don't fire,
        // just seed the baseline. Otherwise, fire if |Δ| > threshold.
        let fired = if !self.initialized {
            self.initialized = true;
            false
        } else {
            (val - self.previous).abs() > self.threshold
        };

        if fired {
            self.flash_until = Some(
                std::time::Instant::now() + std::time::Duration::from_millis(180),
            );
        }
        self.previous = val;

        vec![
            (0, PortValue::Float(if fired { 1.0 } else { 0.0 })),
            (1, PortValue::Float(val)),
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

        // ── Input row ───────────────────────────────────────
        let val_wired = ctx.connections.iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        let val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0).as_float();
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from,
                ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("In:").small());
            if val_wired {
                ui.label(egui::RichText::new(format!("{:.3}", val))
                    .small().color(ui.visuals().hyperlink_color));
            } else {
                ui.label(egui::RichText::new("—").small().color(dim));
            }
        });

        // ── Threshold ───────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Δ ≥").small().color(dim));
            ui.add(egui::DragValue::new(&mut self.threshold)
                .speed(0.001).range(0.0..=1_000_000.0));
        });

        // ── Live "fired" indicator (held for ~180 ms after fire) ──
        let flashing = self.flash_until.map_or(false, |t| std::time::Instant::now() < t);
        ui.horizontal(|ui| {
            let dot = if flashing {
                egui::Color32::from_rgb(255, 200, 60)
            } else {
                egui::Color32::from_rgb(60, 80, 60)
            };
            ui.colored_label(dot, "●");
            ui.label(egui::RichText::new(format!("prev {:.3}", self.previous))
                .small().monospace().color(dim));
        });

        // Keep the UI ticking so the flash decays smoothly.
        if flashing {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
        }

        ui.separator();

        // ── Outputs ─────────────────────────────────────────
        crate::nodes::output_port_row(ui, "Trig",
            if flashing { "1" } else { "0" },
            ctx.node_id, 0, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Trigger);
        crate::nodes::output_port_row(ui, "Value",
            &format!("{:.3}", val),
            ctx.node_id, 1, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Number);
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
