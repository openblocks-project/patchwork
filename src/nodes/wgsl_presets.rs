//! WgslPresets — node that picks a `.wgsl` shader preset and emits its
//! source as `PortValue::Text` on a single output port. Downstream WGSL
//! Viewer reads the text, auto-detects uniforms and renders.
//!
//! Built-in presets are compile-time-embedded from `assets/presets/wgsl/*.wgsl`
//! so they always appear in the dropdown — including in shipped `.app` bundles
//! where the assets dir isn't on disk. Power users can drop additional `.wgsl`
//! files in `~/.patchwork/presets/wgsl/` and restart to see them appended.
//!
//! A "Spawn paired WGSL Viewer" button emits a `WgslPresetsSpawnRequest`
//! that the host (`app/mod.rs`) consumes to add a viewer to the right and
//! auto-wire `Shader → WGSL`. Feedback presets (sources containing
//! `image_a` / `image_b`) cause the host to also pre-set the new viewer's
//! `Input A` to `LastFrame`.
//!
//! Shader contract (per WGSL Viewer):
//!  - `fs_main(in: VertexOutput) -> @location(0) vec4<f32>` entry point
//!  - uniforms referenced as `u.<name>` (flat f32; colors split into
//!    `u.<name>_r/_g/_b`)
//!  - optional `image_a` / `image_b` sampled via `img_sampler`

use crate::graph::{NodeId, PortDef, PortKind, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Request from the picker's "+" button to spawn a paired WGSL Viewer to the
/// right of the picker and auto-wire `Shader → WGSL`. Consumed by `app/mod.rs`.
#[derive(Clone, Debug)]
pub struct WgslPresetsSpawnRequest {
    pub source_node: NodeId,
}

/// Reverse pairing: request from an empty Visuals (WGSL) viewer to spawn a
/// Visual Presets picker to its LEFT (default preset `gradient`) and auto-wire
/// `Shader → WGSL`. Lets a bare viewer bootstrap itself into a usable state
/// with one click. Consumed by `app/mod.rs`.
#[derive(Clone, Debug)]
pub struct WgslViewerLoadPresetRequest {
    pub viewer_node: NodeId,
}

// ── Preset metadata ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct Preset {
    key: String,
    label: String,
    source: String,
}

/// Built-in WGSL presets, embedded at compile time. Listed in curated order
/// (gradient first since it's the simplest, ending with the more involved
/// camera/edge/displacement effects). Adding a new bundled preset = drop the
/// `.wgsl` file in `assets/presets/wgsl/` AND add one line here.
const BUNDLED_PRESETS: &[(&str, &str)] = &[
    ("gradient",     include_str!("../../assets/presets/wgsl/gradient.wgsl")),
    ("plasma",       include_str!("../../assets/presets/wgsl/plasma.wgsl")),
    ("particles",    include_str!("../../assets/presets/wgsl/particles.wgsl")),
    ("spinsquare",   include_str!("../../assets/presets/wgsl/spinsquare.wgsl")),
    ("julia",        include_str!("../../assets/presets/wgsl/julia.wgsl")),
    ("kaleidoscope", include_str!("../../assets/presets/wgsl/kaleidoscope.wgsl")),
    ("mandelbrot",   include_str!("../../assets/presets/wgsl/mandelbrot.wgsl")),
    ("fractal_zoom", include_str!("../../assets/presets/wgsl/fractal_zoom.wgsl")),
    ("lissajous",    include_str!("../../assets/presets/wgsl/lissajous.wgsl")),
    ("camera_grade", include_str!("../../assets/presets/wgsl/camera_grade.wgsl")),
    ("edge_detect",  include_str!("../../assets/presets/wgsl/edge_detect.wgsl")),
    ("displacement", include_str!("../../assets/presets/wgsl/displacement.wgsl")),
];

/// Optional user-customizable directory: `~/.patchwork/presets/wgsl/`. Any
/// `.wgsl` files here are loaded on first preset-list access (so requires a
/// Patchwork restart to pick up new ones). User-added presets append to the
/// bundled list; bundled names win on collision.
fn user_presets_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".patchwork").join("presets").join("wgsl"))
}

/// Lazily-built preset list. Bundled first (always available), then any
/// user-added presets from `~/.patchwork/presets/wgsl/`.
fn all_presets() -> &'static Vec<Preset> {
    static CELL: OnceLock<Vec<Preset>> = OnceLock::new();
    CELL.get_or_init(|| {
        let mut out: Vec<Preset> = Vec::with_capacity(BUNDLED_PRESETS.len() + 4);

        // 1. Bundled presets (always there, even in a shipped .app).
        for (stem, source) in BUNDLED_PRESETS {
            out.push(Preset {
                key: (*stem).to_string(),
                label: humanize_stem(stem),
                source: (*source).to_string(),
            });
        }

        // 2. Optional user presets from `~/.patchwork/presets/wgsl/`.
        if let Some(dir) = user_presets_dir() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut files: Vec<std::path::PathBuf> = entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("wgsl"))
                    .collect();
                files.sort();
                for path in files {
                    if let Ok(source) = std::fs::read_to_string(&path) {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            // Skip if a bundled preset already owns this name —
                            // we never let user files silently shadow shipped ones.
                            if out.iter().any(|p| p.key == stem) { continue; }
                            out.push(Preset {
                                key: stem.to_string(),
                                label: humanize_stem(stem),
                                source,
                            });
                        }
                    }
                }
            }
        }

        out
    })
}

/// `audio_reactive` → `Audio Reactive`
fn humanize_stem(stem: &str) -> String {
    stem.split(|c| c == '_' || c == '-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns true if a preset relies on sampling its previous frame
/// (`image_a` or `image_b`). The host uses this to auto-set Input A to
/// LastFrame when spawning a paired WGSL Viewer.
pub fn is_feedback_preset(key: &str) -> bool {
    if let Some(p) = all_presets().iter().find(|p| p.key == key) {
        return p.source.contains("image_a") || p.source.contains("image_b");
    }
    false
}

/// Public so tests / debugging can introspect the loaded WGSL.
pub fn build_preset_wgsl(preset_key: &str) -> String {
    if let Some(p) = all_presets().iter().find(|p| p.key == preset_key) {
        return p.source.clone();
    }
    // Fall back to the first preset if the saved key no longer exists.
    all_presets()
        .first()
        .map(|p| p.source.clone())
        .unwrap_or_default()
}

// ── Node ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WgslPresetsNode {
    pub preset_key: String,
}

impl Default for WgslPresetsNode {
    fn default() -> Self {
        let key = all_presets()
            .iter()
            .find(|p| p.key == "smear")
            .or_else(|| all_presets().first())
            .map(|p| p.key.clone())
            .unwrap_or_default();
        Self { preset_key: key }
    }
}

impl NodeBehavior for WgslPresetsNode {
    fn title(&self) -> &str { "Visual Presets (WGSL)" }

    fn inputs(&self) -> Vec<PortDef> { Vec::new() }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Shader", PortKind::Text)]
    }

    fn color_hint(&self) -> [u8; 3] { [180, 130, 220] }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let src = build_preset_wgsl(&self.preset_key);
        vec![(0, PortValue::Text(src))]
    }

    fn type_tag(&self) -> &str { "wgsl_presets" }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<WgslPresetsNode>(state.clone()) {
            *self = l;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let presets = all_presets();
        let cur_label = presets
            .iter()
            .find(|p| p.key == self.preset_key)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| "(none)".to_string());

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Preset").small());
            egui::ComboBox::from_id_salt(("wgsl_presets", ctx.node_id))
                .selected_text(cur_label)
                .show_ui(ui, |ui| {
                    for p in presets.iter() {
                        if ui
                            .selectable_label(self.preset_key == p.key, &p.label)
                            .clicked()
                        {
                            self.preset_key = p.key.clone();
                        }
                    }
                });
        });

        ui.separator();

        if ui
            .button("+ WGSL Viewer")
            .on_hover_text("Create a Visuals (WGSL) viewer to the right and connect Shader → WGSL")
            .clicked()
        {
            let req = WgslPresetsSpawnRequest { source_node: ctx.node_id };
            ui.ctx().data_mut(|d| {
                d.insert_temp(egui::Id::new("wgsl_presets_spawn_request"), req);
            });
        }

        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        ui.label(
            egui::RichText::new("Built-in presets. Drop your own .wgsl files in ~/.patchwork/presets/wgsl/ to extend.")
                .small()
                .color(dim),
        );
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    let factory = |state: &serde_json::Value| -> Box<dyn NodeBehavior> {
        if let Ok(n) = serde_json::from_value::<WgslPresetsNode>(state.clone()) {
            Box::new(n)
        } else {
            Box::new(WgslPresetsNode::default())
        }
    };
    registry.register("wgsl_presets", factory);
    // Backwards-compat: saved projects from before the rename used the old
    // type tag. Map them to the same factory.
    registry.register("formula_preset_picker", factory);
}
