// OB Light — WIP. WS2812B strip (200 LEDs) driven by a Glyph C6 over an OB
// Hub. Three pattern modes (Solid/Pointer/Pulse), two channel-port modes
// (RGB/HSL), and two send modes (Continuous/Triggered).
//
// Every settable value has a port AND a manual control. When a port is
// wired, it overrides the manual control and the row shows the live value
// in blue. When unwired, the manual control sets the value.
//
// Wire protocol always carries RGB:
//   /light/<id>/color R G B
//   /light/<id>/effect mode param brightness
// where `param` is `position` (Pointer) or `speed` (Pulse).

use crate::graph::*;
use crate::ob::ObManager;
use eframe::egui;
use std::collections::HashMap;

const MODE_SOLID:   u8 = 0;
const MODE_POINTER: u8 = 1;
const MODE_PULSE:   u8 = 2;

const COLOR_MODE_RGB: u8 = 0;
const COLOR_MODE_HSL: u8 = 1;

const SEND_MODE_CONT:    u8 = 0;
const SEND_MODE_TRIGGER: u8 = 1;

// Port indices — must match graph.rs `inputs()` for ObLight.
const PORT_POSITION:   usize = 0;
const PORT_SPEED:      usize = 1;
const PORT_CH0:        usize = 2;   // R or H
const PORT_CH1:        usize = 3;   // G or S
const PORT_CH2:        usize = 4;   // B or L
const PORT_TRIGGER:    usize = 5;
const PORT_BRIGHTNESS: usize = 6;

const MODES: &[(u8, &str)] = &[
    (MODE_SOLID,   "Solid"),
    (MODE_POINTER, "Pointer"),
    (MODE_PULSE,   "Pulse"),
];

// =========================================================================
// Color conversion helpers
// =========================================================================

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> [f32; 3] {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    if (max - min).abs() < 1e-6 { return [0.0, 0.0, l]; }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    [h, s, l]
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    if s.abs() < 1e-6 {
        let v = (l * 255.0).round().clamp(0.0, 255.0) as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let to_chan = |t: f32| -> f32 {
        let mut t = t;
        if t < 0.0 { t += 1.0; }
        if t > 1.0 { t -= 1.0; }
        if t < 1.0 / 6.0 { return p + (q - p) * 6.0 * t; }
        if t < 1.0 / 2.0 { return q; }
        if t < 2.0 / 3.0 { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
        p
    };
    let r = to_chan(h + 1.0 / 3.0);
    let g = to_chan(h);
    let b = to_chan(h - 1.0 / 3.0);
    [
        (r * 255.0).round().clamp(0.0, 255.0) as u8,
        (g * 255.0).round().clamp(0.0, 255.0) as u8,
        (b * 255.0).round().clamp(0.0, 255.0) as u8,
    ]
}

// =========================================================================
// Render
// =========================================================================

pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    ob_manager: &mut ObManager,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    let (device_id, hub_node_id, mode, color, brightness, position, speed, color_mode, send_mode) = match node_type {
        NodeType::ObLight { device_id, hub_node_id, mode, color, brightness, position, speed, color_mode, send_mode } =>
            (device_id, hub_node_id, mode, color, brightness, position, speed, color_mode, send_mode),
        _ => return,
    };

    // Snap any out-of-range mode to a sane default.
    if !MODES.iter().any(|(v, _)| *v == *mode) { *mode = MODE_SOLID; }
    if *color_mode > COLOR_MODE_HSL { *color_mode = COLOR_MODE_RGB; }
    if *send_mode  > SEND_MODE_TRIGGER { *send_mode = SEND_MODE_CONT; }

    let did = *device_id;
    let hid = *hub_node_id;

    let is_active = {
        let dev = ob_manager.get_hub(hid)
            .and_then(|h| h.get_device("light", did))
            .or_else(|| ob_manager.find_device("light", did).map(|(_, d)| d));
        dev.map(|d| d.is_active).unwrap_or(false)
    };

    // ── WIP banner ─────────────────────────────────────────────
    ui.colored_label(egui::Color32::from_rgb(220, 170, 60),
        egui::RichText::new("⚠ WIP — wireless LED strip").small());

    // ── Header: ID + status ────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("ID:").small());
        ui.add(egui::DragValue::new(device_id).range(1..=255));
        if is_active {
            ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "● Active");
        } else {
            ui.colored_label(egui::Color32::from_rgb(150, 150, 150), "○ Waiting");
        }
    });

    // ── Pattern mode ───────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Mode").small());
        let cur = MODES.iter().find(|(v, _)| *v == *mode).map(|(_, n)| *n).unwrap_or("Solid");
        egui::ComboBox::from_id_salt(egui::Id::new(("light_mode", node_id)))
            .selected_text(cur).width(100.0)
            .show_ui(ui, |ui| {
                for (val, name) in MODES {
                    if ui.selectable_label(*mode == *val, *name).clicked() { *mode = *val; }
                }
            });
    });

    // ── Color picker + RGB/HSL toggle ──────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Color").small());
        let mut c = egui::Color32::from_rgb(color[0], color[1], color[2]);
        if ui.color_edit_button_srgba(&mut c).changed() {
            let a = c.to_array();
            *color = [a[0], a[1], a[2]];
        }
        egui::ComboBox::from_id_salt(egui::Id::new(("light_color_mode", node_id)))
            .selected_text(if *color_mode == COLOR_MODE_HSL { "HSL" } else { "RGB" })
            .width(56.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(*color_mode == COLOR_MODE_RGB, "RGB").clicked() { *color_mode = COLOR_MODE_RGB; }
                if ui.selectable_label(*color_mode == COLOR_MODE_HSL, "HSL").clicked() { *color_mode = COLOR_MODE_HSL; }
            });
    });

    // ── Per-channel rows: port + label + slider (or wired value) ──
    // Reads each port's wired value (if any). The slider is shown when the
    // port is unwired, and modifies `color` directly (in HSL mode the change
    // is applied to that HSL component then converted back to RGB).
    let read_port = |port: usize| -> Option<f32> {
        let wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == port);
        if !wired { return None; }
        Some(Graph::static_input_value(connections, values, node_id, port).as_float().clamp(0.0, 1.0))
    };

    let p0 = read_port(PORT_CH0);
    let p1 = read_port(PORT_CH1);
    let p2 = read_port(PORT_CH2);

    // Compute the displayed/manual values for each channel — derived from
    // `color` (RGB storage) once per render so all sliders agree.
    let baseline_hsl = rgb_to_hsl(color[0], color[1], color[2]);
    let baseline_unit: [f32; 3] = if *color_mode == COLOR_MODE_HSL {
        baseline_hsl
    } else {
        [color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0]
    };

    let (chan_label, chan_max_disp, chan_unit_suffix): (
        [&str; 3], [f32; 3], [&str; 3]
    ) = if *color_mode == COLOR_MODE_HSL {
        (["H", "S", "L"], [360.0, 100.0, 100.0], ["°", "%", "%"])
    } else {
        (["R", "G", "B"], [255.0, 255.0, 255.0], ["", "", ""])
    };

    // Helper: render one channel row. Returns the new RGB color if the slider
    // changed; otherwise None.
    let render_channel = |
        ui: &mut egui::Ui,
        port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
        dragging_from: &mut Option<(NodeId, usize, bool)>,
        pending_disconnects: &mut Vec<(NodeId, usize)>,
        port: usize,
        idx: usize,
        wired_val: Option<f32>,
        current_color: [u8; 3],
    | -> Option<[u8; 3]> {
        let mut new_color: Option<[u8; 3]> = None;
        ui.horizontal(|ui| {
            crate::nodes::inline_port_circle(ui, node_id, port, true, connections,
                port_positions, dragging_from, pending_disconnects, PortKind::Color);
            ui.label(egui::RichText::new(chan_label[idx]).small());

            if let Some(v) = wired_val {
                // Wired → show value in mode-appropriate units.
                let s = match (*color_mode, idx) {
                    (COLOR_MODE_HSL, 0) => format!("{:.0}°", v * 360.0),
                    (COLOR_MODE_HSL, _) => format!("{:.0}%", v * 100.0),
                    _                   => format!("{}", (v * 255.0).round() as u8),
                };
                ui.label(egui::RichText::new(s).small()
                    .color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                // Unwired → integer slider in mode-appropriate units.
                let mut display = baseline_unit[idx] * chan_max_disp[idx];
                let resp = ui.add(
                    egui::Slider::new(&mut display, 0.0..=chan_max_disp[idx])
                        .integer().suffix(chan_unit_suffix[idx]).show_value(false)
                );
                if resp.changed() {
                    let unit = (display / chan_max_disp[idx]).clamp(0.0, 1.0);
                    if *color_mode == COLOR_MODE_HSL {
                        let mut hsl = baseline_hsl;
                        hsl[idx] = unit;
                        new_color = Some(hsl_to_rgb(hsl[0], hsl[1], hsl[2]));
                    } else {
                        let mut c = current_color;
                        c[idx] = (unit * 255.0).round() as u8;
                        new_color = Some(c);
                    }
                }
            }
        });
        new_color
    };

    if let Some(new_c) = render_channel(ui, port_positions, dragging_from, pending_disconnects,
                                        PORT_CH0, 0, p0, *color) { *color = new_c; }
    if let Some(new_c) = render_channel(ui, port_positions, dragging_from, pending_disconnects,
                                        PORT_CH1, 1, p1, *color) { *color = new_c; }
    if let Some(new_c) = render_channel(ui, port_positions, dragging_from, pending_disconnects,
                                        PORT_CH2, 2, p2, *color) { *color = new_c; }

    // Resolve final RGB to send. Wired ports override per-channel.
    let (eff_r, eff_g, eff_b) = if *color_mode == COLOR_MODE_HSL {
        let h = p0.unwrap_or(baseline_hsl[0]);
        let s = p1.unwrap_or(baseline_hsl[1]);
        let l = p2.unwrap_or(baseline_hsl[2]);
        let rgb = hsl_to_rgb(h, s, l);
        (rgb[0], rgb[1], rgb[2])
    } else {
        let r = p0.map(|v| (v * 255.0).round() as u8).unwrap_or(color[0]);
        let g = p1.map(|v| (v * 255.0).round() as u8).unwrap_or(color[1]);
        let b = p2.map(|v| (v * 255.0).round() as u8).unwrap_or(color[2]);
        (r, g, b)
    };

    // ── Brightness (port + slider) ─────────────────────────────
    let bri_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == PORT_BRIGHTNESS);
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, PORT_BRIGHTNESS, true, connections,
            port_positions, dragging_from, pending_disconnects, PortKind::Number);
        if bri_wired {
            let v = Graph::static_input_value(connections, values, node_id, PORT_BRIGHTNESS)
                .as_float().clamp(0.0, 1.0);
            *brightness = v;
            ui.label(egui::RichText::new(format!("Bri: {:.2}", v))
                .small().color(egui::Color32::from_rgb(80, 170, 255)));
        } else {
            ui.label(egui::RichText::new("Bri").small());
            ui.add(egui::Slider::new(brightness, 0.0..=1.0).show_value(false));
        }
    });

    // ── Position (Pointer) ─────────────────────────────────────
    let pos_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == PORT_POSITION);
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, PORT_POSITION, true, connections,
            port_positions, dragging_from, pending_disconnects, PortKind::Number);
        if pos_wired {
            let v = Graph::static_input_value(connections, values, node_id, PORT_POSITION).as_float().clamp(0.0, 1.0);
            *position = v;
            ui.label(egui::RichText::new(format!("Pos: {:.2}", v))
                .small().color(egui::Color32::from_rgb(80, 170, 255)));
        } else {
            ui.label(egui::RichText::new("Pos").small());
            ui.add(egui::Slider::new(position, 0.0..=1.0).show_value(false));
        }
    });

    // ── Speed (Pulse) ──────────────────────────────────────────
    let spd_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == PORT_SPEED);
    ui.horizontal(|ui| {
        crate::nodes::inline_port_circle(ui, node_id, PORT_SPEED, true, connections,
            port_positions, dragging_from, pending_disconnects, PortKind::Number);
        if spd_wired {
            let v = Graph::static_input_value(connections, values, node_id, PORT_SPEED).as_float().clamp(0.0, 1.0);
            *speed = v;
            ui.label(egui::RichText::new(format!("Spd: {:.2}", v))
                .small().color(egui::Color32::from_rgb(80, 170, 255)));
        } else {
            ui.label(egui::RichText::new("Spd").small());
            ui.add(egui::Slider::new(speed, 0.0..=1.0).show_value(false));
        }
    });

    // ── LED preview bar ────────────────────────────────────────
    let bar_w = ui.available_width().min(160.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, 6.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 1.0, egui::Color32::from_rgb(20, 20, 30));
    let bri = brightness.clamp(0.0, 1.0);
    let pc = egui::Color32::from_rgb(
        (eff_r as f32 * bri) as u8,
        (eff_g as f32 * bri) as u8,
        (eff_b as f32 * bri) as u8,
    );
    match *mode {
        MODE_SOLID | MODE_PULSE => { p.rect_filled(rect, 1.0, pc); }
        MODE_POINTER => {
            let x = rect.left() + rect.width() * position.clamp(0.0, 1.0);
            let dot = egui::Rect::from_center_size(egui::pos2(x, rect.center().y), egui::vec2(8.0, 6.0));
            p.rect_filled(dot, 1.0, pc);
        }
        _ => {}
    }

    // ── Send mode + Trigger port + Send button ─────────────────
    let trig_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == PORT_TRIGGER);
    let mut send_now = false;

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Send").small());
        egui::ComboBox::from_id_salt(egui::Id::new(("light_send_mode", node_id)))
            .selected_text(if *send_mode == SEND_MODE_TRIGGER { "On Trigger" } else { "Continuous" })
            .width(96.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(*send_mode == SEND_MODE_CONT,    "Continuous").clicked() { *send_mode = SEND_MODE_CONT; }
                if ui.selectable_label(*send_mode == SEND_MODE_TRIGGER, "On Trigger").clicked() { *send_mode = SEND_MODE_TRIGGER; }
            });

        crate::nodes::inline_port_circle(ui, node_id, PORT_TRIGGER, true, connections,
            port_positions, dragging_from, pending_disconnects, PortKind::Trigger);

        if ui.button("Send").clicked() {
            send_now = true;
        }
    });

    if trig_wired {
        let v = Graph::static_input_value(connections, values, node_id, PORT_TRIGGER).as_float();
        let key = egui::Id::new(("light_prev_trig", node_id));
        let prev: f32 = ui.ctx().data_mut(|d| d.get_temp(key)).unwrap_or(0.0);
        if v > 0.5 && prev <= 0.5 { send_now = true; }
        ui.ctx().data_mut(|d| d.insert_temp(key, v));
    }

    // ── Push to hub ────────────────────────────────────────────
    let active_param = match *mode {
        MODE_POINTER => *position,
        MODE_PULSE   => *speed,
        _            => 0.0,
    };
    let key = format!("{} {} {} {} {:.3} {:.3}",
        *mode, eff_r, eff_g, eff_b, *brightness, active_param);
    let kid = egui::Id::new(("light_cmd_key", node_id));
    let prev_key: Option<String> = ui.ctx().data_mut(|d| d.get_temp(kid));
    let key_changed = prev_key.as_deref() != Some(&key);

    let should_send = match *send_mode {
        SEND_MODE_CONT => key_changed || send_now,
        _              => send_now,
    };

    if should_send {
        ui.ctx().data_mut(|d| d.insert_temp(kid, key));
        if let Some(hub) = if hid != 0 { ob_manager.get_hub_mut(hid) } else { ob_manager.find_any_hub_mut() } {
            hub.send_command(&format!("/light/{}/color {} {} {}", did, eff_r, eff_g, eff_b));
            hub.send_command(&format!("/light/{}/effect {} {:.3} {:.3}", did, *mode, active_param, *brightness));
        }
    }
}
