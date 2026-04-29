use super::*;

pub(super) fn point_near_bezier(p: egui::Pos2, from: egui::Pos2, to: egui::Pos2, threshold: f32) -> bool {
    let dx = (to.x - from.x).abs().max(50.0) * 0.5;
    let cp1 = egui::pos2(from.x + dx, from.y);
    let cp2 = egui::pos2(to.x - dx, to.y);
    for i in 0..=20 {
        let t = i as f32 / 20.0;
        let it = 1.0 - t;
        let pt = egui::pos2(
            it*it*it*from.x + 3.0*it*it*t*cp1.x + 3.0*it*t*t*cp2.x + t*t*t*to.x,
            it*it*it*from.y + 3.0*it*it*t*cp1.y + 3.0*it*t*t*cp2.y + t*t*t*to.y,
        );
        if pt.distance(p) < threshold { return true; }
    }
    false
}

/// Evaluate a cubic bezier at parameter t
fn eval_bezier(from: egui::Pos2, to: egui::Pos2, t: f32) -> egui::Pos2 {
    let dx = (to.x - from.x).abs().max(50.0) * 0.5;
    let cp1 = egui::pos2(from.x + dx, from.y);
    let cp2 = egui::pos2(to.x - dx, to.y);
    let it = 1.0 - t;
    egui::pos2(
        it*it*it*from.x + 3.0*it*it*t*cp1.x + 3.0*it*t*t*cp2.x + t*t*t*to.x,
        it*it*it*from.y + 3.0*it*it*t*cp1.y + 3.0*it*t*t*cp2.y + t*t*t*to.y,
    )
}

/// Wiggly wire parameters (passed through draw_wire/draw_wire_3d)
#[derive(Clone, Copy)]
pub(super) struct WiggleParams {
    pub activity: f32,
    pub time: f32,
    pub gravity: f32,    // 0=none, 1=heavy droop
    pub range: f32,      // amplitude multiplier
    pub speed: f32,      // speed multiplier (0.1=slow, 2.0=fast)
}

impl Default for WiggleParams {
    fn default() -> Self { Self { activity: 0.0, time: 0.0, gravity: 0.0, range: 1.0, speed: 1.0 } }
}

/// Draw a wire connector plug at an endpoint.
/// This is a simple vector shape (rounded rect with border) — replace the SVG path
/// inside this function to use your own custom connector artwork.
///
/// To customize: edit the painter calls below. The `center` is where the wire meets
/// the port. `color` is the PortKind base color. `is_source` indicates whether this
/// is the output (source) end or the input (destination) end.
///
/// To replace with your own vector image later:
/// 1. Load your SVG/vector as a set of `egui::Shape` primitives
/// 2. Replace the painter calls in this function with your shapes
/// 3. Use `color` to tint your vector artwork
pub(super) fn draw_wire_connector(
    painter: &egui::Painter,
    center: egui::Pos2,
    color: egui::Color32,
    is_source: bool,
) {
    let w = 8.0_f32;
    let h = 6.0_f32;
    let corner = 2.0;

    // Outer shape: rounded rect
    let rect = egui::Rect::from_center_size(center, egui::vec2(w, h));
    let border_col = egui::Color32::from_rgb(
        (color.r() as u16 + 80).min(255) as u8,
        (color.g() as u16 + 80).min(255) as u8,
        (color.b() as u16 + 80).min(255) as u8,
    );

    // Fill: darker for input (dest), full color for output (source)
    let fill = if is_source {
        color
    } else {
        egui::Color32::from_rgb(
            (color.r() as f32 * 0.4) as u8,
            (color.g() as f32 * 0.4) as u8,
            (color.b() as f32 * 0.4) as u8,
        )
    };

    painter.rect_filled(rect, corner, fill);
    painter.rect_stroke(rect, corner, egui::Stroke::new(1.5, border_col), egui::StrokeKind::Outside);

    // Small notch/line inside to suggest a plug contact
    let notch_y = center.y;
    let notch_w = w * 0.3;
    painter.line_segment(
        [egui::pos2(center.x - notch_w, notch_y), egui::pos2(center.x + notch_w, notch_y)],
        egui::Stroke::new(1.0, border_col),
    );
}

/// Draw a wire between two points. style: 0=Bezier, 1=Straight, 2=Orthogonal, 3=Wiggly
pub(super) fn draw_wire(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32, width: f32, style: u8, wp: &WiggleParams) {
    match style {
        1 => {
            // Straight line
            painter.line_segment([from, to], egui::Stroke::new(width, color));
        }
        2 => {
            // Orthogonal with rounded corners
            let mid_x = (from.x + to.x) / 2.0;
            let corner_r = 12.0_f32.min((to.x - from.x).abs() / 4.0).min((to.y - from.y).abs() / 2.0);
            let points = if (from.y - to.y).abs() < 2.0 {
                vec![from, to]
            } else {
                let dy = to.y - from.y;
                let sign_y = dy.signum();
                vec![
                    from,
                    egui::pos2(mid_x - corner_r, from.y),
                    egui::pos2(mid_x, from.y + sign_y * corner_r),
                    egui::pos2(mid_x, to.y - sign_y * corner_r),
                    egui::pos2(mid_x + corner_r, to.y),
                    to,
                ]
            };
            for pair in points.windows(2) {
                painter.line_segment([pair[0], pair[1]], egui::Stroke::new(width, color));
            }
        }
        _ => {
            // Curved (default): unified Bezier + Wiggly. When all wiggle
            // params (gravity, range, speed) are zero the output is
            // indistinguishable from a plain cubic bezier — so "Bezier"
            // is just this mode with zeroed wiggle parameters. No need
            // for a separate style.
            let has_wiggle = wp.gravity > 0.0 || wp.range > 0.0 || wp.activity > 0.0;
            if has_wiggle {
                draw_wiggly_wire(painter, from, to, color, width, wp);
            } else {
                let dx = (to.x - from.x).abs().max(50.0) * 0.5;
                painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                    [from, egui::pos2(from.x + dx, from.y), egui::pos2(to.x - dx, to.y), to],
                    false, egui::Color32::TRANSPARENT, egui::Stroke::new(width, color),
                ));
            }
        }
    }
}

/// Draw wiggly wire: sample bezier, add perpendicular sine displacement.
/// - Smooth activity transitions (exponential-smoothed via egui temp data)
/// - Port-area fade: wiggles near-zero at both endpoints (steeper than plain sine envelope)
/// - Gravity: catenary-like downward sag, strongest at midpoint
/// - Phase travels from→to direction
fn draw_wiggly_wire(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32, width: f32, wp: &WiggleParams) {
    let segments = 50;
    let dist = from.distance(to).max(1.0);
    let activity = wp.activity;
    let range = wp.range;

    // Amplitude scales with distance and range, boosted by activity
    let base_amp = (dist * 0.012).clamp(1.0, 4.0) * range;
    let active_amp = (dist * 0.04).clamp(4.0, 16.0) * range;
    let amplitude = base_amp + (active_amp - base_amp) * activity;

    // Frequency: more waves when active
    let base_freq = (dist / 70.0).clamp(1.5, 4.0);
    let active_freq = (dist / 30.0).clamp(3.0, 12.0);
    let frequency = base_freq + (active_freq - base_freq) * activity;

    // Phase travels from→to (positive direction along t), scaled by speed
    let phase = wp.time * (1.5 + activity * 5.0) * wp.speed;

    // Gravity sag (pixels of downward displacement at midpoint)
    let gravity_sag = wp.gravity * dist * 0.12;

    let mut points = Vec::with_capacity(segments + 1);
    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let base = eval_bezier(from, to, t);

        // Tangent for perpendicular
        let t2 = (t + 0.01).min(1.0);
        let next = eval_bezier(from, to, t2);
        let dx = next.x - base.x;
        let dy = next.y - base.y;
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let nx = -dy / len;
        let ny = dx / len;

        // Port-area fade: steep falloff near endpoints, flat in the middle
        // Using sin^3 for steeper edge fade — near-zero for ~15% at each end
        let raw_env = (t * std::f32::consts::PI).sin();
        let envelope = raw_env * raw_env * raw_env; // sin³ = very flat at 0 and 1

        // Sine displacement — phase moves from→to
        let sine = (t * frequency * std::f32::consts::TAU - phase).sin();
        let offset = sine * amplitude * envelope;

        // Gravity: parabolic sag (max at t=0.5, zero at endpoints)
        // 4*t*(1-t) = parabola peaking at 1.0 when t=0.5
        let sag = gravity_sag * 4.0 * t * (1.0 - t);

        points.push(egui::pos2(
            base.x + nx * offset,
            base.y + ny * offset + sag, // sag is always downward (+y)
        ));
    }

    for pair in points.windows(2) {
        painter.line_segment([pair[0], pair[1]], egui::Stroke::new(width, color));
    }
}

/// Draw a 3D-effect wire: shadow underneath + main color + bright highlight on top.
pub(super) fn draw_wire_3d(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32, width: f32, style: u8, wp: &WiggleParams) {
    let rgba = color.to_array();
    // 1. Shadow layer (dark, wider, offset slightly down)
    let shadow = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 40);
    let shadow_from = egui::pos2(from.x, from.y + 1.5);
    let shadow_to = egui::pos2(to.x, to.y + 1.5);
    draw_wire(painter, shadow_from, shadow_to, shadow, width + 3.0, style, wp);

    // 2. Dark edge (creates depth — slightly wider than main)
    let dark_edge = egui::Color32::from_rgb(
        (rgba[0] as u16 / 3).min(255) as u8,
        (rgba[1] as u16 / 3).min(255) as u8,
        (rgba[2] as u16 / 3).min(255) as u8,
    );
    draw_wire(painter, from, to, dark_edge, width + 1.5, style, wp);

    // 3. Main wire
    draw_wire(painter, from, to, color, width, style, wp);

    // 4. Bright highlight strip (specular — thin, brighter, offset slightly up)
    let highlight = egui::Color32::from_rgba_unmultiplied(
        (rgba[0] as u16 + 100).min(255) as u8,
        (rgba[1] as u16 + 100).min(255) as u8,
        (rgba[2] as u16 + 100).min(255) as u8,
        140,
    );
    let hl_from = egui::pos2(from.x, from.y - 0.5);
    let hl_to = egui::pos2(to.x, to.y - 0.5);
    draw_wire(painter, hl_from, hl_to, highlight, (width * 0.25).max(1.0), style, wp);
}

/// Backward-compatible wrapper
#[allow(dead_code)]
pub(super) fn draw_bezier(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32, width: f32) {
    draw_wire(painter, from, to, color, width, 0, &WiggleParams::default());
}

impl super::PatchworkApp {
    pub(super) fn canvas(&self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            let painter = ui.painter();
            let rect = ui.max_rect();
            let zoom = self.canvas_zoom;

            // ── Background image (from Theme BG Image port) ──────────────
            let bg_img: Option<std::sync::Arc<crate::graph::ImageData>> =
                ctx.data_mut(|d| d.get_temp(egui::Id::new("canvas_bg_image")).flatten());
            if let Some(img) = bg_img {
                use std::cell::RefCell;
                thread_local! {
                    static BG_TEX: RefCell<Option<(u64, egui::TextureHandle)>> = const { RefCell::new(None) };
                }
                // Hash width+height+first few pixels to detect changes
                let hash = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    img.width.hash(&mut h);
                    img.height.hash(&mut h);
                    // Sample a few pixels for change detection (not all — too slow)
                    let step = (img.pixels.len() / 64).max(1);
                    for i in (0..img.pixels.len()).step_by(step) {
                        img.pixels[i].hash(&mut h);
                    }
                    h.finish()
                };
                BG_TEX.with(|cell| {
                    let mut cached = cell.borrow_mut();
                    let needs_update = cached.as_ref().map(|(h, _)| *h != hash).unwrap_or(true);
                    if needs_update {
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(
                            [img.width as usize, img.height as usize],
                            &img.pixels,
                        );
                        let tex = ui.ctx().load_texture("canvas_bg", color_image, egui::TextureOptions::LINEAR);
                        *cached = Some((hash, tex));
                    }
                    if let Some((_, tex)) = cached.as_ref() {
                        // Draw image covering the full canvas, maintaining aspect ratio
                        // Guard against zero dimensions (corrupted image or degenerate rect)
                        if img.width == 0 || img.height == 0 || rect.width() < 1.0 || rect.height() < 1.0 {
                            return;
                        }
                        let img_aspect = img.width as f32 / img.height as f32;
                        let rect_aspect = rect.width() / rect.height();
                        let draw_rect = if img_aspect > rect_aspect {
                            // Image wider → fill height, crop sides
                            let w = rect.height() * img_aspect;
                            let x_off = (w - rect.width()) * 0.5;
                            egui::Rect::from_min_size(
                                egui::pos2(rect.left() - x_off, rect.top()),
                                egui::vec2(w, rect.height()),
                            )
                        } else {
                            // Image taller → fill width, crop top/bottom
                            let h = rect.width() / img_aspect;
                            let y_off = (h - rect.height()) * 0.5;
                            egui::Rect::from_min_size(
                                egui::pos2(rect.left(), rect.top() - y_off),
                                egui::vec2(rect.width(), h),
                            )
                        };
                        painter.image(tex.id(), draw_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                    }
                });
            }

            // ── WGSL shader as background (from Theme BG Image connected to WgslViewer) ─
            let bg_wgsl_node: Option<crate::graph::NodeId> =
                ctx.data_mut(|d| d.get_temp(egui::Id::new("canvas_bg_wgsl")).flatten());
            if let Some(wgsl_node_id) = bg_wgsl_node {
                // Paint the WGSL shader fullscreen using the existing GPU pipeline
                ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                    rect,
                    crate::nodes::wgsl_viewer::WgslBgCallback { node_id: wgsl_node_id },
                ));
            }

            let grid = 25.0; // Logical units; set_zoom_factor scales to screen
            // Grid style + color from Theme
            let (gc, gs) = self.graph.nodes.values()
                .find_map(|n| if let NodeType::Theme { grid_color, grid_style, .. } = &n.node_type { Some((*grid_color, *grid_style)) } else { None })
                .unwrap_or(([28, 28, 28], 2)); // default: dotted, matches Theme node defaults
            let col = egui::Color32::from_rgba_premultiplied(gc[0], gc[1], gc[2], 35);
            let off = self.canvas_offset / zoom;

            // gs: 0=Solid (no grid), 1=Square lines, 2=Dotted
            if gs > 0 {
                let x_start = ((rect.left() - off.x) / grid).floor() as i32;
                let x_end = ((rect.right() - off.x) / grid).ceil() as i32;
                let y_start = ((rect.top() - off.y) / grid).floor() as i32;
                let y_end = ((rect.bottom() - off.y) / grid).ceil() as i32;

                if gs == 1 {
                    // Square grid (lines)
                    for i in x_start..=x_end {
                        let x = i as f32 * grid + off.x;
                        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(0.5, col));
                    }
                    for i in y_start..=y_end {
                        let y = i as f32 * grid + off.y;
                        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(0.5, col));
                    }
                } else {
                    // Dotted grid (dots at intersections)
                    let dot_col = egui::Color32::from_rgba_premultiplied(gc[0], gc[1], gc[2], 50);
                    for ix in x_start..=x_end {
                        for iy in y_start..=y_end {
                            let x = ix as f32 * grid + off.x;
                            let y = iy as f32 * grid + off.y;
                            if rect.contains(egui::pos2(x, y)) {
                                painter.circle_filled(egui::pos2(x, y), 1.2, dot_col);
                            }
                        }
                    }
                }
            }

            // Origin marker — subtle, same size as grid dots
            if gs > 0 && rect.contains(egui::pos2(off.x, off.y)) {
                painter.circle_filled(egui::pos2(off.x, off.y), 1.8, egui::Color32::from_rgba_premultiplied(gc[0], gc[1], gc[2], 60));
            }
            // Zoom indicator
            if (zoom - 1.0).abs() > 0.01 {
                painter.text(
                    egui::pos2(rect.right() - 8.0, rect.bottom() - 8.0),
                    egui::Align2::RIGHT_BOTTOM,
                    format!("{:.0}%", zoom * 100.0),
                    egui::FontId::proportional(11.0),
                    egui::Color32::from_rgb(100, 100, 100),
                );
            }
            if self.graph.nodes.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("Double-click to add a node  \u{2022}  Drag & drop a file").size(16.0).color(egui::Color32::from_rgb(100, 100, 100)));
                });
            }

            // Draw box selection rectangle
            if let (Some(start), Some(end)) = (self.box_select_start, self.box_select_end) {
                let sel_rect = egui::Rect::from_two_pos(start, end);
                let painter = ui.painter();
                let acc = ctx.data_mut(|d| d.get_temp::<[u8; 3]>(egui::Id::new("theme_accent")))
                    .unwrap_or([80, 160, 255]);
                painter.rect_filled(sel_rect, 0.0, egui::Color32::from_rgba_unmultiplied(acc[0], acc[1], acc[2], 25));
                painter.rect_stroke(sel_rect, 0.0, egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(acc[0], acc[1], acc[2], 150)), egui::StrokeKind::Outside);
            }
        });
    }

    pub(super) fn render_connections(&mut self, ctx: &egui::Context, values: &HashMap<(NodeId, usize), PortValue>) {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("connections")));
        let pointer = ctx.pointer_latest_pos();
        let clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
        let now = ctx.input(|i| i.time) as f32;
        let mut hovered_conn: Option<usize> = None;

        // ── Wire color palette (derived from PortKind) ──────────────────
        let color_none  = egui::Color32::from_rgb(140, 140, 140);   // gray fallback

        // Wire thickness + style + wiggly params from Theme
        let (wire_thickness, wire_style, wiggle_gravity, wiggle_range, wiggle_speed, wiggle_signal): (f32, u8, f32, f32, f32, f32) = self.graph.nodes.values()
            .find_map(|n| if let NodeType::Theme { wire_thickness, wire_style, wiggle_gravity, wiggle_range, wiggle_speed, wiggle_signal, .. } = &n.node_type {
                Some((*wire_thickness, *wire_style, *wiggle_gravity, *wiggle_range, *wiggle_speed, *wiggle_signal))
            } else { None })
            .unwrap_or((6.0, 0, 0.0, 1.0, 1.0, 0.0));
        let wire_thickness = wire_thickness.max(1.0);

        // (endpoint_radius removed — connectors now use draw_wire_connector)

        // Pre-compute wire colors for all connections + drag
        // Uses PortKind from port definitions for semantic coloring
        let compute_wire_color = |nodes: &HashMap<NodeId, Node>, from_node: NodeId, from_port: usize| -> egui::Color32 {
            if let Some(node) = nodes.get(&from_node) {
                let outputs = node.node_type.outputs();
                if let Some(pdef) = outputs.get(from_port) {
                    let c = pdef.kind.base_color();
                    return egui::Color32::from_rgb(c[0], c[1], c[2]);
                }
            }
            // Fallback: infer from runtime value
            match values.get(&(from_node, from_port)) {
                Some(v) => {
                    let c = PortKind::from_value(v).base_color();
                    egui::Color32::from_rgb(c[0], c[1], c[2])
                }
                _ => color_none,
            }
        };
        // Pre-compute all wire colors (so we don't borrow self.graph during rendering)
        let wire_colors: Vec<egui::Color32> = self.graph.connections.iter()
            .map(|c| compute_wire_color(&self.graph.nodes, c.from_node, c.from_port))
            .collect();
        // Pre-compute destination (input) port colors for connector plugs
        let wire_dest_colors: Vec<egui::Color32> = self.graph.connections.iter()
            .map(|c| {
                if let Some(node) = self.graph.nodes.get(&c.to_node) {
                    let inputs = node.node_type.inputs();
                    if let Some(pdef) = inputs.get(c.to_port) {
                        let col = pdef.kind.base_color();
                        return egui::Color32::from_rgb(col[0], col[1], col[2]);
                    }
                }
                color_none
            })
            .collect();
        let drag_color = if let Some((nid, pidx, is_output)) = self.dragging_from {
            if is_output {
                compute_wire_color(&self.graph.nodes, nid, pidx)
            } else {
                // Dragging from an input: look up the input port's PortKind
                if let Some(node) = self.graph.nodes.get(&nid) {
                    let inputs = node.node_type.inputs();
                    if let Some(pdef) = inputs.get(pidx) {
                        let c = pdef.kind.base_color();
                        egui::Color32::from_rgb(c[0], c[1], c[2])
                    } else { color_none }
                } else { color_none }
            }
        } else {
            color_none
        };

        // ── Draw established connections ──────────────────────────────────
        for (idx, conn) in self.graph.connections.iter().enumerate() {
            let from = self.port_positions.get(&(conn.from_node, conn.from_port, false));
            let to = self.port_positions.get(&(conn.to_node, conn.to_port, true));
            if let (Some(&a), Some(&b)) = (from, to) {
                let conn_id = (conn.from_node, conn.from_port, conn.to_node, conn.to_port);
                let is_selected = self.selected_connection.as_ref() == Some(&conn_id);
                let is_hovered = pointer.map(|p| point_near_bezier(p, a, b, 10.0)).unwrap_or(false);
                if is_hovered { hovered_conn = Some(idx); }

                let base_color = wire_colors[idx];

                let (color, width) = if is_selected {
                    // Keep the port-kind base color so the wire's type
                    // identity stays readable while selected. The accent
                    // appears as a halo drawn underneath (below) — selection
                    // reads as a "border" around the wire, not a recolor.
                    (base_color, wire_thickness + 1.0)
                } else if is_hovered {
                    let arr = base_color.to_array();
                    (egui::Color32::from_rgb(
                        (arr[0] as u16 + 50).min(255) as u8,
                        (arr[1] as u16 + 50).min(255) as u8,
                        (arr[2] as u16 + 50).min(255) as u8,
                    ), wire_thickness + 1.0)
                } else {
                    (base_color, wire_thickness)
                };

                // Compute wire activity for curved mode when range>0 AND the user
                // has given signal weight > 0 (otherwise activity is ignored).
                let wire_activity = if wire_style == 0 && wiggle_range > 0.0 && wiggle_signal > 0.0 {
                    let val_id = egui::Id::new(("wire_val_hash", conn.from_node, conn.from_port));
                    let activity_id = egui::Id::new(("wire_activity", conn.from_node, conn.from_port));
                    let smooth_id = egui::Id::new(("wire_smooth", conn.from_node, conn.from_port));
                    let cur_hash = match values.get(&(conn.from_node, conn.from_port)) {
                        Some(PortValue::Float(f)) => (*f * 10000.0) as i64,
                        Some(PortValue::Text(s)) => s.len() as i64 * 31 + s.bytes().next().unwrap_or(0) as i64,
                        Some(PortValue::Image(img)) => img.width as i64 * img.height as i64 + img.pixels.first().copied().unwrap_or(0) as i64,
                        // GpuImage wires animate via the per-frame `frame_stamp`
                        // counter so the wiggle still pulses even though pixels
                        // never touch the CPU.
                        Some(PortValue::GpuImage(h)) => (h.frame_stamp as i64).wrapping_mul(1_000_003)
                            ^ (h.width as i64 * h.height as i64),
                        _ => 0i64,
                    };
                    let prev_hash = ctx.data_mut(|d| d.get_temp::<i64>(val_id).unwrap_or(0));
                    // Raw target: 1 when changed, decays slowly
                    let prev_raw = ctx.data_mut(|d| d.get_temp::<f32>(activity_id).unwrap_or(0.0));
                    let raw_target = if cur_hash != prev_hash {
                        1.0_f32
                    } else {
                        (prev_raw - 0.015).max(0.0) // slow decay
                    };
                    // Exponential smoothing: smoothed value eases toward raw target
                    let prev_smooth = ctx.data_mut(|d| d.get_temp::<f32>(smooth_id).unwrap_or(0.0));
                    let rise_speed = 0.15; // how fast to ramp up (0.15 = ~7 frames to reach peak)
                    let fall_speed = 0.04; // how fast to settle down (0.04 = ~25 frames to fade)
                    let speed = if raw_target > prev_smooth { rise_speed } else { fall_speed };
                    let smoothed = prev_smooth + (raw_target - prev_smooth) * speed;
                    ctx.data_mut(|d| {
                        d.insert_temp(val_id, cur_hash);
                        d.insert_temp(activity_id, raw_target);
                        d.insert_temp(smooth_id, smoothed);
                    });
                    smoothed
                } else {
                    0.0
                };

                // Signal weight scales the per-wire activity before it drives the wiggle.
                let wp = WiggleParams {
                    activity: wire_activity * wiggle_signal,
                    time: now,
                    gravity: wiggle_gravity,
                    range: wiggle_range,
                    speed: wiggle_speed,
                };

                // Accent halo under selected wires — sits below the 3D wire
                // layers so the base color stays on top. Reads as a glowing
                // border without obscuring the port-kind color identity.
                if is_selected {
                    let acc = crate::nodes::theme::current_accent(ctx).to_array();
                    let halo = egui::Color32::from_rgba_unmultiplied(acc[0], acc[1], acc[2], 180);
                    draw_wire(&painter, a, b, halo, width + 6.0, wire_style, &wp);
                }
                // 3D wire: shadow + dark edge + main + highlight
                draw_wire_3d(&painter, a, b, color, width, wire_style, &wp);

                // Connector plugs at both ends (source=output, dest=input)
                let src_color = wire_colors[idx];
                let dst_color = wire_dest_colors[idx];
                draw_wire_connector(&painter, a, src_color, true);
                draw_wire_connector(&painter, b, dst_color, false);

                // Wire label (centered on bezier midpoint)
                if !conn.label.is_empty() {
                    let mid_x = (a.x + b.x) / 2.0;
                    let mid_y = (a.y + b.y) / 2.0 - 12.0; // above the wire
                    painter.text(
                        egui::pos2(mid_x, mid_y),
                        egui::Align2::CENTER_BOTTOM,
                        &conn.label,
                        egui::FontId::proportional(11.0),
                        color,
                    );
                }
            }
        }

        // ── Click to select/open wire menu ────────────────────────────────
        if clicked {
            if let Some(idx) = hovered_conn {
                let near_port = pointer.map(|p| {
                    self.port_positions.values().any(|&pp| pp.distance(p) < PORT_INTERACT)
                }).unwrap_or(false);
                if !near_port {
                    if let Some(conn) = self.graph.connections.get(idx) {
                        let cid = (conn.from_node, conn.from_port, conn.to_node, conn.to_port);
                        self.selected_connection = Some(cid);
                        self.selected_nodes.clear();
                        // Open wire context menu
                        self.wire_menu_conn = Some(cid);
                        self.wire_menu_pos = pointer.unwrap_or(egui::Pos2::ZERO);
                    }
                }
            }
        }

        // Request continuous repaint when the curved style has active wiggle
        // parameters (animation depends on speed / range / gravity being > 0).
        if wire_style == 0 && (wiggle_range > 0.0 || wiggle_speed > 0.0 || wiggle_gravity > 0.0) {
            ctx.request_repaint();
        }

        // ── Wire context menu (label + delete + insert node) ─────────────
        if let Some(conn_id) = self.wire_menu_conn {
            // Re-resolve identity to current index each frame — survives reindexing
            if let Some(conn_idx) = self.find_connection_index(&conn_id) {
                let orig_style = self.apply_inverse_zoom_style(ctx);
                let mut keep_open = true;
                let pos = self.wire_menu_pos;

                // Resolve the wire's PortKind once per frame for picker filtering.
                // We use the source (output) port kind — it's the type carried
                // along the wire and the only kind that matters for compatibility
                // with both ends of the inserted node.
                let wire_kind: Option<crate::graph::PortKind> = {
                    let c = &self.graph.connections[conn_idx];
                    self.graph.port_kind(c.from_node, c.from_port, true)
                };

                // Insert action collected from the picker click; applied AFTER
                // the closure releases its borrow on `self.graph`.
                let mut insert_request: Option<crate::graph::NodeType> = None;

                egui::Area::new(egui::Id::new("wire_context_menu"))
                    .fixed_pos(pos)
                    .order(egui::Order::Tooltip)
                    .show(ctx, |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_min_width(160.0 / self.canvas_zoom);

                            // Label text field
                            ui.label(egui::RichText::new("Wire Label").small().strong());
                            let label = &mut self.graph.connections[conn_idx].label;
                            let r = ui.text_edit_singleline(label);
                            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                keep_open = false;
                            }

                            ui.separator();

                            // Insert node button — toggles a filtered picker
                            // listing only nodes whose first input AND first
                            // output are compatible with `wire_kind`.
                            let insert_label = if self.wire_insert_picker_open {
                                format!("{} Cancel insert", crate::icons::PLUS)
                            } else {
                                format!("{} Insert node…", crate::icons::PLUS)
                            };
                            if ui.add(egui::Button::new(egui::RichText::new(insert_label)).frame(false)).clicked() {
                                self.wire_insert_picker_open = !self.wire_insert_picker_open;
                                if self.wire_insert_picker_open {
                                    self.wire_insert_search.clear();
                                }
                            }

                            // Picker popup body (inline below the button so the
                            // click-outside detection of the wire menu also
                            // covers the picker without extra plumbing).
                            if self.wire_insert_picker_open {
                                ui.add(egui::TextEdit::singleline(&mut self.wire_insert_search)
                                    .hint_text("Search…")
                                    .desired_width(ui.available_width()));

                                let query = self.wire_insert_search.to_lowercase();
                                let cat = crate::nodes::catalog();
                                let inv = 1.0 / self.canvas_zoom;
                                let dim = ui.visuals().widgets.noninteractive.fg_stroke.color;
                                let mut any_shown = false;
                                egui::ScrollArea::vertical()
                                    .max_height(280.0 * inv)
                                    .show(ui, |ui| {
                                        for entry in &cat {
                                            // Build a sample instance to inspect ports.
                                            let nt = (entry.factory)();
                                            // Must have ≥1 input and ≥1 output that are
                                            // compatible with the wire's port kind —
                                            // anything else can't be inserted on a wire,
                                            // so it doesn't belong in this menu.
                                            let in_kind = match nt.inputs().first() { Some(p) => p.kind, None => continue };
                                            let out_kind = match nt.outputs().first() { Some(p) => p.kind, None => continue };
                                            if let Some(wk) = wire_kind {
                                                if !crate::graph::PortKind::compatible(wk, in_kind) { continue; }
                                                if !crate::graph::PortKind::compatible(out_kind, wk) { continue; }
                                            }
                                            // Search filter (label or category).
                                            if !query.is_empty()
                                                && !entry.label.to_lowercase().contains(&query)
                                                && !entry.category.to_lowercase().contains(&query)
                                            {
                                                continue;
                                            }
                                            if ui.add(egui::Button::new(
                                                egui::RichText::new(format!("{}  {}", crate::icons::node_icon(entry.label), entry.label)).small()
                                            ).frame(false)).clicked() {
                                                insert_request = Some((entry.factory)());
                                            }
                                            any_shown = true;
                                        }
                                        if !any_shown {
                                            ui.label(egui::RichText::new("No compatible nodes").small().italics().color(dim));
                                        }
                                    });
                            }

                            ui.separator();

                            // Delete button
                            if ui.add(egui::Button::new(
                                egui::RichText::new(format!("{} Delete wire", crate::icons::TRASH))
                                    .color(egui::Color32::from_rgb(255, 100, 100))
                            ).frame(false)).clicked() {
                                self.push_undo();
                                self.graph.connections.remove(conn_idx);
                                self.selected_connection = None;
                                keep_open = false;
                            }
                        });
                    });

                // ── Apply insert request ──────────────────────────────────
                // Done outside the closure so we can mutably borrow
                // `self.graph` (add_node + remove + add_connection).
                if let Some(nt) = insert_request {
                    // Capture the original wire's endpoints + midpoint BEFORE
                    // any mutation invalidates the index.
                    let (from_node, from_port, to_node, to_port) = {
                        let c = &self.graph.connections[conn_idx];
                        (c.from_node, c.from_port, c.to_node, c.to_port)
                    };
                    let mid_pos: [f32; 2] = {
                        let from_pos = self.graph.nodes.get(&from_node).map(|n| n.pos).unwrap_or([0.0, 0.0]);
                        let to_pos = self.graph.nodes.get(&to_node).map(|n| n.pos).unwrap_or([0.0, 0.0]);
                        // Slight downward bias so the new node doesn't overlap
                        // an existing label/wire path centerline.
                        [(from_pos[0] + to_pos[0]) * 0.5, (from_pos[1] + to_pos[1]) * 0.5 + 20.0]
                    };

                    self.push_undo();
                    let new_id = self.add_node_selected(ctx, nt, mid_pos);
                    // Remove the original wire (don't trust conn_idx after add_node).
                    self.graph.connections.retain(|c| !(
                        c.from_node == from_node && c.from_port == from_port &&
                        c.to_node == to_node && c.to_port == to_port
                    ));
                    // Wire `from → new.input[0]` and `new.output[0] → to`.
                    self.graph.add_connection(from_node, from_port, new_id, 0);
                    self.graph.add_connection(new_id, 0, to_node, to_port);

                    // Close the menu and clear selection state.
                    self.wire_insert_picker_open = false;
                    self.wire_insert_search.clear();
                    self.wire_menu_conn = None;
                    self.selected_connection = None;
                    keep_open = false;
                }

                // Close on click outside or Escape. The menu rect grows when the
                // picker is open so click-outside detection still works.
                let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
                let menu_height = if self.wire_insert_picker_open { 380.0 } else { 110.0 };
                let click_outside = ctx.input(|i| {
                    i.pointer.button_clicked(egui::PointerButton::Primary)
                        || i.pointer.button_clicked(egui::PointerButton::Secondary)
                }) && ctx.pointer_latest_pos()
                    .map(|p| {
                        let menu_rect = egui::Rect::from_min_size(pos, egui::vec2(220.0, menu_height));
                        !menu_rect.contains(p) && !hovered_conn.map(|h| h == conn_idx).unwrap_or(false)
                    })
                    .unwrap_or(false);

                if esc || click_outside || !keep_open {
                    self.wire_menu_conn = None;
                    self.wire_insert_picker_open = false;
                }
                ctx.set_style(orig_style);
            } else {
                // Connection no longer exists (deleted, undo, etc.) — close menu
                self.wire_menu_conn = None;
                self.wire_insert_picker_open = false;
            }
        }

        // ── Active drag wire (previews data type color) ───────────────────
        if let Some((nid, pidx, is_output)) = self.dragging_from {
            if let Some(&from) = self.port_positions.get(&(nid, pidx, !is_output)) {
                if let Some(ptr) = ctx.pointer_latest_pos() {
                    // Find closest compatible port and snap the wire endpoint to it
                    let src_kind = self.graph.port_kind(nid, pidx, is_output);
                    let hit_radius = PORT_INTERACT * 2.0;
                    let mut snap_target: Option<(f32, egui::Pos2, bool)> = None; // (dist, pos, compatible)
                    for (&(tnid, tpidx, tis_input), &tpos) in &self.port_positions {
                        if tnid == nid { continue; }
                        let valid_dir = (is_output && tis_input) || (!is_output && !tis_input);
                        if !valid_dir { continue; }
                        let dist = tpos.distance(ptr);
                        if dist < hit_radius {
                            let tgt_kind = self.graph.port_kind(tnid, tpidx, !tis_input);
                            let compat = match (src_kind, tgt_kind) {
                                (Some(s), Some(t)) => PortKind::compatible(s, t),
                                _ => true,
                            };
                            if snap_target.as_ref().map(|s| dist < s.0).unwrap_or(true) {
                                snap_target = Some((dist, tpos, compat));
                            }
                        }
                    }

                    let wire_end = if let Some((_, snap_pos, true)) = snap_target {
                        snap_pos // Snap to compatible port
                    } else {
                        ptr
                    };

                    let drag_wp = WiggleParams { activity: 0.3 * wiggle_signal, time: now, gravity: wiggle_gravity, range: wiggle_range, speed: wiggle_speed };
                    if is_output {
                        draw_wire_3d(&painter, from, wire_end, drag_color, wire_thickness, wire_style, &drag_wp);
                        draw_wire_connector(&painter, from, drag_color, true);
                        draw_wire_connector(&painter, wire_end, drag_color, false);
                    } else {
                        draw_wire_3d(&painter, wire_end, from, drag_color, wire_thickness, wire_style, &drag_wp);
                        draw_wire_connector(&painter, from, drag_color, false);
                        draw_wire_connector(&painter, wire_end, drag_color, true);
                    }

                    // Draw highlight ring on snap target
                    if let Some((_, snap_pos, compat)) = snap_target {
                        let ring_color = if compat {
                            egui::Color32::from_rgba_unmultiplied(100, 255, 100, 180) // green = compatible
                        } else {
                            egui::Color32::from_rgba_unmultiplied(255, 80, 80, 180) // red = incompatible
                        };
                        painter.circle_stroke(snap_pos, 12.0, egui::Stroke::new(2.5, ring_color));
                    }
                }
            }
        }

        // Draw dashed association lines between OB Hub and its device nodes
        let dash_color = egui::Color32::from_rgba_unmultiplied(80, 200, 120, 100);
        for (&dev_id, dev_node) in &self.graph.nodes {
            let hub_id = match &dev_node.node_type {
                NodeType::ObJoystick { hub_node_id, .. } if *hub_node_id != 0 => *hub_node_id,
                NodeType::ObEncoder { hub_node_id, .. } if *hub_node_id != 0 => *hub_node_id,
                _ => continue,
            };
            if let (Some(hub_rect), Some(dev_rect)) = (self.node_rects.get(&hub_id), self.node_rects.get(&dev_id)) {
                let from = egui::pos2(hub_rect.right(), hub_rect.center().y);
                let to = egui::pos2(dev_rect.left(), dev_rect.center().y);
                // Draw dashed line
                let total = from.distance(to);
                let dash_len = 6.0;
                let gap_len = 4.0;
                let dir = (to - from) / total;
                let mut d = 0.0;
                while d < total {
                    let seg_start = from + dir * d;
                    let seg_end = from + dir * (d + dash_len).min(total);
                    painter.line_segment([seg_start, seg_end], egui::Stroke::new(1.5, dash_color));
                    d += dash_len + gap_len;
                }
            }
        }
    }
}
