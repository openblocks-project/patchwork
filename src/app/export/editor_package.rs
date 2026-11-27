//! P6 Stage 3.11: the Patchwork editor as an installable package.
//!
//! [`package_editor`] lays out the release editor in a folder the installer
//! builder ([`super::installer`]) turns into `Patchwork Setup.exe`, with the
//! same rules as an export:
//!
//! - `patchwork.exe` (the editor) and `patchwork-player.exe` (the Player
//!   runtime Export as App copies into apps), both release builds of the
//!   same source;
//! - every DLL either imports that Windows does not provide, approved by
//!   the export's dependency rules (DirectML pinned by hash; the Visual C++
//!   runtime only from a Visual Studio redistributable folder, signed by
//!   Microsoft), never from system folders. The Visual C++ files are also
//!   kept in that folder layout under `redist\`, so an installed Patchwork
//!   can export apps on a computer without Visual Studio;
//! - ffmpeg with the notices given for it;
//! - the licence notices, where the source is, and the internal test labels
//!   while embedded components are not cleared for redistribution;
//! - `OPTIONAL-TOOLS.txt`: the tools Patchwork can use but does not bundle.
//!
//! Never included: a `player.json` (beside the editor it would make it a
//! Player), projects, trust records, keys, settings or session data. The
//! package only reads the build; it never writes outside `destination`.
//!
//! It is a release step, not an editor feature: run by
//! `manual_build_editor_installer` (`scripts/build-windows-installers.ps1`).

// Release packaging: used by tests and the manual build only.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{
    CopyItem, ExportEnvironment, FfmpegSource, LICENSES_DIR, PATCHWORK_LICENSE, SOURCE_URL,
    THIRD_PARTY_DIR, check_ffmpeg, launch, list_files, native_dependencies, pe, runtime,
    sha256_file,
};

/// The editor's name: installer, shortcuts, Installed apps.
pub const EDITOR_NAME: &str = "Patchwork";
/// The editor program.
pub const EDITOR_EXE: &str = "patchwork.exe";
/// The tools Patchwork can use but does not bundle.
pub const OPTIONAL_TOOLS_FILE: &str = "OPTIONAL-TOOLS.txt";

/// What to package.
#[derive(Debug, Clone)]
pub struct EditorPackageRequest {
    /// The release editor (`patchwork.exe`), with the Player beside it.
    pub editor: PathBuf,
    /// ffmpeg and its notices (camera, video and audio streams need it).
    pub ffmpeg: FfmpegSource,
    /// The package folder to create (must not exist).
    pub destination: PathBuf,
    /// The Player and the redistributable folders; `None`: as Export as
    /// App finds them for this editor ([`runtime::discover`]).
    pub runtime: Option<super::RuntimeSource>,
}

/// A release build of `flavor` at `exe`, and its build information.
fn check_program(
    exe: &Path,
    flavor: launch::Binary,
    errors: &mut Vec<String>,
) -> Option<(Vec<u8>, pe::BuildInfo)> {
    let bytes = match std::fs::read(exe) {
        Ok(bytes) => bytes,
        Err(e) => {
            errors.push(format!("{}: {e}", exe.display()));
            return None;
        }
    };
    if pe::build_profile(&bytes) != Ok(pe::BuildProfile::Release) {
        errors.push(format!(
            "{} is not a release build; build it with scripts/build-release-windows.ps1.",
            exe.display()
        ));
    }
    match pe::build_info(&bytes) {
        Ok(info) if info.flavor == flavor.flavor() => Some((bytes, info)),
        Ok(info) => {
            errors.push(format!(
                "{} is the Patchwork {} build, not the {} build.",
                exe.display(),
                info.flavor,
                flavor.flavor()
            ));
            None
        }
        Err(why) => {
            errors.push(format!("{}: {why}", exe.display()));
            None
        }
    }
}

/// `licenses/SOURCE.txt` of the editor package.
pub fn editor_source_notice(build: Option<&pe::BuildInfo>) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut text = format!(
        "Corresponding source of Patchwork\r\n\r\n\
         patchwork.exe (the Patchwork editor) and patchwork-player.exe (the Player runtime \
         that Export as App copies into apps) are Patchwork {version}, licensed under the GNU \
         General Public License v3 (Patchwork-LICENSE.txt).\r\n\r\n\
         Source repository: {SOURCE_URL}\r\n"
    );
    match build {
        Some(info) if info.exact_commit().is_some() => text.push_str(&format!(
            "Source commit: {}\r\n\r\nBoth programs were built from exactly this commit, \
             without changes.\r\n",
            info.commit
        )),
        Some(info) if info.commit != "unknown" => text.push_str(&format!(
            "Source commit: {} (with local changes)\r\n\r\nThe programs were NOT built from \
             this commit alone: their source had changes that are not in the repository. \
             That commit is therefore not their corresponding source; the exact source used \
             must be provided with this installation.\r\n",
            info.commit
        )),
        _ => text.push_str(
            "Source commit: unknown\r\n\r\nThe programs do not record which source they were \
             built from; the exact source used must be provided with this installation.\r\n",
        ),
    }
    text.push_str(
        "\r\nA repository link alone does not meet every obligation of the GPL. Whoever \
         distributes Patchwork must make its complete corresponding source available as \
         section 6 of the licence requires, for as long as they distribute it.\r\n",
    );
    text
}

/// `OPTIONAL-TOOLS.txt`: what Patchwork can use but does not bundle.
pub fn optional_tools() -> String {
    "Patchwork — optional tools (not included)\r\n\
     \r\n\
     These programs are used by some nodes when they are installed on the computer (and\r\n\
     found on PATH). They are not bundled: their redistribution has not been verified for\r\n\
     this build. Everything else works without them.\r\n\
     \r\n\
     - ffprobe (part of FFmpeg): media durations in video and audio players. Without it\r\n\
       durations show as unknown. (ffmpeg itself IS included.)\r\n\
     - yt-dlp: YouTube / Vimeo addresses in Video In. Direct video URLs work without it.\r\n\
     - Python with esptool (python -m esptool): flashing ESP32 boards from the hardware\r\n\
       panel. Raspberry Pi Pico boards are flashed without it.\r\n\
     - espeak-ng: phonemes for some text-to-speech voices.\r\n\
     - Rust (cargo): building Rust plugin nodes.\r\n\
     \r\n\
     Exported apps never depend on any of these: an app's folder contains everything it\r\n\
     uses (and ffmpeg when its project needs it).\r\n"
        .to_string()
}

/// `licenses/THIRD-PARTY-NOTICES.txt` of the editor package.
fn editor_notices(
    internal_test: bool,
    dependencies: &[String],
    notices: &BTreeMap<String, String>,
    ffmpeg_notices: bool,
) -> String {
    let mut text = String::new();
    if internal_test {
        text.push_str(&format!(
            "*** {} ***\r\nThis installation must not be distributed: see {} (and ../{}).\r\n\r\n",
            runtime::INTERNAL_TEST_LABEL,
            runtime::NOT_FOR_DISTRIBUTION_NOTICE,
            runtime::INTERNAL_TEST_MARKER
        ));
    }
    text.push_str(&format!(
        "Patchwork {} is free software under the GNU General Public License v3 \
         (Patchwork-LICENSE.txt). Where to get its corresponding source: SOURCE.txt.\r\n\r\n\
         This folder also contains, unmodified:\r\n",
        env!("CARGO_PKG_VERSION")
    ));
    for dep in dependencies {
        text.push_str(&format!("  - {dep}\r\n"));
    }
    text.push_str("\r\nTheir licence terms:\r\n");
    for name in notices.keys() {
        text.push_str(&format!("  - {name}\r\n"));
    }
    text.push_str(
        "\r\nThe programs also contain, compiled in: Rust crates, ONNX Runtime, the MediaPipe \
         tracking models, libopus, HIDAPI, ring and fonts. Their notices are in third-party/ \
         (index: third-party/README.txt):\r\n",
    );
    for (name, _) in runtime::THIRD_PARTY_FILES {
        text.push_str(&format!("  - third-party/{name}\r\n"));
    }
    if ffmpeg_notices {
        text.push_str("  - ffmpeg/ (the notices provided with the bundled ffmpeg)\r\n");
    }
    text
}

/// Where the redistributable folder `redist` is mirrored inside the
/// package: `redist\VC\Redist\MSVC\<version>\x64\<crt>`.
fn mirrored_redist(redist: &Path) -> Option<PathBuf> {
    let parts: Vec<_> = redist.components().collect();
    if parts.len() < 6 || !runtime::is_vc_redist_dir(redist) {
        return None;
    }
    let mut out = PathBuf::from(runtime::BUNDLED_REDIST_DIR);
    for part in &parts[parts.len() - 6..] {
        out.push(part.as_os_str());
    }
    Some(out)
}

/// The internal-test notice of the editor package: the app's wording, about
/// the editor.
pub fn editor_not_for_distribution_text(unresolved: &[(String, String)]) -> String {
    runtime::not_for_distribution_text(EDITOR_NAME, unresolved)
        .replace(
            "is an internal test build of a Patchwork project",
            "is an internal test build of the Patchwork editor",
        )
        .replace("It was exported as", "It was packaged as")
        .replace(
            "and the app exported again as a distributable build",
            "and Patchwork packaged again as a distributable build",
        )
}

/// Lay out the editor package at `request.destination`. Returns its files
/// (relative, sorted). On failure nothing is left at the destination.
pub fn package_editor(
    request: &EditorPackageRequest,
    env: &ExportEnvironment,
) -> Result<Vec<String>, Vec<String>> {
    let mut errors = Vec::new();
    if request.destination.exists() {
        return Err(vec![format!(
            "{} already exists; choose a new folder.",
            request.destination.display()
        )]);
    }
    let editor = check_program(&request.editor, launch::Binary::Editor, &mut errors);
    let discovered = match &request.runtime {
        Some(source) => Ok(source.clone()),
        None => runtime::discover(&request.editor),
    };
    let runtime_source = match discovered {
        Ok(source) => Some(source),
        Err(e) => {
            errors.push(e);
            None
        }
    };
    let player = runtime_source
        .as_ref()
        .and_then(|s| check_program(&s.executable, launch::Binary::Player, &mut errors));
    if let (Some((_, e)), Some((bytes, p))) = (&editor, &player) {
        if e.commit != p.commit || e.modified != p.modified {
            errors.push(format!(
                "The editor ({}) and the Player ({}) were built from different source; build \
                 both together.",
                e.commit, p.commit
            ));
        }
        if pe::subsystem(bytes) != Ok(pe::SUBSYSTEM_GUI) {
            errors.push("The Player runtime must be a GUI program.".to_string());
        }
    }
    // P6 Stage 3.12: the installed editor opens no console window.
    if let Some((bytes, _)) = &editor {
        if pe::subsystem(bytes) != Ok(pe::SUBSYSTEM_GUI) {
            errors.push(format!(
                "{} is a console program, which would open a console window beside the \
                 editor; build it with scripts/build-release-windows.ps1 (the \
                 windows-gui-editor feature).",
                request.editor.display()
            ));
        }
    }
    // Every DLL either program needs, approved by the export's rules.
    let mut files: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut dependencies = Vec::new();
    let mut notices = BTreeMap::new();
    if let (Some(source), Some(_), Some(_)) = (&runtime_source, &editor, &player) {
        for exe in [&request.editor, &source.executable] {
            let imports = match std::fs::read(exe)
                .map_err(|e| e.to_string())
                .and_then(|b| pe::imported_dlls(&b))
            {
                Ok(imports) => imports,
                Err(why) => {
                    errors.push(format!("{}: {why}", exe.display()));
                    continue;
                }
            };
            let found =
                native_dependencies(exe, imports, &source.dependency_dirs, env, &mut errors);
            for item in found.files {
                files.entry(item.to).or_insert(item.from);
            }
            for dep in found.dependencies {
                if !dependencies
                    .iter()
                    .any(|d: &String| d.eq_ignore_ascii_case(&dep))
                {
                    dependencies.push(dep);
                }
            }
            notices.extend(found.notices);
        }
    }
    let ffmpeg = match check_ffmpeg(&request.ffmpeg) {
        Ok(ffmpeg) => Some(ffmpeg),
        Err(e) => {
            errors.push(e);
            None
        }
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    let (Some((_, build)), Some(source), Some((ffmpeg_exe, ffmpeg_licences))) =
        (editor, runtime_source, ffmpeg)
    else {
        return Err(vec!["The package could not be planned.".to_string()]);
    };
    let internal_test = !env.unresolved.is_empty();

    let mut copies: Vec<CopyItem> = vec![
        CopyItem {
            from: request.editor.clone(),
            to: EDITOR_EXE.to_string(),
        },
        CopyItem {
            from: source.executable.clone(),
            to: runtime::PLAYER_EXE.to_string(),
        },
    ];
    for (name, from) in &files {
        // The Visual C++ runtime, also in its redistributable layout (what
        // an installed Patchwork exports apps from).
        if runtime::is_vc_runtime(name) {
            if let Some(dir) = from.parent().and_then(mirrored_redist) {
                copies.push(CopyItem {
                    from: from.clone(),
                    to: dir.join(name).to_string_lossy().replace('\\', "/"),
                });
            }
        }
        copies.push(CopyItem {
            from: from.clone(),
            to: name.clone(),
        });
    }
    copies.push(ffmpeg_exe);
    let ffmpeg_notices = !ffmpeg_licences.is_empty();
    copies.extend(ffmpeg_licences);

    let destination = &request.destination;
    let result = (|| -> Result<(), String> {
        for item in &copies {
            let to = destination.join(item.to.replace('/', "\\"));
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::copy(&item.from, &to).map_err(|e| format!("{}: {e}", item.to))?;
            if sha256_file(&to)? != sha256_file(&item.from)? {
                return Err(format!("{} changed while it was copied.", item.to));
            }
        }
        let licenses = destination.join(LICENSES_DIR);
        std::fs::create_dir_all(licenses.join(THIRD_PARTY_DIR)).map_err(|e| e.to_string())?;
        let write = |name: &str, text: &str| {
            std::fs::write(licenses.join(name), text).map_err(|e| format!("{name}: {e}"))
        };
        write("Patchwork-LICENSE.txt", PATCHWORK_LICENSE)?;
        write("SOURCE.txt", &editor_source_notice(Some(&build)))?;
        for (name, text) in &notices {
            write(name, &text.replace("\r\n", "\n").replace('\n', "\r\n"))?;
        }
        for (name, text) in runtime::THIRD_PARTY_FILES {
            write(&format!("{THIRD_PARTY_DIR}/{name}"), text)?;
        }
        if internal_test {
            let text = editor_not_for_distribution_text(&env.unresolved);
            write(runtime::NOT_FOR_DISTRIBUTION_NOTICE, &text)?;
            std::fs::write(destination.join(runtime::INTERNAL_TEST_MARKER), &text)
                .map_err(|e| e.to_string())?;
        }
        write(
            "THIRD-PARTY-NOTICES.txt",
            &editor_notices(internal_test, &dependencies, &notices, ffmpeg_notices),
        )?;
        std::fs::write(destination.join(OPTIONAL_TOOLS_FILE), optional_tools())
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(destination);
        return Err(vec![e]);
    }
    Ok(list_files(destination))
}
