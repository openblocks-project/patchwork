use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── NoiseRemovalNode ─────────────────────────────────────────────────────────
//
// Voice-focused noise cleanup. Applies a 2nd-order Butterworth high-pass
// (default 80 Hz, 12 dB/octave) to cut room rumble, AC mains hum, and
// mic-handling thumps. An optional amplitude gate silences steady-state
// hiss between words — threshold in linear amplitude, not dB, so the knob
// is easy to dial in by listening.
//
// Ports:
//   In  0: Audio     (Audio)
//   In  1: Cutoff    (Number, Hz) — HPF frequency override
//   In  2: Threshold (Number) — gate threshold; 0 = gate off
//   Out 0: Audio     (Audio)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseRemovalNode {
    #[serde(default = "default_cutoff")]
    pub cutoff_hz: f32,
    #[serde(default)]
    pub gate_threshold: f32,
    /// Cached params slice returned by `audio_params()` — refreshed each
    /// render frame from `cutoff_hz` and `gate_threshold`.
    #[serde(skip)]
    params_cache: Vec<f32>,
}

fn default_cutoff() -> f32 { 80.0 }

impl Default for NoiseRemovalNode {
    fn default() -> Self {
        Self { cutoff_hz: 80.0, gate_threshold: 0.0, params_cache: vec![80.0, 0.0] }
    }
}

impl NodeBehavior for NoiseRemovalNode {
    fn title(&self)      -> &str   { "Noise Removal" }
    fn type_tag(&self)   -> &str   { "noise_removal" }
    fn color_hint(&self) -> [u8;3] { [120, 200, 255] }
    fn min_width(&self)  -> Option<f32> { Some(200.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Audio",     PortKind::Audio),
            PortDef::new("Cutoff",    PortKind::Number),
            PortDef::new("Threshold", PortKind::Number),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Audio", PortKind::Audio)]
    }

    fn audio_params(&self) -> &[f32] { &self.params_cache }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Audio is routed through the audio engine's processor buffers, not
        // the evaluate() path. No per-UI-frame outputs to produce.
        vec![]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim = egui::Color32::from_rgb(100, 100, 110);
        let accent = egui::Color32::from_rgb(120, 200, 255);

        // Audio input port
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Audio,
            );
            ui.label(RichText::new("Audio In").small().color(dim));
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // Cutoff
        let cutoff_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
        if cutoff_wired {
            self.cutoff_hz = crate::graph::Graph::static_input_value(
                ctx.connections, ctx.values, node_id, 1,
            ).as_float().clamp(20.0, 500.0);
        }
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number,
            );
            ui.label(RichText::new("Cutoff").small().color(dim));
            if !cutoff_wired {
                ui.add(egui::DragValue::new(&mut self.cutoff_hz)
                    .speed(1.0)
                    .range(20.0..=500.0)
                    .suffix(" Hz"));
            } else {
                ui.label(RichText::new(format!("{:.0} Hz", self.cutoff_hz))
                    .small().monospace().color(accent));
            }
        });

        // Threshold (0 = off)
        let thr_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);
        if thr_wired {
            self.gate_threshold = crate::graph::Graph::static_input_value(
                ctx.connections, ctx.values, node_id, 2,
            ).as_float().clamp(0.0, 0.5);
        }
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 2, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Number,
            );
            ui.label(RichText::new("Gate").small().color(dim));
            if !thr_wired {
                ui.add(egui::Slider::new(&mut self.gate_threshold, 0.0..=0.2)
                    .show_value(false)
                    .logarithmic(false));
                let label = if self.gate_threshold < 0.0005 {
                    "off".to_string()
                } else {
                    format!("{:.3}", self.gate_threshold)
                };
                ui.label(RichText::new(label).small().monospace());
            } else {
                ui.label(RichText::new(format!("{:.3}", self.gate_threshold))
                    .small().monospace().color(accent));
            }
        });

        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);

        // Output
        crate::nodes::output_port_row(
            ui, "Audio", "",
            node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Audio,
        );

        // Refresh params cache so the audio thread sees the latest values.
        if self.params_cache.len() < 2 { self.params_cache.resize(2, 0.0); }
        self.params_cache[0] = self.cutoff_hz;
        self.params_cache[1] = self.gate_threshold;
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<NoiseRemovalNode>(state.clone()) {
            *self = loaded;
        }
    }
}

// ── Registration ────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("noise_removal", |state| {
        let mut n = NoiseRemovalNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
