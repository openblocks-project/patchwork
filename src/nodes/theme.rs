use crate::graph::{NodeId, PortValue, Connection, PortKind, Graph};
use std::collections::HashMap;
use eframe::egui;

const PORT_SIZE: f32 = 14.0;

/// Draw a small inline input port, register its position.
fn inline_input(
    ui: &mut egui::Ui,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    node_id: NodeId,
    port: usize,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    kind: PortKind,
) -> Option<f32> {
    let connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == port);
    super::inline_port_circle(ui, node_id, port, true, connections, port_positions, dragging_from, pending_disconnects, kind);
    if connected {
        for c in connections {
            if c.to_node == node_id && c.to_port == port {
                return values.get(&(c.from_node, c.from_port)).map(|v| v.as_float());
            }
        }
    }
    None
}

/// Draw a small inline output port, register its position.
fn inline_output(
    ui: &mut egui::Ui,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    connections: &[Connection],
    node_id: NodeId,
    port: usize,
    kind: PortKind,
) {
    super::inline_port_circle(ui, node_id, port, false, connections, port_positions, dragging_from, pending_disconnects, kind);
}

/// Color row: [in R] [in G] [in B] label [picker] R G B [out R] [out G] [out B]
fn color_row(
    ui: &mut egui::Ui,
    label: &str,
    rgb: &mut [u8; 3],
    use_hsl: bool,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    connections: &[Connection],
    node_id: NodeId,
    in_base: usize,
    out_base: usize,
    values: &HashMap<(NodeId, usize), PortValue>,
) {
    // Apply overrides from inputs
    if let Some(v) = inline_input_val(values, connections, node_id, in_base) { rgb[0] = v.clamp(0.0, 255.0) as u8; }
    if let Some(v) = inline_input_val(values, connections, node_id, in_base + 1) { rgb[1] = v.clamp(0.0, 255.0) as u8; }
    if let Some(v) = inline_input_val(values, connections, node_id, in_base + 2) { rgb[2] = v.clamp(0.0, 255.0) as u8; }

    // Label + color swatch (group header)
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small());
        let mut color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        if ui.color_edit_button_srgba(&mut color).changed() {
            *rgb = [color.r(), color.g(), color.b()];
        }
        // Output ports
        if out_base > 0 {
            inline_output(ui, port_positions, dragging_from, pending_disconnects, connections, node_id, out_base, PortKind::Color);
            inline_output(ui, port_positions, dragging_from, pending_disconnects, connections, node_id, out_base + 1, PortKind::Color);
            inline_output(ui, port_positions, dragging_from, pending_disconnects, connections, node_id, out_base + 2, PortKind::Color);
        }
    });

    // R G B — each with its own connector dot + DragValue
    let channel_labels = if use_hsl {
        let (h, s, l) = rgb_to_hsl(rgb[0], rgb[1], rgb[2]);
        vec![("H", h, 0.0f32, 360.0f32), ("S", s, 0.0, 100.0), ("L", l, 0.0, 100.0)]
    } else {
        vec![("R", rgb[0] as f32, 0.0f32, 255.0f32), ("G", rgb[1] as f32, 0.0, 255.0), ("B", rgb[2] as f32, 0.0, 255.0)]
    };

    ui.horizontal(|ui| {
        for (ci, (ch_label, _val, min, max)) in channel_labels.iter().enumerate() {
            let port = in_base + ci;
            let is_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == port);

            super::inline_port_circle(ui, node_id, port, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Color);

            // Label + value
            ui.label(egui::RichText::new(*ch_label).small());
            if is_wired {
                let v = inline_input_val(values, connections, node_id, port).unwrap_or(0.0);
                ui.label(egui::RichText::new(format!("{:.0}", v)).small().monospace());
            } else if use_hsl {
                let (h, s, l) = rgb_to_hsl(rgb[0], rgb[1], rgb[2]);
                let mut vals = [h, s, l];
                if ui.add(egui::DragValue::new(&mut vals[ci]).range(*min..=*max).speed(1.0)).changed() {
                    let (_r, _g, _b) = hsl_to_rgb(vals[0], vals[1], vals[2]);
                    // Only update the changed channel's effect
                    match ci {
                        0 => { let (nr, ng, nb) = hsl_to_rgb(vals[0], s, l); *rgb = [nr, ng, nb]; }
                        1 => { let (nr, ng, nb) = hsl_to_rgb(h, vals[1], l); *rgb = [nr, ng, nb]; }
                        2 => { let (nr, ng, nb) = hsl_to_rgb(h, s, vals[2]); *rgb = [nr, ng, nb]; }
                        _ => {}
                    }
                }
            } else {
                ui.add(egui::DragValue::new(&mut rgb[ci]).range(0..=255).speed(1.0));
            }
        }
    });
}

/// Float row: [in] label [slider] value
fn float_row(
    ui: &mut egui::Ui,
    label: &str,
    val: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    suffix: &str,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    connections: &[Connection],
    node_id: NodeId,
    port: usize,
    values: &HashMap<(NodeId, usize), PortValue>,
    kind: PortKind,
) {
    if let Some(v) = inline_input_val(values, connections, node_id, port) { *val = v; }
    ui.horizontal(|ui| {
        inline_input(ui, port_positions, dragging_from, pending_disconnects, node_id, port, values, connections, kind);
        ui.label(egui::RichText::new(label).small());
        ui.add(egui::Slider::new(val, range).suffix(suffix));
    });
}

fn u8_row(
    ui: &mut egui::Ui,
    label: &str,
    val: &mut u8,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    connections: &[Connection],
    node_id: NodeId,
    port: usize,
    values: &HashMap<(NodeId, usize), PortValue>,
    kind: PortKind,
) {
    if let Some(v) = inline_input_val(values, connections, node_id, port) { *val = v.clamp(0.0, 255.0) as u8; }
    let mut f = *val as f32;
    ui.horizontal(|ui| {
        inline_input(ui, port_positions, dragging_from, pending_disconnects, node_id, port, values, connections, kind);
        ui.label(egui::RichText::new(label).small());
        if ui.add(egui::Slider::new(&mut f, 0.0..=255.0)).changed() { *val = f as u8; }
    });
}

fn inline_input_val(values: &HashMap<(NodeId, usize), PortValue>, connections: &[Connection], node_id: NodeId, port: usize) -> Option<f32> {
    let connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == port);
    if !connected { return None; }
    for c in connections {
        if c.to_node == node_id && c.to_port == port {
            return values.get(&(c.from_node, c.from_port)).map(|v| v.as_float());
        }
    }
    None
}

// Input ports: 0-2 BG, 3-5 Text, 6-8 Accent, 9-11 Win, 12-14 Grid, 15 Font, 16 Round, 17 Space, 18 Alpha, 19 BG Path, 20 Wire, 21 BG Image, 22 W Gravity, 23 W Range, 24 W Speed
// Output ports: 0-2 BG, 3-5 Text, 6-8 Accent

pub fn render(
    ui: &mut egui::Ui,
    dark_mode: &mut bool,
    accent: &mut [u8; 3],
    font_size: &mut f32,
    bg_color: &mut [u8; 3],
    text_color: &mut [u8; 3],
    window_bg: &mut [u8; 3],
    window_alpha: &mut u8,
    grid_color: &mut [u8; 3],
    grid_style: &mut u8,
    wire_style: &mut u8,
    wiggle_gravity: &mut f32,
    wiggle_range: &mut f32,
    wiggle_speed: &mut f32,
    wiggle_signal: &mut f32,
    rounding: &mut f32,
    spacing: &mut f32,
    use_hsl: &mut bool,
    wire_thickness: &mut f32,
    background_path: &mut String,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    // Legacy migration: old saves had style 3 = Wiggly. Wiggly is now
    // unified with Bezier into the single "Curved" style (index 0), where
    // the wiggle params directly control displacement.
    if *wire_style == 3 { *wire_style = 0; }

    // Save / Open
    ui.horizontal(|ui| {
        if ui.small_button("Save...").clicked() {
            let data = serde_json::json!({
                "dark_mode": *dark_mode,
                "bg_color": *bg_color,
                "text_color": *text_color,
                "accent": *accent,
                "window_bg": *window_bg,
                "window_alpha": *window_alpha,
                "grid_color": *grid_color,
                "font_size": *font_size,
                "rounding": *rounding,
                "spacing": *spacing,
                "use_hsl": *use_hsl,
            });
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name("theme.json")
                .add_filter("JSON", &["json"])
                .save_file()
            {
                let _ = std::fs::write(&path, serde_json::to_string_pretty(&data).unwrap_or_default());
            }
        }
        if ui.small_button("Open...").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("JSON", &["json"])
                .pick_file()
            {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(b) = v.get("dark_mode").and_then(|v| v.as_bool()) { *dark_mode = b; }
                        if let Some(a) = v.get("bg_color").and_then(|v| v.as_array()) {
                            if a.len() == 3 { *bg_color = [a[0].as_u64().unwrap_or(0) as u8, a[1].as_u64().unwrap_or(0) as u8, a[2].as_u64().unwrap_or(0) as u8]; }
                        }
                        if let Some(a) = v.get("text_color").and_then(|v| v.as_array()) {
                            if a.len() == 3 { *text_color = [a[0].as_u64().unwrap_or(0) as u8, a[1].as_u64().unwrap_or(0) as u8, a[2].as_u64().unwrap_or(0) as u8]; }
                        }
                        if let Some(a) = v.get("accent").and_then(|v| v.as_array()) {
                            if a.len() == 3 { *accent = [a[0].as_u64().unwrap_or(0) as u8, a[1].as_u64().unwrap_or(0) as u8, a[2].as_u64().unwrap_or(0) as u8]; }
                        }
                        if let Some(a) = v.get("window_bg").and_then(|v| v.as_array()) {
                            if a.len() == 3 { *window_bg = [a[0].as_u64().unwrap_or(0) as u8, a[1].as_u64().unwrap_or(0) as u8, a[2].as_u64().unwrap_or(0) as u8]; }
                        }
                        if let Some(n) = v.get("window_alpha").and_then(|v| v.as_u64()) { *window_alpha = n as u8; }
                        if let Some(a) = v.get("grid_color").and_then(|v| v.as_array()) {
                            if a.len() == 3 { *grid_color = [a[0].as_u64().unwrap_or(0) as u8, a[1].as_u64().unwrap_or(0) as u8, a[2].as_u64().unwrap_or(0) as u8]; }
                        }
                        if let Some(n) = v.get("font_size").and_then(|v| v.as_f64()) { *font_size = n as f32; }
                        if let Some(n) = v.get("rounding").and_then(|v| v.as_f64()) { *rounding = n as f32; }
                        if let Some(n) = v.get("spacing").and_then(|v| v.as_f64()) { *spacing = n as f32; }
                        if let Some(b) = v.get("use_hsl").and_then(|v| v.as_bool()) { *use_hsl = b; }
                    }
                }
            }
        }
    });

    // Presets
    ui.label(egui::RichText::new("Preset").small().color(egui::Color32::GRAY));
    ui.horizontal(|ui| {
        if ui.small_button("Dark").clicked() {
            apply_preset_dark(dark_mode, bg_color, text_color, window_bg, window_alpha, grid_color, accent, font_size, rounding, spacing, wire_thickness, grid_style, wire_style);
        }
        if ui.small_button("Light").clicked() {
            apply_preset_light(dark_mode, bg_color, text_color, window_bg, window_alpha, grid_color, accent, font_size, rounding, spacing, wire_thickness, grid_style, wire_style);
        }
        if ui.small_button("Blue").clicked() {
            apply_preset_blue(dark_mode, bg_color, text_color, window_bg, window_alpha, grid_color, accent, font_size, rounding, spacing, wire_thickness, grid_style, wire_style);
        }
        if ui.small_button("Reset").clicked() {
            apply_defaults(dark_mode, bg_color, text_color, window_bg, window_alpha, grid_color, accent, font_size, rounding, spacing, wire_thickness, grid_style, wire_style);
        }
    });

    ui.separator();

    // Mode
    ui.label(egui::RichText::new("Mode").small().color(egui::Color32::GRAY));
    ui.horizontal(|ui| {
        ui.radio_value(use_hsl, false, "RGB");
        ui.radio_value(use_hsl, true, "HSL");
    });

    ui.separator();

    // Colors with inline ports: in ports (left) ● ● ● label [picker] ● ● ● out ports (right)
    color_row(ui, "BG", bg_color, *use_hsl, port_positions, dragging_from, pending_disconnects, connections, node_id, 0, 1, values);
    color_row(ui, "Text", text_color, *use_hsl, port_positions, dragging_from, pending_disconnects, connections, node_id, 3, 0, values);
    color_row(ui, "Accent", accent, *use_hsl, port_positions, dragging_from, pending_disconnects, connections, node_id, 6, 0, values);
    color_row(ui, "Window", window_bg, *use_hsl, port_positions, dragging_from, pending_disconnects, connections, node_id, 9, 0, values);
    color_row(ui, "Grid", grid_color, *use_hsl, port_positions, dragging_from, pending_disconnects, connections, node_id, 12, 0, values);

    // Grid style selector
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Grid").small());
        for (label, val) in [("Solid", 0u8), ("Square", 1), ("Dotted", 2)] {
            if ui.selectable_label(*grid_style == val, label).clicked() {
                *grid_style = val;
            }
        }
    });

    // Wire style selector — Curved, Straight, Ortho.
    // (Bezier + Wiggly are unified into "Curved": set wiggle params to 0
    // for a plain bezier look, or raise them for a living/animated wire.)
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Wires").small());
        for (label, val) in [("Curved", 0u8), ("Straight", 1), ("Ortho", 2)] {
            if ui.selectable_label(*wire_style == val, label).clicked() {
                *wire_style = val;
            }
        }
    });

    // Wiggle params: always relevant under Curved style (params = 0 → bezier,
    // params > 0 → animated wiggly). Shown only when Curved is active; the
    // ports themselves remain valid across style switches so wires don't
    // need to be disconnected on toggle.
    if *wire_style == 0 {
        // Wiggle params are stored in their internal scaled ranges but displayed
        // as a normalized 0..1 slider. Port inputs are already 0..1 and scale
        // up to the same internal ranges.
        //   Gravity  : 0..10   (sag in downward pixels-per-distance)
        //   Range    : 0..24   (amplitude multiplier)
        //   Speed    : 0..48   (phase advance rate)
        //   Signal   : 0..1    (how much per-wire activity modulates animation)
        const GRAV_MAX: f32 = 10.0;
        const RANGE_MAX: f32 = 24.0;
        const SPEED_MAX: f32 = 48.0;

        // Read from input ports if connected (ports 22, 23, 24, 25)
        let grav_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 22);
        let range_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 23);
        let speed_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 24);
        let signal_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 25);
        if grav_wired {
            *wiggle_gravity = Graph::static_input_value(connections, values, node_id, 22).as_float().clamp(0.0, 1.0) * GRAV_MAX;
        }
        if range_wired {
            *wiggle_range = Graph::static_input_value(connections, values, node_id, 23).as_float().clamp(0.0, 1.0) * RANGE_MAX;
        }
        if speed_wired {
            *wiggle_speed = Graph::static_input_value(connections, values, node_id, 24).as_float().clamp(0.0, 1.0) * SPEED_MAX;
        }
        if signal_wired {
            *wiggle_signal = Graph::static_input_value(connections, values, node_id, 25).as_float().clamp(0.0, 1.0);
        }

        // Helper: normalized slider that reads/writes an internally-scaled value.
        let wiggle_row = |ui: &mut egui::Ui,
                          label: &str,
                          port: usize,
                          wired: bool,
                          value: &mut f32,
                          scale: f32,
                          port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
                          dragging_from: &mut Option<(NodeId, usize, bool)>,
                          pending_disconnects: &mut Vec<(NodeId, usize)>| {
            ui.horizontal(|ui| {
                super::inline_port_circle(ui, node_id, port, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Normalized);
                ui.label(egui::RichText::new(label).small().color(egui::Color32::GRAY));
                let mut norm = (*value / scale).clamp(0.0, 1.0);
                if wired {
                    ui.label(egui::RichText::new(format!("{:.2}", norm)).small().color(egui::Color32::from_rgb(80, 170, 255)));
                } else {
                    let resp = ui.add(egui::Slider::new(&mut norm, 0.0..=1.0).step_by(0.01).show_value(false));
                    ui.add(egui::DragValue::new(&mut norm).speed(0.01).range(0.0..=1.0).max_decimals(2));
                    if resp.changed() || ui.ctx().input(|i| i.pointer.any_released()) {
                        *value = norm * scale;
                    } else {
                        *value = norm * scale;
                    }
                }
            });
        };

        wiggle_row(ui, "Gravity", 22, grav_wired, wiggle_gravity, GRAV_MAX, port_positions, dragging_from, pending_disconnects);
        wiggle_row(ui, "Range", 23, range_wired, wiggle_range, RANGE_MAX, port_positions, dragging_from, pending_disconnects);
        wiggle_row(ui, "Speed", 24, speed_wired, wiggle_speed, SPEED_MAX, port_positions, dragging_from, pending_disconnects);
        // Signal is already normalized 0..1 — no scale needed.
        wiggle_row(ui, "Signal", 25, signal_wired, wiggle_signal, 1.0, port_positions, dragging_from, pending_disconnects);
    }

    ui.separator();

    // Float params with inline input ports
    float_row(ui, "Font", font_size, 8.0..=28.0, "px", port_positions, dragging_from, pending_disconnects, connections, node_id, 15, values, PortKind::Number);
    float_row(ui, "Wire", wire_thickness, 1.0..=16.0, "px", port_positions, dragging_from, pending_disconnects, connections, node_id, 20, values, PortKind::Number);
    float_row(ui, "Round", rounding, 0.0..=32.0, "px", port_positions, dragging_from, pending_disconnects, connections, node_id, 16, values, PortKind::Number);
    float_row(ui, "Space", spacing, 0.0..=12.0, "px", port_positions, dragging_from, pending_disconnects, connections, node_id, 17, values, PortKind::Number);
    u8_row(ui, "Opacity", window_alpha, port_positions, dragging_from, pending_disconnects, connections, node_id, 18, values, PortKind::Normalized);

    ui.separator();

    // ── Background image/video ───────────────────────────────────────
    // Port 19: Background (accepts Image from Image node, WGSL, Video, Blend)
    ui.horizontal(|ui| {
        let bg_connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == 19);
        super::inline_port_circle(ui, node_id, 19, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Text);

        ui.label(egui::RichText::new("BG").small());

        if bg_connected {
            let val = crate::graph::Graph::static_input_value(connections, values, node_id, 19);
            match &val {
                PortValue::Image(img) => {
                    ui.label(egui::RichText::new(format!("{}x{}", img.width, img.height)).small().monospace());
                }
                _ => { ui.label(egui::RichText::new("connected").small().color(egui::Color32::GRAY)); }
            }
        } else {
            // File path input + open button
            if ui.small_button("Open").clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "gif", "bmp", "webp"])
                    .pick_file()
                {
                    *background_path = p.display().to_string();
                }
            }
        }
    });
    if !background_path.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(PORT_SIZE + 4.0);
            let short = if background_path.len() > 25 {
                format!("...{}", &background_path[background_path.len()-25..])
            } else {
                background_path.clone()
            };
            ui.label(egui::RichText::new(short).small().monospace().color(egui::Color32::GRAY));
        });
    }

    // BG Image input port (accepts Image from WGSL Viewer, Camera, Image node, etc.)
    ui.horizontal(|ui| {
        super::inline_port_circle(ui, node_id, 21, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Image);
        let img_connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == 21);
        if img_connected {
            ui.label(egui::RichText::new("BG Image connected").small().color(egui::Color32::from_rgb(80, 200, 120)));
        }
    });
}

/// Apply theme settings to the egui context.
pub fn apply(
    ctx: &egui::Context,
    dark_mode: bool,
    accent: [u8; 3],
    font_size: f32,
    bg_color: [u8; 3],
    text_color: [u8; 3],
    window_bg: [u8; 3],
    window_alpha: u8,
    rounding: f32,
    spacing: f32,
) {
    let mut visuals = if dark_mode { egui::Visuals::dark() } else { egui::Visuals::light() };

    let bg = egui::Color32::from_rgb(bg_color[0], bg_color[1], bg_color[2]);
    let text = egui::Color32::from_rgb(text_color[0], text_color[1], text_color[2]);
    let text_dim = egui::Color32::from_rgba_unmultiplied(text_color[0], text_color[1], text_color[2], 140);
    let accent_color = egui::Color32::from_rgb(accent[0], accent[1], accent[2]);
    let win_bg = egui::Color32::from_rgba_unmultiplied(window_bg[0], window_bg[1], window_bg[2], window_alpha);

    // Derive surface colors from BG (lighter shades for interactive elements)
    let surface_dim = offset_color(bg, if dark_mode { 8 } else { -8 });
    let surface = offset_color(bg, if dark_mode { 16 } else { -16 });
    let surface_bright = offset_color(bg, if dark_mode { 28 } else { -28 });

    // ── Backgrounds ──
    visuals.panel_fill = bg;
    visuals.window_fill = win_bg;
    visuals.faint_bg_color = surface_dim;
    visuals.extreme_bg_color = offset_color(bg, if dark_mode { -4 } else { 4 });

    // ── Text ──
    visuals.override_text_color = Some(text);

    // ── Selection & hyperlinks ──
    visuals.selection.bg_fill = accent_color.gamma_multiply(0.3);
    visuals.selection.stroke = egui::Stroke::new(1.5, accent_color);
    visuals.hyperlink_color = accent_color;

    // ── Window chrome ──
    visuals.window_stroke = egui::Stroke::new(0.5, surface_bright);
    visuals.window_shadow = egui::Shadow::NONE;
    visuals.popup_shadow = egui::Shadow::NONE;

    // ── Widgets: noninteractive (labels, separators, frames) ──
    visuals.widgets.noninteractive.bg_fill = surface_dim;
    visuals.widgets.noninteractive.weak_bg_fill = surface_dim;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(0.5, surface_bright);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, text_dim);

    // ── Widgets: inactive (buttons, sliders at rest) ──
    visuals.widgets.inactive.bg_fill = surface;
    visuals.widgets.inactive.weak_bg_fill = surface;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(0.5, surface_bright);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, text);

    // ── Widgets: hovered ──
    visuals.widgets.hovered.bg_fill = accent_color.gamma_multiply(0.15);
    visuals.widgets.hovered.weak_bg_fill = accent_color.gamma_multiply(0.10);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, accent_color.gamma_multiply(0.5));
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, text);

    // ── Widgets: active (clicked/dragging) ──
    visuals.widgets.active.bg_fill = accent_color.gamma_multiply(0.25);
    visuals.widgets.active.weak_bg_fill = accent_color.gamma_multiply(0.20);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.5, accent_color);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(2.0, text);

    // ── Widgets: open (expanded combo boxes, menus) ──
    visuals.widgets.open.bg_fill = surface;
    visuals.widgets.open.weak_bg_fill = surface;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, accent_color.gamma_multiply(0.4));
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, text);

    // ── Rounding ──
    let r = egui::CornerRadius::same(rounding as u8);
    visuals.window_corner_radius = r;
    visuals.widgets.noninteractive.corner_radius = r;
    visuals.widgets.inactive.corner_radius = r;
    visuals.widgets.hovered.corner_radius = r;
    visuals.widgets.active.corner_radius = r;
    visuals.widgets.open.corner_radius = r;
    visuals.menu_corner_radius = r;

    // ── Scrollbar ──
    visuals.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 1.0 };
    visuals.striped = false;
    visuals.slider_trailing_fill = true;
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);

    // ── Separators ──
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(0.5, surface);

    // ── Text cursor ──
    visuals.text_cursor.stroke = egui::Stroke::new(2.0, accent_color);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    for (_, font_id) in style.text_styles.iter_mut() {
        font_id.size = font_size;
    }
    style.spacing.item_spacing = egui::vec2(spacing, spacing);
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: 6.0,
        ..style.spacing.scroll
    };
    ctx.set_style(style);
}

/// Offset an RGB color by a fixed amount (positive = brighter, negative = darker)
fn offset_color(c: egui::Color32, amount: i16) -> egui::Color32 {
    let clamp = |v: i16| v.clamp(0, 255) as u8;
    egui::Color32::from_rgb(
        clamp(c.r() as i16 + amount),
        clamp(c.g() as i16 + amount),
        clamp(c.b() as i16 + amount),
    )
}

// ── Defaults & Presets ───────────────────────────────────────────────────

/// The canonical defaults — used by Reset button and initial Theme node creation
/// Generate a random accent color by picking a random hue with fixed saturation/lightness.
/// Produces vibrant, readable colors on dark backgrounds every time.
pub fn random_accent() -> [u8; 3] { random_accent_color() }

fn random_accent_color() -> [u8; 3] {
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::sync::atomic::{AtomicU64, Ordering};
    // Counter ensures consecutive calls in the same process get different hues,
    // even if nanosecond timestamps are close. Golden ratio spacing (137.5°)
    // maximizes perceptual distance between sequential hues.
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos()).unwrap_or(0);
    let hue = ((seed + count as u128 * 137) % 360) as f32; // golden-angle spacing
    let s = 0.7_f32;  // saturation
    let l = 0.65_f32; // lightness
    // HSL to RGB
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match hue as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8]
}

pub fn apply_defaults(
    dark_mode: &mut bool, bg_color: &mut [u8; 3], text_color: &mut [u8; 3],
    window_bg: &mut [u8; 3], window_alpha: &mut u8, grid_color: &mut [u8; 3],
    accent: &mut [u8; 3], font_size: &mut f32, rounding: &mut f32, spacing: &mut f32,
    wire_thickness: &mut f32, grid_style: &mut u8, wire_style: &mut u8,
) {
    *dark_mode = true;
    *bg_color = [20, 20, 20]; *text_color = [220, 220, 220];
    // Randomise accent hue — keeps saturation ~70% and lightness ~65% for readability
    *accent = random_accent_color();
    *window_bg = [24, 24, 24]; *window_alpha = 240;
    *grid_color = [28, 28, 28];
    *font_size = 14.0; *rounding = 16.0; *spacing = 4.0;
    *wire_thickness = 6.0; *grid_style = 2; *wire_style = 0;
}

fn apply_preset_dark(
    dark_mode: &mut bool, bg_color: &mut [u8; 3], text_color: &mut [u8; 3],
    window_bg: &mut [u8; 3], window_alpha: &mut u8, grid_color: &mut [u8; 3],
    accent: &mut [u8; 3], font_size: &mut f32, rounding: &mut f32, spacing: &mut f32,
    wire_thickness: &mut f32, grid_style: &mut u8, wire_style: &mut u8,
) {
    apply_defaults(dark_mode, bg_color, text_color, window_bg, window_alpha, grid_color, accent, font_size, rounding, spacing, wire_thickness, grid_style, wire_style);
}

fn apply_preset_light(
    dark_mode: &mut bool, bg_color: &mut [u8; 3], text_color: &mut [u8; 3],
    window_bg: &mut [u8; 3], window_alpha: &mut u8, grid_color: &mut [u8; 3],
    accent: &mut [u8; 3], font_size: &mut f32, rounding: &mut f32, spacing: &mut f32,
    _wire_thickness: &mut f32, grid_style: &mut u8, _wire_style: &mut u8,
) {
    *dark_mode = false;
    *bg_color = [240, 240, 240]; *text_color = [30, 30, 30];
    *accent = [60, 120, 220];
    *window_bg = [255, 255, 255]; *window_alpha = 240;
    *grid_color = [200, 200, 200];
    *font_size = 14.0; *rounding = 16.0; *spacing = 4.0;
    *grid_style = 2;
}

fn apply_preset_blue(
    dark_mode: &mut bool, bg_color: &mut [u8; 3], text_color: &mut [u8; 3],
    window_bg: &mut [u8; 3], window_alpha: &mut u8, grid_color: &mut [u8; 3],
    accent: &mut [u8; 3], font_size: &mut f32, rounding: &mut f32, spacing: &mut f32,
    _wire_thickness: &mut f32, grid_style: &mut u8, _wire_style: &mut u8,
) {
    *dark_mode = true;
    *bg_color = [15, 20, 35]; *text_color = [200, 210, 230];
    *accent = [60, 140, 255];
    *window_bg = [20, 30, 50]; *window_alpha = 230;
    *grid_color = [20, 25, 45];
    *font_size = 14.0; *rounding = 16.0; *spacing = 4.0;
    *grid_style = 2;
}

// ── HSL helpers ──────────────────────────────────────────────────────────

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let rf = r as f32 / 255.0;
    let gf = g as f32 / 255.0;
    let bf = b as f32 / 255.0;
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-6 { return (0.0, 0.0, l * 100.0); }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if (max - rf).abs() < 1e-6 {
        ((gf - bf) / d) + if gf < bf { 6.0 } else { 0.0 }
    } else if (max - gf).abs() < 1e-6 {
        ((bf - rf) / d) + 2.0
    } else {
        ((rf - gf) / d) + 4.0
    };
    (h * 60.0, s * 100.0, l * 100.0)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let s = s / 100.0;
    let l = l / 100.0;
    if s.abs() < 1e-6 { let v = (l * 255.0) as u8; return (v, v, v); }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let h = h / 360.0;
    let r = hue_to_rgb(p, q, h + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h);
    let b = hue_to_rgb(p, q, h - 1.0 / 3.0);
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 { t += 1.0; }
    if t > 1.0 { t -= 1.0; }
    if t < 1.0 / 6.0 { return p + (q - p) * 6.0 * t; }
    if t < 1.0 / 2.0 { return q; }
    if t < 2.0 / 3.0 { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
    p
}

// ── Read-side helpers ──────────────────────────────────────────────────────
//
// These read the colors that `apply_theme` (in `src/app/io.rs`) publishes
// into `ctx.data` each frame. Callers anywhere in the app can use them to
// stay in sync with the active Theme node without holding a reference to it.
// Each helper returns the canonical fallback when no Theme is present, which
// matches the defaults `apply_theme` would otherwise apply.

const DEFAULT_ACCENT: [u8; 3] = [138, 180, 255];
const DEFAULT_TEXT: [u8; 3] = [220, 220, 220];
const DEFAULT_BG: [u8; 3] = [20, 20, 20];

/// Current accent color as `[r, g, b]`. Cheap (single ctx.data read).
pub fn current_accent_arr(ctx: &eframe::egui::Context) -> [u8; 3] {
    ctx.data(|d| d.get_temp::<[u8; 3]>(eframe::egui::Id::new("theme_accent")))
        .unwrap_or(DEFAULT_ACCENT)
}

/// Current text color as `[r, g, b]`.
pub fn current_text_arr(ctx: &eframe::egui::Context) -> [u8; 3] {
    ctx.data(|d| d.get_temp::<[u8; 3]>(eframe::egui::Id::new("theme_text")))
        .unwrap_or(DEFAULT_TEXT)
}

/// Current background (panel_fill) color as `[r, g, b]`.
pub fn current_bg_arr(ctx: &eframe::egui::Context) -> [u8; 3] {
    ctx.data(|d| d.get_temp::<[u8; 3]>(eframe::egui::Id::new("theme_bg")))
        .unwrap_or(DEFAULT_BG)
}

/// Convenience wrappers returning `egui::Color32` directly.
pub fn current_accent(ctx: &eframe::egui::Context) -> eframe::egui::Color32 {
    let [r, g, b] = current_accent_arr(ctx);
    eframe::egui::Color32::from_rgb(r, g, b)
}

#[allow(dead_code)]
pub fn current_text(ctx: &eframe::egui::Context) -> eframe::egui::Color32 {
    let [r, g, b] = current_text_arr(ctx);
    eframe::egui::Color32::from_rgb(r, g, b)
}

#[allow(dead_code)]
pub fn current_bg(ctx: &eframe::egui::Context) -> eframe::egui::Color32 {
    let [r, g, b] = current_bg_arr(ctx);
    eframe::egui::Color32::from_rgb(r, g, b)
}

/// Brighten an `[r,g,b]` by `delta` per channel (saturating). Used for
/// deriving subtle surface variants from `bg`/`surface` colors.
pub fn brighten(c: [u8; 3], delta: i16) -> [u8; 3] {
    [
        (c[0] as i16 + delta).clamp(0, 255) as u8,
        (c[1] as i16 + delta).clamp(0, 255) as u8,
        (c[2] as i16 + delta).clamp(0, 255) as u8,
    ]
}
