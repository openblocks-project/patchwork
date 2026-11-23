//! P6 Stage 2.1: the Player manifest (`player.json` beside the executable).
//!
//! A portable export is a folder holding the Patchwork executable, a
//! `player.json` and the project. When the manifest is present the program
//! is that project's Player and nothing else: it ignores its command line
//! (no other project, no catalogue tool, no editor) and, if the manifest is
//! invalid, shows why instead of starting the editor.
//!
//! The manifest only says *what* to play. It carries no authority: its
//! `project_sha256` is compared with the project's content so a change
//! after export is pointed out, but it is not a publisher signature and is
//! never used to allow anything. Whether the project may run is decided by
//! this computer's own trust records (never by files in the export folder)
//! and, the first time, by the user (see `consent`).

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::graph::NodeId;

/// The manifest's file name, next to the executable.
pub const MANIFEST_FILE: &str = "player.json";
/// `schema` of a Player manifest.
pub const MANIFEST_SCHEMA: &str = "patchwork.player";
/// The only `format` this version reads.
pub const MANIFEST_FORMAT: u32 = 1;
/// A manifest is small; anything bigger is not one.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_PROJECT_PATH: usize = 260;
const MAX_TITLE: usize = 200;
const MAX_VERSION: usize = 64;
/// Bounds on the project folder scan for links / junctions.
const MAX_SCAN_ENTRIES: usize = 200_000;
const MAX_SCAN_DEPTH: usize = 32;

/// `player.json`, exactly (unknown fields are refused).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerManifest {
    pub schema: String,
    pub format: u32,
    /// The project folder, relative to the executable's folder.
    pub project: String,
    /// The Video Out (Window / Display) node to show.
    pub output: NodeId,
    /// Window title.
    pub title: String,
    /// The Patchwork version that exported it (informational).
    pub patchwork_version: String,
    /// The project's content digest at export (not a signature).
    pub project_sha256: String,
}

/// What an export adds to a Player launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportInfo {
    /// The export folder (canonical): the executable's folder.
    pub export_dir: PathBuf,
    pub title: String,
    pub patchwork_version: String,
    /// The content digest recorded at export.
    pub expected_digest: String,
}

/// Parse and check the manifest's fields (not the file system).
pub fn parse_manifest(text: &str) -> Result<PlayerManifest, String> {
    // Read the version first so an unsupported format is reported as such,
    // not as an unknown field of this format.
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| format!("{MANIFEST_FILE} is not valid JSON: {e}"))?;
    let schema = value.get("schema").and_then(|s| s.as_str());
    if schema != Some(MANIFEST_SCHEMA) {
        return Err(format!(
            "{MANIFEST_FILE} is not a Patchwork Player manifest (schema must be \
             \"{MANIFEST_SCHEMA}\")."
        ));
    }
    match value.get("format").and_then(|f| f.as_u64()) {
        Some(f) if f == u64::from(MANIFEST_FORMAT) => {}
        Some(f) => {
            return Err(format!(
                "{MANIFEST_FILE} has format {f}; this Patchwork reads format {MANIFEST_FORMAT} only."
            ));
        }
        None => return Err(format!("{MANIFEST_FILE} has no format number.")),
    }
    let manifest: PlayerManifest = serde_json::from_value(value)
        .map_err(|e| format!("{MANIFEST_FILE} is not a valid manifest: {e}"))?;
    check_relative_project_path(&manifest.project)?;
    if manifest.output == 0 {
        return Err(format!("{MANIFEST_FILE}: output must be a node id."));
    }
    check_text("title", &manifest.title, MAX_TITLE)?;
    check_text(
        "patchwork_version",
        &manifest.patchwork_version,
        MAX_VERSION,
    )?;
    if !is_hex64(&manifest.project_sha256) {
        return Err(format!(
            "{MANIFEST_FILE}: project_sha256 must be 64 lowercase hexadecimal characters."
        ));
    }
    Ok(manifest)
}

fn check_text(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{MANIFEST_FILE}: {field} is empty."));
    }
    if value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(format!("{MANIFEST_FILE}: {field} is not acceptable text."));
    }
    Ok(())
}

fn is_hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The project path must stay inside the export folder by its spelling:
/// relative, plain names only (no root, drive, `.` or `..`), and no
/// Windows spellings that alias another name (`:` streams or drive-relative
/// paths, trailing dots or spaces).
pub fn check_relative_project_path(project: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("{MANIFEST_FILE}: project path '{project}' {why}."));
    if project.is_empty() {
        return bad("is empty");
    }
    if project.len() > MAX_PROJECT_PATH || project.contains('\0') {
        return bad("is not acceptable");
    }
    if project.starts_with('/') || project.starts_with('\\') || project.contains(':') {
        return bad("must be relative to the application folder");
    }
    // Raw segments first: `Path::components` silently drops interior `.`
    // and empty segments, which must not be accepted either.
    for segment in project.split(['/', '\\']) {
        match segment {
            "" => return bad("has an empty segment"),
            "." => return bad("must not contain '.'"),
            ".." => return bad("must not leave the application folder"),
            _ => {}
        }
    }
    let mut parts = 0;
    for component in Path::new(project).components() {
        match component {
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                if name.ends_with('.') || name.ends_with(' ') {
                    return bad("has a name ending in a dot or space");
                }
                parts += 1;
            }
            Component::ParentDir => return bad("must not leave the application folder"),
            Component::CurDir => return bad("must not contain '.'"),
            Component::RootDir | Component::Prefix(_) => {
                return bad("must be relative to the application folder");
            }
        }
    }
    if parts == 0 {
        return bad("names no folder");
    }
    Ok(())
}

fn is_link(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink() || crate::library::bundle::is_reparse_point(meta)
}

/// Read and validate `<export_dir>/player.json` and the project it names.
///
/// `trust_dir` is this computer's trust store folder: an export folder that
/// contains it is refused, so trust can never come from the export.
pub fn load_manifest(
    export_dir: &Path,
    trust_dir: Option<&Path>,
) -> Result<ExportedLaunch, String> {
    let export_dir = std::fs::canonicalize(export_dir)
        .map_err(|e| format!("The application folder cannot be read: {e}"))?;
    let manifest_path = export_dir.join(MANIFEST_FILE);
    let meta = std::fs::symlink_metadata(&manifest_path)
        .map_err(|e| format!("{MANIFEST_FILE} cannot be read: {e}"))?;
    if is_link(&meta) || !meta.is_file() {
        return Err(format!(
            "{MANIFEST_FILE} must be a regular file (not a link)."
        ));
    }
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err(format!("{MANIFEST_FILE} is too large."));
    }
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{MANIFEST_FILE} cannot be read: {e}"))?;
    let manifest = parse_manifest(&text)?;

    if let Some(trust_dir) = trust_dir {
        if let Ok(trust) = std::fs::canonicalize(trust_dir) {
            if trust.starts_with(&export_dir) {
                return Err(
                    "The application folder contains this computer's Patchwork data \
                     (trust records); a Player cannot run from there."
                        .to_string(),
                );
            }
        }
    }

    // Walk the project path one name at a time: no link or junction.
    let mut current = export_dir.clone();
    for component in Path::new(&manifest.project).components() {
        current.push(component.as_os_str());
        let meta = std::fs::symlink_metadata(&current)
            .map_err(|_| format!("The project folder '{}' was not found.", manifest.project))?;
        if is_link(&meta) {
            return Err(format!(
                "The project path '{}' goes through a link or junction.",
                manifest.project
            ));
        }
    }
    if !std::fs::metadata(&current).is_ok_and(|m| m.is_dir()) {
        return Err(format!(
            "The project '{}' is not a folder.",
            manifest.project
        ));
    }
    let project_json = current.join(super::PROJECT_FILE);
    let meta = std::fs::symlink_metadata(&project_json).map_err(|_| {
        format!(
            "The project '{}' has no {}.",
            manifest.project,
            super::PROJECT_FILE
        )
    })?;
    if is_link(&meta) || !meta.is_file() {
        return Err(format!(
            "{} must be a regular file (not a link).",
            super::PROJECT_FILE
        ));
    }
    // Belt and braces: the resolved folder is inside the export folder.
    let canonical = std::fs::canonicalize(&current)
        .map_err(|e| format!("The project folder cannot be read: {e}"))?;
    if !canonical.starts_with(&export_dir) || canonical == export_dir {
        return Err("The project folder is outside the application folder.".to_string());
    }
    // Nothing inside the project may lead outside it.
    scan_for_links(&canonical)?;

    Ok(ExportedLaunch {
        launch: super::PlayerLaunch {
            project_dir: canonical.clone(),
            project_json: canonical.join(super::PROJECT_FILE),
            output: Some(manifest.output),
            export: Some(ExportInfo {
                export_dir,
                title: manifest.title.clone(),
                patchwork_version: manifest.patchwork_version.clone(),
                expected_digest: manifest.project_sha256.clone(),
            }),
        },
        manifest,
    })
}

/// Refuse links and junctions anywhere in the project folder (bounded).
fn scan_for_links(root: &Path) -> Result<(), String> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_SCAN_DEPTH {
            return Err("The project folder is nested too deeply.".to_string());
        }
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("The project cannot be read: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("The project cannot be read: {e}"))?;
            seen += 1;
            if seen > MAX_SCAN_ENTRIES {
                return Err("The project folder has too many files.".to_string());
            }
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("The project cannot be read: {e}"))?;
            if is_link(&meta) {
                let shown = path.strip_prefix(root).unwrap_or(&path);
                return Err(format!(
                    "The project contains a link or junction ({}); a Player does not \
                     follow them.",
                    shown.display()
                ));
            }
            if meta.is_dir() {
                stack.push((path, depth + 1));
            }
        }
    }
    Ok(())
}

/// A validated manifest launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedLaunch {
    pub manifest: PlayerManifest,
    pub launch: super::PlayerLaunch,
}

/// Whether anything named `player.json` is beside the executable (a file,
/// folder or link: all of them mean "this is an exported Player").
pub fn manifest_present(export_dir: &Path) -> bool {
    std::fs::symlink_metadata(export_dir.join(MANIFEST_FILE)).is_ok()
}

/// This computer's Patchwork data folder (`~/.patchwork`: trust records,
/// Library, keys), as `TrustStore::default_location` finds it.
pub fn trust_dir() -> Option<PathBuf> {
    dirs::home_dir()
        .filter(|home| !home.as_os_str().is_empty())
        .map(|home| home.join(".patchwork"))
}
