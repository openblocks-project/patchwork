use crate::graph::{PortDef, PortKind, PortValue, Graph, ImageData};
use std::sync::Arc;
use crate::node_trait::{NodeBehavior, RenderContext};
use serde::{Serialize, Deserialize};
use eframe::egui;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualOutputNode {
    #[serde(default = "default_preview_size")]
    pub preview_size: f32,
}

fn default_preview_size() -> f32 { 200.0 }

impl Default for VisualOutputNode {
    fn default() -> Self {
        Self { preview_size: 200.0 }
    }
}

impl NodeBehavior for VisualOutputNode {
    fn title(&self) -> &str { "Visual Output" }
    fn inputs(&self) -> Vec<PortDef> { vec![PortDef::new("Image", PortKind::Image)] }
    fn outputs(&self) -> Vec<PortDef> { vec![] }
    fn color_hint(&self) -> [u8; 3] { [200, 100, 255] }
    fn inline_ports(&self) -> bool { true }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> { vec![] }

    fn type_tag(&self) -> &str { "visual_output" }
    fn save_state(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<VisualOutputNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;

        // Input port
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, ctx.node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Image);
            ui.label(egui::RichText::new("Image").small());
        });

        let input_val = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, 0);
        // Normalise both Image and GpuImage variants to (img, gpu_source).
        let img_and_src: Option<(Arc<ImageData>, Option<(crate::graph::NodeId, usize)>)> = match &input_val {
            PortValue::Image(img) => {
                let gpu_source = ctx.connections.iter()
                    .find(|c| c.to_node == ctx.node_id && c.to_port == 0)
                    .map(|c| (c.from_node, c.from_port));
                Some((img.clone(), gpu_source))
            }
            PortValue::GpuImage(handle) => {
                let stub = Arc::new(ImageData {
                    width: handle.width,
                    height: handle.height,
                    pixels: Vec::new(),
                });
                Some((stub, Some((handle.node_id, handle.port))))
            }
            _ => None,
        };
        if let Some((img, gpu_source)) = img_and_src {
            ui.label(egui::RichText::new(format!("{}×{}", img.width, img.height)).small().color(dim));
            crate::nodes::image_node::show_image_gpu(ui, ctx.node_id, &img, self.preview_size, gpu_source);

            // Size slider
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Size").small());
                ui.add(egui::Slider::new(&mut self.preview_size, 80.0..=600.0).show_value(false));
            });

            // Pop-out controls
            let popout_id = egui::Id::new(("visual_popout_d", ctx.node_id));
            let fullscreen_id = egui::Id::new(("visual_fullscreen_d", ctx.node_id));
            let is_popout = ui.ctx().data_mut(|d| d.get_temp::<bool>(popout_id).unwrap_or(false));
            let is_fullscreen = ui.ctx().data_mut(|d| d.get_temp::<bool>(fullscreen_id).unwrap_or(false));

            ui.horizontal(|ui| {
                if ui.button(if is_popout { "Close Window" } else { "Pop Out" }).clicked() {
                    let new_val = !is_popout;
                    ui.ctx().data_mut(|d| d.insert_temp(popout_id, new_val));
                    if !new_val {
                        ui.ctx().data_mut(|d| d.insert_temp(fullscreen_id, false));
                    }
                }
                if is_popout {
                    if ui.button(if is_fullscreen { "Exit Fullscreen" } else { "Fullscreen" }).clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(fullscreen_id, !is_fullscreen));
                    }
                }
            });

            if is_popout {
                ui.label(egui::RichText::new("Drag window to display, then Fullscreen").small().color(dim));

                let img_clone = img.clone();
                let nid = ctx.node_id;
                let mut builder = egui::ViewportBuilder::default()
                    .with_title(format!("Visual Output #{}", nid))
                    .with_inner_size([img_clone.width as f32, img_clone.height as f32]);
                if is_fullscreen {
                    builder = builder.with_fullscreen(true).with_decorations(false);
                }

                ui.ctx().show_viewport_immediate(
                    egui::ViewportId::from_hash_of(("visual_popout_vp_d", nid)),
                    builder,
                    |ctx, _class| {
                        egui::CentralPanel::default()
                            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                            .show(ctx, |ui| {
                                let avail = ui.available_size();
                                let img_aspect = img_clone.width as f32 / img_clone.height.max(1) as f32;
                                let screen_aspect = avail.x / avail.y.max(1.0);
                                let (draw_w, draw_h) = if img_aspect > screen_aspect {
                                    (avail.x, avail.x / img_aspect)
                                } else {
                                    (avail.y * img_aspect, avail.y)
                                };
                                let pad_x = (avail.x - draw_w) / 2.0;
                                let pad_y = (avail.y - draw_h) / 2.0;
                                if pad_y > 0.0 { ui.add_space(pad_y); }
                                ui.horizontal(|ui| {
                                    if pad_x > 0.0 { ui.add_space(pad_x); }
                                    crate::nodes::image_node::show_image_gpu(ui, nid, &img_clone, draw_w, None);
                                });
                            });
                        if ctx.input(|i| i.viewport().close_requested()) || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                            ctx.data_mut(|d| {
                                d.insert_temp(popout_id, false);
                                d.insert_temp(fullscreen_id, false);
                            });
                        }
                        ctx.request_repaint();
                    },
                );
            }
        } else {
            ui.add_space(8.0);
            ui.colored_label(dim, "No image connected");
            ui.add_space(8.0);
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("visual_output", |state| {
        if let Ok(n) = serde_json::from_value::<VisualOutputNode>(state.clone()) { Box::new(n) }
        else { Box::new(VisualOutputNode::default()) }
    });
}
