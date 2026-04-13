use eframe::egui::{self, RichText};
use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── AudioAnalyzerNode ────────────────────────────────────────────────────────
//
// Reads audio analysis data (amplitude, peak, bass, mid, treble) injected by
// app/mod.rs from the audio engine. Displays level meters and outputs values.
//
// Ports:
//   In  0: Audio (Audio)
//   Out 0: Audio (Audio, pass-through)
//   Out 1: Amp   (Normalized)
//   Out 2: Peak  (Normalized)
//   Out 3: Bass  (Normalized)
//   Out 4: Mid   (Normalized)
//   Out 5: Treble (Normalized)

#[derive(Debug, Clone)]
pub struct AudioAnalyzerNode;

impl Default for AudioAnalyzerNode {
    fn default() -> Self { Self }
}

impl NodeBehavior for AudioAnalyzerNode {
    fn title(&self)      -> &str   { "Audio Analyzer" }
    fn type_tag(&self)   -> &str   { "audio_analyzer" }
    fn color_hint(&self) -> [u8;3] { [255, 180, 60] }
    fn min_width(&self)  -> Option<f32> { Some(220.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Audio", PortKind::Audio)]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Audio",  PortKind::Audio),
            PortDef::new("Amp",    PortKind::Normalized),
            PortDef::new("Peak",   PortKind::Normalized),
            PortDef::new("Bass",   PortKind::Normalized),
            PortDef::new("Mid",    PortKind::Normalized),
            PortDef::new("Treble", PortKind::Normalized),
        ]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        // Values are injected by app/mod.rs directly into the values map,
        // so evaluate() doesn't need to produce them.
        vec![]
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim = egui::Color32::from_rgb(100, 100, 110);

        // Read analysis data from egui temp (injected by app/mod.rs)
        let (amp, peak, bass, mid, treble, source_name) = ui.ctx().data_mut(|d| {
            let vals: [f32; 5] = d.get_temp(egui::Id::new(("audio_analysis", node_id))).unwrap_or([0.0; 5]);
            let name: String = d.get_temp(egui::Id::new(("audio_analysis_source", node_id))).unwrap_or_default();
            (vals[0], vals[1], vals[2], vals[3], vals[4], name)
        });

        // ── Audio input port ──────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Audio,
            );
            if source_name.is_empty() {
                ui.label(RichText::new("Connect audio input").small().color(dim));
            } else {
                ui.label(RichText::new(format!("← {}", source_name)).small()
                    .color(egui::Color32::from_rgb(80, 200, 120)));
            }
        });

        // Audio pass-through output
        crate::nodes::output_port_row(ui, "Audio", "", node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects, PortKind::Audio);

        ui.add_space(2.0);

        // ── Level meters ──────────────────────────────────────────────────
        let bar_h = 8.0;

        let meters: [(& str, f32, egui::Color32, usize); 5] = [
            ("Amp",    amp,    egui::Color32::from_rgb(80, 200, 120),  1),
            ("Peak",   peak,   egui::Color32::from_rgb(255, 200, 60), 2),
            ("Bass",   bass,   egui::Color32::from_rgb(255, 80, 80),  3),
            ("Mid",    mid,    egui::Color32::from_rgb(80, 160, 255), 4),
            ("Treble", treble, egui::Color32::from_rgb(200, 120, 255), 5),
        ];

        for (label, value, color, port) in &meters {
            ui.horizontal(|ui| {
                ui.label(RichText::new(*label).small().color(egui::Color32::from_rgb(160, 160, 170)));

                // Bar
                let bar_w = ui.available_width() - 60.0;
                let bar_w = bar_w.max(40.0).min(120.0);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(25, 25, 30));
                let fill_w = rect.width() * value.clamp(0.0, 1.0);
                if fill_w > 0.5 {
                    let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
                    painter.rect_filled(fill_rect, 2.0, *color);
                }

                // Value + port
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    crate::nodes::inline_port_circle(
                        ui, node_id, *port, false, ctx.connections,
                        ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized,
                    );
                    ui.label(RichText::new(format!("{:.0}%", value * 100.0)).small().monospace());
                });
            });
        }

        if amp > 0.001 {
            ui.ctx().request_repaint();
        }
    }

    fn save_state(&self) -> serde_json::Value { serde_json::json!({}) }
    fn load_state(&mut self, _state: &serde_json::Value) {}
}

// ── Registration ──────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("audio_analyzer", |state| {
        let mut n = AudioAnalyzerNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
