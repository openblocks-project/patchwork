// 3D Render — owns a wgpu render pipeline (vertex+fragment, depth-tested)
// that draws a Mesh with a Material under a built-in camera, and publishes
// the result to `GpuTextureCache` so downstream Image consumers can composite
// it.
//
// Phase 1: hardcoded directional light + 3 shading models (Lit / Toon / Unlit)
// + optional albedo texture. Camera params live inline on this node — see
// `Render3DNode::eye_x` etc. — with each exposed as a Float input port for
// animation. Phase 4 will add a Light input port and shadow maps.

use crate::graph::{ImageData, NodeId, PortDef, PortKind, PortValue};
use crate::gpu_mesh::{vertex_buffer_layout, GpuMeshCache};
use crate::material::MaterialDef;
use crate::mesh::{build_normal_matrix, CameraParams, PlacedMesh};
use crate::node_trait::{NodeBehavior, RenderContext};
use crate::nodes::inline_port_circle;
use eframe::egui;
use eframe::egui_wgpu;
use eframe::egui_wgpu::wgpu;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

// ── Shader ──────────────────────────────────────────────────────────────────

const RENDER_3D_WGSL: &str = r#"
struct Uniforms {
    view_proj:  mat4x4<f32>,
    model:      mat4x4<f32>,
    normal_mat: mat4x4<f32>,
    light_dir:  vec4<f32>,
    eye_pos:    vec4<f32>,
    albedo:     vec4<f32>,    // tint multiplied with sampled texture (white = pure tex)
    emission:   vec4<f32>,    // xyz emission, w = use_emission_tex (0/1)
    rough_met:  vec4<f32>,    // x roughness, y metallic, z shader_type, w unused
    map_use:    vec4<f32>,    // x use_albedo, y use_roughness, z use_metallic, w use_ao
};
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var samp:           sampler;
@group(0) @binding(2) var albedo_tex:     texture_2d<f32>;
@group(0) @binding(3) var roughness_tex:  texture_2d<f32>;
@group(0) @binding(4) var metallic_tex:   texture_2d<f32>;
@group(0) @binding(5) var ao_tex:         texture_2d<f32>;
@group(0) @binding(6) var emission_tex:   texture_2d<f32>;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal:   vec3<f32>,
    @location(2) uv:       vec2<f32>,
    @location(3) tangent:  vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world_pos4 = u.model * vec4<f32>(in.position, 1.0);
    out.clip = u.view_proj * world_pos4;
    out.world_pos = world_pos4.xyz;
    let n4 = u.normal_mat * vec4<f32>(in.normal, 0.0);
    out.world_normal = normalize(n4.xyz);
    out.uv = in.uv;
    return out;
}

fn shade_lit(albedo: vec3<f32>, n: vec3<f32>, l: vec3<f32>, v: vec3<f32>,
             roughness: f32, metallic: f32) -> vec3<f32> {
    let h = normalize(l + v);
    let n_dot_l = max(dot(n, l), 0.0);
    let n_dot_h = max(dot(n, h), 0.0);
    let kd = albedo * (1.0 - metallic);
    let diffuse = kd * n_dot_l;
    let shininess  = mix(128.0, 4.0, roughness);
    let spec_color = mix(vec3<f32>(0.04), albedo, metallic);
    let specular   = spec_color * pow(n_dot_h, shininess) * step(0.0001, n_dot_l);
    let ambient = albedo * 0.15;
    return ambient + diffuse + specular;
}

fn shade_toon(albedo: vec3<f32>, n: vec3<f32>, l: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    let n_dot_l = dot(n, l);
    // 3-band stepped diffuse: shadow / midtone / lit.
    var band: f32;
    if (n_dot_l > 0.5) { band = 1.0; }
    else if (n_dot_l > 0.0) { band = 0.65; }
    else { band = 0.30; }
    let rim = 1.0 - max(dot(n, v), 0.0);
    let rim_step = step(0.65, rim);
    return albedo * band + vec3<f32>(0.15) * rim_step;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let l = normalize(u.light_dir.xyz);
    let v = normalize(u.eye_pos.xyz - in.world_pos);
    let uv = in.uv;

    // Albedo: tint × optional texture.
    var base = u.albedo.rgb;
    if (u.map_use.x > 0.5) {
        base = base * textureSample(albedo_tex, samp, uv).rgb;
    }

    // Roughness / Metallic: scalar × optional texture (red channel).
    var roughness = u.rough_met.x;
    if (u.map_use.y > 0.5) {
        roughness = clamp(roughness * textureSample(roughness_tex, samp, uv).r, 0.04, 1.0);
    }
    var metallic = u.rough_met.y;
    if (u.map_use.z > 0.5) {
        metallic = clamp(metallic * textureSample(metallic_tex, samp, uv).r, 0.0, 1.0);
    }

    // AO: 1.0 × optional texture (red channel). Multiplied with the final
    // shaded color (shadows/midtones), not the specular highlight (which
    // would be physically wrong for direct lighting).
    var ao = 1.0;
    if (u.map_use.w > 0.5) {
        ao = textureSample(ao_tex, samp, uv).r;
    }

    // Emission: tint × optional texture.
    var emission = u.emission.rgb;
    if (u.emission.w > 0.5) {
        emission = emission * textureSample(emission_tex, samp, uv).rgb;
    }

    let kind = u.rough_met.z;
    var color: vec3<f32>;
    if (kind < 0.5) {
        color = shade_lit(base, n, l, v, roughness, metallic);
    } else if (kind < 1.5) {
        color = shade_toon(base, n, l, v);
    } else {
        // Unlit: emit albedo (tinted texture) directly. AO/emission still apply.
        color = base;
    }

    color = color * ao + emission;
    return vec4<f32>(color, u.albedo.a);
}
"#;

const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT:  wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const UNIFORM_BUF_SIZE: u64 = 384;

// ── Per-node GPU state ──────────────────────────────────────────────────────

struct Render3DGpu {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler:        wgpu::Sampler,
    dummy_white_view: wgpu::TextureView,

    color_texture: wgpu::Texture,
    color_view:    wgpu::TextureView,
    depth_texture: wgpu::Texture,
    depth_view:    wgpu::TextureView,
    size: (u32, u32),

    mesh_cache: GpuMeshCache,

    /// Hash of (cull_mode, double_sided). Pipeline rebuilds when this changes.
    pipeline_signature: u64,
}

#[derive(Default)]
struct Render3DStore {
    nodes: HashMap<NodeId, Render3DGpu>,
}

// ── Node ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Render3DNode {
    #[serde(default = "default_w")] pub width:  u32,
    #[serde(default = "default_h")] pub height: u32,
    // Camera params (folded in from the old Camera 3D node).
    #[serde(default = "default_eye_x")]    pub eye_x:  f32,
    #[serde(default = "default_eye_y")]    pub eye_y:  f32,
    #[serde(default = "default_eye_z")]    pub eye_z:  f32,
    #[serde(default)]                      pub target_x: f32,
    #[serde(default)]                      pub target_y: f32,
    #[serde(default)]                      pub target_z: f32,
    #[serde(default = "default_fov")]      pub fov_y_deg: f32,
    #[serde(default = "default_near")]     pub near: f32,
    #[serde(default = "default_far")]      pub far:  f32,
    // Light direction.
    #[serde(default = "default_light_x")] pub light_dir_x: f32,
    #[serde(default = "default_light_y")] pub light_dir_y: f32,
    #[serde(default = "default_light_z")] pub light_dir_z: f32,

    /// Cached parsed material from the upstream Text/JSON wire. Re-parsed
    /// only when the source string's hash changes.
    #[serde(skip)]
    last_material_hash: u64,
    #[serde(skip)]
    last_material: Option<MaterialDef>,
}

fn default_w() -> u32 { 512 }
fn default_h() -> u32 { 512 }
fn default_eye_x() -> f32 { 3.0 }
fn default_eye_y() -> f32 { 2.0 }
fn default_eye_z() -> f32 { 4.0 }
fn default_fov()   -> f32 { 45.0 }
fn default_near()  -> f32 { 0.1 }
fn default_far()   -> f32 { 100.0 }
fn default_light_x() -> f32 { -0.4 }
fn default_light_y() -> f32 {  0.8 }
fn default_light_z() -> f32 { -0.5 }

impl Default for Render3DNode {
    fn default() -> Self {
        Self {
            width: 512, height: 512,
            eye_x: 3.0, eye_y: 2.0, eye_z: 4.0,
            target_x: 0.0, target_y: 0.0, target_z: 0.0,
            fov_y_deg: 45.0, near: 0.1, far: 100.0,
            light_dir_x: -0.4, light_dir_y: 0.8, light_dir_z: -0.5,
            last_material_hash: 0,
            last_material: None,
        }
    }
}

// Port layout: 0=Mesh, 1=Material, 2..=10 camera floats, 11..=13 light dir.
const CAMERA_FLOAT_PORT_BASE: usize = 2; // eye_x at port 2
const LIGHT_FLOAT_PORT_BASE:  usize = 11;

const CAMERA_PORT_NAMES: [&str; 9] = [
    "Eye X", "Eye Y", "Eye Z",
    "Target X", "Target Y", "Target Z",
    "FOV", "Near", "Far",
];

impl NodeBehavior for Render3DNode {
    fn title(&self)        -> &str    { "3D Render" }
    fn type_tag(&self)     -> &str    { "render_3d" }
    fn color_hint(&self)   -> [u8; 3] { [180, 80, 220] }
    fn inline_ports(&self) -> bool    { true }

    fn inputs(&self) -> Vec<PortDef> {
        let mut v = vec![
            PortDef::new("Mesh",     PortKind::Mesh),
            PortDef::new("Material", PortKind::Text),
        ];
        for n in CAMERA_PORT_NAMES { v.push(PortDef::new(n, PortKind::Number)); }
        v.push(PortDef::new("Light X", PortKind::Number));
        v.push(PortDef::new("Light Y", PortKind::Number));
        v.push(PortDef::new("Light Z", PortKind::Number));
        v
    }

    fn outputs(&self) -> Vec<PortDef> {
        vec![PortDef::new("Image", PortKind::Image)]
    }

    fn needs_cpu_image_input(&self, _port: usize) -> bool { false }

    fn evaluate_with_ctx(
        &mut self,
        inputs: &[PortValue],
        ctx: &mut crate::node_trait::EvalCtx<'_>,
    ) -> Vec<(usize, PortValue)> {
        let Some(rs) = ctx.render_state else {
            return vec![(0, PortValue::None)];
        };

        let read_float = |idx: usize, fallback: f32| -> f32 {
            match inputs.get(idx) {
                Some(PortValue::Float(v)) => *v,
                _ => fallback,
            }
        };

        let mesh: Option<&Arc<PlacedMesh>> = inputs.first().and_then(|v| v.as_mesh());

        // Parse Material from the Text input (JSON). Cache the parsed
        // result keyed by string hash so we only re-parse on real change.
        let material_text: &str = match inputs.get(1) {
            Some(PortValue::Text(s)) => s.as_str(),
            _ => "",
        };
        let mat_hash = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            material_text.hash(&mut h);
            h.finish()
        };
        if mat_hash != self.last_material_hash || self.last_material.is_none() {
            self.last_material = if material_text.is_empty() {
                Some(MaterialDef::default())
            } else {
                MaterialDef::from_json(material_text).or_else(|| Some(MaterialDef::default()))
            };
            self.last_material_hash = mat_hash;
        }
        let default_mat = MaterialDef::default();
        let mat: &MaterialDef = self.last_material.as_ref().unwrap_or(&default_mat);

        let Some(mesh) = mesh else {
            return vec![(0, PortValue::None)];
        };

        // Resolve camera params: stored value, optionally overridden by wired Float port.
        let camera = CameraParams {
            eye:    [
                read_float(CAMERA_FLOAT_PORT_BASE + 0, self.eye_x),
                read_float(CAMERA_FLOAT_PORT_BASE + 1, self.eye_y),
                read_float(CAMERA_FLOAT_PORT_BASE + 2, self.eye_z),
            ],
            target: [
                read_float(CAMERA_FLOAT_PORT_BASE + 3, self.target_x),
                read_float(CAMERA_FLOAT_PORT_BASE + 4, self.target_y),
                read_float(CAMERA_FLOAT_PORT_BASE + 5, self.target_z),
            ],
            up: [0.0, 1.0, 0.0],
            fov_y_deg: read_float(CAMERA_FLOAT_PORT_BASE + 6, self.fov_y_deg),
            near:      read_float(CAMERA_FLOAT_PORT_BASE + 7, self.near),
            far:       read_float(CAMERA_FLOAT_PORT_BASE + 8, self.far),
        };
        let light_dir_raw = [
            read_float(LIGHT_FLOAT_PORT_BASE + 0, self.light_dir_x),
            read_float(LIGHT_FLOAT_PORT_BASE + 1, self.light_dir_y),
            read_float(LIGHT_FLOAT_PORT_BASE + 2, self.light_dir_z),
        ];

        let width  = self.width.max(16);
        let height = self.height.max(16);
        let aspect = width as f32 / height as f32;

        let view_proj = camera.view_projection(aspect);
        let light_dir = glam::Vec3::from_array(light_dir_raw).normalize_or_zero();

        let publish_result = render_offscreen(
            rs, ctx.node_id, ctx.gpu_tex_cache, mesh, mat,
            view_proj.to_cols_array_2d(),
            camera.eye,
            [light_dir.x, light_dir.y, light_dir.z],
            width, height,
        );

        match publish_result {
            Some(handle) => vec![(0, PortValue::GpuImage(handle))],
            None => vec![(0, PortValue::None)],
        }
    }

    fn save_state(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Ok(l) = serde_json::from_value::<Render3DNode>(state.clone()) { *self = l; }
    }

    fn render_with_context(&mut self, ui: &mut egui::Ui, ctx: &mut RenderContext) {
        // Top: Mesh + Material input ports (Material rides on a Text/JSON wire).
        for (idx, name, kind) in [
            (0usize, "Mesh",     PortKind::Mesh),
            (1,      "Material", PortKind::Text),
        ] {
            ui.horizontal(|ui| {
                inline_port_circle(
                    ui, ctx.node_id, idx, true,
                    ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                    kind,
                );
                ui.label(egui::RichText::new(name).small());
            });
        }

        ui.separator();

        // Resolution
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Size").small());
            let mut w = self.width as i32;
            let mut h = self.height as i32;
            if ui.add(egui::DragValue::new(&mut w).speed(8.0).range(64..=4096)).changed() {
                self.width  = w.max(16) as u32;
            }
            ui.label("×");
            if ui.add(egui::DragValue::new(&mut h).speed(8.0).range(64..=4096)).changed() {
                self.height = h.max(16) as u32;
            }
        });

        ui.collapsing("Camera", |ui| {
            xyz_row(
                ui, ctx, "Eye",
                [
                    (CAMERA_FLOAT_PORT_BASE + 0, &mut self.eye_x),
                    (CAMERA_FLOAT_PORT_BASE + 1, &mut self.eye_y),
                    (CAMERA_FLOAT_PORT_BASE + 2, &mut self.eye_z),
                ],
                0.05, -50.0..=50.0,
            );
            xyz_row(
                ui, ctx, "Target",
                [
                    (CAMERA_FLOAT_PORT_BASE + 3, &mut self.target_x),
                    (CAMERA_FLOAT_PORT_BASE + 4, &mut self.target_y),
                    (CAMERA_FLOAT_PORT_BASE + 5, &mut self.target_z),
                ],
                0.05, -50.0..=50.0,
            );
            scalar_row(ui, ctx, "FOV",  CAMERA_FLOAT_PORT_BASE + 6, &mut self.fov_y_deg, 0.5,   1.0..=170.0);
            scalar_row(ui, ctx, "Near", CAMERA_FLOAT_PORT_BASE + 7, &mut self.near,      0.01,  0.001..=10.0);
            scalar_row(ui, ctx, "Far",  CAMERA_FLOAT_PORT_BASE + 8, &mut self.far,       0.5,   1.0..=10000.0);
        });

        ui.collapsing("Light", |ui| {
            xyz_row(
                ui, ctx, "Dir",
                [
                    (LIGHT_FLOAT_PORT_BASE + 0, &mut self.light_dir_x),
                    (LIGHT_FLOAT_PORT_BASE + 1, &mut self.light_dir_y),
                    (LIGHT_FLOAT_PORT_BASE + 2, &mut self.light_dir_z),
                ],
                0.02, -1.0..=1.0,
            );
        });

        ui.separator();

        // Inline preview — renders the most recently published frame from
        // `gpu_tex_cache` via the same paint-callback path image_node.rs uses.
        // The callback reads from `GpuDisplayStore::prerendered`, which our
        // `queue_publish_node_output` → `publish` → `store_for_display` flow
        // populates. 1-frame latency is inherited from that pipeline.
        let preview_w = 240.0_f32.min(ui.available_width());
        let aspect = if self.height > 0 {
            self.width as f32 / self.height as f32
        } else { 1.0 };
        let preview_h = (preview_w / aspect.max(0.1)).clamp(80.0, 400.0);
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(preview_w, preview_h),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(rect, 4.0, egui::Color32::from_gray(15));
        let target_format = ui.ctx().data_mut(|d| {
            d.get_temp::<wgpu::TextureFormat>(egui::Id::new("wgpu_target_format"))
        }).unwrap_or(wgpu::TextureFormat::Bgra8UnormSrgb);
        ui.painter().add(eframe::egui_wgpu::Callback::new_paint_callback(
            rect,
            crate::gpu_image::GpuImageDisplayCallback {
                node_id: ctx.node_id,
                img: Arc::new(ImageData {
                    width: self.width.max(1),
                    height: self.height.max(1),
                    pixels: Vec::new(),
                }),
                target_format,
                gpu_source: Some((ctx.node_id, 0)),
            },
        ));

        ui.separator();

        // Output port
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Image").small());
            inline_port_circle(
                ui, ctx.node_id, 0, false,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Image,
            );
        });
    }
}

#[allow(dead_code)]
pub fn register(registry: &mut crate::node_trait::NodeRegistryInner) {
    registry.register("render_3d", |state| {
        let mut n = Render3DNode::default();
        n.load_state(state);
        Box::new(n)
    });
}

// ── Pipeline / target alloc helpers ─────────────────────────────────────────

fn pipeline_signature(double_sided: bool) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    "render_3d_v3".hash(&mut h);
    double_sided.hash(&mut h);
    h.finish()
}

fn create_render_pipeline(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    double_sided: bool,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("render_3d_shader"),
        source: wgpu::ShaderSource::Wgsl(RENDER_3D_WGSL.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("render_3d_pl"),
        bind_group_layouts: &[bind_group_layout],
        push_constant_ranges: &[],
    });
    let cull_mode = if double_sided { None } else { Some(wgpu::Face::Back) };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("render_3d_pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[vertex_buffer_layout()],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(TARGET_FORMAT.into())],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    })
}

fn create_color_target(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render_3d_color"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    (tex, view)
}

fn create_depth_target(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render_3d_depth"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    (tex, view)
}

fn create_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    // Bindings: 0 = uniform buffer, 1 = shared sampler, 2..=6 = albedo /
    // roughness / metallic / AO / emission texture views (in that order).
    let tex_entry = |b: u32| wgpu::BindGroupLayoutEntry {
        binding: b,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("render_3d_bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            tex_entry(2), // albedo
            tex_entry(3), // roughness
            tex_entry(4), // metallic
            tex_entry(5), // ao
            tex_entry(6), // emission
        ],
    })
}

fn create_dummy_white_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render_3d_dummy_white"),
        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        mip_level_count: 1, sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &tex, mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[255u8, 255, 255, 255],
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
        wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
    );
    tex.create_view(&Default::default())
}

// ── Render entry point ──────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj:  [[f32; 4]; 4],
    model:      [[f32; 4]; 4],
    normal_mat: [[f32; 4]; 4],
    light_dir:  [f32; 4],
    eye_pos:    [f32; 4],
    albedo:     [f32; 4],
    emission:   [f32; 4], // xyz emission, w = use_emission_tex
    rough_met:  [f32; 4], // x rough, y metal, z shader_id, w unused
    map_use:    [f32; 4], // x albedo, y roughness, z metallic, w ao
}

fn render_offscreen(
    rs: &egui_wgpu::RenderState,
    node_id: NodeId,
    tex_cache: &mut crate::gpu_image::GpuTextureCache,
    placed: &Arc<PlacedMesh>,
    material: &MaterialDef,
    view_proj: [[f32; 4]; 4],
    eye_pos: [f32; 3],
    light_dir: [f32; 3],
    width: u32,
    height: u32,
) -> Option<crate::graph::GpuImageHandle> {
    let mesh = &placed.mesh;
    let model      = placed.transform;
    let normal_mat = build_normal_matrix(model);
    let device = &rs.device;
    let queue  = &rs.queue;
    let signature = pipeline_signature(material.double_sided);

    // Resolve every map slot's upstream texture view. Per-frame snapshot
    // first (same path WGSL Viewer uses for image inputs), then upload
    // cache as a fallback for first-frame-after-wiring.
    let resolve_view = |slot: Option<crate::material::MaterialTexRef>| -> Option<wgpu::TextureView> {
        let snap = slot.and_then(|t| {
            crate::gpu_image::frame_snapshot_get_view(t.node_id, t.port).map(|(v, _, _)| v)
        });
        snap.or_else(|| {
            slot.and_then(|t| {
                tex_cache.get_node_output(t.node_id, t.port).map(|(v, _, _)| v.clone())
            })
        })
    };
    let view_albedo    = resolve_view(material.maps.albedo);
    let view_roughness = resolve_view(material.maps.roughness);
    let view_metallic  = resolve_view(material.maps.metallic);
    let view_ao        = resolve_view(material.maps.ao);
    let view_emission  = resolve_view(material.maps.emission);
    let use_albedo    = view_albedo.is_some();
    let use_roughness = view_roughness.is_some();
    let use_metallic  = view_metallic.is_some();
    let use_ao        = view_ao.is_some();
    let use_emission  = view_emission.is_some();

    let mut renderer = rs.renderer.write();
    if renderer.callback_resources.get::<Render3DStore>().is_none() {
        renderer.callback_resources.insert(Render3DStore::default());
    }
    let store = renderer.callback_resources.get_mut::<Render3DStore>()?;

    let needs_init = !store.nodes.contains_key(&node_id);
    if needs_init {
        let bgl = create_bind_group_layout(device);
        let pipeline = create_render_pipeline(device, &bgl, material.double_sided);
        let (color_tex, color_view) = create_color_target(device, width, height);
        let (depth_tex, depth_view) = create_depth_target(device, width, height);
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("render_3d_ub"),
            size: UNIFORM_BUF_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("render_3d_sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let dummy_white_view = create_dummy_white_texture(device, queue);
        store.nodes.insert(node_id, Render3DGpu {
            pipeline,
            bind_group_layout: bgl,
            uniform_buffer,
            sampler,
            dummy_white_view,
            color_texture: color_tex,
            color_view,
            depth_texture: depth_tex,
            depth_view,
            size: (width, height),
            mesh_cache: GpuMeshCache::new(),
            pipeline_signature: signature,
        });
    }

    let gpu = store.nodes.get_mut(&node_id)?;

    if gpu.pipeline_signature != signature {
        gpu.pipeline = create_render_pipeline(device, &gpu.bind_group_layout, material.double_sided);
        gpu.pipeline_signature = signature;
    }

    if gpu.size != (width, height) {
        let (color_tex, color_view) = create_color_target(device, width, height);
        let (depth_tex, depth_view) = create_depth_target(device, width, height);
        gpu.color_texture = color_tex;
        gpu.color_view    = color_view;
        gpu.depth_texture = depth_tex;
        gpu.depth_view    = depth_view;
        gpu.size = (width, height);
    }

    gpu.mesh_cache.begin_frame();
    let mesh_entry = gpu.mesh_cache.get_or_upload(device, mesh);
    let vbuf = mesh_entry.vertex_buffer.clone();
    let ibuf = mesh_entry.index_buffer.clone();
    let icount = mesh_entry.index_count;

    let f01 = |b: bool| if b { 1.0 } else { 0.0 };
    let uniforms = Uniforms {
        view_proj,
        model,
        normal_mat,
        light_dir: [light_dir[0], light_dir[1], light_dir[2], 0.0],
        eye_pos:   [eye_pos[0],   eye_pos[1],   eye_pos[2],   0.0],
        albedo:    material.albedo,
        emission:  [material.emission[0], material.emission[1], material.emission[2], f01(use_emission)],
        rough_met: [
            material.roughness,
            material.metallic,
            material.shader_type.shader_id() as f32,
            0.0,
        ],
        map_use: [f01(use_albedo), f01(use_roughness), f01(use_metallic), f01(use_ao)],
    };
    queue.write_buffer(&gpu.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

    // Build the per-frame bind group: every map slot binds either the
    // resolved upstream view or a 1×1 white texture (white = "no effect"
    // when multiplied into the scalar/color path in the fragment shader).
    let dummy = &gpu.dummy_white_view;
    let bind_albedo    = view_albedo.as_ref().unwrap_or(dummy);
    let bind_roughness = view_roughness.as_ref().unwrap_or(dummy);
    let bind_metallic  = view_metallic.as_ref().unwrap_or(dummy);
    let bind_ao        = view_ao.as_ref().unwrap_or(dummy);
    let bind_emission  = view_emission.as_ref().unwrap_or(dummy);
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("render_3d_bg"),
        layout: &gpu.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: gpu.uniform_buffer.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&gpu.sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(bind_albedo) },
            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(bind_roughness) },
            wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(bind_metallic) },
            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(bind_ao) },
            wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(bind_emission) },
        ],
    });

    let color_view = gpu.color_view.clone();
    let depth_view = gpu.depth_view.clone();
    let pipeline   = &gpu.pipeline;

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("render_3d_encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("render_3d_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.02, g: 0.02, b: 0.04, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.set_vertex_buffer(0, vbuf.slice(..));
        pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..icount, 0, 0..1);
    }
    queue.submit(Some(encoder.finish()));

    let texture_clone = gpu.color_texture.clone();
    drop(renderer);

    // Synchronous publish: caches the texture AND mirrors it into the
    // display store *this frame*. The previous deferred-queue path
    // (`queue_publish_node_output`) lands the publish in the next frame's
    // `begin_frame` drain — for an always-cache-miss producer like 3D
    // Render, that left a window where `prerendered` could be empty,
    // showing as occasional black frames in the inline preview and any
    // downstream Visual Output.  Direct `publish` eliminates the lag and
    // guarantees `prerendered` is populated when the display callback runs.
    tex_cache.publish(node_id, 0, texture_clone, width, height, rs);
    let frame_stamp = tex_cache.frame_stamp(node_id, 0);

    Some(crate::graph::GpuImageHandle {
        node_id,
        port: 0,
        width,
        height,
        frame_stamp,
    })
}

// ── UI helpers (compact rows) ───────────────────────────────────────────────

/// One labelled scalar row: port circle + label + DragValue (or wired-value
/// label). Used for FOV / Near / Far where each axis is independent.
fn scalar_row(
    ui: &mut egui::Ui,
    ctx: &mut RenderContext,
    label: &str,
    port_idx: usize,
    value: &mut f32,
    speed: f32,
    range: std::ops::RangeInclusive<f32>,
) {
    let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == port_idx);
    ui.horizontal(|ui| {
        inline_port_circle(
            ui, ctx.node_id, port_idx, true,
            ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
            PortKind::Number,
        );
        ui.label(egui::RichText::new(label).small());
        if wired {
            let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, port_idx);
            ui.label(egui::RichText::new(format!("{}", v)).small().color(ui.visuals().hyperlink_color));
        } else {
            ui.add(egui::DragValue::new(value).speed(speed).range(range).fixed_decimals(2));
        }
    });
}

/// One labelled XYZ row: header label + three (port + DragValue) cells side
/// by side. Used for Eye / Target / Light Dir where the three axes form a
/// vector and read better on one line.
fn xyz_row(
    ui: &mut egui::Ui,
    ctx: &mut RenderContext,
    label: &str,
    cells: [(usize, &mut f32); 3],
    speed: f32,
    range: std::ops::RangeInclusive<f32>,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).small());
        for (port_idx, value) in cells {
            let wired = ctx.connections.iter().any(|c| c.to_node == ctx.node_id && c.to_port == port_idx);
            inline_port_circle(
                ui, ctx.node_id, port_idx, true,
                ctx.connections, ctx.port_positions, ctx.dragging_from, ctx.pending_disconnects,
                PortKind::Number,
            );
            if wired {
                let v = crate::graph::Graph::static_input_value(ctx.connections, ctx.values, ctx.node_id, port_idx)
                    .as_float();
                ui.label(egui::RichText::new(format!("{:.2}", v)).small()
                    .color(ui.visuals().hyperlink_color));
            } else {
                // Compact width so 3 cells + the header fit in the node body.
                ui.add_sized(
                    [46.0, 18.0],
                    egui::DragValue::new(value).speed(speed).range(range.clone()).fixed_decimals(2),
                );
            }
        }
    });
}
