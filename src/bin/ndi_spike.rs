//! Phase 3 M1 — libloading probe for NDI Runtime.
//!
//! Goal: prove the dlopen → initialize → find → destroy chain works
//! end-to-end BEFORE `src/video_io/ndi.rs` exists. Mirrors the role the
//! Syphon spike played for Phase 2.
//!
//! What this proves:
//! 1. `libloading::Library::new` resolves `libndi.dylib` through the
//!    NewTek-convention search path (env → /usr/local/lib → SDK install
//!    → dyld default).
//! 2. All 6 minimum-viable NDI symbols (`initialize`, `is_supported_CPU`,
//!    `destroy`, `find_create_v2`, `find_destroy`,
//!    `find_get_current_sources`) resolve against a real runtime.
//! 3. Missing runtime → clean user-readable error, no crash, no dyld
//!    dependency on libndi in the compiled binary.
//!
//! Verification plan (from the Phase 3 plan, M1):
//! - With NDI Runtime NOT installed → exits 1 with install link message.
//! - With NDI Runtime installed → prints ≥1 source from the LAN within
//!   a few seconds (NDI Test Pattern, OBS NDI Out, or another spike).
//! - `otool -L target/debug/ndi_spike` shows NO libndi entry — the whole
//!   point of libloading is zero link-time reference.
//!
//! Run: `cargo run --bin ndi_spike`  (macOS only)

#![cfg(target_os = "macos")]
#![allow(non_camel_case_types)]

use libloading::Library;
use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::time::{Duration, Instant};

// ── Opaque NDI instance handle ──────────────────────────────────────────────
type NDIlib_find_instance_t = *mut c_void;

// ── C structs (transcribed from Processing.NDI.Find.h / structs.h) ─────────
//
// Verified against NDI SDK 6.0.1 Processing.NDI.structs.h.
// If NewTek reorder fields in a future SDK, a frame-size `const _:` check
// in `video_io/ndi.rs` (coming in M2) will catch it at compile time.

#[repr(C)]
struct NDIlib_find_create_t {
    show_local_sources: bool,
    p_groups: *const c_char,
    p_extra_ips: *const c_char,
}

// `NDIlib_source_t` contains a union {p_url_address, p_ip_address}. Both
// variants are `const char*` at the same offset, so a single pointer
// field matches the layout exactly — no `union { … }` needed here.
#[repr(C)]
struct NDIlib_source_t {
    p_ndi_name: *const c_char,
    p_url_or_ip_address: *const c_char,
}

// ── FFI function signatures ────────────────────────────────────────────────
type NDIlib_initialize_t = unsafe extern "C" fn() -> bool;
type NDIlib_destroy_t = unsafe extern "C" fn();
type NDIlib_is_supported_CPU_t = unsafe extern "C" fn() -> bool;
type NDIlib_find_create_v2_t =
    unsafe extern "C" fn(*const NDIlib_find_create_t) -> NDIlib_find_instance_t;
type NDIlib_find_destroy_t = unsafe extern "C" fn(NDIlib_find_instance_t);
type NDIlib_find_get_current_sources_t =
    unsafe extern "C" fn(NDIlib_find_instance_t, *mut u32) -> *const NDIlib_source_t;

/// NDI Runtime search path, in priority order. First hit wins.
///
/// The tricky bit on macOS: **NDI Tools doesn't install a shared
/// libndi into `/usr/local/lib`** — each Tools app embeds its own copy
/// under `Contents/Frameworks/libndi.dylib`. The older "NDI Runtime
/// Redist" installer did write to `/usr/local/lib`, but current-gen
/// Tools is self-contained. Same pattern for TouchDesigner, OBS-NDI,
/// etc.  So we probe the standard system paths first (fastest, works
/// when the user ran the Runtime installer or symlinked manually),
/// then fall back to scanning `/Applications/*.app/Contents/Frameworks/`
/// for any libndi an installed app is shipping. Dev-machine findings
/// from M1 on an Apple Silicon Mac with NDI Tools 6 installed:
///   - `/usr/local/lib/libndi.dylib` → not present
///   - `/Applications/NDI Scan Converter.app/Contents/Frameworks/libndi.dylib` → present (the one we load)
///   - Several `/Applications/NDI *.app/Contents/Frameworks/libndi_advanced.dylib` (Advanced SDK variant — skipped; different ABI)
fn candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // NewTek-convention env var overrides (the SDK uses V5 / V6 suffixes
    // per major version; honour both so we pick up either install).
    for env_var in ["NDI_RUNTIME_DIR_V6", "NDI_RUNTIME_DIR_V5"] {
        if let Ok(dir) = std::env::var(env_var) {
            paths.push(PathBuf::from(dir).join("libndi.dylib"));
        }
    }

    // Classic install locations on macOS:
    // - Runtime installer drops the dylib directly into /usr/local/lib.
    // - Full NDI SDK install puts it under its own app-support folder.
    paths.push(PathBuf::from("/usr/local/lib/libndi.dylib"));
    paths.push(PathBuf::from(
        "/Library/NDI SDK for Apple/lib/macOS/libndi.dylib",
    ));

    // Current-gen fallback: scan /Applications for any .app that ships
    // a `Contents/Frameworks/libndi.dylib`. Covers NDI Tools 5/6 and
    // any third-party NDI-aware app that redistributes the lib.
    //
    // We deliberately skip `libndi_advanced.dylib` — it's the Advanced
    // SDK build with a slightly different public surface (extra
    // KVM / routing / IPCam functions), and mixing the two at load
    // time would risk symbol-resolution surprises later.
    paths.extend(scan_application_bundles());

    // Bare filename — dyld's own search (DYLD_LIBRARY_PATH, /usr/lib, …).
    // Catches homebrew and user-made symlinks.
    paths.push(PathBuf::from("libndi.dylib"));

    paths
}

/// Return `/Applications/*.app/Contents/Frameworks/libndi.dylib` paths
/// that actually exist on disk. One `read_dir` over /Applications —
/// cheap (a few ms), called once per process.
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
    // Stable order keeps the output deterministic across runs (handy
    // when diffing spike logs). NDI Tools apps sort before third-party.
    out.sort();
    out
}

fn try_load() -> Result<(Library, PathBuf), String> {
    let mut errors = Vec::new();
    for path in candidate_paths() {
        match unsafe { Library::new(&path) } {
            Ok(lib) => return Ok((lib, path)),
            Err(e) => errors.push(format!("  {}: {}", path.display(), e)),
        }
    }
    Err(format!(
        "libndi.dylib not found — install NDI Runtime from ndi.video\n\nTried:\n{}",
        errors.join("\n"),
    ))
}

/// Resolve an NDI symbol, mapping libloading errors to a user-readable
/// string so the caller can report which symbol went missing (helps when
/// an older Runtime is installed and the `_v2`/`_v3` suffix we wanted
/// isn't there).
unsafe fn sym<'a, T>(
    lib: &'a Library,
    name: &[u8],
) -> Result<libloading::Symbol<'a, T>, String> {
    unsafe { lib.get(name) }.map_err(|e| {
        format!(
            "symbol `{}` missing from libndi — Runtime may be outdated: {}",
            String::from_utf8_lossy(name),
            e,
        )
    })
}

fn run() -> Result<(), String> {
    println!("[ndi-spike] starting libloading probe");

    let (lib, path) = try_load()?;
    println!("[ndi-spike] loaded: {}", path.display());

    // Resolve everything up front so a missing symbol surfaces before
    // any side-effecting call (initialize / create).
    let init: libloading::Symbol<NDIlib_initialize_t> =
        unsafe { sym(&lib, b"NDIlib_initialize")? };
    let destroy: libloading::Symbol<NDIlib_destroy_t> =
        unsafe { sym(&lib, b"NDIlib_destroy")? };
    let is_supported: libloading::Symbol<NDIlib_is_supported_CPU_t> =
        unsafe { sym(&lib, b"NDIlib_is_supported_CPU")? };
    let find_create: libloading::Symbol<NDIlib_find_create_v2_t> =
        unsafe { sym(&lib, b"NDIlib_find_create_v2")? };
    let find_destroy: libloading::Symbol<NDIlib_find_destroy_t> =
        unsafe { sym(&lib, b"NDIlib_find_destroy")? };
    let find_get_current_sources: libloading::Symbol<NDIlib_find_get_current_sources_t> =
        unsafe { sym(&lib, b"NDIlib_find_get_current_sources")? };

    println!("[ndi-spike] all 6 symbols resolved");

    if !unsafe { is_supported() } {
        return Err("NDIlib_is_supported_CPU returned false (CPU missing SSE4.2 on Intel?)".into());
    }
    println!("[ndi-spike] CPU supported");

    if !unsafe { init() } {
        return Err("NDIlib_initialize returned false".into());
    }
    println!("[ndi-spike] NDI initialized");

    // Ensure NDI teardown runs even if the scan loop panics. RAII guard.
    struct NdiShutdown<F: Fn()> {
        destroy: F,
    }
    impl<F: Fn()> Drop for NdiShutdown<F> {
        fn drop(&mut self) {
            (self.destroy)();
            println!("[ndi-spike] NDI destroyed");
        }
    }
    let _teardown = NdiShutdown {
        destroy: || unsafe { destroy() },
    };

    // Finder options: show_local_sources = true so a loopback test can
    // see its own sender without needing a second machine.
    let find_opts = NDIlib_find_create_t {
        show_local_sources: true,
        p_groups: ptr::null(),
        p_extra_ips: ptr::null(),
    };
    let finder = unsafe { find_create(&find_opts) };
    if finder.is_null() {
        return Err("NDIlib_find_create_v2 returned null".into());
    }
    println!("[ndi-spike] finder created");

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        let mut count: u32 = 0;
        let sources_ptr = unsafe { find_get_current_sources(finder, &mut count) };
        let elapsed = start.elapsed().as_secs_f32();

        if sources_ptr.is_null() || count == 0 {
            println!("[ndi-spike] t={:.1}s  {} sources", elapsed, count);
        } else {
            println!("[ndi-spike] t={:.1}s  {} sources:", elapsed, count);
            let sources = unsafe { std::slice::from_raw_parts(sources_ptr, count as usize) };
            for (i, src) in sources.iter().enumerate() {
                let name = cstr(src.p_ndi_name);
                let addr = cstr(src.p_url_or_ip_address);
                println!("  [{}] {}  @  {}", i, name, addr);
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    unsafe { find_destroy(finder) };
    println!("[ndi-spike] finder destroyed");
    // `_teardown` drops here, calling NDIlib_destroy.
    Ok(())
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return "(null)".into();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn main() {
    match run() {
        Ok(()) => println!("[ndi-spike] done."),
        Err(e) => {
            eprintln!("[ndi-spike] ERROR: {}", e);
            std::process::exit(1);
        }
    }
}
