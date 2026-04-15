//! TimeNode — drift-free elapsed time source.
//!
//! Uses std::time::Instant for accurate wall-clock timing.
//! No cumulative float error — elapsed is computed from (now - start),
//! not accumulated dt.
//!
//! Outputs:
//!   Seconds — total elapsed (scaled by speed)
//!   Frac    — fractional part of seconds (0.0-1.0)
//!   Minutes — total elapsed in minutes

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::NodeBehavior;
use serde::{Serialize, Deserialize};
use eframe::egui;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeNode {
    #[serde(default = "default_speed")]
    pub speed: f32,
    #[serde(default = "default_true")]
    pub running: bool,
    /// Accumulated elapsed time in seconds (scaled by speed).
    /// Computed from wall-clock deltas, not frame dt accumulation.
    #[serde(default)]
    pub elapsed: f64,
    /// Wall-clock instant of last tick (not serialized — reset on load)
    #[serde(skip, default = "Instant::now")]
    last_instant: Instant,
}

fn default_speed() -> f32 { 1.0 }
fn default_true() -> bool { true }

impl Default for TimeNode {
    fn default() -> Self {
        Self {
            speed: 1.0,
            running: true,
            elapsed: 0.0,
            last_instant: Instant::now(),
        }
    }
}

impl NodeBehavior for TimeNode {
    fn title(&self) -> &str { "Time" }
    fn inline_ports(&self) -> bool { true }
    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Speed", PortKind::Number)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Seconds", PortKind::Number),
            PortDef::new("Frac", PortKind::Normalized),
            PortDef::new("Minutes", PortKind::Number),
        ]
    }

    fn color_hint(&self) -> [u8; 3] { [180, 220, 100] }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Use speed from input port if connected, otherwise use internal slider value
        if let Some(PortValue::Float(v)) = inputs.first() {
            if *v != 0.0 || inputs.len() > 0 {
                self.speed = *v;
            }
        }
        let now = Instant::now();
        if self.running {
            let wall_dt = now.duration_since(self.last_instant).as_secs_f64();
            // Clamp to avoid jumps after sleep/wake or debugger pause
            let clamped_dt = wall_dt.min(0.25);
            self.elapsed += clamped_dt * self.speed as f64;
        }
        self.last_instant = now;

        let secs = self.elapsed as f32;
        let frac = (self.elapsed % 1.0) as f32;
        let mins = (self.elapsed / 60.0) as f32;

        vec![
            (0, PortValue::Float(secs)),
            (1, PortValue::Float(frac)),
            (2, PortValue::Float(mins)),
        ]
    }

    fn type_tag(&self) -> &str { "time" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<TimeNode>(state.clone()) {
            self.speed = l.speed;
            self.running = l.running;
            self.elapsed = l.elapsed;
            self.last_instant = Instant::now(); // reset clock reference on load
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut crate::node_trait::RenderContext) {
        ui.horizontal(|ui| {
            if ui.button(if self.running { "⏸" } else { "▶" }).clicked() {
                self.running = !self.running;
                self.last_instant = Instant::now();
            }
            if ui.button("Reset").clicked() {
                self.elapsed = 0.0;
                self.last_instant = Instant::now();
            }
        });

        // Speed: inline input port + slider
        let speed_wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == 0);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number);
            ui.label(egui::RichText::new("Speed").small());
            if speed_wired {
                ui.label(egui::RichText::new(format!("{:.1}", self.speed)).small()
                    .color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                ui.add(egui::Slider::new(&mut self.speed, 0.0..=10.0).step_by(0.1));
            }
        });

        // Display
        let secs = self.elapsed;
        let mins = (secs / 60.0) as u32;
        let s = secs % 60.0;
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        ui.label(egui::RichText::new(format!("{:02}:{:05.2}", mins, s)).monospace().strong());
        ui.label(egui::RichText::new(format!("{:.4}s", secs)).small().color(dim));

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // ── Output ports ─────────────────────────────────────────
        let frac = secs % 1.0;
        crate::nodes::output_port_row(ui, "Seconds", &format!("{:.2}", secs),
            ctx.node_id, 0, ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);
        crate::nodes::output_port_row(ui, "Frac", &format!("{:.2}", frac),
            ctx.node_id, 1, ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Normalized);
        crate::nodes::output_port_row(ui, "Minutes", &format!("{:.2}", secs / 60.0),
            ctx.node_id, 2, ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Number);

        if self.running {
            ui.ctx().request_repaint();
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("time", |state| {
        if let Ok(n) = serde_json::from_value::<TimeNode>(state.clone()) { Box::new(n) }
        else { Box::new(TimeNode::default()) }
    });
}
