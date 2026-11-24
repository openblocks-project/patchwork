//! P6 Stage 3.7: a Windows installer (NSIS) for a completed portable export.
//!
//! The builder consumes an export folder written by [`super::export_portable`]
//! and never changes it. It checks the export first (manifest, project
//! digest, Player runtime, notices, dependencies, labels, no links, nothing
//! secret or personal), copies it into a staging folder (each file hashed
//! against the check), and generates the NSIS script itself: nothing from
//! the project is ever run or included as script. Every path and name in the
//! script is escaped. With `makensis` available the installer is compiled
//! into the staging folder, checked and only then published; without it the
//! complete installer source is published instead, to be compiled later.
//!
//! What the installer does:
//! - Per user, no administrator rights: `%LOCALAPPDATA%\Programs\<name>` by
//!   default. A folder that is not empty is refused unless it holds an
//!   earlier installation of the same app (which is uninstalled first).
//! - Installs exactly the export's files (the Player unchanged, its
//!   notices, models embedded in it, DLLs, ffmpeg, the project and
//!   `player.json`, kiosk setting included), a Start menu shortcut, an
//!   optional desktop shortcut, "Launch" on the last page, and an entry in
//!   Installed apps.
//! - Writes nothing else: no trust records (the first run still asks for
//!   permission), no Startup entry, nothing in `~/.patchwork`.
//! - The uninstaller removes only the files it installed (by name, never a
//!   whole folder), its shortcuts and its registry entry. Anything else in
//!   the folder, and every user file, stays.

// Used by tests and the manual build until the Export as App dialog offers
// it (see the Stage 3.7 report).
#![cfg_attr(not(test), allow(dead_code))]

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::runtime;

/// The installer's file name: `<app> Setup.exe`.
pub const SETUP_SUFFIX: &str = " Setup.exe";
/// The installed uninstaller.
pub const UNINSTALLER: &str = "Uninstall.exe";
/// Written beside the installed app: which app this folder holds (so an
/// earlier installation can be told from an unrelated folder).
pub const INSTALL_MARKER: &str = "patchwork-installed-app.txt";
/// The generated script, in the installer source folder.
pub const SCRIPT_FILE: &str = "installer.nsi";
/// The export's copy inside the installer source folder.
pub const PAYLOAD_DIR: &str = "payload";
/// The list of installed files with their SHA-256 (installer source).
pub const FILE_LIST: &str = "installed-files.txt";
/// How to compile the source (installer source).
pub const SOURCE_README: &str = "README-INSTALLER-SOURCE.txt";
/// What `makensis` writes, inside the staging folder.
/// Not `setup.exe`: Windows applies compatibility shims to a program of
/// exactly that name (NSIS warning 9000).
pub const STAGED_SETUP: &str = "staged-installer.exe";
/// Shown with every internal test installer.
pub const INTERNAL_TEST_SUFFIX: &str = " (INTERNAL TEST - NOT FOR DISTRIBUTION)";
/// Runtime libraries every Player export carries (case-insensitive).
const REQUIRED_LIBRARIES: &[&str] = &["directml.dll", "vcruntime140.dll", "msvcp140.dll"];
/// Notices every export carries.
const REQUIRED_NOTICES: &[&str] = &[
    "licenses/Patchwork-LICENSE.txt",
    "licenses/SOURCE.txt",
    "licenses/THIRD-PARTY-NOTICES.txt",
];

/// What to build.
#[derive(Debug, Clone)]
pub struct InstallerRequest {
    /// A completed portable export (the folder with `player.json`).
    pub export_dir: PathBuf,
    /// Where `<app> Setup.exe` (or its source) is published.
    pub output_dir: PathBuf,
    /// Replace an earlier installer of the same name (explicit only).
    pub overwrite: bool,
    /// The NSIS compiler; `None`: publish the installer source only.
    pub makensis: Option<PathBuf>,
    /// This computer's Patchwork data folder (an export inside it is
    /// refused, as the Player refuses it).
    pub user_data: Option<PathBuf>,
    /// Text no installed file may contain (personal paths such as the
    /// user's profile folder), compared without regard to ASCII case, as
    /// UTF-8 and UTF-16.
    pub forbidden_text: Vec<String>,
}

impl InstallerRequest {
    /// The installer for `export_dir`, published in `output_dir`, with this
    /// computer's tool, data folder and profile path.
    pub fn for_this_computer(export_dir: PathBuf, output_dir: PathBuf) -> Self {
        Self {
            export_dir,
            output_dir,
            overwrite: false,
            makensis: find_makensis(),
            user_data: super::super::player::manifest::trust_dir(),
            forbidden_text: personal_paths(),
        }
    }
}

/// The profile folder of the user running this, in both separator styles.
pub fn personal_paths() -> Vec<String> {
    std::env::var("USERPROFILE")
        .ok()
        .filter(|p| p.len() > 3)
        .map(|p| vec![p.clone(), p.replace('\\', "/")])
        .unwrap_or_default()
}

/// `makensis.exe`: `PATCHWORK_MAKENSIS`, then `PATH`, then the standard NSIS
/// install folders. Nothing is installed when it is missing.
pub fn find_makensis() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATCHWORK_MAKENSIS").map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let on_path = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|dir| dir.join("makensis.exe"));
    let standard = ["ProgramFiles(x86)", "ProgramFiles"]
        .iter()
        .filter_map(|var| std::env::var_os(var))
        .map(|dir| PathBuf::from(dir).join("NSIS").join("makensis.exe"));
    on_path.chain(standard).find(|p| p.is_file())
}

/// One installed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledFile {
    /// Relative to the installation folder, `/`-separated.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// What the installer installs, from a checked export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallerPlan {
    pub app_name: String,
    /// `<app>.exe`, the Player runtime.
    pub exe: String,
    /// The Installed apps key name (stable per app name; ASCII).
    pub app_id: String,
    pub version: String,
    /// Labelled NOT FOR DISTRIBUTION.
    pub internal_test: bool,
    pub kiosk: bool,
    pub project_sha256: String,
    /// Every file of the export, sorted.
    pub files: Vec<InstalledFile>,
}

impl InstallerPlan {
    /// The name shown by the installer, Installed apps and the shortcuts'
    /// tooltip: labelled for an internal test build.
    pub fn display_name(&self) -> String {
        if self.internal_test {
            format!("{}{INTERNAL_TEST_SUFFIX}", self.app_name)
        } else {
            self.app_name.clone()
        }
    }

    pub fn setup_file_name(&self) -> String {
        format!("{}{SETUP_SUFFIX}", self.app_name)
    }

    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// The result of a build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallerOutcome {
    /// `<app> Setup.exe` was compiled, checked and published.
    Built {
        installer: PathBuf,
        size: u64,
        sha256: String,
    },
    /// No NSIS compiler: the complete installer source was published.
    SourceOnly { source_dir: PathBuf, reason: String },
}

/// The Installed apps key name for `app_name`.
pub fn app_id(app_name: &str) -> String {
    let digest = Sha256::digest(format!("patchwork.player.installer:{app_name}").as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("PatchworkApp-{hex}")
}

/// Check the export at `export_dir` and plan its installer. Every problem
/// is listed; nothing is written.
pub fn plan_installer(request: &InstallerRequest) -> Result<InstallerPlan, Vec<String>> {
    let dir = &request.export_dir;
    let mut errors = Vec::new();
    // The export as the Player loads it (structure, containment, no links
    // in the project).
    let exported =
        match super::super::player::manifest::load_manifest(dir, request.user_data.as_deref()) {
            Ok(exported) => exported,
            Err(e) => return Err(vec![format!("Not a usable portable export: {e}")]),
        };
    let manifest = &exported.manifest;
    let project_json = &exported.launch.project_json;
    // The project is the content the manifest records.
    match std::fs::read_to_string(project_json)
        .map_err(|e| e.to_string())
        .and_then(|text| crate::library::trust::content::project_digest(&text))
    {
        Ok(digest) if digest == manifest.project_sha256 => {}
        Ok(_) => errors.push(
            "The exported project changed after export (its content no longer matches \
             player.json); export it again."
                .to_string(),
        ),
        Err(e) => errors.push(format!("The exported project cannot be read: {e}")),
    }
    let graph = super::super::io::load_project_graph(project_json).ok();

    // Every file: no links, nothing secret, no name the installer uses.
    let mut files = Vec::new();
    match walk(dir) {
        Ok(found) => files = found,
        Err(e) => errors.push(e),
    }
    let lower: Vec<String> = files.iter().map(|f| f.to_ascii_lowercase()).collect();
    for (file, low) in files.iter().zip(&lower) {
        let name = low.rsplit('/').next().unwrap_or_default();
        if super::FORBIDDEN_NAMES.contains(&name) {
            errors.push(format!("The export contains {file}; refused."));
        }
        if low == &UNINSTALLER.to_ascii_lowercase() || low == &INSTALL_MARKER.to_ascii_lowercase() {
            errors.push(format!(
                "The export contains {file}, a name the installer uses itself."
            ));
        }
    }
    let has = |rel: &str| lower.iter().any(|f| f == &rel.to_ascii_lowercase());

    // The program is the Player runtime, named after the app.
    let exe = format!("{}.exe", manifest.title);
    if !has(&exe) {
        errors.push(format!("The export has no {exe}."));
    } else {
        match std::fs::read(dir.join(&exe)) {
            Ok(bytes)
                if super::pe::subsystem(&bytes) == Ok(super::pe::SUBSYSTEM_GUI)
                    && super::pe::build_info(&bytes).map(|i| i.flavor).as_deref()
                        == Ok(super::launch::Binary::Player.flavor()) => {}
            Ok(_) => errors.push(format!("{exe} is not the Patchwork Player runtime.")),
            Err(e) => errors.push(format!("{exe}: {e}")),
        }
    }
    for library in REQUIRED_LIBRARIES {
        if !has(library) {
            errors.push(format!(
                "The export is missing {library}, which the Player needs."
            ));
        }
    }
    if graph.as_ref().is_some_and(super::needs_ffmpeg) && !has("ffmpeg.exe") {
        errors.push("The project uses ffmpeg, and the export has no ffmpeg.exe.".to_string());
    }
    for notice in REQUIRED_NOTICES {
        if !has(notice) {
            errors.push(format!("The export is missing {notice}."));
        }
    }

    // Labelled exactly as what it is: an internal test build carries both
    // markers; a build with uncleared components must be one.
    let markers = [
        runtime::INTERNAL_TEST_MARKER.to_string(),
        format!("licenses/{}", runtime::NOT_FOR_DISTRIBUTION_NOTICE),
    ];
    let marked = markers.iter().filter(|m| has(m)).count();
    let internal_test = marked > 0;
    if marked == 1 {
        errors.push("The export is missing one of its internal test labels.".to_string());
    }
    if !internal_test && !runtime::UNRESOLVED_COMPONENTS.is_empty() {
        errors.push(
            "The export is labelled distributable, but Patchwork embeds components not cleared \
             for redistribution: only an internal test build can have an installer."
                .to_string(),
        );
    }

    // Hash every file; refuse personal paths.
    let needles = needles(&request.forbidden_text);
    let mut planned = Vec::new();
    for file in &files {
        match std::fs::read(dir.join(file)) {
            Ok(bytes) => {
                if needles.iter().any(|n| contains_ignore_case(&bytes, n)) {
                    errors.push(format!(
                        "{file} contains a personal path of this computer; refused (build the \
                         Player with scripts/build-release-windows.ps1)."
                    ));
                }
                planned.push(InstalledFile {
                    path: file.clone(),
                    size: bytes.len() as u64,
                    sha256: hex(&Sha256::digest(&bytes)),
                });
            }
            Err(e) => errors.push(format!("{file}: {e}")),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(InstallerPlan {
        app_id: app_id(&manifest.title),
        app_name: manifest.title.clone(),
        exe,
        version: manifest.patchwork_version.clone(),
        internal_test,
        kiosk: manifest.kiosk,
        project_sha256: manifest.project_sha256.clone(),
        files: planned,
    })
}

/// Every file under `root` (relative, `/`-separated, sorted). Links and
/// junctions are refused.
fn walk(root: &Path) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            let rel = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            if meta.file_type().is_symlink() || is_reparse_point(&meta) {
                return Err(format!("The export contains a link ({rel}); refused."));
            }
            if meta.is_dir() {
                stack.push(path);
            } else {
                out.push(rel);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &std::fs::Metadata) -> bool {
    false
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The byte patterns of `texts` (lowercase), as UTF-8 and UTF-16LE.
fn needles(texts: &[String]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for text in texts.iter().filter(|t| !t.is_empty()) {
        let lower = text.to_ascii_lowercase();
        out.push(lower.as_bytes().to_vec());
        out.push(lower.encode_utf16().flat_map(|u| u.to_le_bytes()).collect());
    }
    out
}

/// Whether `haystack` contains `needle` (already lowercase), ignoring ASCII
/// case.
fn contains_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    let Some(&first) = needle.first() else {
        return false;
    };
    if haystack.len() < needle.len() {
        return false;
    }
    let last = haystack.len() - needle.len();
    let mut i = 0;
    while i <= last {
        if haystack[i].to_ascii_lowercase() == first
            && haystack[i..i + needle.len()]
                .iter()
                .zip(needle)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
        {
            return true;
        }
        i += 1;
    }
    false
}

/// `text` as an NSIS string literal: quoted, with `$` and `"` escaped and
/// line breaks / tabs written as NSIS escapes. Other control characters
/// are refused (no file or app name contains one).
pub fn nsis_string(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '$' => out.push_str("$$"),
            '"' => out.push_str("$\\\""),
            '\r' => out.push_str("$\\r"),
            '\n' => out.push_str("$\\n"),
            '\t' => out.push_str("$\\t"),
            c if c.is_control() => {
                return Err(format!("{text:?} contains a control character."));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(out)
}

/// `path` (relative, `/`-separated) with Windows separators.
fn windows_path(path: &str) -> String {
    path.replace('/', "\\")
}

/// The folders holding the plan's files, deepest first.
fn folders(plan: &InstallerPlan) -> Vec<String> {
    let mut dirs = std::collections::BTreeSet::new();
    for file in &plan.files {
        let mut parts: Vec<&str> = file.path.split('/').collect();
        parts.pop();
        while !parts.is_empty() {
            dirs.insert(parts.join("/"));
            parts.pop();
        }
    }
    let mut dirs: Vec<String> = dirs.into_iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse((d.matches('/').count(), d.clone())));
    dirs
}

/// `x.y.z.0` for the version resource, when the version is numeric.
fn version_resource(version: &str) -> Option<String> {
    let mut parts: Vec<&str> = version.split('.').collect();
    let numeric = (1..=4).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()));
    if !numeric {
        return None;
    }
    while parts.len() < 4 {
        parts.push("0");
    }
    Some(parts.join("."))
}

/// The NSIS script for `plan`. Source files are read from [`PAYLOAD_DIR`]
/// beside the script; the installer is written to `setup.exe` beside it.
pub fn nsis_script(plan: &InstallerPlan) -> Result<String, String> {
    let q = nsis_string;
    let name = plan.display_name();
    let app = &plan.app_name;
    let key = format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        plan.app_id
    );
    // `$INSTDIR\…` and `$SMPROGRAMS\…`: the variable stays, the rest is
    // escaped.
    let at = |base: &str, rel: &str| -> Result<String, String> {
        let escaped = q(&windows_path(rel))?;
        Ok(format!("\"{base}\\{}", &escaped[1..]))
    };
    let installed_exe = at("$INSTDIR", &plan.exe)?;
    let start_menu = at("$SMPROGRAMS", &format!("{app}.lnk"))?;
    let desktop = at("$DESKTOP", &format!("{app}.lnk"))?;
    let label = if plan.internal_test {
        runtime::INTERNAL_TEST_LABEL
    } else {
        ""
    };
    let mut s = String::new();
    let mut line = |text: String| {
        s.push_str(&text);
        s.push_str("\r\n");
    };
    line("; Generated by Patchwork (P6 Stage 3.7) from a portable export.".into());
    line("; Do not edit: generate it again from the export.".into());
    if plan.internal_test {
        line(format!("; {label}"));
    }
    line(String::new());
    line("Unicode true".into());
    line("ManifestDPIAware true".into());
    line("SetCompressor /SOLID lzma".into());
    line("RequestExecutionLevel user".into());
    line("!include \"MUI2.nsh\"".into());
    line("!include \"LogicLib.nsh\"".into());
    line("!include \"FileFunc.nsh\"".into());
    line("!include \"Sections.nsh\"".into());
    line(String::new());
    line(format!("Name {}", q(&name)?));
    line(format!("OutFile \"{STAGED_SETUP}\""));
    line(format!(
        "InstallDir {}",
        at("$LOCALAPPDATA\\Programs", app)?
    ));
    line(format!(
        "InstallDirRegKey HKCU {} \"InstallLocation\"",
        q(&key)?
    ));
    line(format!(
        "BrandingText {}",
        q(&if plan.internal_test {
            format!("Patchwork — {label}")
        } else {
            "Patchwork".to_string()
        })?
    ));
    line("ShowInstDetails show".into());
    line("ShowUninstDetails show".into());
    if let Some(version) = version_resource(&plan.version) {
        line(format!("VIProductVersion \"{version}\""));
        line(format!("VIAddVersionKey \"ProductName\" {}", q(&name)?));
        line(format!(
            "VIAddVersionKey \"FileDescription\" {}",
            q(&format!("{name} Setup"))?
        ));
        line(format!("VIAddVersionKey \"FileVersion\" \"{version}\""));
        line(format!("VIAddVersionKey \"ProductVersion\" \"{version}\""));
        line(
            "VIAddVersionKey \"LegalCopyright\" \"See the licenses folder of the installed app.\""
                .into(),
        );
        line(format!(
            "VIAddVersionKey \"Comments\" {}",
            q(if plan.internal_test {
                label
            } else {
                "Made with Patchwork"
            })?
        ));
    }
    line(String::new());
    // Pages.
    let welcome = if plan.internal_test {
        format!(
            "This installs {app} for the current Windows user.\r\n\r\n{label}: it embeds \
             components that are not cleared for redistribution. Use it only for internal \
             development and testing.\r\n\r\nNo administrator rights are needed. The first \
             time {app} runs, it asks for permission to run its project."
        )
    } else {
        format!(
            "This installs {app} for the current Windows user. No administrator rights are \
             needed. The first time {app} runs, it asks for permission to run its project."
        )
    };
    line(format!("!define MUI_WELCOMEPAGE_TEXT {}", q(&welcome)?));
    line("!insertmacro MUI_PAGE_WELCOME".into());
    let licence = if plan.internal_test {
        format!("licenses/{}", runtime::NOT_FOR_DISTRIBUTION_NOTICE)
    } else {
        "licenses/Patchwork-LICENSE.txt".to_string()
    };
    line(format!(
        "!insertmacro MUI_PAGE_LICENSE {}",
        q(&format!("{PAYLOAD_DIR}\\{}", windows_path(&licence)))?
    ));
    line("!insertmacro MUI_PAGE_COMPONENTS".into());
    line("!define MUI_PAGE_CUSTOMFUNCTION_LEAVE CheckInstallFolder".into());
    line("!insertmacro MUI_PAGE_DIRECTORY".into());
    line("!insertmacro MUI_PAGE_INSTFILES".into());
    line(format!("!define MUI_FINISHPAGE_RUN {installed_exe}"));
    line(format!(
        "!define MUI_FINISHPAGE_RUN_TEXT {}",
        q(&format!("Launch {app}"))?
    ));
    line("!insertmacro MUI_PAGE_FINISH".into());
    line("!insertmacro MUI_UNPAGE_CONFIRM".into());
    line("!insertmacro MUI_UNPAGE_INSTFILES".into());
    line("!insertmacro MUI_LANGUAGE \"English\"".into());
    line(String::new());

    // The installation folder: never a drive root, the profile folder or
    // Patchwork's data; empty, or an earlier installation of this app.
    line("Function CheckInstallFolder".into());
    line("  StrLen $0 \"$INSTDIR\"".into());
    line("  ${If} $0 <= 3".into());
    line("    MessageBox MB_ICONSTOP \"Choose a folder, not a whole drive.\" /SD IDOK".into());
    line("    Abort".into());
    line("  ${EndIf}".into());
    line("  ${If} \"$INSTDIR\" == \"$PROFILE\"".into());
    line("  ${OrIf} \"$INSTDIR\" == \"$PROFILE\\.patchwork\"".into());
    line("    MessageBox MB_ICONSTOP \"Choose a folder of its own for the app.\" /SD IDOK".into());
    line("    Abort".into());
    line("  ${EndIf}".into());
    line("  StrLen $0 \"$PROFILE\\.patchwork\\\"".into());
    line("  StrCpy $1 \"$INSTDIR\" $0".into());
    line("  ${If} $1 == \"$PROFILE\\.patchwork\\\"".into());
    line("    MessageBox MB_ICONSTOP \"Choose a folder of its own for the app.\" /SD IDOK".into());
    line("    Abort".into());
    line("  ${EndIf}".into());
    line("  Call ReadMarker".into());
    line(format!("  ${{If}} $0 == {}", q(&plan.app_id)?));
    line("    Return".into());
    line("  ${EndIf}".into());
    line("  Call FolderIsEmpty".into());
    line("  ${If} $0 != \"empty\"".into());
    line(format!(
        "    MessageBox MB_ICONSTOP {} /SD IDOK",
        q(&format!(
            "The folder is not empty and does not hold an earlier installation of {app}. \
             Choose an empty folder."
        ))?
    ));
    line("    Abort".into());
    line("  ${EndIf}".into());
    line("FunctionEnd".into());
    line(String::new());
    for prefix in ["", "un."] {
        line(format!("Function {prefix}ReadMarker"));
        line("  StrCpy $0 \"\"".into());
        line(format!(
            "  IfFileExists {} 0 done",
            at("$INSTDIR", INSTALL_MARKER)?
        ));
        line(format!(
            "  FileOpen $2 {} r",
            at("$INSTDIR", INSTALL_MARKER)?
        ));
        line("  FileRead $2 $0".into());
        line("  FileClose $2".into());
        line("  done:".into());
        line("FunctionEnd".into());
        line(String::new());
    }
    line("Function FolderIsEmpty".into());
    line("  StrCpy $0 \"empty\"".into());
    line("  IfFileExists \"$INSTDIR\\*.*\" 0 done".into());
    line("  FindFirst $2 $3 \"$INSTDIR\\*.*\"".into());
    line("  loop:".into());
    line("    StrCmp $3 \"\" close".into());
    line("    StrCmp $3 \".\" next".into());
    line("    StrCmp $3 \"..\" next".into());
    line("    StrCpy $0 \"not empty\"".into());
    line("    Goto close".into());
    line("  next:".into());
    line("    FindNext $2 $3".into());
    line("    Goto loop".into());
    line("  close:".into());
    line("  FindClose $2".into());
    line("  done:".into());
    line("FunctionEnd".into());
    line(String::new());

    // Install.
    line(format!("Section {} SecApp", q(&format!("!{app}"))?));
    line("  SectionIn RO".into());
    line("  SetShellVarContext current".into());
    line("  Call CheckInstallFolder".into());
    line("  ; An earlier installation of this app: uninstall it first.".into());
    line("  Call ReadMarker".into());
    line(format!("  ${{If}} $0 == {}", q(&plan.app_id)?));
    line(format!(
        "  ${{AndIf}} ${{FileExists}} {}",
        at("$INSTDIR", UNINSTALLER)?
    ));
    line(format!(
        "    ExecWait '\"$INSTDIR\\{UNINSTALLER}\" /S _?=$INSTDIR'"
    ));
    line(format!("    Delete {}", at("$INSTDIR", UNINSTALLER)?));
    line("  ${EndIf}".into());
    let mut current_dir: Option<String> = None;
    for file in &plan.files {
        let dir = file
            .path
            .rsplit_once('/')
            .map(|(d, _)| d.to_string())
            .unwrap_or_default();
        if current_dir.as_deref() != Some(&dir) {
            if dir.is_empty() {
                line("  SetOutPath \"$INSTDIR\"".into());
            } else {
                line(format!("  SetOutPath {}", at("$INSTDIR", &dir)?));
            }
            current_dir = Some(dir);
        }
        line(format!(
            "  File {}",
            q(&format!("{PAYLOAD_DIR}\\{}", windows_path(&file.path)))?
        ));
    }
    line("  SetOutPath \"$INSTDIR\"".into());
    line(format!(
        "  FileOpen $2 {} w",
        at("$INSTDIR", INSTALL_MARKER)?
    ));
    line(format!("  FileWrite $2 {}", q(&plan.app_id)?));
    line("  FileClose $2".into());
    line(format!(
        "  WriteUninstaller {}",
        at("$INSTDIR", UNINSTALLER)?
    ));
    line(format!("  CreateShortcut {start_menu} {installed_exe}"));
    let reg = |value: &str, data: String| -> Result<String, String> {
        Ok(format!(
            "  WriteRegStr HKCU {} \"{value}\" {data}",
            q(&key)?
        ))
    };
    line(reg("DisplayName", q(&name)?)?);
    line(reg("DisplayVersion", q(&plan.version)?)?);
    line(reg("Publisher", q("Patchwork export")?)?);
    line(reg("DisplayIcon", installed_exe.clone())?);
    line(reg("InstallLocation", "\"$INSTDIR\"".to_string())?);
    line(reg(
        "UninstallString",
        format!("'\"$INSTDIR\\{UNINSTALLER}\"'"),
    )?);
    line(reg(
        "QuietUninstallString",
        format!("'\"$INSTDIR\\{UNINSTALLER}\" /S'"),
    )?);
    if plan.internal_test {
        line(reg("Comments", q(label)?)?);
    }
    line(format!(
        "  WriteRegDWORD HKCU {} \"EstimatedSize\" {}",
        q(&key)?,
        plan.total_size().div_ceil(1024)
    ));
    line(format!("  WriteRegDWORD HKCU {} \"NoModify\" 1", q(&key)?));
    line(format!("  WriteRegDWORD HKCU {} \"NoRepair\" 1", q(&key)?));
    line("SectionEnd".into());
    line(String::new());
    line("Section /o \"Desktop shortcut\" SecDesktop".into());
    line("  SetShellVarContext current".into());
    line("  SetOutPath \"$INSTDIR\"".into());
    line(format!("  CreateShortcut {desktop} {installed_exe}"));
    line("SectionEnd".into());
    line(String::new());
    // A silent installation (/S) can ask for the desktop shortcut too:
    // /DESKTOPSHORTCUT. Off unless asked for, as on the components page.
    line("Function .onInit".into());
    line("  ${GetParameters} $R0".into());
    line("  ClearErrors".into());
    line("  ${GetOptions} $R0 \"/DESKTOPSHORTCUT\" $R1".into());
    line("  ${IfNot} ${Errors}".into());
    line("    SectionSetFlags ${SecDesktop} ${SF_SELECTED}".into());
    line("  ${EndIf}".into());
    line("FunctionEnd".into());
    line(String::new());

    // Uninstall: only what was installed.
    line("Section \"Uninstall\"".into());
    line("  SetShellVarContext current".into());
    line("  Call un.ReadMarker".into());
    line(format!("  ${{If}} $0 != {}", q(&plan.app_id)?));
    line(format!(
        "    MessageBox MB_ICONSTOP {} /SD IDOK",
        q(&format!(
            "This folder does not hold {app}; nothing was removed."
        ))?
    ));
    line("    Abort".into());
    line("  ${EndIf}".into());
    for file in &plan.files {
        line(format!("  Delete {}", at("$INSTDIR", &file.path)?));
    }
    for dir in folders(plan) {
        line(format!("  RMDir {}", at("$INSTDIR", &dir)?));
    }
    line(format!("  Delete {}", at("$INSTDIR", INSTALL_MARKER)?));
    line(format!("  Delete {}", at("$INSTDIR", UNINSTALLER)?));
    line("  RMDir \"$INSTDIR\"".into());
    line(format!("  Delete {start_menu}"));
    line(format!("  Delete {desktop}"));
    line(format!("  DeleteRegKey HKCU {}", q(&key)?));
    line("SectionEnd".into());
    Ok(s)
}

/// Copy the planned files into `payload` and check each copy against the
/// plan (the export must not change between the check and the build).
pub fn stage_payload(
    plan: &InstallerPlan,
    export_dir: &Path,
    payload: &Path,
) -> Result<(), String> {
    for file in &plan.files {
        let from = export_dir.join(windows_path(&file.path));
        let to = payload.join(windows_path(&file.path));
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::copy(&from, &to).map_err(|e| format!("{}: {e}", file.path))?;
        let bytes = std::fs::read(&to).map_err(|e| format!("{}: {e}", file.path))?;
        if bytes.len() as u64 != file.size || hex(&Sha256::digest(&bytes)) != file.sha256 {
            return Err(format!(
                "{} changed after the export was checked; build the installer again.",
                file.path
            ));
        }
    }
    Ok(())
}

/// The installed files' list (installer source and verification).
pub fn file_list(plan: &InstallerPlan) -> String {
    let mut text = String::new();
    for file in &plan.files {
        text.push_str(&format!(
            "{}  {}  {}\r\n",
            file.sha256, file.size, file.path
        ));
    }
    text
}

fn source_readme(plan: &InstallerPlan, reason: &str) -> String {
    let mut text = format!("{} — installer source\r\n\r\n", plan.display_name());
    if plan.internal_test {
        text.push_str(&format!(
            "{}. Internal development and testing only.\r\n\r\n",
            runtime::INTERNAL_TEST_LABEL
        ));
    }
    text.push_str(&format!(
        "The installer was not compiled: {reason}\r\n\r\n\
         To compile it, install NSIS 3 (https://nsis.sourceforge.io) and run, in this folder:\r\n\
         \r\n    makensis /INPUTCHARSET UTF8 {SCRIPT_FILE}\r\n\r\n\
         It writes {STAGED_SETUP}; rename it to \"{}\". Do not edit {SCRIPT_FILE} or the \
         {PAYLOAD_DIR} folder: {FILE_LIST} lists every file with its SHA-256, and Patchwork \
         generates the script again from the export.\r\n",
        plan.setup_file_name()
    ));
    text
}

/// Check a compiled installer: a Windows program of reasonable size.
pub fn check_setup(path: &Path, plan: &InstallerPlan) -> Result<(u64, String), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("The installer was not written: {e}"))?;
    if super::pe::subsystem(&bytes) != Ok(super::pe::SUBSYSTEM_GUI) {
        return Err("The compiled installer is not a Windows program.".to_string());
    }
    // The payload is compressed, but never to almost nothing.
    if (bytes.len() as u64) < (plan.total_size() / 50).max(64 * 1024) {
        return Err("The compiled installer is too small to hold the app.".to_string());
    }
    Ok((bytes.len() as u64, hex(&Sha256::digest(&bytes))))
}

/// Build the installer (or, without `makensis`, its source) for the export.
pub fn build_installer(request: &InstallerRequest) -> Result<InstallerOutcome, Vec<String>> {
    let plan = plan_installer(request)?;
    let one = |e: String| vec![e];
    let final_setup = request.output_dir.join(plan.setup_file_name());
    let final_source = request
        .output_dir
        .join(format!("{} Setup - installer source", plan.app_name));
    let target = if request.makensis.is_some() {
        &final_setup
    } else {
        &final_source
    };
    if target.exists() && !request.overwrite {
        return Err(one(format!(
            "{} already exists; it is not replaced unless you choose to.",
            target.display()
        )));
    }
    std::fs::create_dir_all(&request.output_dir)
        .map_err(|e| one(format!("{}: {e}", request.output_dir.display())))?;
    let staging = request.output_dir.join(format!(
        ".{} Setup.staging-{}",
        plan.app_name,
        std::process::id()
    ));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| one(e.to_string()))?;
    }
    let result = (|| -> Result<InstallerOutcome, String> {
        std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        stage_payload(&plan, &request.export_dir, &staging.join(PAYLOAD_DIR))?;
        let script = nsis_script(&plan)?;
        // UTF-8 with a byte order mark: how NSIS 3 reads Unicode scripts.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(script.as_bytes());
        std::fs::write(staging.join(SCRIPT_FILE), bytes).map_err(|e| e.to_string())?;
        std::fs::write(staging.join(FILE_LIST), file_list(&plan)).map_err(|e| e.to_string())?;
        match &request.makensis {
            Some(makensis) => {
                let output = std::process::Command::new(makensis)
                    .args(["/V2", "/INPUTCHARSET", "UTF8", SCRIPT_FILE])
                    .current_dir(&staging)
                    .output()
                    .map_err(|e| format!("makensis could not be run: {e}"))?;
                if !output.status.success() {
                    let said = String::from_utf8_lossy(&output.stderr);
                    let out = String::from_utf8_lossy(&output.stdout);
                    return Err(format!(
                        "makensis failed ({}): {}{}",
                        output.status,
                        said.trim(),
                        out.lines().rev().take(5).collect::<Vec<_>>().join(" | ")
                    ));
                }
                let staged = staging.join(STAGED_SETUP);
                let (size, sha256) = check_setup(&staged, &plan)?;
                publish(&staged, &final_setup, request.overwrite)?;
                Ok(InstallerOutcome::Built {
                    installer: final_setup.clone(),
                    size,
                    sha256,
                })
            }
            None => {
                let reason = "makensis (NSIS 3) is not installed on this computer.".to_string();
                std::fs::write(staging.join(SOURCE_README), source_readme(&plan, &reason))
                    .map_err(|e| e.to_string())?;
                publish(&staging, &final_source, request.overwrite)?;
                Ok(InstallerOutcome::SourceOnly {
                    source_dir: final_source.clone(),
                    reason,
                })
            }
        }
    })();
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result.map_err(one)
}

/// Move `from` to `to`, replacing `to` only when allowed.
fn publish(from: &Path, to: &Path, overwrite: bool) -> Result<(), String> {
    if to.exists() {
        if !overwrite {
            return Err(format!("{} appeared during the build.", to.display()));
        }
        if to.is_dir() {
            std::fs::remove_dir_all(to).map_err(|e| e.to_string())?;
        } else {
            std::fs::remove_file(to).map_err(|e| e.to_string())?;
        }
    }
    std::fs::rename(from, to).map_err(|e| format!("{}: {e}", to.display()))
}

/// Check an installation folder against the plan: every planned file is
/// present and unchanged. Besides them only the uninstaller and the marker
/// may be there. Returns the problems found.
pub fn verify_installed(install_dir: &Path, plan: &InstallerPlan) -> Vec<String> {
    let mut problems = Vec::new();
    for file in &plan.files {
        match std::fs::read(install_dir.join(windows_path(&file.path))) {
            Ok(bytes) if hex(&Sha256::digest(&bytes)) == file.sha256 => {}
            Ok(_) => problems.push(format!("{} differs from the export.", file.path)),
            Err(_) => problems.push(format!("{} is missing.", file.path)),
        }
    }
    if let Ok(found) = walk(install_dir) {
        for extra in found {
            let known = plan.files.iter().any(|f| f.path == extra)
                || extra == UNINSTALLER
                || extra == INSTALL_MARKER;
            if !known {
                problems.push(format!("{extra} was not in the export."));
            }
        }
    }
    problems
}
