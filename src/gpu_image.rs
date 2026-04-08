// GPU-accelerated image processing using wgpu
// Upload images as textures → blend/effect via WGSL shader → display or readback

use crate::graph::{ImageData, NodeId};
use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

// ── Pending node invalidations ──────────────────────────────────────────────
// Render functions for Camera/VideoPlayer don't have access to the
// GpuTextureCache directly, so they push the node id here when the source
// changes (camera device switch, video file load). The main update loop
// drains this set in `begin_frame` before any GPU work runs.

thread_local! {
    static PENDING_NODE_INVALIDATIONS: RefCell<HashSet<NodeId>> = RefCell::new(HashSet::new());

    /// Pending publish queue used by GPU producers (e.g. WGSL Viewer's
    /// `render_offscreen`) that don't have direct access to the
    /// `GpuTextureCache`. Drained at the top of the next frame's
    /// `begin_frame` (1 frame of latency, matching the existing WGSL
    /// injection latency).
    static PENDING_CACHE_PUBLISHES: RefCell<Vec<(NodeId, usize, wgpu::Texture, u32, u32)>>
        = RefCell::new(Vec::new());

    /// Per-WgslViewer (or any GPU producer) hint set by the eval loop:
    /// `true` ⇒ at least one downstream consumer needs CPU pixel bytes,
    /// so the producer must readback this frame. Default `true` (safe).
    static CPU_CONSUMER_HINTS: RefCell<HashMap<NodeId, bool>>
        = RefCell::new(HashMap::new());

    /// Per-frame snapshot of the GPU texture cache, indexed by `(NodeId, port)`.
    /// Populated by `GpuTextureCache::snapshot_for_render_frame` at the end of
    /// `begin_frame`, and read by render-time consumers (specifically the
    /// WGSL Viewer's `render_offscreen`, which needs to resolve `External`
    /// image input slots without having the cache plumbed as a parameter).
    /// Cleared at the start of each `begin_frame`.
    static FRAME_TEX_SNAPSHOT: RefCell<HashMap<(NodeId, usize), (wgpu::TextureView, u32, u32)>>
        = RefCell::new(HashMap::new());
}

/// Read a per-frame snapshot of an upstream GPU output texture by
/// `(node_id, port)`. Returns `None` if the source did not produce a
/// cached entry by snapshot time.
///
/// Used by WGSL Viewer's offscreen render path to resolve `External`
/// image inputs without needing direct access to `GpuTextureCache`.
/// The snapshot has the same 1-frame latency as the rest of the GPU
/// publish-queue path: a node that publishes during frame N becomes
/// readable from this function during frame N+1.
pub fn frame_snapshot_get_view(
    node_id: NodeId,
    port: usize,
) -> Option<(wgpu::TextureView, u32, u32)> {
    FRAME_TEX_SNAPSHOT.with(|m| m.borrow().get(&(node_id, port)).cloned())
}

/// Mark a node's GPU cache entries as stale. Safe to call from anywhere on
/// the GUI thread (e.g. inside a node's `render` fn).
pub fn request_node_invalidation(node_id: NodeId) {
    PENDING_NODE_INVALIDATIONS.with(|s| { s.borrow_mut().insert(node_id); });
}

/// Queue a producer's freshly rendered output texture for publication into
/// `GpuTextureCache.entries[(node_id, port)]` at the start of the next
/// frame. The texture is moved into the queue and stays alive until drained.
pub fn queue_publish_node_output(
    node_id: NodeId,
    port: usize,
    texture: wgpu::Texture,
    width: u32,
    height: u32,
) {
    PENDING_CACHE_PUBLISHES.with(|q| {
        q.borrow_mut().push((node_id, port, texture, width, height));
    });
}

/// Set the per-node CPU consumer hint. Called by the eval loop for every
/// GPU producer node before its render function runs.
pub fn set_cpu_consumer_hint(node_id: NodeId, needs_cpu: bool) {
    CPU_CONSUMER_HINTS.with(|m| { m.borrow_mut().insert(node_id, needs_cpu); });
}

/// Read the CPU consumer hint for a node. Returns `None` if not set
/// (callers should default to `true` for safety).
pub fn cpu_consumer_hint(node_id: NodeId) -> Option<bool> {
    CPU_CONSUMER_HINTS.with(|m| m.borrow().get(&node_id).copied())
}

/// Clear all CPU consumer hints. Called at the top of every eval pass to
/// avoid stale hints when graph topology changes.
pub fn clear_cpu_consumer_hints() {
    CPU_CONSUMER_HINTS.with(|m| m.borrow_mut().clear());
}

/// Maximum number of output ports per node we will sweep when invalidating.
/// cache_node_output uses key = node_id*31 + port; we have to enumerate
/// because we don't track which ports were ever cached.
const MAX_NODE_PORTS_FOR_INVALIDATION: usize = 16;

// ── GPU Texture Cache ───────────────────────────────────────────────────────
// Avoids redundant CPU→GPU uploads when the same Arc<ImageData> flows through
// multiple GPU nodes in one frame. Keyed by Arc pointer address.

struct CachedTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    frame: u64,
    /// Original (node_id, port) used for the snapshot rebuild.
    /// `Some` for entries inserted via `cache_node_output` (per-node-output
    /// cache); `None` for entries keyed by `Arc<ImageData>` pointer (the
    /// upload-cache path used by `get_or_upload`/`cache_output`). Only the
    /// `Some` entries are exported into the per-frame thread-local
    /// snapshot used by the WGSL Viewer's image-input feedback feature.
    src_node: Option<NodeId>,
    src_port: usize,
}

pub struct GpuTextureCache {
    entries: HashMap<u64, CachedTexture>,
    current_frame: u64,
}

impl GpuTextureCache {
    pub fn new() -> Self {
        Self { entries: HashMap::new(), current_frame: 0 }
    }

    /// Call at start of each frame
    pub fn begin_frame(&mut self, render_state: Option<&eframe::egui_wgpu::RenderState>) {
        self.current_frame += 1;
        // Drop the previous frame's read snapshot before any new
        // publishes/invalidations land. The WGSL Viewer's image-input
        // resolution reads from this snapshot during render_content; we
        // repopulate it at the end of begin_frame so consumers see all
        // freshly drained publishes from the previous frame.
        FRAME_TEX_SNAPSHOT.with(|m| m.borrow_mut().clear());
        // Evict textures older than 2 frames
        self.entries.retain(|_, v| self.current_frame - v.frame <= 2);

        // Drain any pending node-level invalidations queued from render
        // functions that didn't have access to the cache directly
        // (camera device switch, video file load, node deletion).
        let pending: Vec<NodeId> = PENDING_NODE_INVALIDATIONS.with(|s| {
            s.borrow_mut().drain().collect()
        });
        for nid in pending {
            self.invalidate_node(nid, render_state);
        }

        // Clear prerendered display textures from previous frame BEFORE
        // draining pending publishes. The drain calls `publish` which
        // calls `store_for_display`, which writes into `prerendered`; if
        // we cleared after the drain we'd immediately wipe the entries we
        // just published, leaving GPU producers (anything using
        // `queue_publish_node_output`) invisible to
        // `GpuImageDisplayCallback`.
        if let Some(rs) = render_state {
            let mut renderer = rs.renderer.write();
            if let Some(store) = renderer.callback_resources.get_mut::<GpuDisplayStore>() {
                store.prerendered.clear();
                store.instances.clear();
            }
        }

        // Drain pending GPU publishes from producers like WGSL Viewer that
        // can't reach the cache directly. Done AFTER invalidation so a
        // producer that was both invalidated and re-rendered in the same
        // frame ends up with its fresh texture in the cache.
        let pending_pubs: Vec<(NodeId, usize, wgpu::Texture, u32, u32)> =
            PENDING_CACHE_PUBLISHES.with(|q| std::mem::take(&mut *q.borrow_mut()));
        if let Some(rs) = render_state {
            for (nid, port, tex, w, h) in pending_pubs {
                self.publish(nid, port, tex, w, h, rs);
            }
        }

        // Snapshot every cached entry into the per-frame thread-local map so
        // render-time consumers (WGSL Viewer image inputs) can resolve
        // upstream textures without needing &GpuTextureCache plumbed through
        // render_content. wgpu::TextureView clones are cheap (Arc-counted).
        FRAME_TEX_SNAPSHOT.with(|m| {
            let mut map = m.borrow_mut();
            map.clear();
            for ct in self.entries.values() {
                if let Some(nid) = ct.src_node {
                    map.insert(
                        (nid, ct.src_port),
                        (ct.view.clone(), ct.width, ct.height),
                    );
                }
            }
        });
    }

    /// Drop every cached GPU texture and display-store entry that belongs
    /// to a particular node. Call when the node's source changes (camera
    /// device switch, video file open) or when the node is deleted so VRAM
    /// is released immediately rather than waiting for frame-LRU.
    ///
    /// This also walks every per-node-type GPU pipeline store stashed
    /// inside `egui_wgpu`'s callback_resources (Image Effects, Blend,
    /// Color Curves, WGSL Viewer, Image Style) so that pipelines,
    /// uniform buffers, samplers, and bind groups belonging to a deleted
    /// node are released immediately rather than leaking forever.
    pub fn invalidate_node(
        &mut self,
        node_id: NodeId,
        render_state: Option<&eframe::egui_wgpu::RenderState>,
    ) {
        // Per-node-output entries from cache_node_output.
        for port in 0..MAX_NODE_PORTS_FOR_INVALIDATION {
            let key = (node_id as u64).wrapping_mul(31).wrapping_add(port as u64);
            self.entries.remove(&key);
        }
        // Display + per-node-type pipeline store entries.
        if let Some(rs) = render_state {
            let mut renderer = rs.renderer.write();
            let cr = &mut renderer.callback_resources;
            if let Some(store) = cr.get_mut::<GpuDisplayStore>() {
                store.prerendered.remove(&node_id);
                store.instances.remove(&node_id);
            }
            crate::nodes::wgsl_viewer::cleanup_node(cr, node_id);
            crate::nodes::color_curves::cleanup_node(cr, node_id);
            crate::nodes::blend::cleanup_node(cr, node_id);
            crate::nodes::image_effects::cleanup_node(cr, node_id);
            crate::nodes::image_style_node::cleanup_node(cr, node_id);
        }
    }

    /// Wipe every cached entry and every per-pipeline store. Called
    /// from `PatchworkApp::clear_caches_if_dirty` whenever the graph
    /// is replaced wholesale (load, restore, undo, redo) so a fresh
    /// project's nodes can't read back the previous project's textures
    /// keyed by `(node_id, port)` — `fix_next_id` restarts ids at 1,
    /// so without this every Open would race the 2-frame LRU and
    /// briefly leak the old project's pixels into the new one.
    pub fn clear_all(
        &mut self,
        render_state: Option<&eframe::egui_wgpu::RenderState>,
    ) {
        self.entries.clear();
        if let Some(rs) = render_state {
            let mut renderer = rs.renderer.write();
            let cr = &mut renderer.callback_resources;
            if let Some(store) = cr.get_mut::<GpuDisplayStore>() {
                store.prerendered.clear();
                store.instances.clear();
            }
            crate::nodes::wgsl_viewer::cleanup_all(cr);
            crate::nodes::color_curves::cleanup_all(cr);
            crate::nodes::blend::cleanup_all(cr);
            crate::nodes::image_effects::cleanup_all(cr);
            crate::nodes::image_style_node::cleanup_all(cr);
        }
    }

    /// Get a cached GPU texture for an Arc<ImageData>, or upload it.
    /// Returns a texture view that can be bound to a shader.
    pub fn get_or_upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        img: &Arc<ImageData>,
    ) -> &wgpu::TextureView {
        let key = Arc::as_ptr(img) as u64;
        // Check if cached and same dimensions
        let needs_upload = self.entries.get(&key)
            .map(|c| c.width != img.width || c.height != img.height)
            .unwrap_or(true);

        if needs_upload {
            let texture = upload_texture(device, queue, img, "cached_tex");
            let view = texture.create_view(&Default::default());
            self.entries.insert(key, CachedTexture {
                texture, view, width: img.width, height: img.height, frame: self.current_frame,
                src_node: None, src_port: 0,
            });
        } else {
            // Update frame stamp to prevent eviction
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.frame = self.current_frame;
            }
        }
        &self.entries.get(&key).unwrap().view
    }

    /// Tag a previously-uploaded `get_or_upload` entry with its source
    /// `(node_id, port)` so it shows up in the per-frame thread-local snapshot
    /// used by WGSL Viewer External image inputs.
    ///
    /// `get_or_upload` keys entries by `Arc<ImageData>` pointer and leaves
    /// `src_node = None`, which means by default Camera/Video frames live in
    /// the cache but are invisible to `frame_snapshot_get_view`. Producers
    /// (Camera, VideoPlayer, ML, etc.) should call this immediately after
    /// `get_or_upload` so downstream External slots can resolve them.
    pub fn mark_node_source(&mut self, img: &Arc<ImageData>, node_id: crate::graph::NodeId, port: usize) {
        let key = Arc::as_ptr(img) as u64;
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.src_node = Some(node_id);
            entry.src_port = port;
        }
    }

    /// Store a rendered output texture in the cache, keyed by the output Arc<ImageData>.
    /// Call after readback — the next node in the chain can reuse this texture.
    pub fn cache_output(
        &mut self,
        img: &Arc<ImageData>,
        texture: wgpu::Texture,
    ) {
        let key = Arc::as_ptr(img) as u64;
        let view = texture.create_view(&Default::default());
        self.entries.insert(key, CachedTexture {
            texture, view, width: img.width, height: img.height, frame: self.current_frame,
            src_node: None, src_port: 0,
        });
    }

    /// Check if a GPU texture exists for this image (without uploading)
    pub fn has_texture(&self, img: &Arc<ImageData>) -> bool {
        let key = Arc::as_ptr(img) as u64;
        self.entries.get(&key).map(|c| c.width == img.width && c.height == img.height).unwrap_or(false)
    }

    /// Get a cached view (returns None if not cached)
    pub fn get_view(&self, img: &Arc<ImageData>) -> Option<&wgpu::TextureView> {
        let key = Arc::as_ptr(img) as u64;
        self.entries.get(&key)
            .filter(|c| c.width == img.width && c.height == img.height)
            .map(|c| &c.view)
    }

    /// Store a GPU texture keyed by (NodeId, port) — for GPU-to-GPU passing between nodes.
    pub fn cache_node_output(&mut self, node_id: crate::graph::NodeId, port: usize, texture: wgpu::Texture, width: u32, height: u32) {
        let key = (node_id as u64).wrapping_mul(31).wrapping_add(port as u64);
        let view = texture.create_view(&Default::default());
        self.entries.insert(key, CachedTexture {
            texture, view, width, height, frame: self.current_frame,
            src_node: Some(node_id), src_port: port,
        });
    }

    /// Get a GPU texture by (NodeId, port) — for reading a previous node's GPU output.
    pub fn get_node_output(&self, node_id: crate::graph::NodeId, port: usize) -> Option<(&wgpu::TextureView, u32, u32)> {
        let key = (node_id as u64).wrapping_mul(31).wrapping_add(port as u64);
        self.entries.get(&key).map(|c| (&c.view, c.width, c.height))
    }

    /// Like `get_node_output`, but returns a cloned `wgpu::TextureView` so the caller
    /// can hold the view across subsequent `&mut self` calls (e.g. `cache_node_output`).
    /// `wgpu::TextureView` is Arc-reference-counted internally, so cloning is cheap and
    /// the cache itself keeps the backing texture alive.
    pub fn get_node_output_cloned(&self, node_id: crate::graph::NodeId, port: usize) -> Option<(wgpu::TextureView, u32, u32)> {
        let key = (node_id as u64).wrapping_mul(31).wrapping_add(port as u64);
        self.entries.get(&key).map(|c| (c.view.clone(), c.width, c.height))
    }

    /// Store a pre-rendered output texture in the display callback resources,
    /// so the GpuImageDisplayCallback can render it without re-uploading.
    pub fn store_for_display(
        &self,
        node_id: crate::graph::NodeId,
        render_state: &eframe::egui_wgpu::RenderState,
    ) {
        let key = (node_id as u64).wrapping_mul(31).wrapping_add(0u64);
        if let Some(cached) = self.entries.get(&key) {
            let mut renderer = render_state.renderer.write();
            let store = renderer.callback_resources.entry::<GpuDisplayStore>().or_insert_with(|| GpuDisplayStore {
                resources: None,
                instances: HashMap::new(),
                prerendered: HashMap::new(),
            });
            // Initialize display resources if needed
            if store.resources.is_none() {
                let device = &render_state.device;
                let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("display_shader"), source: wgpu::ShaderSource::Wgsl(DISPLAY_SHADER.into()),
                });
                let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("display_bgl"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                        wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
                    ],
                });
                let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
                let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("display_pipeline"), layout: Some(&pl),
                    vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: wgpu::PipelineCompilationOptions::default() },
                    fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_main"), targets: &[Some(render_state.target_format.into())], compilation_options: wgpu::PipelineCompilationOptions::default() }),
                    primitive: wgpu::PrimitiveState::default(), depth_stencil: None, multisample: wgpu::MultisampleState::default(), multiview: None, cache: None,
                });
                let sampler = device.create_sampler(&wgpu::SamplerDescriptor { mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
                store.resources = Some(GpuDisplayResources { pipeline, bind_group_layout: bgl, sampler });
            }
            // Create bind group for the cached texture
            if let Some(res) = &store.resources {
                let bg = render_state.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None, layout: &res.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&cached.view) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&res.sampler) },
                    ],
                });
                let view = cached.texture.create_view(&Default::default());
                store.prerendered.insert(node_id, (view, bg));
            }
        }
    }

    /// Atomic publish: store the output texture by `(node_id, port)` AND
    /// mirror it into `GpuDisplayStore::prerendered` so the display
    /// paint callback can sample it without re-uploading. Replaces the
    /// two-step `cache_node_output` + `store_for_display` pattern in
    /// `process_gpu_cached` functions.
    pub fn publish(
        &mut self,
        node_id: crate::graph::NodeId,
        port: usize,
        texture: wgpu::Texture,
        width: u32,
        height: u32,
        render_state: &eframe::egui_wgpu::RenderState,
    ) {
        self.cache_node_output(node_id, port, texture, width, height);
        // Only port 0 is mirrored to the display callback (matches the
        // existing `store_for_display` behaviour).
        if port == 0 {
            self.store_for_display(node_id, render_state);
        }
    }

    /// On-demand readback for CPU consumers that received a
    /// `PortValue::GpuImage` they can't handle natively (Crop, Transform,
    /// save-to-disk, ML, etc.). Returns `None` if the texture is no
    /// longer cached (e.g. evicted by the frame-LRU).
    pub fn readback_node_output(
        &self,
        node_id: crate::graph::NodeId,
        port: usize,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Option<Arc<ImageData>> {
        let key = (node_id as u64).wrapping_mul(31).wrapping_add(port as u64);
        let cached = self.entries.get(&key)?;
        Some(readback_texture(device, queue, &cached.texture, cached.width, cached.height))
    }

    /// Per-`(node_id, port)` frame stamp used by the eval-loop param
    /// cache to detect "the producer re-rendered this frame". Returns 0
    /// if the entry is absent.
    pub fn frame_stamp(&self, node_id: crate::graph::NodeId, port: usize) -> u64 {
        let key = (node_id as u64).wrapping_mul(31).wrapping_add(port as u64);
        self.entries.get(&key).map(|c| c.frame).unwrap_or(0)
    }

    /// Refresh a cached entry's frame stamp without re-rendering. Called
    /// from the eval loop's param-cache HIT path so the still-live
    /// texture isn't evicted by the 2-frame LRU even though its producer
    /// was skipped this frame.
    pub fn touch(&mut self, node_id: crate::graph::NodeId, port: usize) {
        let key = (node_id as u64).wrapping_mul(31).wrapping_add(port as u64);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.frame = self.current_frame;
        }
    }
}

// ── Constants ────────────────────────────────────────────────────────────────

const BLEND_SHADER: &str = r#"
struct Params {
    mode: u32,
    mix: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;

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

fn blend_colors(a: vec4<f32>, b: vec4<f32>, mode: u32) -> vec4<f32> {
    switch mode {
        case 0u: { return b; }                                          // Normal
        case 1u: { return a * b; }                                      // Multiply
        case 2u: { return vec4(1.0) - (vec4(1.0) - a) * (vec4(1.0) - b); } // Screen
        case 3u: {                                                       // Overlay
            let r = select(2.0 * a.r * b.r, 1.0 - 2.0 * (1.0-a.r) * (1.0-b.r), a.r > 0.5);
            let g = select(2.0 * a.g * b.g, 1.0 - 2.0 * (1.0-a.g) * (1.0-b.g), a.g > 0.5);
            let bb = select(2.0 * a.b * b.b, 1.0 - 2.0 * (1.0-a.b) * (1.0-b.b), a.b > 0.5);
            return vec4(r, g, bb, max(a.a, b.a));
        }
        case 4u: { return clamp(a + b, vec4(0.0), vec4(1.0)); }        // Add
        case 5u: { return abs(a - b); }                                 // Difference
        case 6u: {                                                       // Soft Light
            let r = select((2.0*b.r - 1.0) * (a.r - a.r*a.r) + a.r, (2.0*b.r) * a.r + a.r*a.r*(1.0-2.0*b.r), b.r <= 0.5);
            let g = select((2.0*b.g - 1.0) * (a.g - a.g*a.g) + a.g, (2.0*b.g) * a.g + a.g*a.g*(1.0-2.0*b.g), b.g <= 0.5);
            let bb = select((2.0*b.b - 1.0) * (a.b - a.b*a.b) + a.b, (2.0*b.b) * a.b + a.b*a.b*(1.0-2.0*b.b), b.b <= 0.5);
            return vec4(r, g, bb, max(a.a, b.a));
        }
        case 7u: {                                                       // Hard Light
            let r = select(1.0 - 2.0*(1.0-a.r)*(1.0-b.r), 2.0*a.r*b.r, b.r <= 0.5);
            let g = select(1.0 - 2.0*(1.0-a.g)*(1.0-b.g), 2.0*a.g*b.g, b.g <= 0.5);
            let bb = select(1.0 - 2.0*(1.0-a.b)*(1.0-b.b), 2.0*a.b*b.b, b.b <= 0.5);
            return vec4(r, g, bb, max(a.a, b.a));
        }
        default: { return b; }
    }
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let a = textureSample(tex_a, tex_sampler, in.uv);
    let b = textureSample(tex_b, tex_sampler, in.uv);
    let blended = blend_colors(a, b, p.mode);
    return mix(a, blended, p.mix);
}
"#;

// ── GPU Resources ────────────────────────────────────────────────────────────

struct GpuBlendResources {
    pipeline: wgpu::RenderPipeline,
    display_pipeline: wgpu::RenderPipeline,
    display_bind_group_layout: wgpu::BindGroupLayout,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform_buffer: wgpu::Buffer,
}

#[allow(dead_code)]
struct GpuBlendInstance {
    _bind_group: wgpu::BindGroup,
    output_texture: wgpu::Texture,
    _output_view: wgpu::TextureView,
    display_bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

/// Stored in egui_wgpu callback resources
pub struct GpuBlendStore {
    resources: Option<GpuBlendResources>,
    instances: HashMap<NodeId, GpuBlendInstance>,
}

pub struct GpuBlendCallback {
    pub node_id: NodeId,
    pub mode: u32,
    pub mix: f32,
    pub img_a: Arc<ImageData>,
    pub img_b: Arc<ImageData>,
    pub target_format: wgpu::TextureFormat,
}

impl egui_wgpu::CallbackTrait for GpuBlendCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let store = callback_resources.entry::<GpuBlendStore>().or_insert_with(|| GpuBlendStore {
            resources: None,
            instances: HashMap::new(),
        });

        // Initialize pipeline on first use
        if store.resources.is_none() {
            store.resources = Some(create_blend_pipeline(device, self.target_format));
        }
        let res = match store.resources.as_ref() {
            Some(r) => r,
            None => return Vec::new(), // Pipeline creation failed — skip this frame
        };

        let w = self.img_a.width.max(self.img_b.width);
        let h = self.img_a.height.max(self.img_b.height);

        // Upload params
        let params = [self.mode, self.mix.to_bits(), 0u32, 0u32];
        queue.write_buffer(&res.uniform_buffer, 0, bytemuck::cast_slice(&params));

        // Create/update textures
        let tex_a = upload_texture(device, queue, &self.img_a, "blend_a");
        let tex_b = upload_texture(device, queue, &self.img_b, "blend_b");
        let view_a = tex_a.create_view(&Default::default());
        let view_b = tex_b.create_view(&Default::default());

        // Create output render target
        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blend_output"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output_texture.create_view(&Default::default());

        // Create bind group
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blend_bind_group"),
            layout: &res.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: res.uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view_a) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view_b) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&res.sampler) },
            ],
        });

        // Render blend to output texture
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blend_offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            pass.set_pipeline(&res.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        // Create display bind group to sample the output texture
        let display_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blend_display_bg"),
            layout: &res.display_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&output_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&res.sampler) },
            ],
        });

        store.instances.insert(self.node_id, GpuBlendInstance {
            _bind_group: bind_group,
            output_texture,
            _output_view: output_view,
            display_bind_group,
            width: w,
            height: h,
        });

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(store) = callback_resources.get::<GpuBlendStore>() {
            if let (Some(res), Some(inst)) = (&store.resources, store.instances.get(&self.node_id)) {
                render_pass.set_pipeline(&res.display_pipeline);
                render_pass.set_bind_group(0, &inst.display_bind_group, &[]);
                render_pass.draw(0..3, 0..1);
            }
        }
    }
}

// ── Helper Functions ─────────────────────────────────────────────────────────

const DISPLAY_SHADER: &str = r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;

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

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(tex, tex_sampler, in.uv);
}
"#;

fn create_blend_pipeline(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> GpuBlendResources {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("blend_shader"),
        source: wgpu::ShaderSource::Wgsl(BLEND_SHADER.into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("blend_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
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
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("blend_pipeline_layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("blend_pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("blend_params"),
        size: 16, // 4 × u32
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("blend_sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });

    // Display pipeline: samples a single texture and renders to screen
    let display_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("display_shader"),
        source: wgpu::ShaderSource::Wgsl(DISPLAY_SHADER.into()),
    });

    let display_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("display_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });

    let display_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("display_pipeline_layout"),
        bind_group_layouts: &[&display_bind_group_layout],
        push_constant_ranges: &[],
    });

    let display_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("display_pipeline"),
        layout: Some(&display_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &display_shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &display_shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
        cache: None,
    });

    GpuBlendResources { pipeline, display_pipeline, display_bind_group_layout, bind_group_layout, sampler, uniform_buffer }
}

pub fn upload_texture(device: &wgpu::Device, queue: &wgpu::Queue, img: &ImageData, label: &str) -> wgpu::Texture {
    // Guard against empty pixel buffers (GPU-only placeholders)
    if img.pixels.is_empty() {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label), size: wgpu::Extent3d { width: img.width.max(1), height: img.height.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST, view_formats: &[],
        });
        let black = vec![0u8; (img.width.max(1) * img.height.max(1) * 4) as usize];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &black,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(img.width.max(1) * 4), rows_per_image: Some(img.height.max(1)) },
            wgpu::Extent3d { width: img.width.max(1), height: img.height.max(1), depth_or_array_layers: 1 },
        );
        return tex;
    }
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &img.pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(img.width * 4),
            rows_per_image: Some(img.height),
        },
        wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
    );

    texture
}

/// Readback GPU texture to CPU ImageData (blocking)
#[allow(dead_code)]
pub fn readback_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Arc<ImageData> {
    let bytes_per_row = (width * 4 + 255) & !255; // Align to 256
    let buf_size = (bytes_per_row * height) as u64;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback_staging"),
        size: buf_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    queue.submit(Some(encoder.finish()));

    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| { let _ = tx.send(result); });
    device.poll(wgpu::Maintain::Wait);
    let _ = rx.recv();

    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * bytes_per_row) as usize;
        let end = start + (width * 4) as usize;
        pixels.extend_from_slice(&data[start..end]);
    }

    Arc::new(ImageData { width, height, pixels })
}

// ── GPU Image Display Callback ──────────────────────────────────────────────
// Renders an Arc<ImageData> directly to screen via wgpu paint callback.
// Bypasses egui's texture system — uploads once per frame, renders directly.

struct GpuDisplayResources {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

struct GpuDisplayInstance {
    bind_group: wgpu::BindGroup,
    _texture: wgpu::Texture,
}

pub struct GpuDisplayStore {
    resources: Option<GpuDisplayResources>,
    instances: HashMap<NodeId, GpuDisplayInstance>,
    /// Pre-rendered GPU textures from upstream nodes (stored here for paint callback access)
    pub prerendered: HashMap<NodeId, (wgpu::TextureView, wgpu::BindGroup)>,
}

pub struct GpuImageDisplayCallback {
    pub node_id: NodeId,
    pub img: Arc<ImageData>,
    pub target_format: wgpu::TextureFormat,
    /// If set, try to use the GPU texture cached for this (source_node, port) first.
    /// Falls back to uploading img if not found in cache.
    pub gpu_source: Option<(NodeId, usize)>,
}

impl egui_wgpu::CallbackTrait for GpuImageDisplayCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let store = callback_resources.entry::<GpuDisplayStore>().or_insert_with(|| GpuDisplayStore {
            resources: None,
            instances: HashMap::new(),
            prerendered: HashMap::new(),
        });

        if store.resources.is_none() {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("display_shader"),
                source: wgpu::ShaderSource::Wgsl(DISPLAY_SHADER.into()),
            });
            let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("display_bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0, visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1, visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None, bind_group_layouts: &[&bind_group_layout], push_constant_ranges: &[],
            });
            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("display_pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_main"), buffers: &[], compilation_options: wgpu::PipelineCompilationOptions::default() },
                fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs_main"), targets: &[Some(self.target_format.into())], compilation_options: wgpu::PipelineCompilationOptions::default() }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None, multisample: wgpu::MultisampleState::default(), multiview: None, cache: None,
            });
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default()
            });
            store.resources = Some(GpuDisplayResources { pipeline, bind_group_layout, sampler });
        }

        let res = match store.resources.as_ref() {
            Some(r) => r,
            None => return Vec::new(),
        };

        // Check if a pre-rendered GPU texture exists for the source node
        if let Some(src) = self.gpu_source {
            if store.prerendered.contains_key(&src.0) {
                // Pre-rendered texture available — skip upload entirely
                store.instances.remove(&self.node_id); // clear old instance
                return Vec::new();
            }
        }

        // Upload image to GPU texture
        if self.img.pixels.is_empty() {
            return Vec::new(); // GPU placeholder with no pixels — nothing to upload
        }
        let texture = upload_texture(device, queue, &self.img, "display_img");
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &res.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&res.sampler) },
            ],
        });

        store.instances.insert(self.node_id, GpuDisplayInstance { bind_group, _texture: texture });
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(store) = callback_resources.get::<GpuDisplayStore>() {
            let res = match &store.resources { Some(r) => r, None => return };

            // Try pre-rendered source first (zero-copy GPU path)
            if let Some(src) = &self.gpu_source {
                if let Some((_, bg)) = store.prerendered.get(&src.0) {
                    render_pass.set_pipeline(&res.pipeline);
                    render_pass.set_bind_group(0, bg, &[]);
                    render_pass.draw(0..3, 0..1);
                    return;
                }
            }

            // Fall back to uploaded instance
            if let Some(inst) = store.instances.get(&self.node_id) {
                render_pass.set_pipeline(&res.pipeline);
                render_pass.set_bind_group(0, &inst.bind_group, &[]);
                render_pass.draw(0..3, 0..1);
            }
        }
    }
}

