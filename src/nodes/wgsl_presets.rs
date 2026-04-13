//! WgslPresets — node that picks a `.wgsl` shader preset and emits its
//! source as `PortValue::Text` on a single output port. Downstream WGSL
//! Viewer reads the text, auto-detects uniforms and renders.
//!
//! Presets are loaded from files in `assets/presets/wgsl/*.wgsl`. Drop a new
//! `.wgsl` file in that directory and restart — it shows up automatically in
//! the dropdown.
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

// ── Preset metadata ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct Preset {
    key: String,
    label: String,
    source: String,
}

/// Directory (relative to the running binary's CWD) scanned at startup for
/// `.wgsl` files. Each file becomes a preset.
const PRESET_DIR: &str = "assets/presets/wgsl";

/// Lazily-built preset list, sorted by filename.
fn all_presets() -> &'static Vec<Preset> {
    static CELL: OnceLock<Vec<Preset>> = OnceLock::new();
    CELL.get_or_init(|| {
        // Try a few candidate roots so the picker works whether the binary
        // is launched from the repo root, `target/debug/`, etc.
        let candidates = [
            std::path::PathBuf::from(PRESET_DIR),
            std::path::PathBuf::from(format!("../{}", PRESET_DIR)),
            std::path::PathBuf::from(format!("../../{}", PRESET_DIR)),
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join(PRESET_DIR)))
                .unwrap_or_default(),
        ];
        let dir = candidates.into_iter().find(|p| p.is_dir());

        let mut out: Vec<Preset> = Vec::new();
        if let Some(dir) = dir {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut files: Vec<std::path::PathBuf> = entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.extension().and_then(|e| e.to_str()) == Some("wgsl")
                    })
                    .collect();
                // Sort by curated order first, then alphabetical for the rest
                let priority_order: &[&str] = &[
                    "gradient", "plasma", "particles", "spinsquare",
                    "julia", "mandelbrot", "lissajous",
                    "camera_grade", "edge_detect", "displacement",
                ];
                files.sort_by(|a, b| {
                    let sa = a.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    let sb = b.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    let ia = priority_order.iter().position(|&p| p == sa).unwrap_or(usize::MAX);
                    let ib = priority_order.iter().position(|&p| p == sb).unwrap_or(usize::MAX);
                    ia.cmp(&ib).then_with(|| sa.cmp(sb))
                });
                for path in files {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        let stem = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("preset")
                            .to_string();
                        out.push(Preset {
                            key: stem.clone(),
                            label: humanize_stem(&stem),
                            source: text,
                        });
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
    fn title(&self) -> &str { "WGSL Presets" }

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
            .button("+ Spawn paired WGSL Viewer")
            .on_hover_text("Create a WGSL Viewer to the right and connect Shader → WGSL")
            .clicked()
        {
            let req = WgslPresetsSpawnRequest { source_node: ctx.node_id };
            ui.ctx().data_mut(|d| {
                d.insert_temp(egui::Id::new("wgsl_presets_spawn_request"), req);
            });
        }

        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        ui.label(
            egui::RichText::new("Loads .wgsl files from assets/presets/wgsl/")
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
