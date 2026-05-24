// 3D mesh data structures and primitive generators.
//
// `MeshData` is the canonical CPU-side mesh representation shared by procedural
// primitives (Cube/Sphere/Plane/Torus) and GLTF imports. `Vertex` layout is
// fixed so the 3D Render node's vertex buffer layout never has to change.
//
// `CameraParams` is a small Copy struct passed through `PortValue::Camera`.
// Matrix math uses `glam` (right-handed, column-major). wgpu uses depth range
// [0, 1] so we use `Mat4::perspective_rh` (which produces that range, unlike
// the GL `_gl` variants).

use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::wgpu;
use glam::{Mat4, Vec3};
use std::sync::Arc;

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal:   [f32; 3],
    pub uv:       [f32; 2],
    pub tangent:  [f32; 4],
}

impl Vertex {
    pub const fn new(position: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> Self {
        Self { position, normal, uv, tangent: [1.0, 0.0, 0.0, 1.0] }
    }

    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute { offset: 0,  shader_location: 0, format: wgpu::VertexFormat::Float32x3 },
                wgpu::VertexAttribute { offset: 12, shader_location: 1, format: wgpu::VertexFormat::Float32x3 },
                wgpu::VertexAttribute { offset: 24, shader_location: 2, format: wgpu::VertexFormat::Float32x2 },
                wgpu::VertexAttribute { offset: 32, shader_location: 3, format: wgpu::VertexFormat::Float32x4 },
            ],
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Aabb {
    pub fn from_vertices(verts: &[Vertex]) -> Self {
        if verts.is_empty() {
            return Aabb::default();
        }
        let mut min = verts[0].position;
        let mut max = min;
        for v in &verts[1..] {
            for i in 0..3 {
                if v.position[i] < min[i] { min[i] = v.position[i]; }
                if v.position[i] > max[i] { max[i] = v.position[i]; }
            }
        }
        Aabb { min, max }
    }

    pub fn center(&self) -> [f32; 3] {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }

    pub fn radius(&self) -> f32 {
        let dx = self.max[0] - self.min[0];
        let dy = self.max[1] - self.min[1];
        let dz = self.max[2] - self.min[2];
        (dx*dx + dy*dy + dz*dz).sqrt() * 0.5
    }
}

#[derive(Debug)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices:  Vec<u32>,
    pub bounds:   Aabb,
    pub name:     Option<String>,
}

impl MeshData {
    pub fn new(vertices: Vec<Vertex>, indices: Vec<u32>, name: Option<String>) -> Arc<Self> {
        let bounds = Aabb::from_vertices(&vertices);
        Arc::new(Self { vertices, indices, bounds, name })
    }
}

/// A mesh + its world-space placement. Used as the wire payload for
/// `PortValue::Mesh` so a producer (3D Shape, future GLTF Loader / Transform
/// Mesh) can hand out the same heavy `Arc<MeshData>` while attaching a cheap
/// per-frame transform on top. `GpuMeshCache` keys on the inner
/// `Arc<MeshData>` pointer, so wiggling a position slider doesn't re-upload
/// the vertex buffer.
#[derive(Debug, Clone)]
pub struct PlacedMesh {
    pub mesh: Arc<MeshData>,
    /// Column-major 4×4 model matrix. Identity = unit-scale at origin.
    pub transform: [[f32; 4]; 4],
}

impl PlacedMesh {
    pub fn new(mesh: Arc<MeshData>) -> Arc<Self> {
        Arc::new(Self { mesh, transform: identity_mat4() })
    }

    pub fn with_transform(mesh: Arc<MeshData>, transform: [[f32; 4]; 4]) -> Arc<Self> {
        Arc::new(Self { mesh, transform })
    }
}

pub fn identity_mat4() -> [[f32; 4]; 4] {
    [[1.0, 0.0, 0.0, 0.0],
     [0.0, 1.0, 0.0, 0.0],
     [0.0, 0.0, 1.0, 0.0],
     [0.0, 0.0, 0.0, 1.0]]
}

/// Build a model matrix from translation, scale, and Euler rotation
/// (degrees, applied as Y → X → Z so "yaw, pitch, roll" reads naturally).
pub fn build_model_matrix(
    position: [f32; 3],
    scale:    [f32; 3],
    rotation_deg: [f32; 3],
) -> [[f32; 4]; 4] {
    let t = glam::Mat4::from_translation(glam::Vec3::from_array(position));
    let r = glam::Mat4::from_euler(
        glam::EulerRot::YXZ,
        rotation_deg[1].to_radians(),
        rotation_deg[0].to_radians(),
        rotation_deg[2].to_radians(),
    );
    let s = glam::Mat4::from_scale(glam::Vec3::from_array(scale));
    (t * r * s).to_cols_array_2d()
}

/// Inverse-transpose of the upper 3×3 of a model matrix, padded into a
/// 4×4 with the last row/column = identity. Used for transforming normals
/// correctly under non-uniform scale.
pub fn build_normal_matrix(model: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let m = glam::Mat4::from_cols_array_2d(&model);
    let n3 = glam::Mat3::from_mat4(m).inverse().transpose();
    glam::Mat4::from_mat3(n3).to_cols_array_2d()
}

// ── Camera ──────────────────────────────────────────────────────────────────

#[derive(Copy, Clone, Debug)]
pub struct CameraParams {
    pub eye:    [f32; 3],
    pub target: [f32; 3],
    pub up:     [f32; 3],
    pub fov_y_deg: f32,
    pub near:   f32,
    pub far:    f32,
}

impl Default for CameraParams {
    fn default() -> Self {
        Self {
            eye:        [3.0, 2.0, 4.0],
            target:     [0.0, 0.0, 0.0],
            up:         [0.0, 1.0, 0.0],
            fov_y_deg:  45.0,
            near:       0.1,
            far:        100.0,
        }
    }
}

impl CameraParams {
    pub fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(
            Vec3::from_array(self.eye),
            Vec3::from_array(self.target),
            Vec3::from_array(self.up),
        )
    }

    pub fn projection_matrix(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov_y_deg.to_radians(), aspect, self.near, self.far)
    }

    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        self.projection_matrix(aspect) * self.view_matrix()
    }
}

// ── Primitive generators ────────────────────────────────────────────────────

pub fn primitive_cube(size: f32) -> Arc<MeshData> {
    let h = size * 0.5;
    let mut verts = Vec::with_capacity(24);
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([ 0.0, 0.0, 1.0], [ 1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), // +Z front
        ([ 0.0, 0.0,-1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), // -Z back
        ([ 1.0, 0.0, 0.0], [ 0.0, 0.0,-1.0], [0.0, 1.0, 0.0]), // +X right
        ([-1.0, 0.0, 0.0], [ 0.0, 0.0, 1.0], [0.0, 1.0, 0.0]), // -X left
        ([ 0.0, 1.0, 0.0], [ 1.0, 0.0, 0.0], [0.0, 0.0,-1.0]), // +Y top
        ([ 0.0,-1.0, 0.0], [ 1.0, 0.0, 0.0], [0.0, 0.0, 1.0]), // -Y bottom
    ];
    for (n, u_axis, v_axis) in faces {
        let center = [n[0]*h, n[1]*h, n[2]*h];
        let corners = [
            (-1.0, -1.0, [0.0, 0.0]),
            ( 1.0, -1.0, [1.0, 0.0]),
            ( 1.0,  1.0, [1.0, 1.0]),
            (-1.0,  1.0, [0.0, 1.0]),
        ];
        for (uu, vv, uv) in corners {
            let p = [
                center[0] + u_axis[0]*uu*h + v_axis[0]*vv*h,
                center[1] + u_axis[1]*uu*h + v_axis[1]*vv*h,
                center[2] + u_axis[2]*uu*h + v_axis[2]*vv*h,
            ];
            verts.push(Vertex::new(p, n, uv));
        }
    }
    let mut indices = Vec::with_capacity(36);
    for f in 0..6u32 {
        let b = f * 4;
        indices.extend_from_slice(&[b, b+1, b+2, b, b+2, b+3]);
    }
    MeshData::new(verts, indices, Some("Cube".into()))
}

pub fn primitive_plane(size: f32) -> Arc<MeshData> {
    let h = size * 0.5;
    let n = [0.0, 1.0, 0.0];
    let verts = vec![
        Vertex::new([-h, 0.0, -h], n, [0.0, 0.0]),
        Vertex::new([ h, 0.0, -h], n, [1.0, 0.0]),
        Vertex::new([ h, 0.0,  h], n, [1.0, 1.0]),
        Vertex::new([-h, 0.0,  h], n, [0.0, 1.0]),
    ];
    let indices = vec![0, 1, 2, 0, 2, 3];
    MeshData::new(verts, indices, Some("Plane".into()))
}

pub fn primitive_sphere(radius: f32, lon_segments: u32, lat_segments: u32) -> Arc<MeshData> {
    let lon = lon_segments.max(3);
    let lat = lat_segments.max(2);
    let mut verts = Vec::with_capacity(((lon + 1) * (lat + 1)) as usize);
    for j in 0..=lat {
        let v = j as f32 / lat as f32;
        let theta = v * std::f32::consts::PI;
        let (st, ct) = theta.sin_cos();
        for i in 0..=lon {
            let u = i as f32 / lon as f32;
            let phi = u * std::f32::consts::TAU;
            let (sp, cp) = phi.sin_cos();
            let n = [st * cp, ct, st * sp];
            let p = [n[0] * radius, n[1] * radius, n[2] * radius];
            verts.push(Vertex::new(p, n, [u, 1.0 - v]));
        }
    }
    let stride = lon + 1;
    let mut indices = Vec::with_capacity((lon * lat * 6) as usize);
    for j in 0..lat {
        for i in 0..lon {
            let a = j * stride + i;
            let b = a + 1;
            let c = a + stride;
            let d = c + 1;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    MeshData::new(verts, indices, Some("Sphere".into()))
}

// ── GLTF / GLB loader ───────────────────────────────────────────────────────

/// Load a `.gltf` or `.glb` file and combine every mesh / primitive into a
/// single `MeshData` (vertex layout matches `Vertex`). Phase 1: ignores
/// materials, animations, and skins — those land with the Phase 2 GLTF
/// Loader node, which exposes per-mesh outputs and embedded materials.
///
/// Errors (file missing, parse failure, no primitives) collapse to `None`
/// so the calling node can show a "—" state instead of crashing.
pub fn load_gltf_or_glb(path: &str) -> Option<Arc<MeshData>> {
    let (doc, buffers, _images) = gltf::import(path).ok()?;

    let mut all_vertices: Vec<Vertex> = Vec::new();
    let mut all_indices:  Vec<u32>    = Vec::new();

    for mesh in doc.meshes() {
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));

            let positions: Vec<[f32; 3]> = match reader.read_positions() {
                Some(it) => it.collect(),
                None => continue, // skip primitives without positions
            };
            if positions.is_empty() { continue; }

            let normals: Vec<[f32; 3]> = reader.read_normals()
                .map(|it| it.collect())
                .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; positions.len()]);
            let uvs: Vec<[f32; 2]> = reader.read_tex_coords(0)
                .map(|it| it.into_f32().collect())
                .unwrap_or_else(|| vec![[0.0, 0.0]; positions.len()]);

            let local_indices: Vec<u32> = reader.read_indices()
                .map(|it| it.into_u32().collect())
                .unwrap_or_else(|| (0..positions.len() as u32).collect());

            let base = all_vertices.len() as u32;
            for i in 0..positions.len() {
                let n = *normals.get(i).unwrap_or(&[0.0, 1.0, 0.0]);
                let uv = *uvs.get(i).unwrap_or(&[0.0, 0.0]);
                all_vertices.push(Vertex::new(positions[i], n, uv));
            }
            for li in local_indices {
                all_indices.push(li + base);
            }
        }
    }

    if all_vertices.is_empty() { return None; }

    let name = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(String::from);
    Some(MeshData::new(all_vertices, all_indices, name))
}

pub fn primitive_torus(major_radius: f32, minor_radius: f32, ring_segments: u32, tube_segments: u32) -> Arc<MeshData> {
    let r = ring_segments.max(3);
    let t = tube_segments.max(3);
    let mut verts = Vec::with_capacity(((r + 1) * (t + 1)) as usize);
    for i in 0..=r {
        let u = i as f32 / r as f32;
        let phi = u * std::f32::consts::TAU;
        let (sp, cp) = phi.sin_cos();
        for j in 0..=t {
            let v = j as f32 / t as f32;
            let theta = v * std::f32::consts::TAU;
            let (sth, cth) = theta.sin_cos();
            let cx = major_radius * cp;
            let cz = major_radius * sp;
            let n = [cth * cp, sth, cth * sp];
            let p = [
                cx + minor_radius * cth * cp,
                minor_radius * sth,
                cz + minor_radius * cth * sp,
            ];
            verts.push(Vertex::new(p, n, [u, v]));
        }
    }
    let stride = t + 1;
    let mut indices = Vec::with_capacity((r * t * 6) as usize);
    for i in 0..r {
        for j in 0..t {
            let a = i * stride + j;
            let b = a + 1;
            let c = a + stride;
            let d = c + 1;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    MeshData::new(verts, indices, Some("Torus".into()))
}
