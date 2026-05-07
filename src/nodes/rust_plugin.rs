use crate::graph::*;
use eframe::egui;
use crate::nodes::ScrollAreaExt;
use std::collections::HashMap;
use std::path::PathBuf;


/// Build status from background compilation thread
#[derive(Clone, Debug)]
pub enum BuildStatus {
    Idle,
    Building,
    Success(PathBuf), // path to .dylib
    Error(String),
}

/// Render the Rust Plugin node UI
pub fn render(
    ui: &mut egui::Ui,
    node_id: NodeId,
    node_type: &mut NodeType,
    _values: &HashMap<(NodeId, usize), PortValue>,
    _connections: &[Connection],
) {
    let (input_names, output_names, code, error, last_values) = match node_type {
        NodeType::RustPlugin { input_names, output_names, code, error, last_values, .. } =>
            (input_names, output_names, code, error, last_values),
        _ => return,
    };

    // ── Input/Output name editors ──
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Inputs").small().strong());
        if ui.small_button("+").clicked() {
            input_names.push(format!("in{}", input_names.len()));
        }
    });
    let mut rm = None;
    for i in 0..input_names.len() {
        ui.horizontal(|ui| {
            if ui.small_button("-").clicked() { rm = Some(i); }
            ui.add(egui::TextEdit::singleline(&mut input_names[i]).desired_width(80.0));
        });
    }
    if let Some(i) = rm { input_names.remove(i); }

    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Outputs").small().strong());
        if ui.small_button("+").clicked() {
            output_names.push(format!("out{}", output_names.len()));
            last_values.push(0.0);
        }
    });
    let mut rm = None;
    for i in 0..output_names.len() {
        ui.horizontal(|ui| {
            if ui.small_button("-").clicked() { rm = Some(i); }
            ui.add(egui::TextEdit::singleline(&mut output_names[i]).desired_width(80.0));
            if let Some(v) = last_values.get(i) {
                ui.label(egui::RichText::new(format!("= {:.3}", v)).small().color(egui::Color32::from_rgb(120, 200, 120)));
            }
        });
    }
    if let Some(i) = rm {
        output_names.remove(i);
        if i < last_values.len() { last_values.remove(i); }
    }

    // ── Code editor ──
    ui.separator();
    ui.label(egui::RichText::new("Rust Code").small().strong());
    ui.label(egui::RichText::new("Write a process() function body. inputs[] → outputs[]").small().color(egui::Color32::GRAY));

    // Show template hint
    if code.is_empty() {
        *code = "// inputs: &[f64] — values from input ports\n// Return: Vec<f64> — values for output ports\n\nvec![inputs.get(0).copied().unwrap_or(0.0) * 2.0]".to_string();
    }

    egui::ScrollArea::vertical()
        .id_salt(egui::Id::new(("rust_code_scroll", node_id)))
        .max_height(200.0)
        .show_pannable(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(code)
                    .desired_rows(6)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
            );
        });

    // Open in external editor
    if ui.small_button(egui::RichText::new("Open in editor").small()).clicked() {
        let tmp = std::env::temp_dir().join(format!("patchwork_rust_{}.rs", node_id));
        if std::fs::write(&tmp, code.as_str()).is_ok() {
            #[cfg(target_os = "macos")]
            let _ = std::process::Command::new("open").arg(&tmp).spawn();
            #[cfg(target_os = "windows")]
            let _ = std::process::Command::new("cmd").args(["/C", "start", "", &tmp.display().to_string()]).spawn();
            #[cfg(target_os = "linux")]
            let _ = std::process::Command::new("xdg-open").arg(&tmp).spawn();
        }
    }

    // ── Build button ──
    ui.separator();
    let build_status_id = egui::Id::new(("rust_build_status", node_id));
    let status: BuildStatus = ui.ctx().data_mut(|d| d.get_temp::<BuildStatus>(build_status_id).unwrap_or(BuildStatus::Idle));

    ui.horizontal(|ui| {
        let building = matches!(status, BuildStatus::Building);
        if ui.add_enabled(!building, egui::Button::new("🔨 Build")).clicked() {
            // Start build in background
            let code_clone = code.clone();
            let input_names_clone = input_names.clone();
            let output_names_clone = output_names.clone();
            let node_id_copy = node_id;
            let ctx = ui.ctx().clone();

            ui.ctx().data_mut(|d| d.insert_temp(build_status_id, BuildStatus::Building));

            std::thread::spawn(move || {
                let result = build_rust_plugin(&code_clone, &input_names_clone, &output_names_clone, node_id_copy);
                ctx.data_mut(|d| {
                    match result {
                        Ok(dylib_path) => d.insert_temp(build_status_id, BuildStatus::Success(dylib_path)),
                        Err(e) => d.insert_temp(build_status_id, BuildStatus::Error(e)),
                    }
                });
                ctx.request_repaint();
            });
        }

        match &status {
            BuildStatus::Idle => { ui.label(egui::RichText::new("Not built").small().color(egui::Color32::GRAY)); }
            BuildStatus::Building => { ui.spinner(); ui.label("Compiling..."); }
            BuildStatus::Success(_) => { ui.colored_label(egui::Color32::from_rgb(80, 200, 80), "✅ Built"); }
            BuildStatus::Error(_) => { ui.colored_label(egui::Color32::from_rgb(255, 100, 100), "❌ Failed"); }
        }
    });

    // Show build error
    if let BuildStatus::Error(e) = &status {
        egui::ScrollArea::vertical().max_height(80.0).show_pannable(ui, |ui| {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100),
                egui::RichText::new(e).small().monospace());
        });
    }

    // Show error from runtime
    if !error.is_empty() {
        ui.colored_label(egui::Color32::from_rgb(255, 180, 80), egui::RichText::new(&*error).small());
    }

    // ── Load/Save ──
    ui.separator();
    ui.horizontal(|ui| {
        if ui.small_button("Load...").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("Rust", &["rs"]).pick_file() {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    *code = content;
                }
            }
        }
        if ui.small_button("Save...").clicked() {
            if let Some(path) = rfd::FileDialog::new().add_filter("Rust", &["rs"]).save_file() {
                let full = generate_full_source(code, input_names, output_names);
                let _ = std::fs::write(&path, full);
            }
        }
    });
}

/// Generate the full Rust library source for compilation
fn generate_full_source(user_code: &str, _input_names: &[String], _output_names: &[String]) -> String {
    format!(r#"
#[no_mangle]
pub extern "C" fn process(inputs_ptr: *const f64, inputs_len: usize, outputs_ptr: *mut f64, outputs_len: usize) {{
    let inputs: &[f64] = unsafe {{ std::slice::from_raw_parts(inputs_ptr, inputs_len) }};
    let result: Vec<f64> = (|| {{
        {user_code}
    }})();
    let outputs: &mut [f64] = unsafe {{ std::slice::from_raw_parts_mut(outputs_ptr, outputs_len) }};
    for (i, v) in result.iter().enumerate() {{
        if i < outputs.len() {{
            outputs[i] = *v;
        }}
    }}
}}
"#, user_code = user_code)
}

/// Build the plugin as a .dylib in a temp directory
fn build_rust_plugin(code: &str, input_names: &[String], output_names: &[String], node_id: NodeId) -> Result<PathBuf, String> {
    let plugin_dir = std::env::temp_dir().join(format!("patchwork_plugin_{}", node_id));
    let src_dir = plugin_dir.join("src");
    std::fs::create_dir_all(&src_dir).map_err(|e| format!("mkdir: {}", e))?;

    // Write Cargo.toml
    let cargo_toml = r#"[package]
name = "patchwork_plugin"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]
"#;
    std::fs::write(plugin_dir.join("Cargo.toml"), cargo_toml).map_err(|e| format!("write Cargo.toml: {}", e))?;

    // Write lib.rs
    let source = generate_full_source(code, input_names, output_names);
    std::fs::write(src_dir.join("lib.rs"), &source).map_err(|e| format!("write lib.rs: {}", e))?;

    // Run cargo build
    let output = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&plugin_dir)
        .output()
        .map_err(|e| format!("cargo not found: {}. Is Rust installed?", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Extract just the error lines, not the full cargo output
        let errors: Vec<&str> = stderr.lines()
            .filter(|l| l.contains("error") || l.contains("warning") || l.starts_with("  "))
            .take(20)
            .collect();
        return Err(errors.join("\n"));
    }

    // Find the built .dylib
    let target_dir = plugin_dir.join("target").join("release");
    let dylib_name = if cfg!(target_os = "macos") {
        "libpatchwork_plugin.dylib"
    } else if cfg!(target_os = "linux") {
        "libpatchwork_plugin.so"
    } else {
        "patchwork_plugin.dll"
    };
    let dylib_path = target_dir.join(dylib_name);
    if !dylib_path.exists() {
        return Err(format!("Build succeeded but {} not found", dylib_name));
    }

    // Copy to a stable location
    let dest = plugin_dir.join(dylib_name);
    std::fs::copy(&dylib_path, &dest).map_err(|e| format!("copy: {}", e))?;

    Ok(dest)
}

/// Called during evaluation to run the plugin
pub fn evaluate(
    node_id: NodeId,
    node_type: &mut NodeType,
    inputs: &[PortValue],
    ctx_data: &egui::Context,
) -> Vec<(usize, PortValue)> {
    let (output_names, last_values, error) = match node_type {
        NodeType::RustPlugin { output_names, last_values, error, .. } =>
            (output_names, last_values, error),
        _ => return vec![],
    };

    let build_status_id = egui::Id::new(("rust_build_status", node_id));
    let status: BuildStatus = ctx_data.data_mut(|d| d.get_temp::<BuildStatus>(build_status_id).unwrap_or(BuildStatus::Idle));

    let dylib_path = match &status {
        BuildStatus::Success(p) => p.clone(),
        _ => {
            // Output last known values
            return last_values.iter().enumerate()
                .map(|(i, v)| (i, PortValue::Float(*v as f32)))
                .collect();
        }
    };

    // Load and call the plugin
    let _lib_id = egui::Id::new(("rust_plugin_lib", node_id));

    // Collect input values as f64
    let input_f64: Vec<f64> = inputs.iter().map(|v| v.as_float() as f64).collect();
    let mut output_f64 = vec![0.0f64; output_names.len()];

    // Use libloading to call the process function
    unsafe {
        match libloading::Library::new(&dylib_path) {
            Ok(lib) => {
                match lib.get::<unsafe extern "C" fn(*const f64, usize, *mut f64, usize)>(b"process") {
                    Ok(process_fn) => {
                        process_fn(
                            input_f64.as_ptr(),
                            input_f64.len(),
                            output_f64.as_mut_ptr(),
                            output_f64.len(),
                        );
                        error.clear();
                    }
                    Err(e) => {
                        *error = format!("Symbol not found: {}", e);
                    }
                }
            }
            Err(e) => {
                *error = format!("Load failed: {}", e);
            }
        }
    }

    // Update last_values
    last_values.resize(output_names.len(), 0.0);
    for (i, v) in output_f64.iter().enumerate() {
        if i < last_values.len() {
            last_values[i] = *v;
        }
    }

    output_f64.iter().enumerate()
        .map(|(i, v)| (i, PortValue::Float(*v as f32)))
        .collect()
}
