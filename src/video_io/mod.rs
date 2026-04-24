//! Video I/O bridges — pipe frames between PatchWork and other apps.
//!
//! Phase 2 ships `syphon` for macOS. NDI (Phase 3) and Spout / Windows
//! (Phase 4) land here too once those phases start. The whole module
//! is `cfg(target_os = "macos")`-gated today since Syphon is Mac-only.
//!
//! Wiring: `VideoOutNode` calls `syphon::SyphonServer::publish(...)` on
//! the main thread every frame; `VideoInNode` polls
//! `syphon::SyphonClient::take_latest()` to pull incoming Metal
//! textures. Both are wrapped in `Arc<Mutex<...>>` inside the nodes
//! because `NodeBehavior: Send + Sync`.

#[cfg(target_os = "macos")]
pub mod wgpu_metal;

#[cfg(target_os = "macos")]
pub mod syphon;
