//! NDI® Sender + Receiver + Finder wrappers. macOS (Phase 3 v1).
//!
//! Thin libloading-based wrappers around the NDI C API. Users install
//! NDI Runtime separately (NewTek EULA, can't bundle); if `libndi` is
//! missing at startup, `library()` returns `Err(...)` and the NDI UI
//! entries disable themselves with a tooltip pointing at ndi.video.
//!
//! ## Design notes
//!
//! - **Runtime-loaded FFI** via `libloading::Library::new` at first use.
//!   A `OnceLock<Result<NdiLibrary, String>>` means one load attempt per
//!   process, shared across all NDI nodes. The Library handle lives for
//!   the whole process — never dropped — so the function pointers we
//!   copy out of `Symbol<T>` stay valid.
//! - **No compile-time NDI dep.** Contributors without the NDI SDK /
//!   Runtime still build PatchWork.  `otool -L` on `patchwork` shows
//!   zero libndi entries.
//! - **App-bundle fallback.** NDI Tools 5 / 6 don't write libndi to
//!   `/usr/local/lib` — each Tools app embeds it under
//!   `Contents/Frameworks/libndi.dylib`. We probe standard system
//!   paths first (Runtime Redist installer target), then scan
//!   `/Applications/*.app/Contents/Frameworks/` as fallback. Confirmed
//!   on a dev machine where only NDI Tools 6 was installed.
//! - **Send, not Sync.** NDI docs guarantee thread safety across
//!   *instances*, not concurrent calls on one instance. Callers own
//!   `Arc<Mutex<Option<...>>>` when they need to share across threads
//!   (see `VideoOutNode::ndi_sender`, mirroring the Syphon pattern).
//! - **Drop order matters.** `Drop::drop` on `NdiSender` /
//!   `NdiReceiver` / `NdiFinder` calls the matching `NDIlib_*_destroy`
//!   so the directory / receiver drops our entry cleanly. Any
//!   in-flight `send_video_v2` call must have returned first — the
//!   C API is synchronous, so this is automatic.
//!
//! ## Not implemented in M2
//!
//! - Background-thread capture loop for `NdiReceiver` (M5 adds that on
//!   top of the blocking `capture_video(...)` primitive exposed here).
//! - Swizzle from BGRA → RGBA on capture (M3 `pixel_swizzle.rs`, wired
//!   in M5).
//! - fps cap on sender (M4).
//!
//! Checked against NDI SDK 6.0.1 `Processing.NDI.*.h` @ 2026-04-24.
//! Struct field order + size checked via `const _:` assertions below;
//! bump the expected sizes when updating to a newer NDI SDK.

use libloading::Library;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::OnceLock;
use std::time::Duration;

// ─── FFI types: opaque instance handles ─────────────────────────────────────

#[allow(non_camel_case_types)] pub type NDIlib_send_instance_t = *mut c_void;
#[allow(non_camel_case_types)] pub type NDIlib_recv_instance_t = *mut c_void;
#[allow(non_camel_case_types)] pub type NDIlib_find_instance_t = *mut c_void;

// ─── FFI types: enums (FourCC / frame type / format / color / bandwidth) ───
//
// NDI uses `int`-sized enums in C. Rust `#[repr(C)] enum` with an
// `i32` discriminant matches. We declare only the values we actually
// care about; unknown values from the wire just fail our match arms
// and we skip the frame.

/// `NDIlib_FourCC_video_type_e`. Only two values used in v1: BGRA for
/// alpha-carrying frames, BGRX when the sender omits alpha.
#[repr(C)]
#[allow(non_camel_case_types, dead_code)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NDIlib_FourCC_video_type_e {
    BGRA = 0x41524742, // little-endian 'BGRA'
    BGRX = 0x58524742, // 'BGRX'
    // v1 doesn't emit others, but senders on the network can. We treat
    // any non-BGRA/BGRX frame as a capture error in M5 rather than
    // trying to decode YUV / 10-bit / P-format variants.
}

/// `NDIlib_frame_format_type_e`. We only send progressive; receivers
/// can carry any.
#[repr(C)]
#[allow(non_camel_case_types, dead_code)]
#[derive(Debug, Copy, Clone)]
pub enum NDIlib_frame_format_type_e {
    Progressive = 1,
    Interleaved = 0,
    Field0 = 2,
    Field1 = 3,
}

/// `NDIlib_frame_type_e` — return value of `recv_capture_v3`.
#[repr(C)]
#[allow(non_camel_case_types, dead_code)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum NDIlib_frame_type_e {
    None = 0,
    Video = 1,
    Audio = 2,
    Metadata = 3,
    Error = 4,
    StatusChange = 100,
}

/// `NDIlib_recv_color_format_e`. We request BGRX_BGRA (0) so every
/// video frame lands in one of two known formats — BGRX if the sender
/// doesn't carry alpha, BGRA otherwise. Both are 32-bit little-endian
/// B, G, R, (X|A) byte order; our downstream swizzle treats them the
/// same (X byte gets ignored and we set A=255).
#[repr(C)]
#[allow(non_camel_case_types, dead_code)]
pub enum NDIlib_recv_color_format_e {
    BGRX_BGRA = 0,
    UYVY_BGRA = 1,
    RGBX_RGBA = 2,
    UYVY_RGBA = 3,
    Fastest = 100,
    Best = 101,
}

/// `NDIlib_recv_bandwidth_e`. Default to highest — UI dial is a
/// later-phase polish if anyone asks for throttling.
#[repr(C)]
#[allow(non_camel_case_types, dead_code)]
pub enum NDIlib_recv_bandwidth_e {
    MetadataOnly = -10,
    Lowest = 0,
    AudioOnly = 10,
    Highest = 100,
}

// ─── FFI types: structs ──────────────────────────────────────────────────────

#[repr(C)]
#[allow(non_camel_case_types)]
pub struct NDIlib_source_t {
    pub p_ndi_name: *const c_char,
    /// C-level union of `p_url_address` / `p_ip_address`. Both variants
    /// are `const char*` at the same offset, so a single pointer field
    /// matches the layout exactly.
    pub p_url_or_ip_address: *const c_char,
}

const _: () = assert!(std::mem::size_of::<NDIlib_source_t>() == 16);

#[repr(C)]
#[allow(non_camel_case_types)]
pub struct NDIlib_find_create_t {
    pub show_local_sources: bool,
    pub p_groups: *const c_char,
    pub p_extra_ips: *const c_char,
}

#[repr(C)]
#[allow(non_camel_case_types)]
pub struct NDIlib_send_create_t {
    pub p_ndi_name: *const c_char,
    pub p_groups: *const c_char,
    /// Whether NDI should rate-limit our publish calls to match
    /// `frame_rate_N / frame_rate_D`. We do our own fps cap at the
    /// node level, so set to `false` and pass frames as fast as
    /// `send_send_video_v2` is called.
    pub clock_video: bool,
    pub clock_audio: bool,
}

#[repr(C)]
#[allow(non_camel_case_types)]
pub struct NDIlib_recv_create_v3_t {
    pub source_to_connect_to: NDIlib_source_t,
    pub color_format: NDIlib_recv_color_format_e,
    pub bandwidth: NDIlib_recv_bandwidth_e,
    pub allow_video_fields: bool,
    pub p_ndi_recv_name: *const c_char,
}

// Field names mirror the C header verbatim (`FourCC`, `frame_rate_N`,
// `frame_rate_D`) so someone cross-referencing Processing.NDI.*.h
// sees 1:1 correspondence. `allow(non_snake_case)` silences the lint.
#[repr(C)]
#[allow(non_camel_case_types, non_snake_case)]
pub struct NDIlib_video_frame_v2_t {
    pub xres: c_int,
    pub yres: c_int,
    pub FourCC: NDIlib_FourCC_video_type_e,
    pub frame_rate_N: c_int,
    pub frame_rate_D: c_int,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: NDIlib_frame_format_type_e,
    /// `NDIlib_send_timecode_synthesize` sentinel = `INT64_MAX` — tells
    /// NDI to generate its own timecode. We use this on send; on recv
    /// this field is filled by the sender.
    pub timecode: i64,
    pub p_data: *mut u8,
    /// C-level union of `line_stride_in_bytes` / `data_size_in_bytes`.
    /// Interpretation depends on FourCC — for BGRA/BGRX (what we use),
    /// it's `line_stride_in_bytes = width * 4`.
    pub line_stride_or_data_size: c_int,
    pub p_metadata: *const c_char,
    pub timestamp: i64,
}

// Size check — catches silent struct-layout drift between SDK versions.
// NDI SDK 6.0.1 layout = 72 bytes on 64-bit (see plan §Risks #2).
const _: () = assert!(std::mem::size_of::<NDIlib_video_frame_v2_t>() == 72);

// ─── FFI types: function pointer typedefs ───────────────────────────────────
//
// Raw `fn` pointers (not `libloading::Symbol<T>`) so they have `'static`
// lifetime once loaded — we copy them out of Symbol in `NdiLibrary::load`.

#[allow(non_camel_case_types)] type Fn_initialize =
    unsafe extern "C" fn() -> bool;
#[allow(non_camel_case_types)] type Fn_destroy =
    unsafe extern "C" fn();
#[allow(non_camel_case_types)] type Fn_is_supported_cpu =
    unsafe extern "C" fn() -> bool;

#[allow(non_camel_case_types)] type Fn_find_create_v2 =
    unsafe extern "C" fn(*const NDIlib_find_create_t) -> NDIlib_find_instance_t;
#[allow(non_camel_case_types)] type Fn_find_destroy =
    unsafe extern "C" fn(NDIlib_find_instance_t);
#[allow(non_camel_case_types)] type Fn_find_get_current_sources =
    unsafe extern "C" fn(NDIlib_find_instance_t, *mut u32) -> *const NDIlib_source_t;
#[allow(non_camel_case_types)] type Fn_find_wait_for_sources =
    unsafe extern "C" fn(NDIlib_find_instance_t, u32) -> bool;

#[allow(non_camel_case_types)] type Fn_send_create =
    unsafe extern "C" fn(*const NDIlib_send_create_t) -> NDIlib_send_instance_t;
#[allow(non_camel_case_types)] type Fn_send_destroy =
    unsafe extern "C" fn(NDIlib_send_instance_t);
#[allow(non_camel_case_types)] type Fn_send_send_video_v2 =
    unsafe extern "C" fn(NDIlib_send_instance_t, *const NDIlib_video_frame_v2_t);
#[allow(non_camel_case_types)] type Fn_send_get_no_connections =
    unsafe extern "C" fn(NDIlib_send_instance_t, u32) -> c_int;

#[allow(non_camel_case_types)] type Fn_recv_create_v3 =
    unsafe extern "C" fn(*const NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t;
#[allow(non_camel_case_types)] type Fn_recv_destroy =
    unsafe extern "C" fn(NDIlib_recv_instance_t);
#[allow(non_camel_case_types)] type Fn_recv_capture_v3 =
    unsafe extern "C" fn(
        NDIlib_recv_instance_t,
        *mut NDIlib_video_frame_v2_t,
        *mut c_void, // audio frame — we pass null
        *mut c_void, // metadata frame — we pass null
        u32,         // timeout_ms
    ) -> NDIlib_frame_type_e;
#[allow(non_camel_case_types)] type Fn_recv_free_video_v2 =
    unsafe extern "C" fn(NDIlib_recv_instance_t, *const NDIlib_video_frame_v2_t);

// ─── NdiLibrary ──────────────────────────────────────────────────────────────

/// Function-pointer table + Library handle. Built once in
/// `library()`'s `OnceLock::get_or_init`; lives for the lifetime of
/// the process.
pub struct NdiLibrary {
    /// Keep the dylib loaded. Dropping this unloads the library and
    /// dangles every function pointer below — we never drop it.
    _lib: Library,
    /// Which path on disk we ended up loading from. Logged on startup
    /// so "why is NDI behaving weirdly on machine X" is answerable.
    pub loaded_from: PathBuf,

    pub initialize: Fn_initialize,
    pub destroy: Fn_destroy,
    pub is_supported_cpu: Fn_is_supported_cpu,

    pub find_create_v2: Fn_find_create_v2,
    pub find_destroy: Fn_find_destroy,
    pub find_get_current_sources: Fn_find_get_current_sources,
    pub find_wait_for_sources: Fn_find_wait_for_sources,

    pub send_create: Fn_send_create,
    pub send_destroy: Fn_send_destroy,
    pub send_send_video_v2: Fn_send_send_video_v2,
    pub send_get_no_connections: Fn_send_get_no_connections,

    pub recv_create_v3: Fn_recv_create_v3,
    pub recv_destroy: Fn_recv_destroy,
    pub recv_capture_v3: Fn_recv_capture_v3,
    pub recv_free_video_v2: Fn_recv_free_video_v2,
}

// SAFETY: every field is either (a) immutable after construction, or
// (b) a raw function pointer (`fn`). Function pointers are Copy and
// Send + Sync. The `Library` handle is Send + Sync per libloading.
unsafe impl Send for NdiLibrary {}
unsafe impl Sync for NdiLibrary {}

static NDI_LIB: OnceLock<Result<NdiLibrary, String>> = OnceLock::new();

/// Get the NDI library handle, or a user-readable error if libndi
/// isn't available. Safe to call every frame — amortises to a single
/// pointer compare after the first call.
///
/// ## Error shape
/// The `&str` is shaped for direct user display — tooltip, status row,
/// log line. Callers should avoid wrapping / re-formatting it.
pub fn library() -> Result<&'static NdiLibrary, &'static str> {
    NDI_LIB.get_or_init(load_library).as_ref().map_err(|s| s.as_str())
}

fn load_library() -> Result<NdiLibrary, String> {
    let mut errors = Vec::new();
    for path in candidate_paths() {
        match unsafe { Library::new(&path) } {
            Ok(lib) => {
                // Resolve every symbol up-front — if we're on an older
                // Runtime missing one of `_v2` / `_v3` suffixes we want
                // to know now, not on first frame.
                match unsafe { NdiLibrary::from_lib(lib, path.clone()) } {
                    Ok(lib) => return Ok(lib),
                    Err(e) => errors.push(format!("  {}: {}", path.display(), e)),
                }
            }
            Err(e) => errors.push(format!("  {}: {}", path.display(), e)),
        }
    }
    let msg = format!(
        "NDI Runtime not installed — NDI sources and sinks are disabled. \
         Install from https://ndi.video/ to enable. Checked paths:\n{}",
        errors.join("\n"),
    );
    // Log once at startup so users discover the right URL via the
    // log panel even if they never open the NDI dropdown. This
    // `load_library` runs exactly once per process via the OnceLock
    // around `library()`, so no risk of duplicate spam.
    crate::system_log::warn(msg.clone());
    Err(msg)
}

impl NdiLibrary {
    /// SAFETY: `lib` must be a valid libndi handle — all 15 symbols we
    /// look up must be present, or we return `Err`.
    unsafe fn from_lib(lib: Library, loaded_from: PathBuf) -> Result<Self, String> {
        // Helper: resolve one symbol out of the lib. The `Symbol<T>`
        // borrows the Library, but a function-pointer `T` has `'static`
        // lifetime by construction — we dereference and discard the
        // borrow with the `*sym` copy.
        unsafe fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Result<T, String> {
            let sym: libloading::Symbol<T> = unsafe { lib.get(name) }.map_err(|e| {
                format!(
                    "symbol `{}` missing from libndi — outdated Runtime? ({})",
                    String::from_utf8_lossy(name),
                    e,
                )
            })?;
            Ok(*sym)
        }

        let initialize = unsafe { sym::<Fn_initialize>(&lib, b"NDIlib_initialize")? };
        let destroy = unsafe { sym::<Fn_destroy>(&lib, b"NDIlib_destroy")? };
        let is_supported_cpu =
            unsafe { sym::<Fn_is_supported_cpu>(&lib, b"NDIlib_is_supported_CPU")? };

        let find_create_v2 =
            unsafe { sym::<Fn_find_create_v2>(&lib, b"NDIlib_find_create_v2")? };
        let find_destroy = unsafe { sym::<Fn_find_destroy>(&lib, b"NDIlib_find_destroy")? };
        let find_get_current_sources = unsafe {
            sym::<Fn_find_get_current_sources>(&lib, b"NDIlib_find_get_current_sources")?
        };
        let find_wait_for_sources = unsafe {
            sym::<Fn_find_wait_for_sources>(&lib, b"NDIlib_find_wait_for_sources")?
        };

        let send_create = unsafe { sym::<Fn_send_create>(&lib, b"NDIlib_send_create")? };
        let send_destroy = unsafe { sym::<Fn_send_destroy>(&lib, b"NDIlib_send_destroy")? };
        let send_send_video_v2 =
            unsafe { sym::<Fn_send_send_video_v2>(&lib, b"NDIlib_send_send_video_v2")? };
        let send_get_no_connections = unsafe {
            sym::<Fn_send_get_no_connections>(&lib, b"NDIlib_send_get_no_connections")?
        };

        let recv_create_v3 =
            unsafe { sym::<Fn_recv_create_v3>(&lib, b"NDIlib_recv_create_v3")? };
        let recv_destroy = unsafe { sym::<Fn_recv_destroy>(&lib, b"NDIlib_recv_destroy")? };
        let recv_capture_v3 =
            unsafe { sym::<Fn_recv_capture_v3>(&lib, b"NDIlib_recv_capture_v3")? };
        let recv_free_video_v2 =
            unsafe { sym::<Fn_recv_free_video_v2>(&lib, b"NDIlib_recv_free_video_v2")? };

        // Sanity checks from NDI docs:
        //   1) CPU must support SSE4.2 / NEON; is_supported_cpu tells us.
        //   2) initialize must return true before any send/recv/find call.
        if !unsafe { is_supported_cpu() } {
            return Err("NDIlib_is_supported_CPU returned false — NDI requires SSE4.2 (Intel) or NEON (Apple Silicon)".into());
        }
        if !unsafe { initialize() } {
            return Err("NDIlib_initialize returned false — libndi rejected initialization".into());
        }

        Ok(NdiLibrary {
            _lib: lib,
            loaded_from,
            initialize,
            destroy,
            is_supported_cpu,
            find_create_v2,
            find_destroy,
            find_get_current_sources,
            find_wait_for_sources,
            send_create,
            send_destroy,
            send_send_video_v2,
            send_get_no_connections,
            recv_create_v3,
            recv_destroy,
            recv_capture_v3,
            recv_free_video_v2,
        })
    }
}

/// libndi search path, priority-ordered. See `load_library` module doc.
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for env_var in ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5"] {
        if let Ok(dir) = std::env::var(env_var) {
            paths.push(PathBuf::from(dir).join("libndi.dylib"));
        }
    }
    paths.push(PathBuf::from("/usr/local/lib/libndi.dylib"));
    paths.push(PathBuf::from(
        "/Library/NDI SDK for Apple/lib/macOS/libndi.dylib",
    ));
    paths.extend(scan_application_bundles());
    paths.push(PathBuf::from("libndi.dylib"));
    paths
}

fn scan_application_bundles() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/Applications") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("app") {
            continue;
        }
        let candidate = path.join("Contents/Frameworks/libndi.dylib");
        if candidate.exists() {
            out.push(candidate);
        }
    }
    out.sort();
    out
}

// ─── NdiFinder ──────────────────────────────────────────────────────────────

/// Source directory. Keep alive for the duration of a discovery session
/// — the internal list populates asynchronously via NDI's multicast
/// listener, so a just-created finder will return 0 sources for the
/// first 0.5–1 s.
pub struct NdiFinder {
    raw: NDIlib_find_instance_t,
}

// SAFETY: NDI finder instance is documented thread-safe per instance.
// Callers still hold a Mutex for shared access — we don't claim Sync.
unsafe impl Send for NdiFinder {}

/// One discovered NDI source. Name is the display name shown in every
/// NDI app's source picker (`HOSTNAME (Source Name)`); address is the
/// IP:port the sender is broadcasting from.
#[derive(Debug, Clone)]
pub struct NdiSourceInfo {
    pub name: String,
    pub address: String,
}

impl NdiFinder {
    pub fn new() -> Result<Self, String> {
        let lib = library()?;
        let opts = NDIlib_find_create_t {
            show_local_sources: true, // see our own sender for loopback tests
            p_groups: ptr::null(),
            p_extra_ips: ptr::null(),
        };
        let raw = unsafe { (lib.find_create_v2)(&opts) };
        if raw.is_null() {
            return Err("NDIlib_find_create_v2 returned null".into());
        }
        Ok(Self { raw })
    }

    /// Block for up to `timeout` waiting for the finder's internal
    /// source list to change. Returns `true` if there was a change
    /// during the window, `false` on timeout. Not required before
    /// calling `sources()` — that one returns whatever's known.
    pub fn wait_for_sources(&self, timeout: Duration) -> bool {
        let Ok(lib) = library() else { return false; };
        let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        unsafe { (lib.find_wait_for_sources)(self.raw, ms) }
    }

    /// Snapshot of currently-discovered sources.
    ///
    /// Copies each `p_ndi_name` / `p_url_or_ip_address` into owned
    /// Strings so the caller doesn't have to worry about the NDI-owned
    /// array being replaced on the next call. Cheap — a handful of
    /// strdups per invocation.
    pub fn sources(&self) -> Vec<NdiSourceInfo> {
        let Ok(lib) = library() else { return Vec::new(); };
        let mut count: u32 = 0;
        let ptr = unsafe { (lib.find_get_current_sources)(self.raw, &mut count) };
        if ptr.is_null() || count == 0 {
            return Vec::new();
        }
        let arr = unsafe { std::slice::from_raw_parts(ptr, count as usize) };
        arr.iter()
            .map(|src| NdiSourceInfo {
                name: cstr_to_string(src.p_ndi_name),
                address: cstr_to_string(src.p_url_or_ip_address),
            })
            .collect()
    }
}

impl Drop for NdiFinder {
    fn drop(&mut self) {
        if let Ok(lib) = library() {
            unsafe { (lib.find_destroy)(self.raw) };
        }
    }
}

// ─── NdiSender ───────────────────────────────────────────────────────────────

/// A running NDI sender. Drop = `NDIlib_send_destroy`, so any NDI
/// receiver subscribed to us loses the entry within one announce
/// cycle (~2–3 s on LAN).
pub struct NdiSender {
    raw: NDIlib_send_instance_t,
    /// User-chosen publish name, remembered so UI doesn't have to
    /// re-resolve it from libndi (which would require another FFI call).
    name: String,
}

// SAFETY: per NDI docs, a sender instance is thread-safe for use from
// a single owning thread at a time; Send matches. We don't claim Sync
// — the node wraps it in `Arc<Mutex<Option<...>>>`.
unsafe impl Send for NdiSender {}

impl NdiSender {
    pub fn new(name: &str) -> Result<Self, String> {
        let lib = library()?;
        let name_c =
            CString::new(name).map_err(|_| "sender name contains NUL byte".to_string())?;
        let opts = NDIlib_send_create_t {
            p_ndi_name: name_c.as_ptr(),
            p_groups: ptr::null(),
            clock_video: false, // node-side fps cap, not library-side
            clock_audio: false,
        };
        // SAFETY: `send_create` copies the name into its own buffer
        // per NDI docs, so `name_c` dropping here is safe.
        let raw = unsafe { (lib.send_create)(&opts) };
        if raw.is_null() {
            return Err(format!(
                "NDIlib_send_create returned null for name `{}`",
                name
            ));
        }
        Ok(Self { raw, name: name.to_string() })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Number of NDI receivers currently subscribed to this sender.
    /// `timeout = 0` returns immediately with the current count; non-zero
    /// blocks up to that many ms waiting for a subscriber to appear.
    /// Used by `VideoOutNode`'s UI state indicator.
    pub fn connection_count(&self, timeout_ms: u32) -> i32 {
        let Ok(lib) = library() else { return 0; };
        unsafe { (lib.send_get_no_connections)(self.raw, timeout_ms) as i32 }
    }

    /// Publish one BGRA frame. `bgra_pixels.len()` must equal
    /// `width * height * 4`; caller is responsible for swizzling
    /// RGBA → BGRA before this call (see `pixel_swizzle::rgba_to_bgra_in_place`).
    ///
    /// Synchronous — the NDI C API blocks until the frame is handed
    /// off to the network stack, so `bgra_pixels` can be safely reused
    /// immediately after return.
    pub fn publish_bgra(
        &self,
        width: u32,
        height: u32,
        bgra_pixels: &[u8],
        frame_rate_num: i32,
        frame_rate_den: i32,
    ) -> Result<(), String> {
        let expected = (width as usize) * (height as usize) * 4;
        if bgra_pixels.len() != expected {
            return Err(format!(
                "publish_bgra: buffer size {} != {width}x{height}x4 = {expected}",
                bgra_pixels.len()
            ));
        }
        let lib = library()?;
        let frame = NDIlib_video_frame_v2_t {
            xres: width as c_int,
            yres: height as c_int,
            FourCC: NDIlib_FourCC_video_type_e::BGRA,
            frame_rate_N: frame_rate_num,
            frame_rate_D: frame_rate_den,
            picture_aspect_ratio: width as f32 / height.max(1) as f32,
            frame_format_type: NDIlib_frame_format_type_e::Progressive,
            timecode: i64::MAX, // NDIlib_send_timecode_synthesize
            p_data: bgra_pixels.as_ptr() as *mut u8, // NDI reads only; const cast is OK
            line_stride_or_data_size: (width as c_int) * 4,
            p_metadata: ptr::null(),
            timestamp: 0, // NDI overwrites on send
        };
        // SAFETY: `frame.p_data` valid for the duration of this call;
        // NDI API is synchronous so the buffer is released by the time
        // we return. `line_stride_or_data_size` interpretation matches
        // FourCC = BGRA.
        unsafe { (lib.send_send_video_v2)(self.raw, &frame) };
        Ok(())
    }
}

impl Drop for NdiSender {
    fn drop(&mut self) {
        if let Ok(lib) = library() {
            unsafe { (lib.send_destroy)(self.raw) };
        }
    }
}

// ─── NdiReceiver ─────────────────────────────────────────────────────────────

/// A connected NDI receiver. M2 exposes a blocking-capture primitive
/// (`capture_video`); M5 wraps this in a background thread + swizzle
/// pipeline that lands frames into `VideoInNode::current_frame`.
pub struct NdiReceiver {
    raw: NDIlib_recv_instance_t,
}

// SAFETY: single-thread-at-a-time usage; the node's Mutex enforces.
unsafe impl Send for NdiReceiver {}

/// One captured video frame, BGRA byte order (FourCC guaranteed BGRA
/// or BGRX; callers treat BGRX byte-3 as alpha = 255 and don't have to
/// distinguish). Owned — we've already freed the NDI-internal buffer.
///
/// `Debug` skips the `bgra` vec for legibility; we print dims + stride
/// shape only.
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub frame_rate_num: i32,
    pub frame_rate_den: i32,
}

impl std::fmt::Debug for CapturedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bgra.len()", &self.bgra.len())
            .field("frame_rate", &(self.frame_rate_num, self.frame_rate_den))
            .finish()
    }
}

/// Outcome of a `capture_video` poll. `Frame` = video pixels ready;
/// `Empty` = everything else (timeout, audio, metadata, status change,
/// transient error). Callers loop on `Empty` — the session stays
/// alive until explicit stop. No variant for "fatal error" because
/// we haven't found a reliable signal NDI's C API emits for it; a
/// source that's really gone just times out in `Empty` forever.
#[derive(Debug)]
pub enum CaptureOutcome {
    Frame(CapturedFrame),
    Empty,
}

impl NdiReceiver {
    /// Connect to `source`. `recv_name` appears in the sender's
    /// connection list (useful for debugging when 3+ PatchWork
    /// instances are subscribed).
    pub fn new(source: &NdiSourceInfo, recv_name: &str) -> Result<Self, String> {
        let lib = library()?;
        // Both `source_to_connect_to.p_ndi_name` and `p_ndi_recv_name`
        // must stay alive for the duration of the C call. NDI copies
        // both internally (per SDK docs), so local CStrings are fine.
        let name_c = CString::new(source.name.as_str())
            .map_err(|_| "source name contains NUL".to_string())?;
        let addr_c = CString::new(source.address.as_str())
            .map_err(|_| "source address contains NUL".to_string())?;
        let recv_name_c = CString::new(recv_name)
            .map_err(|_| "recv name contains NUL".to_string())?;
        let opts = NDIlib_recv_create_v3_t {
            source_to_connect_to: NDIlib_source_t {
                p_ndi_name: name_c.as_ptr(),
                p_url_or_ip_address: addr_c.as_ptr(),
            },
            color_format: NDIlib_recv_color_format_e::BGRX_BGRA,
            bandwidth: NDIlib_recv_bandwidth_e::Highest,
            allow_video_fields: false,
            p_ndi_recv_name: recv_name_c.as_ptr(),
        };
        let raw = unsafe { (lib.recv_create_v3)(&opts) };
        if raw.is_null() {
            return Err(format!(
                "NDIlib_recv_create_v3 returned null for source `{}`",
                source.name
            ));
        }
        Ok(Self { raw })
    }

    /// Block for up to `timeout_ms` waiting for the next video frame.
    /// Returns `None` on timeout, `None` on a non-video frame (audio /
    /// metadata — we ignore those in v1), or `Err` on a capture error.
    ///
    /// On success, the BGRA bytes have been copied out of NDI's
    /// internal buffer and the buffer has been freed — the returned
    /// `CapturedFrame` is fully owned.
    /// Result of one `capture_video` attempt. `Frame` = got pixels;
    /// `Empty` = timeout / audio / metadata / status-change /
    /// error — all treated as "try again next tick" by the worker.
    /// The worker only exits on stop-flag flip, not on capture
    /// outcomes, so a source that momentarily hiccups doesn't tear
    /// down the receive session.
    pub fn capture_video(&self, timeout_ms: u32) -> CaptureOutcome {
        let Ok(lib) = library() else {
            return CaptureOutcome::Empty;
        };
        // Can't `mem::zeroed` because `NDIlib_FourCC_video_type_e`
        // has no `= 0` variant (0 isn't a valid NDI FourCC). NDI
        // fully overwrites the struct before returning Video, so
        // `MaybeUninit` is the correct shape.
        let mut video_mu = std::mem::MaybeUninit::<NDIlib_video_frame_v2_t>::uninit();
        // SAFETY: `video_mu.as_mut_ptr()` points to owned stack space
        // of the right size; NDI treats it as a write-only out-param.
        // Audio / metadata ptrs null → NDI skips. Timeout is u32 ms.
        let kind = unsafe {
            (lib.recv_capture_v3)(
                self.raw,
                video_mu.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                timeout_ms,
            )
        };

        // Read the enum discriminant as its raw int so an undocumented
        // value from a future NDI SDK doesn't trip UB on an exhaustive
        // Rust match. Compare against known variants explicitly.
        let kind_int = kind as i32;
        const VIDEO: i32 = NDIlib_frame_type_e::Video as i32;

        if kind_int == VIDEO {
            // SAFETY: NDI populated the struct on a Video return.
            let video = unsafe { video_mu.assume_init_ref() };
            let result = extract_frame(video);
            unsafe { (lib.recv_free_video_v2)(self.raw, video) };
            match result {
                Ok(frame) => CaptureOutcome::Frame(frame),
                Err(_) => CaptureOutcome::Empty,
            }
        } else {
            // Everything else (None, Audio, Metadata, Error,
            // StatusChange, undocumented) → keep the session alive.
            // `video_mu` stays uninit; no invalid-enum read.
            CaptureOutcome::Empty
        }
    }
}

impl Drop for NdiReceiver {
    fn drop(&mut self) {
        if let Ok(lib) = library() {
            unsafe { (lib.recv_destroy)(self.raw) };
        }
    }
}

fn extract_frame(frame: &NDIlib_video_frame_v2_t) -> Result<CapturedFrame, String> {
    if frame.p_data.is_null() {
        return Err("captured frame p_data is null".into());
    }
    let width = frame.xres.max(0) as u32;
    let height = frame.yres.max(0) as u32;
    let stride = frame.line_stride_or_data_size.max(0) as usize;
    let expected_packed = (width as usize) * 4;
    if stride < expected_packed {
        return Err(format!(
            "captured frame stride {stride} < expected {expected_packed} for {width}x{height}"
        ));
    }
    // Tolerate receivers that also handle BGRX — treat X as fully
    // opaque alpha downstream by setting the 4th byte to 255 if
    // FourCC == BGRX. Our BGRX_BGRA request ensures one of these two.
    match frame.FourCC {
        NDIlib_FourCC_video_type_e::BGRA | NDIlib_FourCC_video_type_e::BGRX => {}
    }

    let mut bgra = Vec::with_capacity(expected_packed * height as usize);
    // SAFETY: `p_data` valid for `stride * height` bytes per NDI docs.
    // We copy row-by-row to strip any trailing-stride padding.
    unsafe {
        for y in 0..height as usize {
            let row = frame.p_data.add(y * stride);
            let slice = std::slice::from_raw_parts(row, expected_packed);
            bgra.extend_from_slice(slice);
        }
    }

    // For BGRX → BGRA, force alpha = 255 so downstream blends / preview
    // don't render transparent.
    if matches!(frame.FourCC, NDIlib_FourCC_video_type_e::BGRX) {
        for px in bgra.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }

    Ok(CapturedFrame {
        width,
        height,
        bgra,
        frame_rate_num: frame.frame_rate_N,
        frame_rate_den: frame.frame_rate_D.max(1),
    })
}

fn cstr_to_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: NDI guarantees null-terminated strings on source fields.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: skip the test body when NDI isn't available. Keeps CI
    /// green on machines without the NDI Runtime.
    macro_rules! skip_without_ndi {
        () => {{
            if library().is_err() {
                eprintln!("skipping — NDI Runtime not installed on this machine");
                return;
            }
        }};
    }

    #[test]
    fn library_loads_and_caches() {
        skip_without_ndi!();
        let a = library().unwrap() as *const _;
        let b = library().unwrap() as *const _;
        assert_eq!(a, b, "library() must return the same NdiLibrary every call");
    }

    #[test]
    fn finder_creates_and_destroys() {
        skip_without_ndi!();
        let finder = NdiFinder::new().expect("NdiFinder::new");
        // Returns fast whether or not any sources are live.
        let _ = finder.sources();
        // Drop clean — should not crash.
    }

    #[test]
    fn sender_creates_and_destroys() {
        skip_without_ndi!();
        let sender = NdiSender::new("patchwork-test-sender").expect("NdiSender::new");
        assert_eq!(sender.name(), "patchwork-test-sender");
        // No connections yet (no timeout) — must be 0.
        assert_eq!(sender.connection_count(0), 0);
        // Drop clean.
    }

    /// Loopback: a sender publishes one solid-magenta BGRA frame while a
    /// receiver subscribed to it captures. Proves the full init →
    /// create → send → capture → destroy chain works end-to-end.
    ///
    /// Timing-sensitive — NDI's mDNS announce takes ~0.5–2 s to converge
    /// on localhost, so we retry the finder for up to 8 s before giving
    /// up. Real-world capture then usually lands within the next second.
    #[test]
    fn loopback_sender_receiver() {
        skip_without_ndi!();
        use std::thread;
        use std::time::Instant;

        const W: u32 = 64;
        const H: u32 = 64;
        const SENDER_NAME: &str = "patchwork-loopback";

        // Start the sender + push frames at 30 Hz from a background
        // thread so the receiver has something to capture.
        let sender = NdiSender::new(SENDER_NAME).expect("NdiSender::new");
        let mut bgra = vec![0u8; (W * H * 4) as usize];
        for px in bgra.chunks_exact_mut(4) {
            px[0] = 255; // B
            px[1] = 0;   // G
            px[2] = 255; // R (magenta: B=255, G=0, R=255)
            px[3] = 255;
        }
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();
        let sender_thread = thread::spawn(move || {
            while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = sender.publish_bgra(W, H, &bgra, 30, 1);
                thread::sleep(Duration::from_millis(33));
            }
            drop(sender);
        });

        // Discover it via a finder, retrying for up to 8 s.
        let finder = NdiFinder::new().expect("NdiFinder::new");
        let start = Instant::now();
        let info = loop {
            finder.wait_for_sources(Duration::from_millis(500));
            let sources = finder.sources();
            if let Some(info) = sources.into_iter().find(|s| s.name.contains(SENDER_NAME)) {
                break info;
            }
            if start.elapsed() > Duration::from_secs(8) {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                let _ = sender_thread.join();
                panic!("finder never saw our loopback sender `{}`", SENDER_NAME);
            }
        };

        let recv = NdiReceiver::new(&info, "patchwork-loopback-recv")
            .expect("NdiReceiver::new");

        // Capture one video frame. NDI typically delivers the first
        // within 0.5–2 s on localhost.
        let mut got_frame = false;
        let cap_start = Instant::now();
        while cap_start.elapsed() < Duration::from_secs(5) {
            if let CaptureOutcome::Frame(frame) = recv.capture_video(500) {
                assert_eq!(frame.width, W, "captured width");
                assert_eq!(frame.height, H, "captured height");
                assert_eq!(frame.bgra.len(), (W * H * 4) as usize);
                // Sanity: first pixel should be magenta-ish (B high,
                // G low, R high). NDI's network codec does light
                // chroma compression even at Highest bandwidth so we
                // use tolerance bands, not bit-exact equality.
                let p = &frame.bgra[0..4];
                assert!(p[0] > 200, "B channel ~255, got {}", p[0]);
                assert!(p[1] < 30, "G channel ~0, got {}", p[1]);
                assert!(p[2] > 200, "R channel ~255, got {}", p[2]);
                got_frame = true;
                break;
            }
        }

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = sender_thread.join();
        drop(recv);
        drop(finder);

        assert!(got_frame, "loopback failed to deliver a video frame in 5 s");
    }
}
