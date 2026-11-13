#![allow(dead_code)]
use crate::graph::*;
use eframe::egui;

pub fn render(ui: &mut egui::Ui, node_id: NodeId, node_type: &mut NodeType) {
    let (strokes, canvas_size, color, line_width) = match node_type {
        NodeType::Draw {
            strokes,
            canvas_size,
            color,
            line_width,
        } => (strokes, canvas_size, color, line_width),
        _ => return,
    };

    // Controls
    ui.horizontal(|ui| {
        ui.label("Color:");
        let mut c = egui::Color32::from_rgb(color[0], color[1], color[2]);
        if ui.color_edit_button_srgba(&mut c).changed() {
            *color = [c.r(), c.g(), c.b()];
        }
        ui.label("Width:");
        ui.add(
            egui::DragValue::new(line_width)
                .range(0.5..=20.0)
                .speed(0.5),
        );
        if ui.button("Clear").clicked() {
            strokes.clear();
        }
    });

    // Canvas
    let size = *canvas_size;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    // Background
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(15, 15, 20));
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 60)),
        egui::StrokeKind::Outside,
    );

    // Draw existing strokes
    for stroke in strokes.iter() {
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
    let drawing_id = egui::Id::new(("draw_active", node_id));
    let is_drawing: bool = ui
        .ctx()
        .data_mut(|d| d.get_temp(drawing_id).unwrap_or(false));

    if response.drag_started() {
        // Start new stroke
        strokes.push(DrawStroke {
            points: vec![],
            color: *color,
            width: *line_width,
        });
        ui.ctx().data_mut(|d| d.insert_temp(drawing_id, true));
    }

    if response.dragged() && is_drawing {
        if let Some(pos) = response.hover_pos() {
            let nx = ((pos.x - rect.left()) / size).clamp(0.0, 1.0);
            let ny = ((pos.y - rect.top()) / size).clamp(0.0, 1.0);
            if let Some(stroke) = strokes.last_mut() {
                stroke.points.push([nx, ny]);
            }
        }
    }

    if response.drag_stopped() {
        ui.ctx().data_mut(|d| d.insert_temp(drawing_id, false));
    }

    // Stroke count
    ui.label(
        egui::RichText::new(format!("{} strokes", strokes.len()))
            .small()
            .color(egui::Color32::GRAY),
    );
}

/// Render strokes to an ImageData
pub fn render_to_image(strokes: &[DrawStroke], size: u32) -> std::sync::Arc<ImageData> {
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
            // Simple Bresenham line
            draw_line(
                &mut pixels,
                size,
                x0,
                y0,
                x1,
                y1,
                stroke.color,
                stroke.width,
            );
        }
    }
    std::sync::Arc::new(ImageData::new(size, size, pixels))
}

fn draw_line(
    pixels: &mut [u8],
    size: u32,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    color: [u8; 3],
    _width: f32,
) {
    let dx = (x1 - x0).abs();
    let dy = (y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx - dy;
    let mut x = x0;
    let mut y = y0;

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
