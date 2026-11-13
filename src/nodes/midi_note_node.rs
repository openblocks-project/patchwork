use crate::graph::*;
use crate::node_trait::*;
use eframe::egui;
use serde_json::Value;

/// MIDI Note — converts raw MIDI note/velocity numbers to musical values.
/// Input: Note (0-127), Velocity (0-127)
/// Output: Frequency (Hz), Gate (0/1), Amplitude (0-1), Note (pass-through)
#[derive(Clone, Debug)]
pub struct MidiNoteNode {
    last_note: f32,
    last_velocity: f32,
}

impl Default for MidiNoteNode {
    fn default() -> Self {
        Self {
            last_note: 0.0,
            last_velocity: 0.0,
        }
    }
}

impl MidiNoteNode {
    /// MIDI note number → frequency in Hz.
    /// A4 (note 69) = 440 Hz. Each semitone = 2^(1/12).
    fn note_to_freq(note: f32) -> f32 {
        440.0 * (2.0_f32).powf((note - 69.0) / 12.0)
    }

    /// Note name from MIDI number (for display)
    fn note_name(note: u8) -> String {
        const NAMES: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        let octave = (note as i32 / 12) - 1;
        let name = NAMES[(note % 12) as usize];
        format!("{}{}", name, octave)
    }
}

impl NodeBehavior for MidiNoteNode {
    fn title(&self) -> &str {
        "MIDI Note"
    }
    fn type_tag(&self) -> &str {
        "midi_note"
    }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Note", PortKind::Number),
            PortDef::new("Vel", PortKind::Number),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Freq", PortKind::Number),
            PortDef::new("Gate", PortKind::Gate),
            PortDef::new("Amp", PortKind::Normalized),
            PortDef::new("Note", PortKind::Number),
        ]
    }

    fn color_hint(&self) -> [u8; 3] {
        [80, 200, 160]
    } // same as MIDI In

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let note = inputs
            .get(0)
            .map(|v| v.as_float())
            .unwrap_or(self.last_note);
        let velocity = inputs
            .get(1)
            .map(|v| v.as_float())
            .unwrap_or(self.last_velocity);

        // Only update when we get actual input
        if note > 0.0 {
            self.last_note = note;
        }
        self.last_velocity = velocity;

        let freq = Self::note_to_freq(self.last_note);
        let gate = if velocity > 0.0 { 1.0 } else { 0.0 };
        let amp = (velocity / 127.0).clamp(0.0, 1.0);

        vec![
            (0, PortValue::Float(freq)),
            (1, PortValue::Float(gate)),
            (2, PortValue::Float(amp)),
            (3, PortValue::Float(self.last_note)),
        ]
    }

    fn render_ui(&mut self, ui: &mut egui::Ui) {
        let note = self.last_note as u8;
        let freq = Self::note_to_freq(self.last_note);
        let gate = self.last_velocity > 0.0;
        let amp = (self.last_velocity / 127.0).clamp(0.0, 1.0);

        // Note display
        let note_name = Self::note_name(note);
        let gate_color = if gate {
            egui::Color32::from_rgb(80, 220, 80)
        } else {
            egui::Color32::from_rgb(80, 80, 90)
        };

        ui.horizontal(|ui| {
            // Big note name
            ui.label(
                egui::RichText::new(&note_name)
                    .size(20.0)
                    .strong()
                    .color(gate_color),
            );
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(format!("{:.1} Hz", freq))
                        .small()
                        .monospace(),
                );
                ui.label(
                    egui::RichText::new(format!("vel {:.0}", self.last_velocity))
                        .small()
                        .monospace()
                        .color(egui::Color32::GRAY),
                );
            });
        });

        // Velocity bar
        let bar_h = 6.0;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().min(140.0), bar_h),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(25, 25, 30));
        let fill_w = rect.width() * amp;
        if fill_w > 0.5 {
            let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
            painter.rect_filled(fill_rect, 2.0, gate_color);
        }
    }

    fn save_state(&self) -> Value {
        serde_json::json!({
            "last_note": self.last_note,
            "last_velocity": self.last_velocity,
        })
    }

    fn load_state(&mut self, state: &Value) {
        if let Some(n) = state.get("last_note").and_then(|v| v.as_f64()) {
            self.last_note = n as f32;
        }
        if let Some(v) = state.get("last_velocity").and_then(|v| v.as_f64()) {
            self.last_velocity = v as f32;
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("midi_note", |state| {
        let mut n = MidiNoteNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
