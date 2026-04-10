use crate::graph::{PortDef, PortKind, PortValue};
use crate::node_trait::NodeBehavior;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MouseTrackerNode {
    #[serde(skip)]
    pub x: f32,
    #[serde(skip)]
    pub y: f32,
    #[serde(skip)]
    pub nx: f32,
    #[serde(skip)]
    pub ny: f32,
}

impl NodeBehavior for MouseTrackerNode {
    fn title(&self) -> &str { "Mouse Tracker" }
    fn inputs(&self) -> Vec<PortDef> { vec![] }
    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("X",  PortKind::Number),
            PortDef::new("Y",  PortKind::Number),
            PortDef::new("NX", PortKind::Normalized),
            PortDef::new("NY", PortKind::Normalized),
        ]
    }
    fn color_hint(&self) -> [u8; 3] { [200, 100, 100] }

    fn evaluate(&mut self, _inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        vec![
            (0, PortValue::Float(self.x)),
            (1, PortValue::Float(self.y)),
            (2, PortValue::Float(self.nx)),
            (3, PortValue::Float(self.ny)),
        ]
    }

    fn type_tag(&self) -> &str { "mouse_tracker" }
    fn save_state(&self) -> serde_json::Value { serde_json::json!({}) }

    fn render_ui(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;

        // Update position
        if let Some(pos) = ui.ctx().pointer_latest_pos() {
            self.x = pos.x;
            self.y = pos.y;
            let screen = ui.ctx().screen_rect();
            self.nx = if screen.width()  > 0.0 { (pos.x / screen.width()).clamp(0.0, 1.0) } else { 0.0 };
            self.ny = if screen.height() > 0.0 { (pos.y / screen.height()).clamp(0.0, 1.0) } else { 0.0 };
        }

        // ── Tracker visual ───────────────────────────────────────────────────
        let pad = 4.0;
        let box_size = egui::vec2(ui.available_width().max(80.0), 80.0);
        let (rect, _) = ui.allocate_exact_size(box_size, egui::Sense::hover());
        let painter = ui.painter();

        // Background
        painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
        // Subtle border
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);

        // Crosshair guide lines (very dim)
        let cx = rect.left() + pad + self.nx * (rect.width()  - pad * 2.0);
        let cy = rect.top()  + pad + self.ny * (rect.height() - pad * 2.0);
        let guide_color = ui.visuals().widgets.noninteractive.bg_stroke.color.linear_multiply(0.4);
        painter.line_segment(
            [egui::pos2(rect.left() + pad, cy), egui::pos2(rect.right() - pad, cy)],
            egui::Stroke::new(1.0, guide_color),
        );
        painter.line_segment(
            [egui::pos2(cx, rect.top() + pad), egui::pos2(cx, rect.bottom() - pad)],
            egui::Stroke::new(1.0, guide_color),
        );

        // Dot in accent color
        let accent = ui.visuals().hyperlink_color;
        let dot = egui::pos2(cx, cy);
        painter.circle_filled(dot, 5.0, accent);
        painter.circle_stroke(dot, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE.linear_multiply(0.6)));

        // Keep repainting while the pointer moves
        ui.ctx().request_repaint();
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("mouse_tracker", |_| Box::new(MouseTrackerNode::default()));
}
