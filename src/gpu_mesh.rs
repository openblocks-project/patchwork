// GPU mesh cache — uploads `MeshData` to wgpu vertex/index buffers and reuses
// them across frames. Modeled on `GpuTextureCache` but simpler:
// no per-node-output cache, no display store, no HAL snapshot. The 3D Render
// node consumes meshes via `evaluate_with_ctx`, which has direct access to the
// cache, so the publish-queue machinery in `gpu_image.rs` is not needed here.

use crate::graph::NodeId;
use crate::mesh::{MeshData, Vertex};
use eframe::egui_wgpu::wgpu;
use eframe::egui_wgpu::wgpu::util::DeviceExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Plain-data handle to a GPU mesh stored in `GpuMeshCache`. Mirrors
/// `GpuImageHandle` — cheap to clone, no GPU resources held.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct GpuMeshHandle {
    pub node_id: NodeId,
    pub port:    usize,
    pub vertex_count: u32,
    pub index_count:  u32,
    pub frame_stamp:  u64,
}

pub struct CachedMesh {
    pub vertex_buffer: wgpu::Buffer,
    pub index_buffer:  wgpu::Buffer,
    pub vertex_count:  u32,
    pub index_count:   u32,
    pub frame:         u64,
}

pub struct GpuMeshCache {
    /// Keyed by `Arc<MeshData>` pointer address — same trick as
    /// `GpuTextureCache.entries` for `Arc<ImageData>`.
    entries: HashMap<usize, CachedMesh>,
    current_frame: u64,
}

impl GpuMeshCache {
    pub fn new() -> Self {
        Self { entries: HashMap::new(), current_frame: 0 }
    }

    pub fn begin_frame(&mut self) {
        self.current_frame += 1;
        let cf = self.current_frame;
        self.entries.retain(|_, v| cf - v.frame <= 2);
    }

    pub fn current_frame(&self) -> u64 { self.current_frame }

    /// Upload `mesh` if not already cached for this frame, return the cached entry.
    pub fn get_or_upload(
        &mut self,
        device: &wgpu::Device,
        mesh: &Arc<MeshData>,
    ) -> &CachedMesh {
        let key = Arc::as_ptr(mesh) as usize;
        let cf = self.current_frame;

        if !self.entries.contains_key(&key) {
            let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("patchwork.gpu_mesh.vertex_buffer"),
                contents: bytemuck::cast_slice(&mesh.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
            let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("patchwork.gpu_mesh.index_buffer"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
            let cached = CachedMesh {
                vertex_buffer,
                index_buffer,
                vertex_count: mesh.vertices.len() as u32,
                index_count:  mesh.indices.len() as u32,
                frame: cf,
            };
            self.entries.insert(key, cached);
        } else if let Some(entry) = self.entries.get_mut(&key) {
            entry.frame = cf;
        }
        self.entries.get(&key).unwrap()
    }
}

/// Standard vertex buffer layout for all 3D meshes. Matches `Vertex` in
/// `src/mesh.rs`. Pulled out so the 3D Render node and any future mesh-aware
/// pipeline can reference the same layout.
pub fn vertex_buffer_layout() -> wgpu::VertexBufferLayout<'static> {
    Vertex::layout()
}
