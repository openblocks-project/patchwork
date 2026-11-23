//! P6 Stage 2.3: where an export's runtime comes from, and which native
//! dependencies may be bundled with it.
//!
//! - The runtime is `patchwork-player.exe` from the editor's own folder: the
//!   Windows GUI-subsystem build of the same source ([`discover`]).
//! - Every DLL it (or a bundled DLL) imports that Windows does not provide
//!   must be an approved redistributable ([`DependencyRules`]):
//!   - **DirectML** (`DirectML.dll`): only the exact build Patchwork is
//!     built against, identified by its SHA-256. It is distributed under
//!     Microsoft's DirectML licence terms, which allow distribution in
//!     applications ([`DIRECTML_NOTICE`]).
//!   - **Microsoft Visual C++ runtime** (`vcruntime140*.dll`,
//!     `msvcp140*.dll`, `concrt140.dll`, `vccorlib140.dll`): only from an
//!     installed Visual Studio's `VC\Redist\MSVC\<version>\x64\
//!     Microsoft.VC14x.CRT` folder (the "Distributable Code" Visual Studio's
//!     licence lets licensed users redistribute; never `debug_nonredist`),
//!     with a valid Authenticode signature.
//!   - Anything else is refused: nothing is copied from Windows' folders or
//!     from hand-assembled folders, and nothing unverified is used.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The runtime binary built beside the editor (`[[bin]] patchwork-player`).
pub const PLAYER_EXE: &str = "patchwork-player.exe";

/// DirectML 1.15.4 (x64), the build ONNX Runtime 1.24.2 (pyke's `ort`
/// binaries) ships and Patchwork links against.
pub const DIRECTML_SHA256: &str =
    "9c9e6d822561c6c41b90e6994b3e8857cf1d66dbfb1e0c4c799c7c89b4e92da1";

pub const DIRECTML_NOTICE: &str = "\
DirectML (DirectML.dll), version 1.15.4, x64
Copyright (c) Microsoft Corporation. All rights reserved.

Included unmodified. It is the DirectML build distributed with ONNX Runtime
1.24.2 (the binaries Patchwork's `ort` dependency uses); SHA-256
9c9e6d822561c6c41b90e6994b3e8857cf1d66dbfb1e0c4c799c7c89b4e92da1.

It is licensed under the MICROSOFT SOFTWARE LICENSE TERMS - MICROSOFT
DIRECTX MACHINE LEARNING (DIRECTML), which allow copying and distributing the
software in applications you develop, and forbid removing or modifying
Microsoft's notices or distributing it as a stand-alone offering. The full
terms: https://www.nuget.org/packages/Microsoft.AI.DirectML/1.15.4/License
";

/// The full DirectML 1.15.4 licence terms (from nuget.org).
pub const DIRECTML_LICENSE: &str =
    include_str!("../../../licenses/third-party/DirectML-LICENSE.txt");

/// P6 Stage 2.4: the reviewed licence notices of everything compiled into
/// the Player runtime, shipped as `licenses/third-party/<name>` (see
/// `licenses/third-party/README.md`).
pub const THIRD_PARTY_FILES: &[(&str, &str)] = &[
    (
        "README.txt",
        include_str!("../../../licenses/third-party/README.md"),
    ),
    (
        "rust-crates.txt",
        include_str!("../../../licenses/third-party/rust-crates.txt"),
    ),
    (
        "onnxruntime-LICENSE.txt",
        include_str!("../../../licenses/third-party/onnxruntime-LICENSE.txt"),
    ),
    (
        "onnxruntime-ThirdPartyNotices.txt",
        include_str!("../../../licenses/third-party/onnxruntime-ThirdPartyNotices.txt"),
    ),
    (
        "libopus-COPYING.txt",
        include_str!("../../../licenses/third-party/libopus-COPYING.txt"),
    ),
    (
        "hidapi-LICENSE.txt",
        include_str!("../../../licenses/third-party/hidapi-LICENSE.txt"),
    ),
    (
        "hidapi-LICENSE-bsd.txt",
        include_str!("../../../licenses/third-party/hidapi-LICENSE-bsd.txt"),
    ),
    (
        "ring-LICENSE.txt",
        include_str!("../../../licenses/third-party/ring-LICENSE.txt"),
    ),
    (
        "ring-LICENSE-BoringSSL.txt",
        include_str!("../../../licenses/third-party/ring-LICENSE-BoringSSL.txt"),
    ),
    (
        "ring-LICENSE-other-bits.txt",
        include_str!("../../../licenses/third-party/ring-LICENSE-other-bits.txt"),
    ),
    (
        "Phosphor-LICENSE.txt",
        include_str!("../../../licenses/third-party/Phosphor-LICENSE.txt"),
    ),
    (
        "Outfit-OFL.txt",
        include_str!("../../../licenses/third-party/Outfit-OFL.txt"),
    ),
];

/// P6 Stage 2.4: components embedded in every Patchwork binary whose
/// redistribution terms are not established. While any is listed, real
/// exports are refused (fail closed); see `licenses/third-party/README.md`.
pub const UNRESOLVED_COMPONENTS: &[(&str, &str)] = &[
    // P6 Stage 2.5: traced by SHA-256 to Qualcomm AI Hub's Hugging Face
    // repositories, whose model cards put these exported assets under the
    // proprietary, revocable "Qualcomm AI Hub Proprietary License" (not
    // GPL-compatible; restrictions would bind every export author).
    (
        "MediaPipeHandDetector.onnx (Qualcomm AI Hub, qualcomm/MediaPipe-Hand-Detection)",
        "it is licensed under the proprietary, revocable Qualcomm AI Hub Proprietary License",
    ),
    (
        "MediaPipeFaceLandmarkDetector.onnx (Qualcomm AI Hub, qualcomm/MediaPipe-Face-Detection)",
        "it is licensed under the proprietary, revocable Qualcomm AI Hub Proprietary License",
    ),
    (
        "MediaPipePoseDetector.onnx (Qualcomm AI Hub, qualcomm/MediaPipe-Pose-Estimation)",
        "it is licensed under the proprietary, revocable Qualcomm AI Hub Proprietary License",
    ),
    // Not found in any official source checked (Qualcomm AI Hub and OpenCV
    // Model Zoo histories); tf2onnx 1.16.1 conversions of unknown origin.
    (
        "MediaPipeFaceDetector.onnx",
        "its source and licence could not be traced",
    ),
    (
        "MediaPipeHandLandmark.onnx",
        "its source and licence could not be traced",
    ),
    (
        "MediaPipePoseLandmark.onnx",
        "its source and licence could not be traced",
    ),
];

/// The exact files behind [`UNRESOLVED_COMPONENTS`] (file in
/// `assets/presets/ml/`, SHA-256). Kept so every internal test build names
/// precisely what it contains; a test checks them against the files.
pub const UNRESOLVED_MODEL_SHA256: &[(&str, &str)] = &[
    (
        "MediaPipeHandDetector.onnx",
        "2af615f1e6df6ac0ef01a85d93713ea21c9bc59a87524caa3d5f16b8e74e60f3",
    ),
    (
        "MediaPipeFaceLandmarkDetector.onnx",
        "3482ce52873546a94a05c2fb9d38e3499adba87a7c55745061cb8a375ee838df",
    ),
    (
        "MediaPipePoseDetector.onnx",
        "8288a955cb0b379e520fd3bc76b6c19b670e9bac39ed3c26898fc7f513cbe6d8",
    ),
    (
        "MediaPipeFaceDetector.onnx",
        "4d4700749360befd0d9fb56fedd2e8f682649de826dfa34dbeccbb169dcb7dbe",
    ),
    (
        "MediaPipeHandLandmark.onnx",
        "ce9cd3e871d8b11819ee6a695c44fd8ac2dee7361b5d94c55d01803e99221744",
    ),
    (
        "MediaPipePoseLandmark.onnx",
        "91366321af68df7ce3b7f786c17e668e07327fe0f3cf41141ffae9767c007600",
    ),
];

/// P6 Stage 2.6: how builds containing uncleared components are labelled.
pub const INTERNAL_TEST_LABEL: &str = "INTERNAL TEST BUILD — NOT FOR DISTRIBUTION";
/// The marker at the root of an internal test export.
pub const INTERNAL_TEST_MARKER: &str = "INTERNAL-TEST-BUILD-NOT-FOR-DISTRIBUTION.txt";
/// The same marker beside the licences.
pub const NOT_FOR_DISTRIBUTION_NOTICE: &str = "NOT-FOR-DISTRIBUTION.txt";

/// The label this binary must carry: it embeds uncleared components, so
/// any Player built from it is an internal test build. Compiled in, so no
/// file in an export folder can remove it.
pub fn internal_test_label() -> Option<&'static str> {
    (!UNRESOLVED_COMPONENTS.is_empty()).then_some(INTERNAL_TEST_LABEL)
}

/// The text of an internal test build's markers.
pub fn not_for_distribution_text(app_name: &str, unresolved: &[(String, String)]) -> String {
    let mut text = format!(
        "{INTERNAL_TEST_LABEL}\r\n\r\n\
         {app_name} is an internal test build of a Patchwork project, made for development \
         and testing only. It must not be distributed, published, sold or given to anyone \
         outside that testing: it is NOT cleared for distribution.\r\n"
    );
    if unresolved.is_empty() {
        text.push_str("\r\nIt was exported as an internal test build by choice.\r\n");
    } else {
        text.push_str(
            "\r\nIt contains components whose redistribution is not permitted or not \
             established:\r\n",
        );
        for (what, why) in unresolved {
            text.push_str(&format!("  - {what}: {why}\r\n"));
        }
        text.push_str("\r\nExact files (SHA-256):\r\n");
        for (file, sha) in UNRESOLVED_MODEL_SHA256 {
            text.push_str(&format!("  - {file}  {sha}\r\n"));
        }
    }
    text.push_str(
        "\r\nOutstanding before any distribution: each component must be cleared (its source \
         and redistribution licence established and its notice added) or replaced, and the \
         app exported again as a distributable build. Evidence and options: \
         licenses/third-party/README.txt (Patchwork: licenses/third-party/README.md).\r\n",
    );
    text
}

pub const VC_RUNTIME_NOTICE: &str = "\
Microsoft Visual C++ runtime
Copyright (c) Microsoft Corporation. All rights reserved.

Included unmodified from the Visual Studio installation named in
THIRD-PARTY-NOTICES.txt (its VC\\Redist\\MSVC folder). These files are
\"Distributable Code\" under the Microsoft Software License Terms for Visual
Studio, which permit holders of a validly licensed copy of Visual Studio to
distribute them with their programs, subject to those terms. The
Distributable Code list:
https://learn.microsoft.com/visualstudio/releases/2022/redistribution
";

/// A DLL accepted only as one exact file.
#[derive(Debug, Clone)]
pub struct PinnedDll {
    /// Lower-case file name.
    pub name: String,
    /// Lower-case hex SHA-256.
    pub sha256: String,
    /// What it is (for notices and messages).
    pub component: String,
    /// Written to `licenses/<notice_file>`.
    pub notice_file: String,
    pub notice: String,
}

/// Checks a file's Authenticode signature.
pub type SignatureCheck = fn(&Path) -> Result<(), String>;

/// Which DLLs an export may bundle (see the module docs).
#[derive(Debug, Clone)]
pub struct DependencyRules {
    pub pinned: Vec<PinnedDll>,
    pub verify_signature: SignatureCheck,
}

impl DependencyRules {
    /// The rules for real exports.
    pub fn windows() -> Self {
        Self {
            pinned: vec![PinnedDll {
                name: "directml.dll".to_string(),
                sha256: DIRECTML_SHA256.to_string(),
                component: "DirectML 1.15.4 (Microsoft)".to_string(),
                notice_file: "DirectML-NOTICE.txt".to_string(),
                notice: format!("{DIRECTML_NOTICE}\n{DIRECTML_LICENSE}"),
            }],
            verify_signature: verify_authenticode,
        }
    }
}

/// How a bundled DLL was approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    /// A pinned file (index into [`DependencyRules::pinned`]).
    Pinned(usize),
    /// A Visual C++ runtime file from this Visual Studio redist folder.
    VcRuntime { redist: PathBuf },
}

/// Is `name` a Visual C++ runtime file?
pub fn is_vc_runtime(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let Some(stem) = name.strip_suffix(".dll") else {
        return false;
    };
    stem == "concrt140"
        || stem == "vccorlib140"
        || stem.starts_with("vcruntime140")
        || stem.starts_with("msvcp140")
}

/// `dir` is `…\VC\Redist\MSVC\<version>\x64\Microsoft.VC14x.CRT` (not a
/// debug or OneCore variant): Visual Studio's redistributable folder.
pub fn is_vc_redist_dir(dir: &Path) -> bool {
    let parts: Vec<String> = dir
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    let n = parts.len();
    if n < 6 {
        return false;
    }
    let crt = &parts[n - 1];
    let version = &parts[n - 3];
    crt.starts_with("microsoft.vc14")
        && crt.ends_with(".crt")
        && !crt.contains("debug")
        && parts[n - 2] == "x64"
        && !version.is_empty()
        && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        && parts[n - 4] == "msvc"
        && parts[n - 5] == "redist"
        && parts[n - 6] == "vc"
        && !parts
            .iter()
            .any(|p| p.contains("debug_nonredist") || p == "onecore")
}

/// Approve `file` (found for import `name`) or say why not.
pub fn approve(rules: &DependencyRules, name: &str, file: &Path) -> Result<Approval, String> {
    let lower = name.to_ascii_lowercase();
    if let Some(index) = rules.pinned.iter().position(|p| p.name == lower) {
        let pinned = &rules.pinned[index];
        let digest = sha256_of(file)?;
        if digest != pinned.sha256 {
            return Err(format!(
                "{} is not the verified {} build (SHA-256 {digest}, expected {}). Rebuild \
                 Patchwork so its own copy is used.",
                file.display(),
                pinned.component,
                pinned.sha256
            ));
        }
        return Ok(Approval::Pinned(index));
    }
    if is_vc_runtime(&lower) {
        let dir = file.parent().unwrap_or(Path::new(""));
        if !is_vc_redist_dir(dir) {
            return Err(format!(
                "{name} must come from an installed Visual Studio redistributable folder \
                 (VC\\Redist\\MSVC\\<version>\\x64\\Microsoft.VC14x.CRT), not {}.",
                dir.display()
            ));
        }
        (rules.verify_signature)(file)
            .map_err(|why| format!("{} does not have a valid signature: {why}", file.display()))?;
        return Ok(Approval::VcRuntime {
            redist: dir.to_path_buf(),
        });
    }
    Err(format!(
        "{name} is not an approved redistributable dependency; the export cannot include it."
    ))
}

fn sha256_of(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

// ── Discovery ───────────────────────────────────────────────────────────────

/// The newest Visual Studio x64 VC++ redistributable folder under the
/// `Microsoft Visual Studio` folders of `program_files` (e.g.
/// `C:\Program Files`), if any.
pub fn find_vc_redist(program_files: &[PathBuf]) -> Option<PathBuf> {
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    let dirs = |p: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(p)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default()
    };
    for root in program_files {
        for year in dirs(&root.join("Microsoft Visual Studio")) {
            for edition in dirs(&year) {
                for version in dirs(&edition.join("VC").join("Redist").join("MSVC")) {
                    let Some(numbers) =
                        version.file_name().and_then(|n| n.to_str()).and_then(|n| {
                            n.split('.')
                                .map(|p| p.parse::<u64>().ok())
                                .collect::<Option<Vec<u64>>>()
                        })
                    else {
                        continue;
                    };
                    for crt in dirs(&version.join("x64")) {
                        if is_vc_redist_dir(&crt) && crt.join("vcruntime140.dll").is_file() {
                            let newer = best.as_ref().is_none_or(|(b, _)| numbers > *b);
                            if newer {
                                best = Some((numbers.clone(), crt));
                            }
                        }
                    }
                }
            }
        }
    }
    best.map(|(_, dir)| dir)
}

/// The runtime for exports made by the editor at `editor_exe`: the Player
/// build beside it, and this computer's Visual Studio redistributables.
pub fn discover(editor_exe: &Path) -> Result<super::RuntimeSource, String> {
    let dir = editor_exe
        .parent()
        .ok_or("The Patchwork folder cannot be found.")?;
    let player = dir.join(PLAYER_EXE);
    if !player.is_file() {
        return Err(format!(
            "The Patchwork Player runtime ({PLAYER_EXE}) is not next to Patchwork in {}. \
             Build it with `cargo build --release` (it is built with the editor).",
            dir.display()
        ));
    }
    let program_files: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|var| std::env::var_os(var).map(PathBuf::from))
        .collect();
    let redist = find_vc_redist(&program_files).ok_or(
        "No Visual Studio redistributable folder (VC\\Redist\\MSVC\\…\\x64\\Microsoft.VC14x.CRT) \
         was found. Install Visual Studio 2022 (or its Build Tools) with the C++ workload; the \
         Visual C++ runtime is only taken from there.",
    )?;
    Ok(super::RuntimeSource {
        executable: player,
        dependency_dirs: vec![redist],
    })
}

// ── Authenticode ────────────────────────────────────────────────────────────

/// A valid Authenticode signature chaining to a trusted root (Windows'
/// `WinVerifyTrust`, offline: no revocation or URL retrieval).
#[cfg(windows)]
pub fn verify_authenticode(path: &Path) -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }
    #[repr(C)]
    struct WintrustFileInfo {
        cb_struct: u32,
        file_path: *const u16,
        file: isize,
        known_subject: *const Guid,
    }
    #[repr(C)]
    struct WintrustData {
        cb_struct: u32,
        policy_callback_data: *mut c_void,
        sip_client_data: *mut c_void,
        ui_choice: u32,
        revocation_checks: u32,
        union_choice: u32,
        file: *mut WintrustFileInfo,
        state_action: u32,
        state_data: isize,
        url_reference: *mut u16,
        prov_flags: u32,
        ui_context: u32,
        signature_settings: *mut c_void,
    }
    #[link(name = "wintrust")]
    unsafe extern "system" {
        fn WinVerifyTrust(hwnd: isize, action: *mut Guid, data: *mut c_void) -> i32;
    }
    const WTD_UI_NONE: u32 = 2;
    const WTD_REVOKE_NONE: u32 = 0;
    const WTD_CHOICE_FILE: u32 = 1;
    const WTD_STATEACTION_VERIFY: u32 = 1;
    const WTD_STATEACTION_CLOSE: u32 = 2;
    const WTD_CACHE_ONLY_URL_RETRIEVAL: u32 = 0x1000;
    const WTD_REVOCATION_CHECK_NONE: u32 = 0x10;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut file = WintrustFileInfo {
        cb_struct: std::mem::size_of::<WintrustFileInfo>() as u32,
        file_path: wide.as_ptr(),
        file: 0,
        known_subject: std::ptr::null(),
    };
    let mut data = WintrustData {
        cb_struct: std::mem::size_of::<WintrustData>() as u32,
        policy_callback_data: std::ptr::null_mut(),
        sip_client_data: std::ptr::null_mut(),
        ui_choice: WTD_UI_NONE,
        revocation_checks: WTD_REVOKE_NONE,
        union_choice: WTD_CHOICE_FILE,
        file: &mut file,
        state_action: WTD_STATEACTION_VERIFY,
        state_data: 0,
        url_reference: std::ptr::null_mut(),
        prov_flags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE,
        ui_context: 0,
        signature_settings: std::ptr::null_mut(),
    };
    // WINTRUST_ACTION_GENERIC_VERIFY_V2
    let mut action = Guid {
        data1: 0x00aa_c56b,
        data2: 0xcd44,
        data3: 0x11d0,
        data4: [0x8c, 0xc2, 0x00, 0xc0, 0x4f, 0xc2, 0x95, 0xee],
    };
    // SAFETY: the structures match the Windows SDK layouts, every pointer
    // refers to locals that outlive both calls, and the second call releases
    // the state the first allocated.
    let status = unsafe {
        let status = WinVerifyTrust(-1, &mut action, (&mut data as *mut WintrustData).cast());
        data.state_action = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(-1, &mut action, (&mut data as *mut WintrustData).cast());
        status
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!("signature check failed (0x{:08x})", status as u32))
    }
}

#[cfg(not(windows))]
pub fn verify_authenticode(_path: &Path) -> Result<(), String> {
    Err("signatures can only be checked on Windows".to_string())
}
