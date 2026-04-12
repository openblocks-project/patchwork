use eframe::egui;
use serde::{Serialize, Deserialize};
use crate::graph::{PortDef, PortKind, PortValue, Graph};
use crate::node_trait::{NodeBehavior, RenderContext};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn format_port_value(v: &PortValue) -> String {
    match v {
        PortValue::Float(f) => format!("{:.3}", f),
        PortValue::Text(s) => {
            if s.len() > 16 { format!("\"{}...\"", &s[..16]) } else { format!("\"{}\"", s) }
        }
        PortValue::Image(img) => format!("[{}x{}]", img.width, img.height),
        PortValue::GpuImage(h) => format!("[gpu {}x{}]", h.width, h.height),
        PortValue::None => "—".into(),
    }
}

fn compute_output(a: &PortValue, b: &PortValue, sel: f32, mode: u8) -> PortValue {
    let b_active = sel >= 0.5;
    if mode == 1 {
        match (a, b) {
            (PortValue::Float(fa), PortValue::Float(fb)) => PortValue::Float(fa * (1.0 - sel) + fb * sel),
            _ => if b_active { b.clone() } else { a.clone() },
        }
    } else {
        if b_active { b.clone() } else { a.clone() }
    }
}

// ── Node struct ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectNode {
    /// 0 = hard switch, 1 = crossfade (floats only)
    #[serde(default)]
    pub mode: u8,
}

impl Default for SelectNode {
    fn default() -> Self { Self { mode: 0 } }
}

// ── NodeBehavior ──────────────────────────────────────────────────────────────

impl NodeBehavior for SelectNode {
    fn title(&self)        -> &str    { "Select" }
    fn type_tag(&self)     -> &str    { "select" }
    fn color_hint(&self)   -> [u8; 3] { [200, 160, 120] }
    fn inline_ports(&self) -> bool    { true }

    fn inputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("A",        PortKind::Generic),
            PortDef::new("B",        PortKind::Generic),
            PortDef::new("Selector", PortKind::Normalized),
        ]
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![
            PortDef::new("Out",    PortKind::Generic),
            PortDef::new("Active", PortKind::Gate),
        ]
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let val_a    = inputs.get(0).cloned().unwrap_or(PortValue::Float(0.0));
        let val_b    = inputs.get(1).cloned().unwrap_or(PortValue::Float(0.0));
        let selector = inputs.get(2).map(|v| v.as_float()).unwrap_or(0.0).clamp(0.0, 1.0);
        let b_active = selector >= 0.5;
        let output   = compute_output(&val_a, &val_b, selector, self.mode);
        vec![
            (0, output),
            (1, PortValue::Float(if b_active { 1.0 } else { 0.0 })),
        ]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::json!({ "mode": self.mode })
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(m) = state.get("mode").and_then(|v| v.as_u64()) {
            self.mode = m as u8;
        }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        let node_id = ctx.node_id;

        let a_wired   = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        let b_wired   = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 1);
        let sel_wired = ctx.connections.iter().any(|c| c.to_node == node_id && c.to_port == 2);

        let val_a    = Graph::static_input_value(ctx.connections, ctx.values, node_id, 0);
        let val_b    = Graph::static_input_value(ctx.connections, ctx.values, node_id, 1);
        let selector = Graph::static_input_value(ctx.connections, ctx.values, node_id, 2).as_float();
        let sel_clamped = selector.clamp(0.0, 1.0);
        let b_active = sel_clamped >= 0.5;

        let a_color = egui::Color32::from_rgb(80, 180, 255);
        let b_color = egui::Color32::from_rgb(255, 140, 80);
        let dim     = egui::Color32::from_rgb(80, 80, 90);

        // ── Input ports ───────────────────────────────────────────────────
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 0, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
            let label_col = if !b_active { a_color } else { dim };
            ui.label(egui::RichText::new("A:").small().color(label_col));
            if a_wired {
                ui.label(egui::RichText::new(format_port_value(&val_a)).small()
                    .color(if !b_active { a_color } else { egui::Color32::GRAY }));
            } else {
                ui.label(egui::RichText::new("—").small().color(egui::Color32::GRAY));
            }
            if !b_active {
                ui.label(egui::RichText::new("◀").small().color(a_color));
            }
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 1, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Generic);
            let label_col = if b_active { b_color } else { dim };
            ui.label(egui::RichText::new("B:").small().color(label_col));
            if b_wired {
                ui.label(egui::RichText::new(format_port_value(&val_b)).small()
                    .color(if b_active { b_color } else { egui::Color32::GRAY }));
            } else {
                ui.label(egui::RichText::new("—").small().color(egui::Color32::GRAY));
            }
            if b_active {
                ui.label(egui::RichText::new("◀").small().color(b_color));
            }
        });

        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, 2, true, ctx.connections,
                ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects, PortKind::Normalized);
            ui.label(egui::RichText::new("Sel:").small());
            if sel_wired {
                ui.label(egui::RichText::new(format!("{:.2}", selector)).small()
                    .color(egui::Color32::from_rgb(200, 200, 80)));
            } else {
                ui.label(egui::RichText::new("0.0").small().color(egui::Color32::GRAY));
            }
        });

        ui.separator();

        // ── Mode selector ─────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Mode:").small());
            if ui.selectable_label(self.mode == 0, "Switch").clicked() { self.mode = 0; }
            if ui.selectable_label(self.mode == 1, "Fade").clicked()   { self.mode = 1; }
        });

        ui.separator();

        // ── Visual ────────────────────────────────────────────────────────
        let vis_w = 150.0;
        let vis_h = 60.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(vis_w, vis_h), egui::Sense::hover());
        let painter = ui.painter();

        painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 22, 28));
        painter.rect_stroke(rect, 4.0,
            egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 60)),
            egui::StrokeKind::Outside);

        let pad      = 8.0;
        let center_x = rect.center().x;
        let center_y = rect.center().y;

        if self.mode == 0 {
            let a_y     = rect.top() + pad + 8.0;
            let b_y     = rect.bottom() - pad - 8.0;
            let out_y   = center_y;
            let a_alpha = if !b_active { 255 } else { 60 };
            let b_alpha = if b_active  { 255 } else { 60 };

            painter.line_segment(
                [egui::pos2(rect.left() + pad, a_y), egui::pos2(center_x - 10.0, a_y)],
                egui::Stroke::new(3.0, egui::Color32::from_rgba_unmultiplied(80, 180, 255, a_alpha)));
            painter.line_segment(
                [egui::pos2(rect.left() + pad, b_y), egui::pos2(center_x - 10.0, b_y)],
                egui::Stroke::new(3.0, egui::Color32::from_rgba_unmultiplied(255, 140, 80, b_alpha)));
            painter.line_segment(
                [egui::pos2(center_x + 10.0, out_y), egui::pos2(rect.right() - pad, out_y)],
                egui::Stroke::new(3.0, egui::Color32::from_rgb(80, 220, 120)));

            let source_y   = if b_active { b_y } else { a_y };
            let switch_col = if b_active { b_color } else { a_color };
            painter.line_segment(
                [egui::pos2(center_x - 10.0, source_y), egui::pos2(center_x + 10.0, out_y)],
                egui::Stroke::new(3.0, switch_col));
            painter.circle_filled(egui::pos2(center_x, (source_y + out_y) / 2.0), 4.0, switch_col);

            let font = egui::FontId::new(10.0, egui::FontFamily::Proportional);
            painter.text(egui::pos2(rect.left() + 3.0, a_y), egui::Align2::LEFT_CENTER, "A",
                font.clone(), egui::Color32::from_rgba_unmultiplied(80, 180, 255, a_alpha));
            painter.text(egui::pos2(rect.left() + 3.0, b_y), egui::Align2::LEFT_CENTER, "B",
                font, egui::Color32::from_rgba_unmultiplied(255, 140, 80, b_alpha));
        } else {
            let bar_h    = 16.0;
            let bar_y    = center_y - bar_h / 2.0;
            let segments = 30;
            let seg_w    = (vis_w - pad * 2.0) / segments as f32;
            for i in 0..segments {
                let t = i as f32 / (segments - 1) as f32;
                let r = (80.0  + t * (255.0 - 80.0))  as u8;
                let g = (180.0 + t * (140.0 - 180.0)) as u8;
                let b = (255.0 + t * (80.0  - 255.0)) as u8;
                let x = rect.left() + pad + i as f32 * seg_w;
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(x, bar_y), egui::vec2(seg_w + 1.0, bar_h)),
                    0.0, egui::Color32::from_rgb(r, g, b));
            }
            let marker_x = rect.left() + pad + sel_clamped * (vis_w - pad * 2.0);
            painter.rect_filled(
                egui::Rect::from_center_size(egui::pos2(marker_x, center_y), egui::vec2(3.0, bar_h + 8.0)),
                1.0, egui::Color32::WHITE);
            painter.add(egui::Shape::convex_polygon(vec![
                egui::pos2(marker_x - 5.0, bar_y - 6.0),
                egui::pos2(marker_x + 5.0, bar_y - 6.0),
                egui::pos2(marker_x, bar_y - 1.0),
            ], egui::Color32::WHITE, egui::Stroke::NONE));

            let font = egui::FontId::new(10.0, egui::FontFamily::Proportional);
            painter.text(egui::pos2(rect.left() + pad, bar_y - 8.0),
                egui::Align2::LEFT_BOTTOM, "A", font.clone(), a_color);
            painter.text(egui::pos2(rect.right() - pad, bar_y - 8.0),
                egui::Align2::RIGHT_BOTTOM, "B", font.clone(), b_color);
            painter.text(egui::pos2(center_x, bar_y + bar_h + 4.0),
                egui::Align2::CENTER_TOP,
                format!("{:.0}% A / {:.0}% B", (1.0 - sel_clamped) * 100.0, sel_clamped * 100.0),
                egui::FontId::new(9.0, egui::FontFamily::Proportional),
                egui::Color32::from_rgb(180, 180, 190));
        }

        ui.separator();

        // ── Output ports ──────────────────────────────────────────────────
        let output     = compute_output(&val_a, &val_b, sel_clamped, self.mode);
        let output_str = format_port_value(&output);
        let active_label = if b_active { "B" } else { "A" };

        crate::nodes::output_port_row(ui, "Out", &output_str,
            node_id, 0, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Generic);
        crate::nodes::output_port_row(ui, "Active", active_label,
            node_id, 1, ctx.port_positions, ctx.dragging_from,
            ctx.connections, ctx.pending_disconnects, PortKind::Gate);
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("select", |state| {
        let mut n = SelectNode::default();
        n.load_state(state);
        Box::new(n)
    });
}
