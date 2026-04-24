//! Syphon Server + Client wrappers. macOS only.
//!
//! Thin Rust wrappers around `SyphonMetalServer` and `SyphonMetalClient`
//! (vendored pre-built binary at `vendor/Syphon.framework.prebuilt/`).
//!
//! Design notes:
//! - **Raw FFI via `objc2`.** Syphon isn't a `ClassType` in objc2's
//!   typed registry (it's a third-party framework), so we use dynamic
//!   `class!()` lookup + `msg_send!` instead of the typed `msg_send_id!`
//!   macros. The M1 spike proved this pattern works.
//! - **Send, not Sync.** SyphonMetalServer is thread-safe per upstream
//!   docs, so `unsafe impl Send` is valid. We deliberately *don't* mark
//!   Sync — callers own a `Mutex<Option<...>>` when they need to share
//!   across threads (see `VideoOutNode`).
//! - **Dedicated command queue.** Syphon's publish API wants an
//!   `MTLCommandBuffer`. We allocate a dedicated `MTLCommandQueue`
//!   per server so wgpu's queue stays off the publish critical path.
//! - **M3 Client is polling-based.** No `block2` block handler yet —
//!   `take_latest()` calls `-newFrameImage` lazily. M5 upgrades to
//!   block-based push with `ctx.request_repaint()`.

use foreign_types_shared::{ForeignType, ForeignTypeRef};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSArray, NSDictionary, NSPoint, NSRect, NSSize, NSString};
use std::sync::{Arc, Mutex, OnceLock};

// ── SyphonServer ─────────────────────────────────────────────────────────────

/// A running Syphon Metal server. Drop = unregister from the
/// SyphonServerDirectory so client apps lose the entry cleanly.
pub struct SyphonServer {
    /// `SyphonMetalServer*` — ObjC retained handle.
    raw: Retained<AnyObject>,
    /// Dedicated `MTLCommandQueue` used only for publish commits.
    /// Keeps wgpu's own queue out of Syphon's command scheduling.
    cmd_queue: metal::CommandQueue,
    /// User-chosen publish name, remembered for UI/status.
    name: String,
}

// SAFETY: SyphonMetalServer is documented thread-safe across instances
// (see class docs in `vendor/Syphon-Framework/SyphonMetalServer.h`).
// The `metal::CommandQueue` and `metal::Device`-derived handles are
// already Send per the `metal` crate's impls. We do NOT mark Sync —
// consumers serialise access through a `Mutex`.
unsafe impl Send for SyphonServer {}

impl SyphonServer {
    /// Spawn a new server publishing under `name`. The server appears
    /// immediately in any listening client's `SyphonServerDirectory`.
    ///
    /// Returns `Err` only if the ObjC init returns nil (very rare,
    /// usually means Syphon isn't linked correctly).
    pub fn new(name: &str, device: &metal::Device) -> Result<Self, String> {
        let cmd_queue = device.new_command_queue();
        let raw = unsafe { alloc_init_server(name, device) }
            .ok_or_else(|| "SyphonMetalServer init returned nil".to_string())?;
        Ok(Self { raw, cmd_queue, name: name.to_string() })
    }

    /// Publish one frame. Allocates a command buffer on our dedicated
    /// queue, hands it to Syphon, commits, and waits for completion so
    /// the wgpu texture can't be recycled while Syphon's still reading.
    ///
    /// Pass `flipped = false` for textures that were rendered with the
    /// normal Metal origin (top-left). Invert if downstream shows the
    /// image upside-down.
    pub fn publish(&self, texture: &metal::TextureRef, w: u32, h: u32, flipped: bool) {
        let cmd_buf = self.cmd_queue.new_command_buffer();
        let region = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize { width: w as f64, height: h as f64 },
        };
        unsafe {
            let tex_ptr: *const AnyObject = texture.as_ptr() as *const AnyObject;
            let cmd_ptr: *const AnyObject = cmd_buf.as_ptr() as *const AnyObject;
            let _: () = msg_send![
                &*self.raw,
                publishFrameTexture: tex_ptr,
                onCommandBuffer: cmd_ptr,
                imageRegion: region,
                flipped: flipped,
            ];
        }
        cmd_buf.commit();
        // Wait so the source wgpu texture isn't reused mid-publish. At
        // ~16ms frame budget this costs sub-ms per call on M-series
        // (confirmed in the M1 spike against Apple M4).
        cmd_buf.wait_until_completed();
    }

    /// `true` if any Syphon client has connected to this server.
    /// Useful for UI "is anyone listening?" indicators.
    pub fn has_clients(&self) -> bool {
        unsafe { msg_send![&*self.raw, hasClients] }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for SyphonServer {
    fn drop(&mut self) {
        // -[SyphonMetalServer stop] unregisters from the directory so
        // any listening app (TD / OBS / etc.) drops our name from
        // their picker within one announce cycle. Skipping this leaves
        // zombie entries until their GC kicks in.
        unsafe {
            let _: () = msg_send![&*self.raw, stop];
        }
    }
}

/// SAFETY: Caller must ensure `device` is valid and non-null, and we're
/// on an autoreleased thread context (true for all main-thread egui
/// callbacks).
unsafe fn alloc_init_server(name: &str, device: &metal::Device) -> Option<Retained<AnyObject>> {
    let cls = class!(SyphonMetalServer);
    let ns_name = NSString::from_str(name);
    let device_ptr: *const AnyObject = device.as_ptr() as *const AnyObject;
    let options: *const AnyObject = std::ptr::null();
    let alloc_ptr: *mut AnyObject = msg_send![cls, alloc];
    let inited_ptr: *mut AnyObject = msg_send![
        alloc_ptr,
        initWithName: &*ns_name,
        device: device_ptr,
        options: options,
    ];
    Retained::from_raw(inited_ptr)
}

// ── SyphonServerDirectory ────────────────────────────────────────────────────

/// Description of one server discovered on the local machine. Kept
/// alongside its source `NSDictionary*` because `SyphonMetalClient::new`
/// needs the same dict to establish a connection.
pub struct SyphonServerInfo {
    pub app_name: String,
    pub name: String,
    pub uuid: String,
    /// The raw `NSDictionary*` server description. Retained so the
    /// caller can pass us back here later without re-enumerating.
    pub(crate) description: Retained<AnyObject>,
}

// SAFETY: NSDictionary is immutable and thread-safe per Apple docs;
// the String fields are already Send. Matches SyphonServer's rationale.
unsafe impl Send for SyphonServerInfo {}

/// Enumerate every Syphon server currently visible on the local
/// machine. The list refreshes whenever servers come/go; callers
/// should re-invoke this (e.g. on a Refresh button click, or on a 2s
/// timer) to keep their UI current.
///
/// Wrapped in `autoreleasepool` so any ObjC objects that Cocoa
/// autoreleases during the enumeration drain immediately instead of
/// leaking a few CFStrings per call (M6 soak showed ~85 bytes/sec
/// growth without this). Combined with the caller-side 1 s cache in
/// `VideoInNode::render_syphon_ui`, per-frame allocation pressure
/// drops effectively to zero.
pub fn servers() -> Vec<SyphonServerInfo> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let cls = class!(SyphonServerDirectory);
        let dir: *mut AnyObject = msg_send![cls, sharedDirectory];
        if dir.is_null() { return Vec::new(); }
        // `-servers` returns `NSArray<NSDictionary>`.
        let arr_ptr: *mut NSArray<NSDictionary> = msg_send![dir, servers];
        let Some(arr) = Retained::retain(arr_ptr) else { return Vec::new(); };

        let count: usize = msg_send![&*arr, count];
        let mut out = Vec::with_capacity(count);
        let name_key = NSString::from_str("SyphonServerDescriptionNameKey");
        let app_key = NSString::from_str("SyphonServerDescriptionAppNameKey");
        let uuid_key = NSString::from_str("SyphonServerDescriptionUUIDKey");

        for i in 0..count {
            let dict_ptr: *mut NSDictionary = msg_send![&*arr, objectAtIndex: i];
            let Some(dict) = Retained::retain(dict_ptr) else { continue; };
            let name = dict_string(&dict, &name_key).unwrap_or_default();
            let app_name = dict_string(&dict, &app_key).unwrap_or_default();
            let uuid = dict_string(&dict, &uuid_key).unwrap_or_default();

            // Cast the typed NSDictionary back to AnyObject so callers
            // can hand it to SyphonMetalClient without knowing the
            // concrete generic param.
            let raw_any: Retained<AnyObject> = Retained::cast(dict);
            out.push(SyphonServerInfo {
                name,
                app_name,
                uuid,
                description: raw_any,
            });
        }
        out
    })
}

/// Look up a String-typed value in an NSDictionary. Returns None if
/// the key is missing or the value isn't an NSString.
unsafe fn dict_string(dict: &NSDictionary, key: &NSString) -> Option<String> {
    let val: *mut NSString = msg_send![dict, objectForKey: key];
    if val.is_null() { return None; }
    let retained = Retained::retain(val)?;
    Some(retained.to_string())
}

// ── SyphonClient ─────────────────────────────────────────────────────────────

/// Connection to a Syphon server. Polling-based in M3 — call
/// `take_latest()` each frame to get the newest published texture, if
/// any. M5 upgrades to a block-based push notification.
pub struct SyphonClient {
    raw: Retained<AnyObject>,
    /// Latest texture, double-buffered so `take_latest()` can swap it
    /// out cheaply. In M3 we just pull-on-demand; the Mutex is
    /// infrastructure for the M5 block-driven push path.
    latest: Arc<Mutex<Option<metal::Texture>>>,
}

// SAFETY: same rationale as SyphonServer — Syphon's client is
// thread-safe per upstream docs. `metal::Texture` is Send.
unsafe impl Send for SyphonClient {}

impl SyphonClient {
    /// Connect to `info` using `device`. Returns `Err` if the ObjC
    /// init returns nil (usually means the server disappeared between
    /// `servers()` and `new()`).
    pub fn new(info: &SyphonServerInfo, device: &metal::Device) -> Result<Self, String> {
        let raw = unsafe { alloc_init_client(&info.description, device) }
            .ok_or_else(|| "SyphonMetalClient init returned nil".to_string())?;
        Ok(Self {
            raw,
            latest: Arc::new(Mutex::new(None)),
        })
    }

    /// Pull the latest frame from the server. Returns `None` if no new
    /// frame is available since the last call. The returned texture is
    /// owned by the caller — release it when done.
    ///
    /// M3 polls by calling `-newFrameImage` directly. That works but
    /// wastes a call per UI frame when no new server frame arrived.
    /// M5 will replace this with a block handler that pushes to the
    /// `latest` slot from a Syphon-owned thread.
    pub fn take_latest(&self) -> Option<metal::Texture> {
        unsafe {
            let tex_ptr: *mut AnyObject = msg_send![&*self.raw, newFrameImage];
            if tex_ptr.is_null() { return None; }
            // `newFrameImage` returns a +1 retained id<MTLTexture>;
            // wrap as metal::Texture which takes ownership.
            Some(metal::Texture::from_ptr(tex_ptr as *mut _))
        }
    }
}

impl Drop for SyphonClient {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![&*self.raw, stop];
        }
    }
}

/// SAFETY: `description` must be a valid `NSDictionary*` from
/// `SyphonServerDirectory::servers()`. `device` non-null & valid.
unsafe fn alloc_init_client(
    description: &Retained<AnyObject>,
    device: &metal::Device,
) -> Option<Retained<AnyObject>> {
    let cls = class!(SyphonMetalClient);
    let device_ptr: *const AnyObject = device.as_ptr() as *const AnyObject;
    let desc_ptr: *const AnyObject = &**description;
    let options: *const AnyObject = std::ptr::null();
    let handler: *const AnyObject = std::ptr::null(); // M5: block handler

    let alloc_ptr: *mut AnyObject = msg_send![cls, alloc];
    let inited_ptr: *mut AnyObject = msg_send![
        alloc_ptr,
        initWithServerDescription: desc_ptr,
        device: device_ptr,
        options: options,
        newFrameHandler: handler,
    ];
    Retained::from_raw(inited_ptr)
}

// ── Class lookup cache ──────────────────────────────────────────────────────
//
// `class!()` is already cached inside objc2, so re-calling it is cheap.
// This OnceLock is reserved for future protocol lookups that aren't.

#[allow(dead_code)]
static _CLASS_CACHE: OnceLock<()> = OnceLock::new();
