use crate::graph::{NodeId, PortValue};
use std::collections::HashMap;
use eframe::egui;
use crate::nodes::ScrollAreaExt;

pub fn render(
    ui: &mut egui::Ui,
    name: &mut String,
    input_names: &mut Vec<String>,
    output_names: &mut Vec<String>,
    code: &mut String,
    last_values: &mut Vec<f32>,
    error: &mut String,
    continuous: &mut bool,
    trigger: &mut bool,
    _values: &HashMap<(NodeId, usize), PortValue>,
    node_id: NodeId,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
) {
    // Input port base: port 0 = Code (continuous) OR port 0 = Exec, port 1 = Code (trigger mode).
    // Inputs follow at `input_base`.
    let prev_continuous = *continuous;
    let input_base = |cont: bool| -> usize { if cont { 1 } else { 2 } };
    // ── Name ──
    ui.horizontal(|ui| {
        ui.label("Name:");
        ui.add(egui::TextEdit::singleline(name).desired_width(100.0));
    });

    // ── Mode toggle + Execute button ──
    ui.horizontal(|ui| {
        ui.checkbox(continuous, "Continuous");
        if !*continuous {
            if ui.button("Run").clicked() {
                *trigger = true;
            }
        }
    });

    // ── Inputs with + / - ──
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Inputs").small().strong());
        if ui.small_button("+").clicked() {
            let idx = input_names.len();
            input_names.push(format!("in{}", idx));
        }
    });
    let mut input_remove: Option<usize> = None;
    for i in 0..input_names.len() {
        ui.horizontal(|ui| {
            if ui.small_button("-").clicked() {
                input_remove = Some(i);
            }
            ui.add(egui::TextEdit::singleline(&mut input_names[i]).desired_width(80.0));
        });
    }
    if let Some(idx) = input_remove {
        // Disconnect the removed input AND all subsequent ones, since their
        // port indices would shift down after removal and silently bind to
        // the wrong slot.
        let base = input_base(*continuous);
        for i in idx..input_names.len() {
            pending_disconnects.push((node_id, base + i));
        }
        input_names.remove(idx);
    }

    // ── Outputs with + / - ──
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Outputs").small().strong());
        if ui.small_button("+").clicked() {
            let idx = output_names.len();
            output_names.push(format!("out{}", idx));
            last_values.push(0.0);
        }
    });
    let mut output_remove: Option<usize> = None;
    for i in 0..output_names.len() {
        ui.horizontal(|ui| {
            if ui.small_button("-").clicked() {
                output_remove = Some(i);
            }
            ui.add(egui::TextEdit::singleline(&mut output_names[i]).desired_width(80.0));
            // Show last computed value next to output name
            if let Some(v) = last_values.get(i) {
                ui.label(egui::RichText::new(format!("= {:.3}", v)).small().color(egui::Color32::from_rgb(120, 200, 120)));
            }
        });
    }
    if let Some(idx) = output_remove {
        // NOTE: Output-side stale wires (where from_port shifts down) are
        // cleaned up by the generic Script stale-connection pruning pass
        // in app/mod.rs, which prunes any connection with out-of-range
        // from_port for Script nodes.
        output_names.remove(idx);
        if idx < last_values.len() { last_values.remove(idx); }
    }

    // If the user toggles continuous mode, the input port base shifts
    // (adds/removes the Exec port). Disconnect the old input ports so
    // wires don't silently re-bind to Exec/Code after the shift.
    if *continuous != prev_continuous {
        let old_base = input_base(prev_continuous);
        for i in 0..input_names.len() {
            pending_disconnects.push((node_id, old_base + i));
        }
    }

    // ── Code ──
    // Check if code is coming from input port
    let _code_port_idx: usize = if *continuous { 0 } else { 1 };
    let _code_connected = _values.iter().any(|((nid, _), _)| *nid == node_id) ||
        false; // simplified check
    // Check connections for the Code port
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Code").small().strong());
        ui.label(egui::RichText::new("(or connect Text to Code port ↑)").small().color(egui::Color32::GRAY));
    });
    egui::ScrollArea::vertical()
        .id_salt(egui::Id::new(("script_code_scroll", node_id)))
        .max_height(200.0)
        .show_pannable(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(code)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
            );
        });

    // Open in external editor
    if ui.small_button(egui::RichText::new("Open in editor").small()).clicked() {
        let tmp = std::env::temp_dir().join(format!("patchwork_script_{}.rhai", node_id));
        if std::fs::write(&tmp, code.as_str()).is_ok() {
            #[cfg(target_os = "macos")]
            let _ = std::process::Command::new("open").arg(&tmp).spawn();
            #[cfg(target_os = "windows")]
            let _ = std::process::Command::new("cmd").args(["/C", "start", "", &tmp.display().to_string()]).spawn();
            #[cfg(target_os = "linux")]
            let _ = std::process::Command::new("xdg-open").arg(&tmp).spawn();
        }
    }

    // ── Error display ──
    if !error.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(255, 100, 100), &*error);
    }

    // ── Load / Save ──
    ui.separator();
    ui.horizontal(|ui| {
        if ui.small_button("Load...").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("json", &["json"]).pick_file() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(script) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(obj) = script.as_object() {
                            if let Some(n) = obj.get("name").and_then(|v| v.as_str()) { *name = n.to_string(); }
                            if let Some(ins) = obj.get("inputs").and_then(|v| v.as_array()) {
                                *input_names = ins.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                            }
                            if let Some(outs) = obj.get("outputs").and_then(|v| v.as_array()) {
                                *output_names = outs.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                            }
                            if let Some(c) = obj.get("code").and_then(|v| v.as_str()) { *code = c.to_string(); }
                        }
                    }
                }
            }
        }
        if ui.small_button("Save...").clicked() {
            let script_obj = serde_json::json!({
                "name": name,
                "inputs": input_names,
                "outputs": output_names,
                "code": code,
            });
            if let Some(path) = rfd::FileDialog::new().add_filter("json", &["json"]).save_file() {
                let _ = std::fs::write(&path, serde_json::to_string_pretty(&script_obj).unwrap_or_default());
            }
        }
    });
}
