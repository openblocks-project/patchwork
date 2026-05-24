// Material node — builds a `MaterialDef` from albedo / roughness / metallic /
// emission / shader-type inputs and emits the result as a JSON string on a
// Text port. The 3D Render node parses the JSON back into `MaterialDef`.
//
// Why Text+JSON: see `feedback_new_node_checklist.md` (typed-vs-config rule).
// Composes for free with String Format / Text Editor / AI Request / network
// nodes — anything that produces conformant JSON can drive 3D Render directly.

use crate::graph::{Graph, NodeId, PortDef, PortKind, PortValue};
use crate::gpu_mesh::vertex_buffer_layout;
use crate::material::{MaterialDef, MaterialMaps, MaterialTexRef, ShaderType};
use crate::mesh::primitive_sphere;
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::inline_port_circle;
use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use eframe::egui_wgpu::wgpu::util::DeviceExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialNode {
    #[serde(default)]                       pub shader_type: ShaderType,
    #[serde(default = "default_albedo_r")] pub albedo_r: f32,
    #[serde(default = "default_albedo_g")] pub albedo_g: f32,
    #[serde(default = "default_albedo_b")] pub albedo_b: f32,
    #[serde(default = "default_roughness")] pub roughness: f32,
    #[serde(default)]                       pub metallic:  f32,
    #[serde(default)]                       pub emission_r: f32,
    #[serde(default)]                       pub emission_g: f32,
    #[serde(default)]                       pub emission_b: f32,
    #[serde(default)]                       pub double_sided: bool,
}

fn default_albedo_r() -> f32 { 0.85 }
fn default_albedo_g() -> f32 { 0.20 }
fn default_albedo_b() -> f32 { 0.20 }
fn default_roughness() -> f32 { 0.5 }

impl Default for MaterialNode {
    fn default() -> Self {
        Self {
            shader_type: ShaderType::Lit,
            albedo_r: 0.85, albedo_g: 0.20, albedo_b: 0.20,
            roughness: 0.5, metallic: 0.0,
            emission_r: 0.0, emission_g: 0.0, emission_b: 0.0,
            double_sided: false,
        }
    }
}

// Port layout:
//   0..7   : 8 scalar inputs (Albedo RGB / Roughness / Metallic / Emission RGB)
//   8..12  : 5 Image inputs (Albedo / Roughness / Metallic / AO / Emission Tex)
const FLOAT_PORTS: [&str; 8] = [
    "Albedo R", "Albedo G", "Albedo B",
    "Roughness", "Metallic",
    "Emission R", "Emission G", "Emission B",
];
const ALBEDO_TEX_PORT:    usize = 8;
const ROUGHNESS_TEX_PORT: usize = 9;
const METALLIC_TEX_PORT:  usize = 10;
const AO_TEX_PORT:        usize = 11;
const EMISSION_TEX_PORT:  usize = 12;

/// Each map port: name + port index, paired with its slot in `MaterialMaps`.
/// Names are the full common terms used by glTF / Unreal / Blender, not the
/// jargon abbreviations — keeps the Maps section readable to artists and
/// matches the scalar header naming above ("Base Color" rather than "Albedo").
const MAP_PORTS: [(usize, &str); 5] = [
    (ALBEDO_TEX_PORT,    "Base Color Texture"),
    (ROUGHNESS_TEX_PORT, "Roughness Texture"),
    (METALLIC_TEX_PORT,  "Metallic Texture"),
    (AO_TEX_PORT,        "Ambient Occlusion Texture"),
    (EMISSION_TEX_PORT,  "Emission Texture"),
];

impl NodeBehavior for MaterialNode {
    fn title(&self)        -> &str    { "Material" }
    fn type_tag(&self)     -> &str    { "material" }
    fn color_hint(&self)   -> [u8; 3] { [220, 180, 80] }
    fn inline_ports(&self) -> bool    { true }

    fn inputs(&self) -> Vec<PortDef> {
        let mut v: Vec<PortDef> = FLOAT_PORTS.iter().map(|n| PortDef::new(n, PortKind::Number)).collect();
        for (_, name) in MAP_PORTS {
            v.push(PortDef::new(name, PortKind::Image));
        }
        v
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Material", PortKind::Text)]
    }

    /// All texture inputs are GPU-resident — opt out of CPU readback so
    /// upstream GPU producers (Image FX, WGSL Viewer, GLB albedo, etc.)
    /// can hand off zero-copy.
    fn needs_cpu_image_input(&self, port: usize) -> bool {
        !MAP_PORTS.iter().any(|(p, _)| *p == port)
    }

    fn evaluate(&mut self, inputs: &[PortValue]) -> Vec<(usize, PortValue)> {
        let read = |idx: usize, fallback: f32| -> f32 {
            match inputs.get(idx) {
                Some(PortValue::Float(v)) => *v,
                _ => fallback,
            }
        };
        // Resolve each Image input to a `MaterialTexRef`. The 3D Render
        // node re-resolves the actual `wgpu::TextureView` per-frame via
        // `frame_snapshot_get_view(node_id, port)` — we just need the
        // upstream identity here.
        let resolve_tex = |port: usize| -> Option<MaterialTexRef> {
            match inputs.get(port) {
                Some(PortValue::GpuImage(h)) => Some(MaterialTexRef {
                    node_id: h.node_id,
                    port:    h.port,
                    width:   h.width,
                    height:  h.height,
                }),
                _ => None,
            }
        };

        let maps = MaterialMaps {
            albedo:    resolve_tex(ALBEDO_TEX_PORT),
            roughness: resolve_tex(ROUGHNESS_TEX_PORT),
            metallic:  resolve_tex(METALLIC_TEX_PORT),
            ao:        resolve_tex(AO_TEX_PORT),
            emission:  resolve_tex(EMISSION_TEX_PORT),
        };

        let mat = MaterialDef {
            shader_type: self.shader_type,
            albedo: [
                read(0, self.albedo_r).clamp(0.0, 1.0),
                read(1, self.albedo_g).clamp(0.0, 1.0),
                read(2, self.albedo_b).clamp(0.0, 1.0),
                1.0,
            ],
            roughness: read(3, self.roughness).clamp(0.04, 1.0),
            metallic:  read(4, self.metallic).clamp(0.0, 1.0),
            emission: [
                read(5, self.emission_r).max(0.0),
                read(6, self.emission_g).max(0.0),
                read(7, self.emission_b).max(0.0),
            ],
            maps,
            double_sided: self.double_sided,
        };
        vec![(0, PortValue::Text(mat.to_json().to_string()))]
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<MaterialNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        // Shader-type dropdown
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Shader").small());
            egui::ComboBox::from_id_salt(("material_shader_type", ctx.node_id))
                .selected_text(self.shader_type.label())
                .show_ui(ui, |ui| {
                    for k in ShaderType::ALL {
                        ui.selectable_value(&mut self.shader_type, k, k.label());
                    }
                });
        });

        // ── Inline preview (sphere with current material applied). ──
        // Uses egui_wgpu paint callback to render directly into the node
        // body. Texture maps are intentionally NOT sampled — the preview
        // is for color / shader / lighting feedback, not per-pixel test.
        if let Some(rs) = ctx.wgpu_render_state {
            let preview_size = 96.0_f32.min(ui.available_width());
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(preview_size, preview_size),
                egui::Sense::hover(),
            );
            // Background fill so the area outside the sphere reads as a
            // solid swatch, not whatever was previously painted.
            ui.painter().rect_filled(rect, 4.0, egui::Color32::from_gray(18));
            let uniforms = build_preview_uniforms(self);
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                rect,
                MaterialPreviewCallback {
                    node_id: ctx.node_id,
                    uniforms,
                    target_format: rs.target_format,
                },
            ));
        }

        // Roughness + Metallic are PBR-only (Toon and Unlit ignore them).
        // Hybrid visibility: hidden when inactive AND unwired; dimmed when
        // inactive but wired (so the wire endpoint stays visible).
        // Base Color, AO, Emission apply to all shader types.
        let pbr_active = shader_uses_pbr(self.shader_type);

        // ── Surface scalars first (cheap rows). ──
        if should_render(pbr_active, 3, ctx) {
            scalar_row(ui, ctx, "Roughness", 3, &mut self.roughness, 0.01, 0.04..=1.0, pbr_active);
        }
        if should_render(pbr_active, 4, ctx) {
            scalar_row(ui, ctx, "Metallic",  4, &mut self.metallic,  0.01, 0.0..=1.0, pbr_active);
        }

        // ── Base Color: swatch header + single 3-cell RGB row. ──
        // (Industry naming: glTF / Unity / Unreal all call this "Base
        // Color"; "Albedo" is the older PBR-jargon term.)
        ui.add_space(2.0);
        let base_color = egui::Color32::from_rgb(
            (self.albedo_r * 255.0) as u8,
            (self.albedo_g * 255.0) as u8,
            (self.albedo_b * 255.0) as u8,
        );
        let base_tex_wired = ctx.connections.iter()
            .any(|c| c.to_node == ctx.node_id && c.to_port == ALBEDO_TEX_PORT);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 3.0, base_color);
            ui.painter().rect_stroke(
                rect, 3.0,
                egui::Stroke::new(1.0, egui::Color32::from_gray(80)),
                egui::epaint::StrokeKind::Outside,
            );
            let label = if base_tex_wired { "Base Color (× tex)" } else { "Base Color" };
            ui.label(egui::RichText::new(label).small());
        });
        ui.horizontal(|ui| {
            rgb_cell(ui, ctx, "R", 0, &mut self.albedo_r, 0.01, 0.0..=1.0, true);
            rgb_cell(ui, ctx, "G", 1, &mut self.albedo_g, 0.01, 0.0..=1.0, true);
            rgb_cell(ui, ctx, "B", 2, &mut self.albedo_b, 0.01, 0.0..=1.0, true);
        });

        // ── Emission: header + 3-cell RGB row. ──
        ui.add_space(2.0);
        ui.label(egui::RichText::new("Emission").small());
        ui.horizontal(|ui| {
            rgb_cell(ui, ctx, "R", 5, &mut self.emission_r, 0.02, 0.0..=8.0, true);
            rgb_cell(ui, ctx, "G", 6, &mut self.emission_g, 0.02, 0.0..=8.0, true);
            rgb_cell(ui, ctx, "B", 7, &mut self.emission_b, 0.02, 0.0..=8.0, true);
        });

        // ── Texture maps under a collapsible (progressive disclosure). ──
        // All 5 ports always exist in the graph regardless of whether this
        // section is open, so wires never break on collapse/expand.
        let any_map_wired = MAP_PORTS.iter().any(|(p, _)| {
            ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == *p)
        });
        let header = if any_map_wired { "Maps •" } else { "Maps" };
        egui::CollapsingHeader::new(egui::RichText::new(header).small())
            .default_open(any_map_wired)
            .id_salt(("material_maps", ctx.node_id))
            .show(ui, |ui| {
                for (port_idx, name) in MAP_PORTS {
                    // Roughness Tex + Metallic Tex follow the same
                    // PBR-only rule as the scalar rows above (hidden if
                    // inactive & unwired, dimmed if inactive but wired).
                    let map_active = match port_idx {
                        ROUGHNESS_TEX_PORT | METALLIC_TEX_PORT => pbr_active,
                        _ => true,
                    };
                    if !should_render(map_active, port_idx, ctx) { continue; }
                    ui.horizontal(|ui| {
                        inline_port_circle(
                            ui, ctx.node_id, port_idx, true,
                            ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                            PortKind::Image,
                        );
                        let wired = ctx.connections.iter()
                            .any(|c| c.to_node == ctx.node_id && c.to_port == port_idx);
                        let color = if !map_active {
                            INACTIVE_TEXT_COLOR
                        } else if wired {
                            ui.visuals().hyperlink_color
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.label(egui::RichText::new(name).small().color(color));
                    });
                }
            });

        ui.checkbox(&mut self.double_sided, egui::RichText::new("Double-sided").small());

        ui.separator();

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Material (JSON)").small());
            inline_port_circle(
                ui, ctx.node_id, 0, false,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Text,
            );
        });
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("material", |state| {
        let mut n = MaterialNode::default();
        n.load_state(state);
        Box::new(n)
    });
}

// ── UI helpers ──────────────────────────────────────────────────────────────

/// Color used for labels of inactive controls (those whose values
/// don't reach the fragment shader for the current shader type).
/// Significantly dimmer than normal text but still legible.
const INACTIVE_TEXT_COLOR: egui::Color32 = egui::Color32::from_gray(85);

/// True if the shader's lighting math actually consumes the
/// roughness / metallic scalars + textures (PBR specular path).
/// Toon and Unlit ignore them, so they're hidden (when unwired) or
/// dimmed (when wired) in the UI.
fn shader_uses_pbr(s: ShaderType) -> bool {
    matches!(s, ShaderType::Lit)
}

/// Hybrid visibility rule for shader-conditional controls:
/// - `active` (current shader uses this control) → always render.
/// - inactive but a wire exists → still render (dimmed) so the wire's
///   endpoint stays visible; underlying graph connection is preserved.
/// - inactive and unwired → don't render at all (UI cleanup).
fn should_render(active: bool, port_idx: usize, ctx: &RenderContext) -> bool {
    if active { return true; }
    ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == port_idx)
}

/// One labelled scalar row: port circle + full label + DragValue (or
/// dimmed wired-value label). When `active == false` the label and
/// wired-value are drawn in dim gray to signal the value is ignored
/// by the current shader; the port itself stays bright (so existing
/// wires read normally) and the DragValue stays interactive (so the
/// user can preset a value before switching shader back).
fn scalar_row(
    ui: &mut egui::Ui,
    ctx: &mut RenderContext,
    label: &str,
    port_idx: usize,
    value: &mut f32,
    speed: f32,
    range: std::ops::RangeInclusive<f32>,
    active: bool,
) {
    let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == port_idx);
    let label_color = if active { ui.visuals().text_color() } else { INACTIVE_TEXT_COLOR };
    let value_color = if active { ui.visuals().hyperlink_color } else { INACTIVE_TEXT_COLOR };
    ui.horizontal(|ui| {
        inline_port_circle(
            ui, ctx.node_id, port_idx, true,
            ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
            PortKind::Number,
        );
        ui.label(egui::RichText::new(label).small().color(label_color));
        if wired {
            let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, port_idx);
            ui.label(egui::RichText::new(format!("{}", v)).small().color(value_color));
        } else {
            ui.add(egui::DragValue::new(value).speed(speed).range(range).fixed_decimals(2));
        }
    });
}

/// One cell of a 3-up RGB row: small port circle + axis letter + compact
/// DragValue. Three of these laid out horizontally compress an Albedo /
/// Emission triplet from 3 vertical rows down to 1 + a header.
/// `active` follows the same dimming convention as `scalar_row`.
fn rgb_cell(
    ui: &mut egui::Ui,
    ctx: &mut RenderContext,
    label: &str,
    port_idx: usize,
    value: &mut f32,
    speed: f32,
    range: std::ops::RangeInclusive<f32>,
    active: bool,
) {
    let label_color = if active { ui.visuals().text_color() } else { INACTIVE_TEXT_COLOR };
    let value_color = if active { ui.visuals().hyperlink_color } else { INACTIVE_TEXT_COLOR };
    inline_port_circle(
        ui, ctx.node_id, port_idx, true,
        ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
        PortKind::Number,
    );
    ui.label(egui::RichText::new(label).small().color(label_color));
    let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == port_idx);
    if wired {
        let v = Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, port_idx)
            .as_float();
        ui.label(egui::RichText::new(format!("{:.2}", v)).small().color(value_color));
    } else {
        // Compact DragValue (~46px) so 3 cells fit comfortably across the
        // node body. `fixed_decimals(2)` keeps the displayed text bounded.
        ui.add_sized(
            [46.0, 18.0],
            egui::DragValue::new(value).speed(speed).range(range).fixed_decimals(2),
        );
    }
}

// ── Inline preview (sphere render) ──────────────────────────────────────────
//
// A small sphere rendered inside the Material node body, updated every frame
// from the current MaterialDef. Uses a simplified single-light shader (no
// texture sampling — color/roughness/metallic/emission only) so it doesn't
// need to plumb the GPU texture cache through the egui paint callback.
//
// All Material nodes share one bind-group-layout + one sphere mesh upload;
// only the per-node uniform buffer + bind group is unique. Total per-node
// GPU footprint is ~256 bytes (uniform buffer) — negligible.

const PREVIEW_WGSL: &str = r#"
struct PU {
    view_proj:  mat4x4<f32>,
    light_dir:  vec4<f32>,
    base_color: vec4<f32>,
    emission:   vec4<f32>,
    pbr:        vec4<f32>,  // x=roughness, y=metallic, z=shader_id (0=Lit,1=Toon,2=Unlit)
};
@group(0) @binding(0) var<uniform> u: PU;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal:   vec3<f32>,
    @location(2) uv:       vec2<f32>,
    @location(3) tangent:  vec4<f32>,
};
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_normal: vec3<f32>,
    @location(1) world_pos:    vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(in.position, 1.0);
    out.world_normal = in.normal;
    out.world_pos = in.position;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let l = normalize(u.light_dir.xyz);
    let v = normalize(vec3<f32>(0.0, 0.0, 2.5) - in.world_pos);
    let n_dot_l = max(dot(n, l), 0.0);

    let base = u.base_color.rgb;
    let kind = u.pbr.z;
    var color: vec3<f32>;
    if (kind < 0.5) {
        let roughness = u.pbr.x;
        let metallic  = u.pbr.y;
        let h = normalize(l + v);
        let n_dot_h = max(dot(n, h), 0.0);
        let kd = base * (1.0 - metallic);
        let ambient = base * 0.18;
        let diffuse = kd * n_dot_l;
        let shininess  = mix(128.0, 4.0, roughness);
        let spec_color = mix(vec3<f32>(0.04), base, metallic);
        let specular   = spec_color * pow(n_dot_h, shininess) * step(0.0001, n_dot_l);
        color = ambient + diffuse + specular;
    } else if (kind < 1.5) {
        var band: f32;
        if (n_dot_l > 0.5) { band = 1.0; }
        else if (n_dot_l > 0.0) { band = 0.65; }
        else { band = 0.30; }
        color = base * band;
    } else {
        color = base;
    }

    color = color + u.emission.rgb;
    return vec4<f32>(color, u.base_color.a);
}
"#;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct PreviewUniforms {
    view_proj:  [[f32; 4]; 4],
    light_dir:  [f32; 4],
    base_color: [f32; 4],
    emission:   [f32; 4],
    pbr:        [f32; 4],
}

const PREVIEW_UNIFORM_SIZE: u64 = 256;

struct PreviewSharedGpu {
    bind_group_layout: wgpu::BindGroupLayout,
    /// Pipelines keyed by target color format (egui's surface format may
    /// differ across hosts: BGRA8UnormSrgb on most, RGBA8UnormSrgb on
    /// some). Cached per-format on first use.
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    sphere_vertex: wgpu::Buffer,
    sphere_index:  wgpu::Buffer,
    sphere_index_count: u32,
}

struct PreviewNodeGpu {
    uniform_buffer: wgpu::Buffer,
    bind_group:     wgpu::BindGroup,
}

#[derive(Default)]
struct MaterialPreviewStore {
    shared: Option<PreviewSharedGpu>,
    nodes:  HashMap<NodeId, PreviewNodeGpu>,
}

fn create_shared_gpu(device: &wgpu::Device) -> PreviewSharedGpu {
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("material_preview_bgl"),
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
    let sphere = primitive_sphere(0.7, 32, 24);
    let sphere_vertex = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("material_preview_sphere_v"),
        contents: bytemuck::cast_slice(&sphere.vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let sphere_index = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("material_preview_sphere_i"),
        contents: bytemuck::cast_slice(&sphere.indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    PreviewSharedGpu {
        bind_group_layout,
        pipelines: HashMap::new(),
        sphere_vertex,
        sphere_index,
        sphere_index_count: sphere.indices.len() as u32,
    }
}

fn create_preview_pipeline(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    target_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("material_preview_shader"),
        source: wgpu::ShaderSource::Wgsl(PREVIEW_WGSL.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("material_preview_pl"),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("material_preview_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[vertex_buffer_layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            // Back-face culling lets us skip the depth attachment — for a
            // closed convex shape (sphere), back-facing triangles are
            // always behind front-facing ones, so visual order is correct
            // without depth testing. egui's render pass has no depth
            // buffer, so this is the only practical path.
            cull_mode: Some(wgpu::Face::Back),
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

struct MaterialPreviewCallback {
    node_id: NodeId,
    uniforms: PreviewUniforms,
    target_format: wgpu::TextureFormat,
}

impl egui_wgpu::CallbackTrait for MaterialPreviewCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _enc: &mut wgpu::CommandEncoder,
        cr: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if cr.get::<MaterialPreviewStore>().is_none() {
            cr.insert(MaterialPreviewStore::default());
        }
        let store = cr.get_mut::<MaterialPreviewStore>().unwrap();

        if store.shared.is_none() {
            store.shared = Some(create_shared_gpu(device));
        }

        // Pipeline per target format.
        {
            let shared = store.shared.as_mut().unwrap();
            if !shared.pipelines.contains_key(&self.target_format) {
                let p = create_preview_pipeline(device, &shared.bind_group_layout, self.target_format);
                shared.pipelines.insert(self.target_format, p);
            }
        }

        // Per-node uniform buffer + bind group.
        if !store.nodes.contains_key(&self.node_id) {
            let bgl = &store.shared.as_ref().unwrap().bind_group_layout;
            let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("material_preview_ub"),
                size: PREVIEW_UNIFORM_SIZE,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("material_preview_bg"),
                layout: bgl,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                }],
            });
            store.nodes.insert(self.node_id, PreviewNodeGpu { uniform_buffer, bind_group });
        }

        // Upload this frame's uniforms.
        let node = store.nodes.get(&self.node_id).unwrap();
        queue.write_buffer(&node.uniform_buffer, 0, bytemuck::bytes_of(&self.uniforms));

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        cr: &egui_wgpu::CallbackResources,
    ) {
        let Some(store) = cr.get::<MaterialPreviewStore>() else { return };
        let Some(shared) = store.shared.as_ref() else { return };
        let Some(pipeline) = shared.pipelines.get(&self.target_format) else { return };
        let Some(node) = store.nodes.get(&self.node_id) else { return };

        render_pass.set_pipeline(pipeline);
        render_pass.set_bind_group(0, &node.bind_group, &[]);
        render_pass.set_vertex_buffer(0, shared.sphere_vertex.slice(..));
        render_pass.set_index_buffer(shared.sphere_index.slice(..), wgpu::IndexFormat::Uint32);
        render_pass.draw_indexed(0..shared.sphere_index_count, 0, 0..1);
    }
}

/// Build the per-frame `PreviewUniforms` from the node's current state.
/// Camera is fixed at +Z looking at origin with a 35° FOV; light comes
/// from upper-left so highlights read clearly on the sphere's silhouette.
fn build_preview_uniforms(node: &MaterialNode) -> PreviewUniforms {
    let aspect = 1.0_f32; // square preview
    let view = glam::Mat4::look_at_rh(
        glam::Vec3::new(0.0, 0.0, 2.5),
        glam::Vec3::ZERO,
        glam::Vec3::Y,
    );
    let proj = glam::Mat4::perspective_rh(35.0_f32.to_radians(), aspect, 0.1, 10.0);
    let view_proj = (proj * view).to_cols_array_2d();
    let light_dir = glam::Vec3::new(-0.4, 0.8, 0.5).normalize();
    PreviewUniforms {
        view_proj,
        light_dir:  [light_dir.x, light_dir.y, light_dir.z, 0.0],
        base_color: [node.albedo_r, node.albedo_g, node.albedo_b, 1.0],
        emission:   [node.emission_r, node.emission_g, node.emission_b, 0.0],
        pbr: [
            node.roughness.clamp(0.04, 1.0),
            node.metallic.clamp(0.0, 1.0),
            node.shader_type.shader_id() as f32,
            0.0,
        ],
    }
}
