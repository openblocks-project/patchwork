use crate::graph::{PortDef, PortValue};
use crate::node_trait::NodeBehavior;
use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileMenuNode;

impl NodeBehavior for FileMenuNode {
    fn title(&self) -> &str {
        "File"
    }
    fn inputs(&self) -> Vec<PortDef> {
        vec![]
    }
    fn outputs(&self) -> Vec<PortDef> {
        vec![]
    }
    fn color_hint(&self) -> [u8; 3] {
        [200, 200, 200]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![]
    }

    fn type_tag(&self) -> &str {
        "file_menu"
    }
    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    fn render_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("New").clicked() {
                ui.ctx()
                    .data_mut(|d| d.insert_temp(egui::Id::new("file_action_new"), true));
            }
            if ui.button("Open").clicked() {
                ui.ctx()
                    .data_mut(|d| d.insert_temp(egui::Id::new("file_action_load"), true));
            }
            if ui.button("Save").clicked() {
                ui.ctx()
                    .data_mut(|d| d.insert_temp(egui::Id::new("file_action_save"), true));
            }
        });
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("file_menu", |_| Box::new(FileMenuNode));
}
