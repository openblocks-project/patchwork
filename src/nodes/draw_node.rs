use crate::graph::{ImageData, PortDef, PortKind, PortValue};
use crate::node_trait::NodeBehavior;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stroke {
    pub points: Vec<[f32; 2]>,
    pub color: [u8; 3],
    pub width: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrawNode {
    #[serde(default)]
    pub strokes: Vec<Stroke>,
    #[serde(default = "default_size")]
    pub canvas_size: f32,
    #[serde(default = "default_color")]
    pub color: [u8; 3],
    #[serde(default = "default_width")]
    pub line_width: f32,
}

fn default_size() -> f32 {
    200.0
}
fn default_color() -> [u8; 3] {
    [255, 255, 255]
}
fn default_width() -> f32 {
    2.0
}

impl Default for DrawNode {
    fn default() -> Self {
        Self {
            strokes: Vec::new(),
            canvas_size: 200.0,
            color: [255, 255, 255],
            line_width: 2.0,
        }
    }
}

impl NodeBehavior for DrawNode {
    fn title(&self) -> &str {
        "Draw"
    }
    fn inputs(&self) -> Vec<PortDef> {
        vec![]
    }
    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Image", PortKind::Image),
            PortDef::new("Points", PortKind::Text),
        ]
    }
    fn color_hint(&self) -> [u8; 3] {
        [200, 180, 100]
    }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let img = render_to_image(&self.strokes, self.canvas_size as u32);
        let json = serde_json::to_string(&self.strokes).unwrap_or_default();
        vec![(0, PortValue::Image(img)), (1, PortValue::Text(json))]
    }

    fn type_tag(&self) -> &str {
        "draw"
    }
    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<DrawNode>(state.clone()) {
            *self = l;
        }
    }

    fn render_ui(&mut self, ui: &mut egui::Ui) {
        let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let node_id_hash = ui.id().value();

        // Controls
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Color:").small());
            let mut c = egui::Color32::from_rgb(self.color[0], self.color[1], self.color[2]);
            if ui.color_edit_button_srgba(&mut c).changed() {
                self.color = [c.r(), c.g(), c.b()];
            }
            ui.label(egui::RichText::new("Width:").small());
            ui.add(
                egui::DragValue::new(&mut self.line_width)
                    .range(0.5..=20.0)
                    .speed(0.5),
            );
            if ui.button("Clear").clicked() {
                self.strokes.clear();
            }
        });

        // Canvas
        let size = self.canvas_size;
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        painter.rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(1.0, dim),
            egui::StrokeKind::Outside,
        );

        // Draw existing strokes
        for stroke in &self.strokes {
            if stroke.points.len() < 2 {
                continue;
            }
            let col = egui::Color32::from_rgb(stroke.color[0], stroke.color[1], stroke.color[2]);
            for i in 1..stroke.points.len() {
                let a = egui::pos2(
                    rect.left() + stroke.points[i - 1][0] * size,
                    rect.top() + stroke.points[i - 1][1] * size,
                );
                let b = egui::pos2(
                    rect.left() + stroke.points[i][0] * size,
                    rect.top() + stroke.points[i][1] * size,
                );
                painter.line_segment([a, b], egui::Stroke::new(stroke.width, col));
            }
        }

        // Drawing interaction
        let drawing_id = egui::Id::new(("draw_active_d", node_id_hash));
        let is_drawing: bool = ui
            .ctx()
            .data_mut(|d| d.get_temp(drawing_id).unwrap_or(false));

        if response.drag_started() {
            self.strokes.push(Stroke {
                points: vec![],
                color: self.color,
                width: self.line_width,
            });
            ui.ctx().data_mut(|d| d.insert_temp(drawing_id, true));
        }
        if response.dragged() && is_drawing {
            if let Some(pos) = response.hover_pos() {
                let nx = ((pos.x - rect.left()) / size).clamp(0.0, 1.0);
                let ny = ((pos.y - rect.top()) / size).clamp(0.0, 1.0);
                if let Some(stroke) = self.strokes.last_mut() {
                    stroke.points.push([nx, ny]);
                }
            }
        }
        if response.drag_stopped() {
            ui.ctx().data_mut(|d| d.insert_temp(drawing_id, false));
        }

        ui.label(
            egui::RichText::new(format!("{} strokes", self.strokes.len()))
                .small()
                .color(dim),
        );
    }
}

fn render_to_image(strokes: &[Stroke], size: u32) -> Arc<ImageData> {
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    for stroke in strokes {
        if stroke.points.len() < 2 {
            continue;
        }
        for i in 1..stroke.points.len() {
            let x0 = (stroke.points[i - 1][0] * size as f32) as i32;
            let y0 = (stroke.points[i - 1][1] * size as f32) as i32;
            let x1 = (stroke.points[i][0] * size as f32) as i32;
            let y1 = (stroke.points[i][1] * size as f32) as i32;
            draw_line(&mut pixels, size, x0, y0, x1, y1, stroke.color);
        }
    }
    Arc::new(ImageData::new(size, size, pixels))
}

fn draw_line(pixels: &mut [u8], size: u32, x0: i32, y0: i32, x1: i32, y1: i32, color: [u8; 3]) {
    let dx = (x1 - x0).abs();
    let dy = (y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx - dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        if x >= 0 && x < size as i32 && y >= 0 && y < size as i32 {
            let idx = ((y as u32 * size + x as u32) * 4) as usize;
            if idx + 3 < pixels.len() {
                pixels[idx] = color[0];
                pixels[idx + 1] = color[1];
                pixels[idx + 2] = color[2];
                pixels[idx + 3] = 255;
            }
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 > -dy {
            err -= dy;
            x += sx;
        }
        if e2 < dx {
            err += dx;
            y += sy;
        }
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("draw", |state| {
        if let Ok(n) = serde_json::from_value::<DrawNode>(state.clone()) {
            Box::new(n)
        } else {
            Box::new(DrawNode::default())
        }
    });
}
