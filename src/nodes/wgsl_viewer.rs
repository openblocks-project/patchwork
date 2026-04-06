use crate::graph::*;
use eframe::egui;
use eframe::egui_wgpu;
use eframe::egui_wgpu::wgpu;
use std::collections::{HashMap, HashSet};

// ── Constants ────────────────────────────────────────────────────────────────

const MAX_UNIFORM_FLOATS: usize = 64;
const UNIFORM_BUF_SIZE: u64 = (MAX_UNIFORM_FLOATS * 4) as u64; // 256 bytes

/// Request to auto-create a source node and connect it to a WGSL uniform port.
#[derive(Clone, Debug)]
pub struct WgslSpawnRequest {
    pub source_type: String,  // "time", "slider", "color"
    pub target_node: crate::graph::NodeId,
    pub target_port: usize,
    pub uniform_name: String,
}

/// Determine what source node to create and with what defaults based on uniform name.
fn uniform_spawn_request(name: &str, target_node: crate::graph::NodeId, target_port: usize) -> WgslSpawnRequest {
    let lower = name.to_lowercase();
    let source_type = if lower == "time" || lower == "t" { "time" } else { "slider" };
    WgslSpawnRequest {
        source_type: source_type.to_string(),
        target_node,
        target_port,
        uniform_name: name.to_string(),
    }
}

/// Get smart slider defaults (min, max, step, initial value) based on uniform name.
pub fn slider_defaults_for_uniform(name: &str) -> (f32, f32, f32, f32) {
    let lower = name.to_lowercase();
    if lower.contains("count") || lower.contains("num") || lower == "n" || lower == "i" {
        (0.0, 20.0, 1.0, 5.0)       // integers
    } else if lower.contains("speed") || lower.contains("rate") {
        (0.0, 10.0, 0.1, 1.0)
    } else if lower.contains("size") || lower.contains("scale") || lower.contains("radius") {
        (0.0, 2.0, 0.01, 0.5)
    } else if lower.contains("freq") {
        (20.0, 2000.0, 1.0, 440.0)
    } else if lower.contains("angle") || lower.contains("rotation") || lower.contains("rot") {
        (0.0, 6.2832, 0.01, 0.0)    // 0–2π
    } else if lower.contains("mix") || lower.contains("blend") || lower.contains("amount") || lower.contains("alpha") {
        (0.0, 1.0, 0.01, 0.5)
    } else {
        (0.0, 1.0, 0.01, 0.5)       // safe default
    }
}

// time is NOT a builtin — it's a normal uniform port that users connect a Time node to.
// This lets them control speed, sync with audio, or use Timer phase.
const BUILTINS: &[&str] = &[
    "resolution", "mouse",
    "resolution_x", "resolution_y", "mouse_x", "mouse_y",
    "_p0", "_p1", "_p2",
];

/// Built-in vertex shader for fragment-only shaders (fullscreen triangle)
const BUILTIN_VS: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>(x * 0.5 + 0.5, 1.0 - (y * 0.5 + 0.5));
    return out;
}
"#;

// ── Uniform Detection ────────────────────────────────────────────────────────

/// Scan shader code for `u.xxx` references, return (names, types).
/// Filters out built-ins. Detects _r/_g/_b color groups.
fn detect_uniforms(code: &str) -> (Vec<String>, Vec<String>) {
    let re = regex::Regex::new(r"u\.([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    let builtin_set: HashSet<&str> = BUILTINS.iter().copied().collect();

    // Collect all unique user uniform references in order
    let mut seen = HashSet::new();
    let mut all_refs: Vec<String> = Vec::new();
    for cap in re.captures_iter(code) {
        let name = cap[1].to_string();
        if !builtin_set.contains(name.as_str()) && seen.insert(name.clone()) {
            all_refs.push(name);
        }
    }

    // Detect color groups: if xxx_r, xxx_g, xxx_b all present
    let ref_set: HashSet<&str> = all_refs.iter().map(|s| s.as_str()).collect();
    let mut color_bases: HashSet<String> = HashSet::new();
    for name in &all_refs {
        if let Some(base) = name.strip_suffix("_r") {
            let g = format!("{}_g", base);
            let b = format!("{}_b", base);
            if ref_set.contains(g.as_str()) && ref_set.contains(b.as_str()) {
                color_bases.insert(base.to_string());
            }
        }
    }

    let mut names = Vec::new();
    let mut types = Vec::new();
    let mut skip_set: HashSet<String> = HashSet::new();

    // Add color groups first (from their _r position in order)
    for name in &all_refs {
        if let Some(base) = name.strip_suffix("_r") {
            if color_bases.contains(base) && !skip_set.contains(base) {
                names.push(base.to_string());
                types.push("color".to_string());
                skip_set.insert(base.to_string());
                skip_set.insert(format!("{}_r", base));
                skip_set.insert(format!("{}_g", base));
                skip_set.insert(format!("{}_b", base));
            }
        }
    }

    // Add remaining as floats
    for name in &all_refs {
        if !skip_set.contains(name) {
            names.push(name.clone());
            types.push("float".to_string());
        }
    }

    (names, types)
}

/// Build the WGSL uniform struct declaration from detected uniforms.
fn build_uniform_block(names: &[String], types: &[String]) -> String {
    let mut s = String::from("struct Uniforms {\n");
    s += "    time: f32,\n";
    s += "    resolution: vec2<f32>,\n";
    s += "    mouse: vec2<f32>,\n";
    s += "    _p0: f32,\n";
    // user uniforms (skip "time" — it's already in the header)
    for (i, name) in names.iter().enumerate() {
        if name == "time" { continue; }
        let t = types.get(i).map(|s| s.as_str()).unwrap_or("float");
        match t {
            "color" => {
                s += &format!("    {}_r: f32,\n", name);
                s += &format!("    {}_g: f32,\n", name);
                s += &format!("    {}_b: f32,\n", name);
            }
            _ => {
                s += &format!("    {}: f32,\n", name);
            }
        }
    }
    s += "};\n";
    s += "@group(0) @binding(0) var<uniform> u: Uniforms;\n\n";
    s
}

/// Pack uniform values into f32 array for GPU buffer.
fn pack_uniforms(
    time: f32,
    res_x: f32,
    res_y: f32,
    mouse_x: f32,
    mouse_y: f32,
    user_values: &[f32],
) -> Vec<f32> {
    let mut buf = vec![0.0f32; MAX_UNIFORM_FLOATS];
    buf[0] = time;
    buf[1] = res_x;
    buf[2] = res_y;
    // vec2 alignment: resolution takes indices 1-2, then mouse at 3-4
    // But we're using the struct layout: time(0), resolution(1,2), mouse(3,4), _p0(5)
    // Wait — with vec2 in struct, WGSL alignment puts resolution at offset 8 (2 floats).
    // Let's use a cleaner layout: all f32 individually.
    // Actually, with the struct as declared above:
    //   time: f32          -> offset 0  (4 bytes)
    //   resolution: vec2   -> offset 8  (aligned to 8 bytes = 2 floats)
    //   mouse: vec2        -> offset 16 (aligned to 8 bytes = 4 floats)
    //   _p0: f32           -> offset 24 (6 floats)
    //   user[0]            -> offset 28 (7 floats)
    //
    // So the Rust buffer must match this layout:
    buf[0] = time;
    buf[1] = 0.0; // padding for vec2 alignment
    buf[2] = res_x;
    buf[3] = res_y;
    buf[4] = mouse_x;
    buf[5] = mouse_y;
    buf[6] = 0.0; // _p0
    buf[7] = 0.0; // padding to align next field

    // User uniforms start at index 8
    // But wait — if user uniforms are all f32, they pack tightly after _p0.
    // With _p0 at offset 24, next f32 is at offset 28 = index 7.
    // Actually let me reconsider the struct alignment properly.
    //
    // WGSL struct layout rules:
    // - f32: align 4, size 4
    // - vec2<f32>: align 8, size 8
    // - vec3<f32>: align 16, size 12
    //
    // struct Uniforms {
    //   time: f32,           // offset 0, size 4
    //   // padding 4 bytes to align vec2
    //   resolution: vec2,    // offset 8, size 8
    //   mouse: vec2,         // offset 16, size 8
    //   _p0: f32,            // offset 24, size 4
    //   user0: f32,          // offset 28, size 4
    //   user1: f32,          // offset 32, size 4
    //   ...
    // }
    //
    // Total header in f32 indices: [0]=time, [1]=pad, [2..3]=resolution, [4..5]=mouse, [6]=_p0, [7]=user0...
    // That gives us user uniforms starting at index 7. Let's use that.

    let user_start = 7;
    for (i, v) in user_values.iter().enumerate() {
        if user_start + i < MAX_UNIFORM_FLOATS {
            buf[user_start + i] = *v;
        }
    }
    buf
}

// ── Callback Resources (shared via egui_wgpu) ───────────────────────────────

struct WgslNodeGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    vertex_count: u32,
    shader_hash: u64,
}

/// Per-node GPU resources, keyed by NodeId
struct WgslGpuStore {
    nodes: HashMap<NodeId, WgslNodeGpu>,
}

/// Per-node uniform data for the current frame
struct WgslUniformStore {
    data: HashMap<NodeId, Vec<f32>>,
}

/// Per-node callback identity (carries node_id into paint)
struct WgslPaintCallback {
    node_id: NodeId,
}

impl egui_wgpu::CallbackTrait for WgslPaintCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let uni_store = resources.get::<WgslUniformStore>();
        let gpu_store = resources.get::<WgslGpuStore>();
        if let (Some(unis), Some(gpus)) = (uni_store, gpu_store) {
            if let (Some(data), Some(node_gpu)) = (unis.data.get(&self.node_id), gpus.nodes.get(&self.node_id)) {
                let bytes: &[u8] = bytemuck::cast_slice(data);
                queue.write_buffer(&node_gpu.uniform_buffer, 0, bytes);
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(store) = resources.get::<WgslGpuStore>() {
            if let Some(gpu) = store.nodes.get(&self.node_id) {
                render_pass.set_pipeline(&gpu.pipeline);
                render_pass.set_bind_group(0, &gpu.bind_group, &[]);
                render_pass.draw(0..gpu.vertex_count, 0..1);
            }
        }
    }
}

/// Public callback for rendering a WGSL shader as canvas background.
/// Reuses the same GPU pipeline/uniforms as the node's preview.
pub struct WgslBgCallback {
    pub node_id: NodeId,
}

impl egui_wgpu::CallbackTrait for WgslBgCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Uniforms already uploaded by the node's own WgslPaintCallback::prepare.
        // But if the BG renders before the node, we upload here too.
        let uni_store = resources.get::<WgslUniformStore>();
        let gpu_store = resources.get::<WgslGpuStore>();
        if let (Some(unis), Some(gpus)) = (uni_store, gpu_store) {
            if let (Some(data), Some(node_gpu)) = (unis.data.get(&self.node_id), gpus.nodes.get(&self.node_id)) {
                let bytes: &[u8] = bytemuck::cast_slice(data);
                queue.write_buffer(&node_gpu.uniform_buffer, 0, bytes);
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(store) = resources.get::<WgslGpuStore>() {
            if let Some(gpu) = store.nodes.get(&self.node_id) {
                render_pass.set_pipeline(&gpu.pipeline);
                render_pass.set_bind_group(0, &gpu.bind_group, &[]);
                render_pass.draw(0..gpu.vertex_count, 0..1);
            }
        }
    }
}

// ── Main Render Function ────────────────────────────────────────────────────

pub fn render(
    ui: &mut egui::Ui,
    wgsl_code: &mut String,
    uniform_names: &mut Vec<String>,
    uniform_types: &mut Vec<String>,
    uniform_values: &mut Vec<f32>,
    canvas_w: &mut f32,
    canvas_h: &mut f32,
    node_id: NodeId,
    values: &HashMap<(NodeId, usize), PortValue>,
    connections: &[Connection],
    wgpu_render_state: &Option<egui_wgpu::RenderState>,
    pending_disconnects: &mut Vec<(NodeId, usize)>,
    port_positions: &mut HashMap<(NodeId, usize, bool), egui::Pos2>,
    dragging_from: &mut Option<(NodeId, usize, bool)>,
) {
    // ── Port 0: WGSL code input (inline) ────────────────────────────
    {
        let is_wired = connections.iter().any(|c| c.to_node == node_id && c.to_port == 0);
        ui.horizontal(|ui| {
            super::inline_port_circle(ui, node_id, 0, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Text);
            ui.label(egui::RichText::new("WGSL").small());
            if is_wired {
                ui.label(egui::RichText::new("⟵ connected").small().color(egui::Color32::from_rgb(80, 170, 255)));
            } else {
                ui.colored_label(egui::Color32::GRAY, "—");
            }
        });
    }

    // Read shader code from input port 0
    let input_code = Graph::static_input_value(connections, values, node_id, 0);
    let code = match &input_code {
        PortValue::Text(s) => s.clone(),
        _ => String::new(),
    };
    *wgsl_code = code.clone();

    if code.is_empty() {
        ui.colored_label(egui::Color32::GRAY, "Connect WGSL code to render");
        return;
    }

    // Auto-detect uniforms from shader code
    let (detected_names, detected_types) = detect_uniforms(&code);

    // Diff old vs new uniform layout to detect ports whose semantic changed.
    // Port layout: port 0 = WGSL code, ports 1+ = uniform components
    // (float uniforms = 1 port, color uniforms = 3 ports for r/g/b).
    // If a port's logical name changed (removed or shifted), disconnect it
    // so stale wires don't silently drive the wrong uniform.
    let build_port_labels = |names: &[String], types: &[String]| -> Vec<String> {
        let mut labels = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let t = types.get(i).map(|s| s.as_str()).unwrap_or("float");
            if t == "color" {
                labels.push(format!("{}.r", name));
                labels.push(format!("{}.g", name));
                labels.push(format!("{}.b", name));
            } else {
                labels.push(name.clone());
            }
        }
        labels
    };
    let old_labels = build_port_labels(uniform_names, uniform_types);
    let new_labels = build_port_labels(&detected_names, &detected_types);
    if old_labels != new_labels {
        let max_len = old_labels.len().max(new_labels.len());
        for i in 0..max_len {
            let old_l = old_labels.get(i);
            let new_l = new_labels.get(i);
            if old_l != new_l {
                // Uniform port indices start at 1 (port 0 = WGSL code).
                pending_disconnects.push((node_id, i + 1));
            }
        }
    }

    *uniform_names = detected_names;
    *uniform_types = detected_types;

    // Ensure uniform_values matches detected uniforms size
    let total_floats: usize = uniform_names.iter().enumerate().map(|(i, _)| {
        let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
        if t == "color" { 3 } else { 1 }
    }).sum();
    uniform_values.resize(total_floats, 0.0);

    // Read user uniform values: prefer connected port, else use inline uniform_values
    let mut user_values: Vec<f32> = Vec::new();
    let mut port_idx = 1usize;
    let mut value_idx = 0usize;
    for (i, _name) in uniform_names.iter().enumerate() {
        let t = uniform_types.get(i).map(|s| s.as_str()).unwrap_or("float");
        if t == "color" {
            for c in 0..3 {
                let port_connected = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port_idx);
                if port_connected {
                    let v = Graph::static_input_value(connections, values, node_id, port_idx);
                    let fv = v.as_float() / 255.0;
                    user_values.push(fv);
                    if value_idx + c < uniform_values.len() { uniform_values[value_idx + c] = fv; }
                } else {
                    let fv = if value_idx + c < uniform_values.len() { uniform_values[value_idx + c] } else { 0.0 };
                    user_values.push(fv);
                }
                port_idx += 1;
            }
            value_idx += 3;
        } else {
            let port_connected = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port_idx);
            if port_connected {
                let v = Graph::static_input_value(connections, values, node_id, port_idx);
                let fv = v.as_float();
                user_values.push(fv);
                if value_idx < uniform_values.len() { uniform_values[value_idx] = fv; }
            } else {
                let fv = if value_idx < uniform_values.len() { uniform_values[value_idx] } else { 0.0 };
                user_values.push(fv);
            }
            port_idx += 1;
            value_idx += 1;
        }
    }

    // Detect vertex shader
    let has_vs = code.contains("@vertex");
    let has_uniforms_decl = code.contains("var<uniform>");

    // Build final shader
    let uniform_block = if has_uniforms_decl {
        String::new()
    } else {
        build_uniform_block(uniform_names, uniform_types)
    };

    let final_code = if has_vs {
        format!("{}{}", uniform_block, code)
    } else {
        format!("{}{}\n{}", uniform_block, BUILTIN_VS, code)
    };

    let vertex_count = if has_vs && (code.contains(", 6>") || code.contains(",6>")) {
        6u32
    } else {
        3
    };

    // Canvas size controls
    ui.horizontal(|ui| {
        ui.label("Size:");
        ui.add(egui::DragValue::new(canvas_w).speed(1.0).range(100.0..=1920.0).prefix("W:"));
        ui.add(egui::DragValue::new(canvas_h).speed(1.0).range(100.0..=1080.0).prefix("H:"));
    });

    // Pop-out button
    let popout_id = egui::Id::new(("wgsl_popout", node_id));
    let mut popout_open = ui.ctx().data_mut(|d| d.get_temp::<bool>(popout_id).unwrap_or(false));
    if ui.button(if popout_open { "Close Window" } else { "Pop Out" }).clicked() {
        popout_open = !popout_open;
        ui.ctx().data_mut(|d| d.insert_temp(popout_id, popout_open));
    }

    // ── Uniform ports (inline — each with connector + DragValue) ────
    if !uniform_names.is_empty() {
        ui.separator();
        ui.label(egui::RichText::new("Uniforms").small().strong());
        let mut vi = 0usize;
        let mut pi = 1usize;
        let names_clone = uniform_names.clone();
        let types_clone = uniform_types.clone();
        let ch_labels = ["R", "G", "B"];

        for (i, name) in names_clone.iter().enumerate() {
            let t = types_clone.get(i).map(|s| s.as_str()).unwrap_or("float");
            if t == "color" {
                // Color group: label + optional "+" button
                let any_wired = (0..3).any(|c| connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == pi + c));
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("u.{}", name)).small());
                    if !any_wired {
                        if ui.small_button("+").on_hover_text("Create Color node").clicked() {
                            let spawn = WgslSpawnRequest {
                                source_type: "color".to_string(),
                                target_node: node_id,
                                target_port: pi,
                                uniform_name: name.clone(),
                            };
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("wgsl_spawn_request"), spawn));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    for c in 0..3 {
                        let port = pi;
                        let is_wired = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port);
                        super::inline_port_circle(ui, node_id, port, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Color);
                        ui.label(egui::RichText::new(ch_labels[c]).small());
                        if is_wired {
                            let v = if vi + c < uniform_values.len() { uniform_values[vi + c] } else { 0.0 };
                            ui.label(egui::RichText::new(format!("{:.2}", v)).small().monospace());
                        } else if vi + c < uniform_values.len() {
                            ui.add(egui::DragValue::new(&mut uniform_values[vi + c]).speed(0.01).range(0.0..=1.0));
                        }
                        pi += 1;
                    }
                });
                vi += 3;
            } else {
                // Float: ● label [DragValue] [+]
                let port = pi;
                let is_wired = connections.iter().any(|cn| cn.to_node == node_id && cn.to_port == port);
                ui.horizontal(|ui| {
                    super::inline_port_circle(ui, node_id, port, true, connections, port_positions, dragging_from, pending_disconnects, PortKind::Number);
                    ui.label(egui::RichText::new(format!("u.{}", name)).small());
                    if is_wired {
                        let v = if vi < uniform_values.len() { uniform_values[vi] } else { 0.0 };
                        ui.label(egui::RichText::new(format!("{:.3}", v)).strong().monospace());
                        ui.label(egui::RichText::new("⟵").small().color(egui::Color32::from_rgb(80, 170, 255)));
                    } else {
                        if vi < uniform_values.len() {
                            ui.add(egui::DragValue::new(&mut uniform_values[vi]).speed(0.01));
                        }
                        // "+" button: auto-create the right source node
                        if ui.small_button("+").on_hover_text("Create source node").clicked() {
                            let spawn = uniform_spawn_request(name, node_id, port);
                            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("wgsl_spawn_request"), spawn));
                        }
                    }
                });
                pi += 1;
                vi += 1;
            }
        }
    }

    // Validate with naga
    match validate_wgsl_naga(&final_code) {
        Err(e) => {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("Error: {}", e));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(*canvas_w, *canvas_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(40, 15, 15));
            // Log to system console (deduplicated — only when error text changes)
            let err_id = egui::Id::new(("wgsl_last_err", node_id));
            let prev: Option<String> = ui.ctx().data_mut(|d| d.get_temp(err_id));
            if prev.as_deref() != Some(&e) {
                crate::system_log::error(format!("WGSL (id:{}): {}", node_id, e));
                ui.ctx().data_mut(|d| d.insert_temp(err_id, e));
            }
        }
        Ok(()) => {
            if let Some(render_state) = wgpu_render_state {
                // Use connected Time node value if "time" uniform port is wired,
                // otherwise fall back to egui's internal clock.
                let time = {
                    let time_uniform_idx = uniform_names.iter().position(|n| n == "time");
                    if let Some(idx) = time_uniform_idx {
                        // Check if this uniform's port is connected (port 0 = WGSL code, so time port = 1 + idx)
                        let time_port = 1 + idx;
                        let connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == time_port);
                        if connected {
                            user_values.get(idx).copied().unwrap_or(0.0)
                        } else {
                            ui.ctx().input(|i| i.time) as f32
                        }
                    } else {
                        ui.ctx().input(|i| i.time) as f32
                    }
                };
                // Filter "time" from user_values for GPU buffer — it's in the header at buf[0]
                let gpu_values: Vec<f32> = uniform_names.iter().zip(user_values.iter())
                    .filter(|(name, _)| name.as_str() != "time")
                    .map(|(_, &v)| v)
                    .collect();
                let packed = pack_uniforms(
                    time, *canvas_w, *canvas_h,
                    0.0, 0.0,
                    &gpu_values,
                );

                render_with_wgpu(
                    ui, &final_code, render_state, vertex_count,
                    node_id, packed, *canvas_w, *canvas_h,
                );

                // Pop-out window rendering
                if popout_open {
                    let packed2 = pack_uniforms(time, *canvas_w, *canvas_h, 0.0, 0.0, &gpu_values);
                    render_popout_window(ui.ctx(), &final_code, render_state, vertex_count, node_id, packed2, canvas_w, canvas_h);
                }
            } else {
                ui.colored_label(egui::Color32::YELLOW, "No WGPU render state");
            }
        }
    }

    // Image output port (right-aligned)
    super::audio_port_row(ui, "Image", node_id, 0, false, port_positions, dragging_from, connections, pending_disconnects, PortKind::Image);

    // GPU readback: render to offscreen texture and produce PortValue::Image
    // Only when the Image output port is connected (avoids cost otherwise)
    let output_connected = connections.iter().any(|c| c.from_node == node_id && c.from_port == 0);
    if output_connected && !final_code.is_empty() {
        if let Some(render_state) = wgpu_render_state {
            let readback_w = (*canvas_w as u32).max(1).min(800);
            let readback_h = (*canvas_h as u32).max(1).min(600);
            // Reuse same time logic as main render — connected port or internal clock
            let rb_time = {
                let time_idx = uniform_names.iter().position(|n| n == "time");
                if let Some(idx) = time_idx {
                    let time_port = 1 + idx;
                    let connected = connections.iter().any(|c| c.to_node == node_id && c.to_port == time_port);
                    if connected { user_values.get(idx).copied().unwrap_or(0.0) }
                    else { ui.ctx().input(|i| i.time) as f32 }
                } else { ui.ctx().input(|i| i.time) as f32 }
            };
            let rb_gpu_values: Vec<f32> = uniform_names.iter().zip(user_values.iter())
                .filter(|(name, _)| name.as_str() != "time")
                .map(|(_, &v)| v).collect();
            let packed = pack_uniforms(rb_time, readback_w as f32, readback_h as f32, 0.0, 0.0, &rb_gpu_values);
            if let Some(img) = render_offscreen(render_state, &final_code, node_id, packed, readback_w, readback_h) {
                ui.ctx().data_mut(|d| {
                    d.insert_temp(egui::Id::new(("wgsl_image_output", node_id)), img);
                });
            }
        }
    }

    // Show code preview (collapsible)
    ui.collapsing("Shader Code", |ui| {
        let preview = if final_code.len() > 600 {
            format!("{}...", &final_code[..600])
        } else {
            final_code.clone()
        };
        let mut p = preview;
        ui.code_editor(&mut p);
    });
}

/// Render the shader to an offscreen RGBA8 texture and read back to CPU.
/// Returns None if the shader fails to compile or render.
fn render_offscreen(
    render_state: &egui_wgpu::RenderState,
    shader_code: &str,
    node_id: NodeId,
    packed_uniforms: Vec<f32>,
    width: u32,
    height: u32,
) -> Option<std::sync::Arc<ImageData>> {
    let device = &render_state.device;
    let queue = &render_state.queue;
    let readback_format = wgpu::TextureFormat::Rgba8UnormSrgb;

    // Cache key for the offscreen pipeline (separate from screen pipeline)
    let shader_hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        shader_code.hash(&mut h);
        "offscreen".hash(&mut h);
        h.finish()
    };

    // Check cache for existing offscreen pipeline
    let has_pipeline = {
        let renderer = render_state.renderer.read();
        renderer.callback_resources.get::<WgslOffscreenStore>()
            .and_then(|s| s.nodes.get(&node_id))
            .map(|g| g.shader_hash == shader_hash)
            .unwrap_or(false)
    };

    if !has_pipeline {
        // Create offscreen pipeline with Rgba8UnormSrgb target
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgsl_offscreen_shader"),
            source: wgpu::ShaderSource::Wgsl(shader_code.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgsl_offscreen_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgsl_offscreen_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(readback_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let error = pollster::block_on(device.pop_error_scope());
        if error.is_some() { return None; }

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgsl_offscreen_ub"),
            size: UNIFORM_BUF_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let node_gpu = WgslOffscreenGpu {
            pipeline, bind_group, uniform_buffer, shader_hash,
        };

        let mut renderer = render_state.renderer.write();
        if let Some(store) = renderer.callback_resources.get_mut::<WgslOffscreenStore>() {
            store.nodes.insert(node_id, node_gpu);
        } else {
            let mut nodes = HashMap::new();
            nodes.insert(node_id, node_gpu);
            renderer.callback_resources.insert(WgslOffscreenStore { nodes });
        }
    }

    // Create offscreen texture
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("wgsl_offscreen_tex"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: readback_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());

    // Upload uniforms and render
    let renderer = render_state.renderer.read();
    let store = renderer.callback_resources.get::<WgslOffscreenStore>()?;
    let gpu = store.nodes.get(&node_id)?;

    let mut padded = packed_uniforms;
    padded.resize(MAX_UNIFORM_FLOATS, 0.0);
    queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::cast_slice(&padded));

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wgsl_offscreen_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            ..Default::default()
        });
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &gpu.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    queue.submit(Some(encoder.finish()));
    drop(renderer); // Release the read lock before readback

    // Publish the freshly rendered texture into the GPU cache so downstream
    // GPU consumers can sample it directly without a re-upload. The drain
    // happens at the top of the next frame's `begin_frame` (1 frame of
    // latency, matching the existing WGSL injection latency).
    // wgpu::Texture is reference-counted internally, so the clone is cheap
    // and both refer to the same GPU resource.
    crate::gpu_image::queue_publish_node_output(node_id, 0, texture.clone(), width, height);

    // Conditional readback: only stall the CPU on `device.poll(Wait)` if
    // some downstream node actually needs CPU pixel bytes. Default-true
    // (readback) is the safe path; if the hint isn't set yet (graph just
    // changed), we readback once and recover next frame.
    let needs_readback = crate::gpu_image::cpu_consumer_hint(node_id).unwrap_or(true);
    if needs_readback {
        Some(crate::gpu_image::readback_texture(device, queue, &texture, width, height))
    } else {
        // Return a zero-pixel stub so the existing eval-loop injection path
        // keeps working without the readback stall.
        Some(std::sync::Arc::new(ImageData {
            width,
            height,
            pixels: Vec::new(),
        }))
    }
}

struct WgslOffscreenGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    shader_hash: u64,
}

struct WgslOffscreenStore {
    nodes: HashMap<NodeId, WgslOffscreenGpu>,
}

fn render_with_wgpu(
    ui: &mut egui::Ui,
    shader_code: &str,
    render_state: &egui_wgpu::RenderState,
    vertex_count: u32,
    node_id: NodeId,
    mut packed_uniforms: Vec<f32>,
    canvas_w: f32,
    canvas_h: f32,
) {
    let device = &render_state.device;
    let target_format = render_state.target_format;

    // Compute shader hash for caching
    let shader_hash = {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        shader_code.hash(&mut h);
        h.finish()
    };

    // Check if we need to recreate pipeline
    let need_recreate = {
        let renderer = render_state.renderer.read();
        if let Some(store) = renderer.callback_resources.get::<WgslGpuStore>() {
            match store.nodes.get(&node_id) {
                Some(gpu) => gpu.shader_hash != shader_hash,
                None => true,
            }
        } else {
            true
        }
    };

    if need_recreate {
        // Create pipeline + bind group + buffer
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wgsl_user_shader"),
            source: wgpu::ShaderSource::Wgsl(shader_code.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wgsl_uniform_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wgsl_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wgsl_user_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(target_format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let error = pollster::block_on(device.pop_error_scope());
        if let Some(err) = error {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), format!("❌ GPU: {}", err));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(canvas_w, canvas_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 4.0, egui::Color32::from_rgb(40, 15, 15));
            return;
        }

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("wgsl_uniform_buffer"),
            size: UNIFORM_BUF_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wgsl_uniform_bind_group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let node_gpu = WgslNodeGpu {
            pipeline,
            bind_group,
            uniform_buffer,
            vertex_count,
            shader_hash,
        };

        let mut renderer = render_state.renderer.write();
        if let Some(store) = renderer.callback_resources.get_mut::<WgslGpuStore>() {
            store.nodes.insert(node_id, node_gpu);
        } else {
            let mut nodes = HashMap::new();
            nodes.insert(node_id, node_gpu);
            renderer.callback_resources.insert(WgslGpuStore { nodes });
        }
    }

    // Allocate canvas rect
    let (rect, response) = ui.allocate_exact_size(egui::vec2(canvas_w, canvas_h), egui::Sense::hover());

    // Update mouse relative to canvas
    if let Some(pos) = response.hover_pos() {
        let rel_x = (pos.x - rect.min.x) / canvas_w;
        let rel_y = (pos.y - rect.min.y) / canvas_h;
        packed_uniforms[4] = rel_x.clamp(0.0, 1.0); // mouse_x at index 4
        packed_uniforms[5] = rel_y.clamp(0.0, 1.0); // mouse_y at index 5
    }

    // Ensure uniform_floats length is MAX
    packed_uniforms.resize(MAX_UNIFORM_FLOATS, 0.0);

    // Store uniform data for prepare()
    {
        let mut renderer = render_state.renderer.write();
        if let Some(store) = renderer.callback_resources.get_mut::<WgslUniformStore>() {
            store.data.insert(node_id, packed_uniforms);
        } else {
            let mut data = HashMap::new();
            data.insert(node_id, packed_uniforms);
            renderer.callback_resources.insert(WgslUniformStore { data });
        }
    }

    // Add paint callback
    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
        rect,
        WgslPaintCallback { node_id },
    ));

    ui.ctx().request_repaint(); // keep animating
}

fn render_popout_window(
    ctx: &egui::Context,
    _shader_code: &str,
    render_state: &egui_wgpu::RenderState,
    _vertex_count: u32,
    node_id: NodeId,
    packed_uniforms: Vec<f32>,
    _canvas_w: &mut f32,
    _canvas_h: &mut f32,
) {
    let viewport_id = egui::ViewportId::from_hash_of(("wgsl_popout", node_id));
    let render_state = render_state.clone();
    let uniforms = packed_uniforms;
    let popout_id = egui::Id::new(("wgsl_popout", node_id));

    ctx.show_viewport_immediate(
        viewport_id,
        egui::ViewportBuilder::default()
            .with_title(format!("Shader #{}", node_id))
            .with_inner_size([800.0, 600.0]),
        move |ctx, _class| {
            // Detect native window close (red ❌ button) and set popout flag to false
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.data_mut(|d| d.insert_temp(popout_id, false));
                return;
            }

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
                .show(ctx, |ui| {
                    let avail = ui.available_size();
                    let w = avail.x.max(100.0);
                    let h = avail.y.max(100.0);

                    let (rect, response) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());

                    let mut packed = uniforms.clone();
                    // Update mouse
                    if let Some(pos) = response.hover_pos() {
                        let rel_x = (pos.x - rect.min.x) / w;
                        let rel_y = (pos.y - rect.min.y) / h;
                        packed[4] = rel_x.clamp(0.0, 1.0);
                        packed[5] = rel_y.clamp(0.0, 1.0);
                    }
                    // Update resolution to match window size
                    packed[2] = w;
                    packed[3] = h;
                    packed.resize(MAX_UNIFORM_FLOATS, 0.0);

                    // Store uniform data for the main node_id
                    // (The popout shares the same pipeline as the inline render)
                    {
                        let mut renderer = render_state.renderer.write();
                        if let Some(store) = renderer.callback_resources.get_mut::<WgslUniformStore>() {
                            store.data.insert(node_id, packed);
                        } else {
                            let mut data = HashMap::new();
                            data.insert(node_id, packed);
                            renderer.callback_resources.insert(WgslUniformStore { data });
                        }
                    }

                    ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                        rect,
                        WgslPaintCallback { node_id },
                    ));

                    ctx.request_repaint();
                });
        },
    );
}

fn validate_wgsl_naga(code: &str) -> Result<(), String> {
    let module = naga::front::wgsl::parse_str(code)
        .map_err(|e| format!("Parse: {}", e))?;
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator.validate(&module).map_err(|e| format!("Validation: {}", e))?;
    Ok(())
}

/// Drop all GPU resources for a deleted WGSL Viewer node so VRAM is
/// released immediately rather than leaking inside egui_wgpu's
/// callback_resources. Called from
/// `crate::gpu_image::GpuTextureCache::invalidate_node`.
pub(crate) fn cleanup_node(
    callback_resources: &mut eframe::egui_wgpu::CallbackResources,
    node_id: crate::graph::NodeId,
) {
    if let Some(store) = callback_resources.get_mut::<WgslGpuStore>() {
        store.nodes.remove(&node_id);
    }
    if let Some(store) = callback_resources.get_mut::<WgslOffscreenStore>() {
        store.nodes.remove(&node_id);
    }
    if let Some(store) = callback_resources.get_mut::<WgslUniformStore>() {
        store.data.remove(&node_id);
    }
}
