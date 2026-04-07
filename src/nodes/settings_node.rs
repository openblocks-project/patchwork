//! Settings node — UI for editing global PatchWork preferences stored
//! in `~/.patchwork/settings.json`. Currently exposes image-cache size
//! limits and a manual "Clear cache" button. New settings should be
//! added here as the surface area grows.
//!
//! The node itself stores nothing meaningful — it reads from disk on
//! every render and writes back when the user edits a value. This way
//! it doesn't matter which project the node lives in: it always
//! reflects the global settings.

use crate::graph::{PortDef, PortValue};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::settings::Settings;
use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsNode {}

impl NodeBehavior for SettingsNode {
    fn title(&self) -> &str { "Settings" }
    fn inputs(&self) -> Vec<PortDef> { vec![] }
    fn outputs(&self) -> Vec<PortDef> { vec![] }
    fn color_hint(&self) -> [u8; 3] { [140, 140, 160] }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![]
    }

    fn type_tag(&self) -> &str { "settings" }
    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<SettingsNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, _ctx: &mut RenderContext) {
        let mut settings = Settings::load();
        let (file_count, total_bytes) = crate::nodes::image_node::image_cache_stats();
        let total_mb = total_bytes as f64 / 1_048_576.0;

        ui.label(egui::RichText::new("Image cache").strong().small());
        ui.label(
            egui::RichText::new(format!("{} files · {:.1} MB", file_count, total_mb))
                .small()
                .color(egui::Color32::GRAY),
        );

        ui.add_space(2.0);

        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Max MB").small());
            let mut v = settings.image_cache_max_mb as i32;
            if ui
                .add(egui::DragValue::new(&mut v).range(10..=10_000).speed(5))
                .changed()
            {
                settings.image_cache_max_mb = v.max(1) as u32;
                changed = true;
            }
        });

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Max files").small());
            let mut v = settings.image_cache_max_files as i32;
            if ui
                .add(egui::DragValue::new(&mut v).range(1..=10_000).speed(1))
                .changed()
            {
                settings.image_cache_max_files = v.max(1) as u32;
                changed = true;
            }
        });

        ui.add_space(4.0);

        ui.horizontal(|ui| {
            if ui
                .small_button("Apply now")
                .on_hover_text("Run eviction with current limits")
                .clicked()
            {
                settings.apply_image_cache();
            }
            if ui
                .small_button("Clear cache")
                .on_hover_text("Delete every cached image")
                .clicked()
            {
                crate::nodes::image_node::clear_image_cache();
            }
        });

        if changed {
            let _ = settings.save();
            settings.apply_image_cache();
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("settings", |state| {
        if let Ok(n) = serde_json::from_value::<SettingsNode>(state.clone()) {
            Box::new(n)
        } else {
            Box::new(SettingsNode::default())
        }
    });
}
