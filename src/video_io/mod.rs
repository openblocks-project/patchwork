//! Video I/O bridges — pipe frames between PatchWork and other apps.
//!
//! Phase 2 ships `syphon` for macOS. Phase 3 adds `ndi` (also macOS
//! in v1, cross-platform by Phase 4). Spout / Windows (Phase 4) lands
//! here too. Each submodule is `cfg`-gated on its target platform.
//!
//! ## Wiring
//!
//! - `VideoOutNode` calls `syphon::SyphonServer::publish(...)` /
//!   `ndi::NdiSender::publish_bgra(...)` on the main thread every
//!   (capped) frame.
//! - `VideoInNode` polls `syphon::SyphonClient::take_latest()` /
//!   `ndi::NdiReceiver::capture_video(...)` on a background thread
//!   and forwards frames into the GPU texture cache.
//! - All external handles are wrapped in `Arc<Mutex<Option<...>>>`
//!   inside the nodes because `NodeBehavior: Send + Sync`.
//!
//! ## Runtime availability
//!
//! - Syphon: always available on macOS (we vendor the framework under
//!   `vendor/Syphon.framework.prebuilt/`).
//! - NDI: availability at runtime depends on whether the user has
//!   `libndi.dylib` installed (NewTek EULA blocks bundling). See
//!   `ndi::library()` for the load outcome — UI gates off that.

#[cfg(target_os = "macos")]
pub mod wgpu_metal;

#[cfg(target_os = "macos")]
pub mod syphon;

#[cfg(target_os = "macos")]
pub mod ndi;

/// RGBA ↔ BGRA byte-order helpers. Cross-platform — pure CPU pixel
/// math, no platform deps. Used today by `ndi::NdiSender::publish_bgra`
/// (sender) and the M5 receiver thread (recv).
pub mod pixel_swizzle;

/// File recorder — pipe RGBA frames into an ffmpeg subprocess. Phase 6
/// sink for `VideoOutNode`. See module doc for codec / container rules
/// and graceful-vs-hard stop semantics.
pub mod file_recorder;
