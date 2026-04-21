//! Voice Effects node — karaoke-style voice transformer with 8 modes.
//!
//! The UI is a mode dropdown + three contextual parameter knobs + Mix.
//! Each mode labels its knobs appropriately. Audio is routed through the
//! audio engine's `VoiceEffectsProcessor`; this node only exposes params
//! via `audio_params()` (no per-frame evaluation).
//!
//! Param layout (must match VoiceEffectsProcessor::set_params):
//!   [0] effect id (0..7, rounded to int on the DSP side)
//!   [1] p1  (mode-specific)
//!   [2] p2  (mode-specific)
//!   [3] p3  (reserved, unused today)
//!   [4] mix (0..1)

use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    Hall,
    Echo,
    Chorus,
    Megaphone,
    Chipmunk,
    Robot,
    Alien,
    Ghost,
}

impl Default for Effect {
    fn default() -> Self { Self::Hall }
}

impl Effect {
    fn id(&self) -> f32 {
        match self {
            Self::Hall => 0.0, Self::Echo => 1.0, Self::Chorus => 2.0,
            Self::Megaphone => 3.0, Self::Chipmunk => 4.0, Self::Robot => 5.0,
            Self::Alien => 6.0, Self::Ghost => 7.0,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Hall => "Hall",
            Self::Echo => "Echo",
            Self::Chorus => "Chorus",
            Self::Megaphone => "Megaphone",
            Self::Chipmunk => "Chipmunk/Demon",
            Self::Robot => "Robot",
            Self::Alien => "Alien",
            Self::Ghost => "Ghost",
        }
    }

    /// Default (p1, p2) when switching to this effect.
    fn defaults(&self) -> (f32, f32) {
        match self {
            Self::Hall      => (0.60, 0.30),   // size, damping
            Self::Echo      => (0.30, 0.40),   // time s, feedback
            Self::Chorus    => (1.20, 0.50),   // rate Hz, depth
            Self::Megaphone => (6.00, 0.50),   // drive, tone
            Self::Chipmunk  => (7.00, 0.50),   // semitones, tone
            Self::Robot     => (120.0, 0.40),  // carrier Hz, drive
            Self::Alien     => (0.80, 0.60),   // rate Hz, amount
            Self::Ghost     => (0.70, 0.50),   // size, shimmer
        }
    }

    /// Labels + ranges for the two knobs this effect uses.
    /// Returns (p1_label, p1_range, p2_label, p2_range).
    fn param_labels(&self) -> (&'static str, std::ops::RangeInclusive<f32>,
                               &'static str, std::ops::RangeInclusive<f32>) {
        match self {
            Self::Hall      => ("Size",    0.0..=1.0,   "Damping", 0.0..=1.0),
            Self::Echo      => ("Time (s)",0.04..=0.8,  "Feedback",0.0..=0.9),
            Self::Chorus    => ("Rate Hz", 0.1..=6.0,   "Depth",   0.0..=1.0),
            Self::Megaphone => ("Drive",   1.0..=20.0,  "Tone",    0.0..=1.0),
            Self::Chipmunk  => ("Pitch",  -12.0..=12.0, "Tone",    0.0..=1.0),
            Self::Robot     => ("Freq Hz", 40.0..=800.0,"Drive",   0.0..=1.0),
            Self::Alien     => ("Rate Hz", 0.1..=3.0,   "Amount",  0.0..=1.0),
            Self::Ghost     => ("Size",    0.0..=1.0,   "Shimmer", 0.0..=1.0),
        }
    }
}

const ALL: &[Effect] = &[
    Effect::Hall, Effect::Echo, Effect::Chorus, Effect::Megaphone,
    Effect::Chipmunk, Effect::Robot, Effect::Alien, Effect::Ghost,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceEffectsNode {
    #[serde(default)]
    pub effect: Effect,
    #[serde(default = "default_p1")]
    pub p1: f32,
    #[serde(default = "default_p2")]
    pub p2: f32,
    #[serde(default = "default_mix")]
    pub mix: f32,
    #[serde(skip)]
    params_cache: Vec<f32>,
}

fn default_p1() -> f32 { 0.6 }
fn default_p2() -> f32 { 0.3 }
fn default_mix() -> f32 { 0.5 }

impl Default for VoiceEffectsNode {
    fn default() -> Self {
        Self {
            effect: Effect::Hall,
            p1: 0.6,
            p2: 0.3,
            mix: 0.5,
            params_cache: vec![0.0, 0.6, 0.3, 0.0, 0.5],
        }
    }
}

impl NodeBehavior for VoiceEffectsNode {
    fn title(&self) -> &str { "Voice Effects" }
    fn type_tag(&self) -> &str { "voice_effects" }
    fn color_hint(&self) -> [u8; 3] { [220, 120, 200] }
    fn min_width(&self) -> Option<f32> { Some(220.0) }
    fn inline_ports(&self) -> bool { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Audio", PortKind::Audio),
            PortDef::new("Mix",   PortKind::Normalized),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Audio", PortKind::Audio)]
    }

    fn audio_params(&self) -> &[f32] { &self.params_cache }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(loaded) = serde_json::from_value::<VoiceEffectsNode>(state.clone()) {
            *self = loaded;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;
        let dim     = egui::Color32::from_rgb(110, 110, 120);
        let accent  = egui::Color32::from_rgb(self.color_hint()[0], self.color_hint()[1], self.color_hint()[2]);

        // ── Audio in port ────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Audio,
            );
            ui.label(RichText::new("Audio In").small().color(dim));
        });

        ui.add_space(2.0); ui.separator(); ui.add_space(4.0);

        // ── Effect selector ──────────────────────────────────────────
        let prev_effect = self.effect;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Effect").small().color(dim));
            egui::ComboBox::from_id_salt(egui::Id::new(("voice_fx_mode", node_id)))
                .selected_text(self.effect.label())
                .width(140.0)
                .show_ui(ui, |ui| {
                    for e in ALL {
                        ui.selectable_value(&mut self.effect, *e, e.label());
                    }
                });
        });
        if self.effect != prev_effect {
            // Snap params to the new mode's defaults.
            let (a, b) = self.effect.defaults();
            self.p1 = a;
            self.p2 = b;
        }

        // ── Contextual param knobs ───────────────────────────────────
        let (l1, r1, l2, r2) = self.effect.param_labels();

        ui.horizontal(|ui| {
            ui.label(RichText::new(l1).small().color(dim));
            ui.add(egui::Slider::new(&mut self.p1, r1.clone()).show_value(true));
        });
        self.p1 = self.p1.clamp(*r1.start(), *r1.end());

        ui.horizontal(|ui| {
            ui.label(RichText::new(l2).small().color(dim));
            ui.add(egui::Slider::new(&mut self.p2, r2.clone()).show_value(true));
        });
        self.p2 = self.p2.clamp(*r2.start(), *r2.end());

        // ── Mix (either from port or slider) ─────────────────────────
        let mix_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(
                ui, node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Normalized,
            );
            ui.label(RichText::new("Mix").small().color(dim));
            if mix_wired {
                let v = crate::graph::Graph::static_input_value(
                    ctx.connections, ctx.values, node_id, 1,
                ).as_float().clamp(0.0, 1.0);
                // Preview only — the underlying field keeps the user's value
                // for when the port disconnects again.
                let mut preview = v;
                ui.add_enabled(false, egui::Slider::new(&mut preview, 0.0..=1.0).show_value(true));
            } else {
                ui.add(egui::Slider::new(&mut self.mix, 0.0..=1.0).show_value(true));
            }
        });

        ui.add_space(2.0); ui.separator(); ui.add_space(2.0);

        // ── Output port ─────────────────────────────────────────────
        crate::nodes::output_port_row(
            ui, "Audio", "",
            node_id, 0,
            ctx.port_positions, ctx.dragging_from, ctx.connections, ctx.pending_disconnects,
            PortKind::Audio,
        );

        let _ = accent; // reserved for future visual cue

        // ── Push the live values into the param cache so the audio
        //    thread picks them up on the next block. When Mix is wired
        //    we feed the port value; otherwise the stored slider value.
        if self.params_cache.len() < 5 { self.params_cache.resize(5, 0.0); }
        self.params_cache[0] = self.effect.id();
        self.params_cache[1] = self.p1;
        self.params_cache[2] = self.p2;
        self.params_cache[3] = 0.0; // reserved
        self.params_cache[4] = if mix_wired {
            crate::graph::Graph::static_input_value(
                ctx.connections, ctx.values, node_id, 1,
            ).as_float().clamp(0.0, 1.0)
        } else {
            self.mix.clamp(0.0, 1.0)
        };
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("voice_effects", |state| {
        let mut n = VoiceEffectsNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
