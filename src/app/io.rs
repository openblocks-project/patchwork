use super::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Clip a long error body (HTTP response, backend message, …) so the node
/// tooltip and Console line stay readable. Keeps the first ~160 chars.
fn truncate_for_error(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= 160 {
        trimmed.to_string()
    } else {
        let mut end = 160;
        while !trimmed.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…", &trimmed[..end])
    }
}

/// Convert an absolute asset path to relative (relative to project directory).
fn make_relative(abs_path: &str, project_dir: &str) -> String {
    if abs_path.is_empty() {
        return String::new();
    }
    let abs = Path::new(abs_path);
    let dir = Path::new(project_dir);
    if let Ok(rel) = abs.strip_prefix(dir) {
        rel.display().to_string()
    } else {
        // Not under project dir — keep absolute
        abs_path.to_string()
    }
}

/// Convert a relative asset path to absolute (resolved against project directory).
///
/// **P0-4 hardening:** rejects path-traversal attempts (any `..` component in
/// a relative path) so a malicious project can't reference files outside its
/// own folder (e.g. `path: "../../../etc/passwd"` resolving to `/etc/passwd`).
/// A rejected path returns empty — the node surfaces "file not found"
/// gracefully rather than silently reading a system file.
///
/// Absolute paths inside `project.json` are still accepted (a user may
/// legitimately reference a shared asset at a fixed location) but logged
/// when the path looks suspicious (HOME / system dirs).
fn make_absolute(rel_path: &str, project_dir: &str) -> String {
    if rel_path.is_empty() {
        return String::new();
    }
    let p = Path::new(rel_path);
    if p.is_absolute() {
        // Light scrutiny on absolute paths: log a warning if they target a
        // typically-sensitive location. Don't auto-reject — users may have
        // legitimate references and the asset-relinking dialog (P2-28) is
        // the right UX for full path cleanup.
        if looks_sensitive_abs(rel_path) {
            crate::system_log::warn(format!(
                "Project references absolute path that looks sensitive: {} — \
                 verify this asset is yours",
                rel_path
            ));
        }
        return rel_path.to_string();
    }
    // Lexical traversal check: reject any `..` component before join, so the
    // resolved path can't escape `project_dir`.
    use std::path::Component;
    for comp in p.components() {
        if matches!(comp, Component::ParentDir) {
            crate::system_log::warn(format!(
                "Rejected asset path with `..` traversal in project: {} — \
                 path is suspicious (would escape project folder); ignoring",
                rel_path
            ));
            return String::new();
        }
    }
    let dir = Path::new(project_dir);
    dir.join(p).display().to_string()
}

/// Heuristic check for absolute paths that look like they target system or
/// user-private locations. Used only for a warning log, not for hard rejection.
fn looks_sensitive_abs(p: &str) -> bool {
    const SENSITIVE_PREFIXES: &[&str] = &[
        "/etc/",
        "/private/etc/",
        "/var/",
        "/private/var/",
        "/usr/",
        "/bin/",
        "/sbin/",
        "/proc/",
        "/sys/",
        "/.ssh",
        "/.aws",
        "/.gnupg",
        "C:\\Windows\\",
        "C:\\Program Files",
    ];
    let lower = p.to_ascii_lowercase();
    SENSITIVE_PREFIXES
        .iter()
        .any(|pfx| lower.starts_with(&pfx.to_ascii_lowercase()))
        || lower.contains("/.ssh/")
        || lower.contains("/.aws/")
        || lower.contains("/.gnupg/")
}

/// Convert all asset paths in a graph to relative (for saving).
fn relativize_paths(graph: &mut Graph, project_dir: &str) {
    for node in graph.nodes.values_mut() {
        match &mut node.node_type {
            NodeType::ImageNode {
                path, save_path, ..
            } => {
                *path = make_relative(path, project_dir);
                *save_path = make_relative(save_path, project_dir);
            }
            NodeType::AudioPlayer { file_path, .. } => {
                *file_path = make_relative(file_path, project_dir);
            }
            NodeType::VideoPlayer { path, .. } => {
                *path = make_relative(path, project_dir);
            }
            NodeType::ClapPlugin { plugin_path, .. } => {
                *plugin_path = make_relative(plugin_path, project_dir);
            }
            NodeType::CustomNodeInstance { file, .. } => {
                *file = make_relative(file, project_dir);
            }
            NodeType::MlModel {
                model_path,
                labels_path,
                ..
            } => {
                *model_path = make_relative(model_path, project_dir);
                *labels_path = make_relative(labels_path, project_dir);
            }
            _ => {}
        }
    }
}

/// Outcome of [`save_project_files`].
pub(super) struct PortableSave {
    pub plan: crate::library::project::ProjectPackagePlan,
    pub portability: crate::library::project::PortabilityReport,
}

/// P4.2 Stage 4: write a project portably. The graph is serialized from a
/// copy, so the live graph is untouched unless the save succeeds. On
/// success, packaged nodes in the live graph and in every undo/redo
/// snapshot are pointed at their project copies (the same paths a reopen
/// produces), their runtimes are invalidated, and no undo step is added.
#[allow(clippy::too_many_arguments)]
pub(super) fn save_project_files(
    graph: &mut Graph,
    undo_history: &mut UndoHistory,
    pinned_nodes: Vec<NodeId>,
    canvas_offset: [f32; 2],
    canvas_zoom: f32,
    dir: &Path,
    source_project: Option<&Path>,
    library_root: Option<&Path>,
) -> Result<PortableSave, String> {
    let plan =
        crate::library::project::plan_project_packages(graph, dir, source_project, library_root)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create project folder: {e}"))?;
    let dir_str = dir.to_string_lossy().into_owned();
    let mut saved_graph = graph.clone();
    relativize_paths(&mut saved_graph, &dir_str);
    apply_project_package_paths(&mut saved_graph, &plan);
    let pf = ProjectFile {
        graph: saved_graph,
        pinned_nodes,
        canvas_offset,
        canvas_zoom,
    };
    let json =
        serde_json::to_string_pretty(&pf).map_err(|e| format!("Serialization failed: {e}"))?;
    if json.len() < 10 {
        return Err("Save produced empty JSON — skipping write".to_string());
    }
    let report = crate::library::project::commit_project_save(dir, &plan, &json)?;

    // Saved: synchronize the live state with what project.json now says.
    let files = live_package_files(graph, &plan, &dir_str);
    sync_live_package_paths(graph, &files);
    undo_history.map_graphs(|snapshot| {
        sync_live_package_paths(snapshot, &files);
    });
    let portability = crate::library::project::PortabilityReport::new(
        &plan,
        &report,
        count_external_assets(graph, dir),
    );
    Ok(PortableSave { plan, portability })
}

/// Absolute project-copy path per package reference, exactly as a reopen
/// of the saved project produces it.
fn live_package_files(
    graph: &Graph,
    plan: &crate::library::project::ProjectPackagePlan,
    dir: &str,
) -> HashMap<crate::library::resolve::InstancePackageRef, String> {
    let mut files = HashMap::new();
    for (id, relative) in &plan.instance_files {
        if let Some(NodeType::CustomNodeInstance {
            package: Some(package),
            ..
        }) = graph.nodes.get(id).map(|n| &n.node_type)
        {
            files.insert(package.clone(), make_absolute(relative, dir));
        }
    }
    files
}

/// Point packaged instances at their project copies (by package
/// reference). The reference is kept; path-only instances are untouched.
/// Changed instances drop their cached runtime and resolution, so they
/// reload from the new location. Returns how many changed.
fn sync_live_package_paths(
    graph: &mut Graph,
    files: &HashMap<crate::library::resolve::InstancePackageRef, String>,
) -> usize {
    let targets: Vec<(NodeId, String)> = graph
        .nodes
        .iter()
        .filter_map(|(&id, node)| match &node.node_type {
            NodeType::CustomNodeInstance {
                package: Some(package),
                ..
            } => files.get(package).map(|file| (id, file.clone())),
            _ => None,
        })
        .collect();
    targets
        .into_iter()
        .filter(|(id, file)| graph.retarget_custom_instance(*id, file))
        .count()
}

/// Tracked asset files (media, models, plugins; the same fields saved as
/// relative paths) that lie outside `dir`.
fn count_external_assets(graph: &Graph, dir: &Path) -> usize {
    let mut paths: Vec<&str> = Vec::new();
    for node in graph.nodes.values() {
        match &node.node_type {
            NodeType::ImageNode { path, .. } => paths.push(path),
            NodeType::AudioPlayer { file_path, .. } => paths.push(file_path),
            NodeType::VideoPlayer { path, .. } => paths.push(path),
            NodeType::ClapPlugin { plugin_path, .. } => paths.push(plugin_path),
            NodeType::MlModel {
                model_path,
                labels_path,
                ..
            } => {
                paths.push(model_path);
                paths.push(labels_path);
            }
            _ => {}
        }
    }
    let mut outside: Vec<&str> = paths
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .filter(|p| {
            let path = Path::new(p);
            path.is_absolute() && !crate::library::project::is_within(path, dir)
        })
        .collect();
    outside.sort_unstable();
    outside.dedup();
    outside.len()
}

/// P4.2 Stage 4: point packaged instances at their bundled project copy
/// (relative `packages/<id>/<version>/<entry>`). The package reference
/// stays authoritative; path-only instances are left as they are.
fn apply_project_package_paths(
    graph: &mut Graph,
    plan: &crate::library::project::ProjectPackagePlan,
) {
    for (id, relative) in &plan.instance_files {
        if let Some(node) = graph.nodes.get_mut(id) {
            if let NodeType::CustomNodeInstance {
                file,
                package: Some(_),
                ..
            } = &mut node.node_type
            {
                *file = relative.clone();
            }
        }
    }
}

/// Convert all asset paths in a graph to absolute (after loading).
fn absolutize_paths(graph: &mut Graph, project_dir: &str) {
    for node in graph.nodes.values_mut() {
        match &mut node.node_type {
            NodeType::ImageNode {
                path, save_path, ..
            } => {
                *path = make_absolute(path, project_dir);
                *save_path = make_absolute(save_path, project_dir);
            }
            NodeType::AudioPlayer { file_path, .. } => {
                *file_path = make_absolute(file_path, project_dir);
            }
            NodeType::VideoPlayer { path, .. } => {
                *path = make_absolute(path, project_dir);
            }
            NodeType::ClapPlugin { plugin_path, .. } => {
                *plugin_path = make_absolute(plugin_path, project_dir);
            }
            NodeType::CustomNodeInstance { file, .. } => {
                *file = make_absolute(file, project_dir);
            }
            NodeType::MlModel {
                model_path,
                labels_path,
                ..
            } => {
                *model_path = make_absolute(model_path, project_dir);
                *labels_path = make_absolute(labels_path, project_dir);
            }
            _ => {}
        }
    }
}

/// Project file format — includes graph, pinned nodes, and viewport state.
/// Backward-compatible: old project.json files (raw Graph) are detected and loaded.
#[derive(Serialize, Deserialize)]
struct ProjectFile {
    graph: Graph,
    #[serde(default)]
    pinned_nodes: Vec<NodeId>,
    #[serde(default)]
    canvas_offset: [f32; 2],
    #[serde(default = "default_one_f32")]
    canvas_zoom: f32,
}

fn default_one_f32() -> f32 {
    1.0
}

/// Minimal session state that gets auto-saved on close and restored on launch.
#[derive(Serialize, Deserialize)]
struct SessionState {
    graph: Graph,
    canvas_offset: [f32; 2],
    canvas_zoom: f32,
    pinned_nodes: Vec<NodeId>,
    #[serde(default)]
    project_path: Option<String>,
    #[serde(default)]
    api_keys: HashMap<String, String>,
    /// Persisted across launches so the user gets the same `*` indicator
    /// on relaunch as they had on quit (e.g. quit-with-Don't-Save).
    #[serde(default)]
    is_dirty: bool,
    #[serde(default)]
    recent_projects: Vec<String>,
    /// Session-fix: no project was open at shutdown (only custom-node
    /// tabs). The next launch starts with a clean Untitled project; only
    /// recent projects and keys are carried over. Absent in older files.
    #[serde(default)]
    clean_start: bool,
    /// B1: the user discarded unsaved changes at shutdown. `graph` is empty
    /// and must not be used; the next launch reopens `project_path` from
    /// its saved `project.json`. Absent (false) in older files.
    #[serde(default)]
    reopen_from_disk: bool,
}

/// Project state captured for the legacy single-document session.
#[derive(Clone)]
pub(super) struct SessionSnapshot {
    pub graph: Graph,
    pub canvas_offset: [f32; 2],
    pub canvas_zoom: f32,
    pub pinned_nodes: Vec<NodeId>,
    pub project_path: Option<String>,
    pub is_dirty: bool,
}

/// Open projects in session priority order: the active tab if it is a
/// project (its live state), then the other project tabs, most recently
/// opened (highest id) first. Custom-node tabs are never included.
fn session_project_candidates(
    documents: &crate::document::DocumentManager,
    live: SessionSnapshot,
) -> Vec<SessionSnapshot> {
    use crate::document::DocumentKind;
    let active_id = documents.active().id;
    let mut candidates = Vec::new();
    if documents.active().kind == DocumentKind::Project {
        candidates.push(live);
    }
    let mut others: Vec<_> = documents
        .documents
        .iter()
        .filter(|doc| doc.kind == DocumentKind::Project && doc.id != active_id)
        .collect();
    others.sort_by_key(|doc| std::cmp::Reverse(doc.id));
    candidates.extend(others.into_iter().map(|document| {
        SessionSnapshot {
            graph: document.graph.clone(),
            canvas_offset: [document.canvas_offset.x, document.canvas_offset.y],
            canvas_zoom: document.canvas_zoom,
            pinned_nodes: document.pinned_nodes.iter().copied().collect(),
            project_path: document
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            is_dirty: document.is_dirty,
        }
    }));
    candidates
}

/// Which project the session should capture at shutdown. The legacy
/// session holds exactly one *project*: the active tab if it is a project,
/// otherwise the most recently opened project tab (highest id). Custom-node
/// tabs are never captured. `None` means no project is open.
pub(super) fn select_session_project(
    documents: &crate::document::DocumentManager,
    live: SessionSnapshot,
) -> Option<SessionSnapshot> {
    session_project_candidates(documents, live)
        .into_iter()
        .next()
}

/// True when `dir` holds a saved project (`project.json`).
fn is_saved_project_dir(dir: &str) -> bool {
    let dir = dir.trim();
    !dir.is_empty()
        && crate::document::Document::path_fits_kind(
            crate::document::DocumentKind::Project,
            Path::new(dir),
        )
        && Path::new(dir).join("project.json").is_file()
}

/// B1: session written after "Discard All & Quit". No graph state is
/// stored; the first open project (same priority as a normal session) that
/// has a saved `project.json` is reopened from disk next time. Without one,
/// the next launch is clean. The same rule applies whichever tab is active.
fn discard_session_state(
    documents: &crate::document::DocumentManager,
    live: SessionSnapshot,
    api_keys: HashMap<String, String>,
    recent_projects: Vec<String>,
) -> SessionState {
    let saved = session_project_candidates(documents, live)
        .into_iter()
        .find(|c| c.project_path.as_deref().is_some_and(is_saved_project_dir));
    match saved {
        Some(project) => SessionState {
            graph: Graph::new(),
            canvas_offset: project.canvas_offset,
            canvas_zoom: project.canvas_zoom,
            pinned_nodes: Vec::new(),
            project_path: project.project_path,
            api_keys,
            is_dirty: false,
            recent_projects,
            clean_start: false,
            reopen_from_disk: true,
        },
        None => session_state_from(None, api_keys, recent_projects),
    }
}

/// How a session file should be restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionRestorePlan {
    /// Start a clean Untitled project (keep recent projects and keys).
    Clean { reason: Option<String> },
    /// B1: reopen this saved project from its `project.json`; the session
    /// graph is discarded data and is never used.
    ReopenFromDisk { project_path: String },
    /// Restore the saved graph as a project with this location.
    Project {
        project_path: Option<String>,
        /// Force the unsaved indicator (the saved location is unusable,
        /// so the restored graph exists only in memory).
        mark_dirty: bool,
        note: Option<String>,
    },
}

/// Decide how to restore a session. Only project documents are restored:
/// a custom-node (`.pwnode`) location is never restored as a project.
/// (Tests use this two-flag form; `restore_session` uses `_with`.)
#[cfg(test)]
pub(super) fn plan_session_restore(
    project_path: Option<&str>,
    clean_start: bool,
) -> SessionRestorePlan {
    plan_session_restore_with(project_path, clean_start, false)
}

/// [`plan_session_restore`] including the B1 `reopen_from_disk` flag.
pub(super) fn plan_session_restore_with(
    project_path: Option<&str>,
    clean_start: bool,
    reopen_from_disk: bool,
) -> SessionRestorePlan {
    if clean_start {
        return SessionRestorePlan::Clean { reason: None };
    }
    if reopen_from_disk {
        // Never fall back to the (discarded) session graph.
        return match project_path
            .map(str::trim)
            .filter(|p| is_saved_project_dir(p))
        {
            Some(path) => SessionRestorePlan::ReopenFromDisk {
                project_path: path.to_string(),
            },
            None => SessionRestorePlan::Clean {
                reason: Some(format!(
                    "The last project ({}) could not be reopened from disk; \
                     starting with a new project.",
                    project_path.unwrap_or("none")
                )),
            },
        };
    }
    let Some(path) = project_path.map(str::trim).filter(|p| !p.is_empty()) else {
        return SessionRestorePlan::Project {
            project_path: None,
            mark_dirty: false,
            note: None,
        };
    };
    let location = std::path::Path::new(path);
    if !crate::document::Document::path_fits_kind(crate::document::DocumentKind::Project, location)
    {
        return SessionRestorePlan::Clean {
            reason: Some(format!(
                "The last session was a custom node ({path}), not a project; \
                 starting with a new project. The custom node file was not changed."
            )),
        };
    }
    if location.is_dir() {
        return SessionRestorePlan::Project {
            project_path: Some(path.to_string()),
            mark_dirty: false,
            note: None,
        };
    }
    SessionRestorePlan::Project {
        project_path: None,
        mark_dirty: true,
        note: Some(format!(
            "The last session's project folder is unavailable ({path}); \
             its contents were restored as an unsaved Untitled project."
        )),
    }
}

fn session_state_from(
    project: Option<SessionSnapshot>,
    api_keys: HashMap<String, String>,
    recent_projects: Vec<String>,
) -> SessionState {
    match project {
        Some(project) => SessionState {
            graph: project.graph,
            canvas_offset: project.canvas_offset,
            canvas_zoom: project.canvas_zoom,
            pinned_nodes: project.pinned_nodes,
            project_path: project.project_path,
            api_keys,
            is_dirty: project.is_dirty,
            recent_projects,
            clean_start: false,
            reopen_from_disk: false,
        },
        None => SessionState {
            graph: Graph::new(),
            canvas_offset: [0.0, 0.0],
            canvas_zoom: 1.0,
            pinned_nodes: Vec::new(),
            project_path: None,
            api_keys,
            is_dirty: false,
            recent_projects,
            clean_start: true,
            reopen_from_disk: false,
        },
    }
}

/// One unsaved document shown in the quit dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UnsavedDocument {
    pub id: u64,
    pub title: String,
    pub kind: crate::document::DocumentKind,
    /// Saved by the next "Save & Continue".
    pub next: bool,
}

impl UnsavedDocument {
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            crate::document::DocumentKind::Project => "Project",
            crate::document::DocumentKind::CustomNode => "Custom Node",
        }
    }
}

/// Every unsaved document, in tab order. The active document's state lives
/// in the editor (`active_dirty`). "Next" matches Save & Continue: the
/// active document if it has changes, otherwise the first unsaved tab.
pub(super) fn unsaved_documents(
    documents: &crate::document::DocumentManager,
    active_dirty: bool,
) -> Vec<UnsavedDocument> {
    let active_id = documents.active().id;
    let mut list: Vec<UnsavedDocument> = documents
        .documents
        .iter()
        .filter(|doc| {
            if doc.id == active_id {
                active_dirty
            } else {
                doc.is_dirty
            }
        })
        .map(|doc| UnsavedDocument {
            id: doc.id,
            title: doc.title.clone(),
            kind: doc.kind,
            next: false,
        })
        .collect();
    let next = list
        .iter()
        .position(|d| d.id == active_id)
        .or(if list.is_empty() { None } else { Some(0) });
    if let Some(index) = next {
        list[index].next = true;
    }
    list
}

/// A button in the quit dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuitChoice {
    SaveAndContinue,
    DiscardAll,
    Cancel,
}

/// What the quit dialog does after a choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuitStep {
    /// A save failed or was cancelled: stay in the dialog, nothing closes.
    KeepOpen,
    /// Move to the next unsaved document and keep asking.
    SwitchTo(u64),
    /// Close the app (`discard`: the user chose Discard All).
    Close { discard: bool },
    /// Cancel: close the dialog, keep every document and its changes.
    Dismiss,
}

/// Pure quit-dialog decision. `saved` is the result of saving the active
/// document (`None` when it had nothing to save); `next_dirty` is the next
/// unsaved document after that save.
pub(super) fn quit_step(
    choice: QuitChoice,
    saved: Option<bool>,
    next_dirty: Option<u64>,
) -> QuitStep {
    match choice {
        QuitChoice::Cancel => QuitStep::Dismiss,
        QuitChoice::DiscardAll => QuitStep::Close { discard: true },
        QuitChoice::SaveAndContinue => {
            if saved == Some(false) {
                QuitStep::KeepOpen
            } else {
                match next_dirty {
                    Some(id) => QuitStep::SwitchTo(id),
                    None => QuitStep::Close { discard: false },
                }
            }
        }
    }
}

/// Read a saved project's `project.json` (used to reopen it from disk).
fn read_project_file(dir: &Path) -> Result<ProjectFile, String> {
    let path = dir.join("project.json");
    let json = std::fs::read_to_string(&path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&json).map_err(|e| format!("{} is not valid: {e}", path.display()))
}

fn session_path() -> std::path::PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".patchwork");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("last_session.json")
}

/// Per-user API-keys store at `~/.patchwork/keys.json`. This is the canonical
/// location — **never** writes inside a project folder, so keys don't ride
/// along when a user zips/shares/posts their project. (`.env`-style: belongs
/// to the user, not the artifact.)
fn user_keys_path() -> std::path::PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".patchwork");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("keys.json")
}

/// Persist the api-keys map to `~/.patchwork/keys.json`. Called on every save
/// (project or session) so the canonical store stays in sync with in-memory
/// state. Best-effort: failures are logged but don't block the calling save.
pub(super) fn save_user_keys(keys: &HashMap<String, String>) {
    let path = user_keys_path();
    match serde_json::to_string_pretty(keys) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                crate::system_log::warn(format!("Failed to write {}: {}", path.display(), e));
            }
            // Set mode 0600 on Unix so other users on a shared machine can't
            // read the key file. Same hardening pattern as `ai_configs/`.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
        Err(e) => {
            crate::system_log::warn(format!("Failed to serialize api_keys: {}", e));
        }
    }
}

/// Load api-keys from `~/.patchwork/keys.json`. Returns empty if missing or
/// unreadable (silent — first-run users haven't created it yet).
pub(super) fn load_user_keys() -> HashMap<String, String> {
    let path = user_keys_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

impl super::PatchworkApp {
    /// Save current session state to ~/.patchwork/last_session.json
    pub fn save_session(&self) {
        // P0-2: also persist api keys to the dedicated per-user store. They're
        // still serialized into the session blob for now (backwards compat with
        // older Patchwork builds that don't read keys.json), but the user store
        // is the canonical source going forward.
        save_user_keys(&self.api_keys);
        let state = self.session_state();
        if let Ok(json) = serde_json::to_string_pretty(&state) {
            let _ = std::fs::write(session_path(), json);
        }
    }

    /// Session contents: the project to restore next time (never a
    /// custom-node tab), or a clean-start marker if no project is open.
    /// Writes no project or custom-node file.
    fn session_state(&self) -> SessionState {
        let live = SessionSnapshot {
            graph: self.graph.clone(),
            canvas_offset: [self.canvas_offset.x, self.canvas_offset.y],
            canvas_zoom: self.canvas_zoom,
            pinned_nodes: self.pinned_nodes.iter().copied().collect(),
            project_path: self.project_path.clone(),
            is_dirty: self.is_dirty,
        };
        if self.discard_on_exit {
            // B1: never persist discarded graph state.
            return discard_session_state(
                &self.documents,
                live,
                self.api_keys.clone(),
                self.recent_projects.clone(),
            );
        }
        session_state_from(
            select_session_project(&self.documents, live),
            self.api_keys.clone(),
            self.recent_projects.clone(),
        )
    }

    /// Try to restore session from ~/.patchwork/last_session.json.
    /// Returns true if session was successfully restored.
    pub fn restore_session(&mut self) -> bool {
        let path = session_path();
        let json = match std::fs::read_to_string(&path) {
            Ok(j) => j,
            Err(_) => return false,
        };
        let state: SessionState = match serde_json::from_str(&json) {
            Ok(s) => s,
            Err(e) => {
                crate::system_log::warn(format!("Failed to restore session: {}", e));
                return false;
            }
        };
        // Recent projects and keys are kept whatever happens to the graph.
        let user_keys = load_user_keys();
        let api_keys = if !user_keys.is_empty() {
            user_keys
        } else {
            state.api_keys.clone()
        };
        // What to load: the session graph, or (B1) the saved project from
        // disk. Discarded session graph data is never used.
        struct Loaded {
            graph: Graph,
            canvas_offset: [f32; 2],
            canvas_zoom: f32,
            pinned_nodes: Vec<NodeId>,
            project_path: Option<String>,
            is_dirty: bool,
        }
        let plan = plan_session_restore_with(
            state.project_path.as_deref(),
            state.clean_start,
            state.reopen_from_disk,
        );
        let loaded = match plan {
            SessionRestorePlan::Clean { reason } => {
                if let Some(reason) = reason {
                    crate::system_log::warn(reason);
                }
                self.api_keys = api_keys;
                self.recent_projects = state.recent_projects;
                // Caller spawns a clean Untitled project.
                return false;
            }
            SessionRestorePlan::ReopenFromDisk { project_path } => {
                match read_project_file(Path::new(&project_path)) {
                    Ok(pf) => {
                        let mut graph = pf.graph;
                        graph.fix_next_id();
                        absolutize_paths(&mut graph, &project_path);
                        Loaded {
                            graph,
                            canvas_offset: pf.canvas_offset,
                            canvas_zoom: pf.canvas_zoom,
                            pinned_nodes: pf.pinned_nodes,
                            project_path: Some(project_path),
                            is_dirty: false,
                        }
                    }
                    Err(error) => {
                        crate::system_log::warn(format!(
                            "Could not reopen the last project: {error}. \
                             Starting with a new project."
                        ));
                        self.api_keys = api_keys;
                        self.recent_projects = state.recent_projects;
                        return false;
                    }
                }
            }
            SessionRestorePlan::Project {
                project_path,
                mark_dirty,
                note,
            } => {
                if let Some(note) = note {
                    crate::system_log::warn(note);
                }
                let mut graph = state.graph;
                graph.fix_next_id();
                Loaded {
                    graph,
                    canvas_offset: state.canvas_offset,
                    canvas_zoom: state.canvas_zoom,
                    pinned_nodes: state.pinned_nodes,
                    project_path,
                    is_dirty: state.is_dirty || mark_dirty,
                }
            }
        };
        let mut graph = loaded.graph;
        // Always start with DSP, Camera, Mic off for safety
        for node in graph.nodes.values_mut() {
            match &mut node.node_type {
                NodeType::AudioDevice { enabled, .. } => {
                    *enabled = false;
                }
                NodeType::Camera { active, .. } => {
                    *active = false;
                }
                NodeType::AudioInput { active, .. } => {
                    *active = false;
                }
                NodeType::VideoPlayer { playing, .. } => {
                    *playing = false;
                }
                _ => {}
            }
        }
        self.graph = graph;
        self.graph.audio_topology_dirty = true;
        self.canvas_offset = egui::Vec2::new(loaded.canvas_offset[0], loaded.canvas_offset[1]);
        self.canvas_zoom = loaded.canvas_zoom;
        self.pinned_nodes = loaded.pinned_nodes.into_iter().collect();
        self.project_path = loaded.project_path;
        // P0-2: user-store keys are canonical; if present, they override any
        // keys carried in last_session.json (older builds wrote both; new
        // builds read keys from `~/.patchwork/keys.json` first).
        self.api_keys = api_keys;
        self.is_dirty = loaded.is_dirty;
        self.recent_projects = state.recent_projects;
        self.port_positions.clear();
        self.node_rects.clear();
        self.undo_history.clear();
        // See `load_project` — same node-id-collision concern after
        // a session restore. Cleared on the next frame in update().
        self.caches_dirty = true;
        true
    }

    /// Wipe the GPU texture cache and all egui per-node temp data.
    /// Called from `update()` when `caches_dirty` is set (load,
    /// restore, undo, redo, delete). The new project's nodes get
    /// node ids starting from 1 (`fix_next_id`), and several caches
    /// are keyed by `(node_id, port)` or `Id::new(("xxx", node_id))`,
    /// so without an explicit wipe a fresh node could pick up the
    /// previous project's image / GPU handle / pending HTTP fetch.
    ///
    /// We use the heavy hammer (`d.clear()`) instead of removing each
    /// well-known key by name because every cache stores a different
    /// concrete type, and `egui::IdTypeMap::remove::<T>` is type-erased
    /// — there's no way to enumerate without knowing every type. The
    /// next frame just rebuilds whatever it needs.
    pub(super) fn clear_caches_if_dirty(&mut self, ctx: &egui::Context) {
        if !self.caches_dirty {
            return;
        }
        self.caches_dirty = false;
        self.gpu_tex_cache
            .clear_all(self.wgpu_render_state.as_ref());
        ctx.data_mut(|d| d.clear());
        // Theme settings rely on `theme_accent` in temp data; re-emit
        // it immediately so the very next render doesn't flash a
        // default-color frame before `apply_theme` runs again.
        self.apply_theme(ctx);
    }
    pub(super) fn handle_file_drop(&mut self, ctx: &egui::Context) {
        // Capture pointer position BEFORE processing drops (macOS clears it during drop)
        let drop_pos = ctx
            .input(|i| {
                // Try hover_pos first (most reliable during drag-over), then pointer
                i.pointer.hover_pos().or_else(|| i.pointer.latest_pos())
            })
            .unwrap_or(egui::pos2(300.0, 300.0));

        let dropped: Vec<_> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        // Stage 0: dropping files never changes a read-only tab.
        if !dropped.is_empty() && !self.require_editable("Dropping files") {
            return;
        }
        if !dropped.is_empty() {
            self.push_undo();
        }
        let image_exts = ["png", "jpg", "jpeg", "gif", "bmp", "webp"];
        let video_exts = ["mp4", "mov", "avi", "webm", "mkv"];
        let audio_exts = ["mp3", "wav", "ogg", "flac", "aac", "m4a"];
        let mesh_exts = ["gltf", "glb"];
        let off_e = self.canvas_offset / self.canvas_zoom;
        let mut new_ids: std::collections::HashSet<crate::graph::NodeId> =
            std::collections::HashSet::new();
        for (idx, path) in dropped.iter().enumerate() {
            // Stack multiple dropped files vertically from the drop point
            let canvas_x = drop_pos.x - off_e.x;
            let canvas_y = drop_pos.y - off_e.y + (idx as f32 * 40.0);

            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            let nid = if audio_exts.contains(&ext.as_str()) {
                self.graph.add_node(
                    NodeType::AudioPlayer {
                        file_path: path.display().to_string(),
                        volume: 1.0,
                        looping: false,
                        duration_secs: 0.0,
                    },
                    [canvas_x, canvas_y],
                )
            } else if image_exts.contains(&ext.as_str()) {
                let image_data =
                    crate::nodes::image_node::load_image_from_path(&path.display().to_string());
                self.graph.add_node(
                    NodeType::ImageNode {
                        path: path.display().to_string(),
                        save_path: String::new(),
                        image_data,
                        preview_size: 150.0,
                        last_save_hash: 0,
                        cached_file: String::new(),
                    },
                    [canvas_x, canvas_y],
                )
            } else if video_exts.contains(&ext.as_str()) {
                self.graph.add_node(
                    NodeType::VideoPlayer {
                        path: path.display().to_string(),
                        playing: false,
                        looping: false,
                        res_w: 640,
                        res_h: 480,
                        current_frame: None,
                        duration: 0.0,
                        speed: 1.0,
                        status: "Loaded".into(),
                    },
                    [canvas_x, canvas_y],
                )
            } else if mesh_exts.contains(&ext.as_str()) {
                // If the drop landed on an existing 3D Shape node, update its
                // custom path in place instead of spawning a fresh node — the
                // user wanted to swap the asset, not duplicate it. Falls
                // through to spawn-new on any miss.
                let target_screen = drop_pos + egui::vec2(0.0, idx as f32 * 40.0);
                let hit_id: Option<NodeId> = self
                    .node_rects
                    .iter()
                    .find(|(_, r)| r.contains(target_screen))
                    .map(|(&id, _)| id);
                let mut updated_existing = false;
                if let Some(id) = hit_id {
                    if let Some(node) = self.graph.nodes.get_mut(&id) {
                        if let NodeType::Dynamic { inner } = &mut node.node_type {
                            let any_mut = &mut *inner.node as &mut dyn std::any::Any;
                            if let Some(s) =
                                any_mut.downcast_mut::<crate::nodes::shape_3d::Shape3DNode>()
                            {
                                s.kind = crate::nodes::shape_3d::ShapeKind::Custom;
                                s.custom_path = path.display().to_string();
                                s.cached_custom = None;
                                s.custom_error = None;
                                updated_existing = true;
                            }
                        }
                    }
                }
                if updated_existing {
                    hit_id.unwrap()
                } else {
                    let node = crate::nodes::shape_3d::Shape3DNode::from_gltf_path(
                        path.display().to_string(),
                    );
                    self.graph.add_node(
                        NodeType::Dynamic {
                            inner: crate::graph::DynNode {
                                node: Box::new(node),
                            },
                        },
                        [canvas_x, canvas_y],
                    )
                }
            } else {
                let mut file_node = crate::nodes::file_node::FileNode::default();
                file_node.path = path.display().to_string();
                file_node.load_file();
                self.graph.add_node(
                    NodeType::Dynamic {
                        inner: crate::graph::DynNode {
                            node: Box::new(file_node),
                        },
                    },
                    [canvas_x, canvas_y],
                )
            };
            new_ids.insert(nid);
        }
        // Select all dropped nodes as a group (mirrors paste/duplicate flow).
        if !new_ids.is_empty() {
            self.selected_nodes = new_ids;
        }
    }

    pub(super) fn poll_midi_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        // Reconcile MidiIn subscriptions with port_name: connect when set+unconnected,
        // disconnect when cleared. Mirrors the OscIn pattern in poll_osc_inputs so
        // MCP-driven `update_node({port_name: ...})` takes effect on the next frame.
        let mut midi_actions: Vec<crate::midi::MidiAction> = Vec::new();
        for &nid in &node_ids {
            if let Some(node) = self.graph.nodes.get(&nid) {
                if let NodeType::MidiIn { port_name, .. } = &node.node_type {
                    let connected = self.midi.is_input_connected(nid);
                    if !port_name.is_empty() && !connected {
                        midi_actions.push(crate::midi::MidiAction::ConnectInput {
                            node_id: nid,
                            port_name: port_name.clone(),
                        });
                    } else if port_name.is_empty() && connected {
                        midi_actions
                            .push(crate::midi::MidiAction::DisconnectInput { node_id: nid });
                    }
                }
            }
        }
        if !midi_actions.is_empty() {
            self.midi.process(midi_actions);
        }

        for nid in node_ids {
            if let Some(msg) = self.midi.poll_input(nid) {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    if let NodeType::MidiIn {
                        channel,
                        note,
                        velocity,
                        log,
                        ..
                    } = &mut node.node_type
                    {
                        if msg.len() >= 3 {
                            *channel = msg[0] & 0x0F;
                            let status = msg[0] & 0xF0;
                            match status {
                                0x80 | 0x90 | 0xA0 | 0xB0 => {
                                    *note = msg[1];
                                    *velocity = msg[2];
                                }
                                _ => {}
                            }
                        }
                        log.push(nodes::midi_in::format_midi_message(&msg));
                    }
                }
            }
        }
    }

    pub(super) fn poll_serial_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        // Reconcile Serial subscriptions with port_name+baud_rate (same shape as MIDI).
        let mut serial_actions: Vec<crate::serial::SerialAction> = Vec::new();
        for &nid in &node_ids {
            if let Some(node) = self.graph.nodes.get(&nid) {
                if let NodeType::Serial {
                    port_name,
                    baud_rate,
                    ..
                } = &node.node_type
                {
                    let connected = self.serial.is_connected(nid);
                    if !port_name.is_empty() && !connected {
                        serial_actions.push(crate::serial::SerialAction::Connect {
                            node_id: nid,
                            port_name: port_name.clone(),
                            baud_rate: *baud_rate,
                        });
                    } else if port_name.is_empty() && connected {
                        serial_actions
                            .push(crate::serial::SerialAction::Disconnect { node_id: nid });
                    }
                }
            }
        }
        if !serial_actions.is_empty() {
            self.serial.process(serial_actions);
        }

        for nid in node_ids {
            let lines = self.serial.poll(nid);
            if !lines.is_empty() {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    if let NodeType::Serial { log, last_line, .. } = &mut node.node_type {
                        for line in lines {
                            *last_line = line.clone();
                            log.push(line);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn poll_osc_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        // Auto-start/stop listeners for MCP-triggered OscIn nodes
        for &nid in &node_ids {
            if let Some(node) = self.graph.nodes.get(&nid) {
                if let NodeType::OscIn {
                    listening, port, ..
                } = &node.node_type
                {
                    if *listening && !self.osc.is_listening(nid) && *port > 0 {
                        self.osc
                            .process(vec![crate::osc::OscAction::StartListening {
                                node_id: nid,
                                port: *port,
                            }]);
                    } else if !*listening && self.osc.is_listening(nid) {
                        self.osc
                            .process(vec![crate::osc::OscAction::StopListening { node_id: nid }]);
                    }
                }
            }
        }
        for nid in node_ids {
            let messages = self.osc.poll(nid);
            if !messages.is_empty() {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    if let NodeType::OscIn {
                        address_filter,
                        arg_count,
                        last_args,
                        last_args_text,
                        log,
                        discovered,
                        ..
                    } = &mut node.node_type
                    {
                        for msg in messages {
                            // Auto-discover: track unique addresses with their arg counts
                            let preview = msg.args_text.join(", ");
                            if let Some(entry) =
                                discovered.iter_mut().find(|(a, _, _)| *a == msg.address)
                            {
                                entry.1 = msg.args_float.len();
                                entry.2 = preview.clone();
                            } else {
                                discovered.push((
                                    msg.address.clone(),
                                    msg.args_float.len(),
                                    preview.clone(),
                                ));
                            }

                            // Log ALL messages (before filtering)
                            log.push(format!("{} [{}]", msg.address, msg.args_text.join(", ")));
                            if log.len() > 200 {
                                log.remove(0);
                            }

                            // Address filter: skip if doesn't match
                            if !address_filter.is_empty()
                                && !msg.address.contains(address_filter.as_str())
                            {
                                continue;
                            }

                            // Update last_args (float) and last_args_text
                            for (i, &val) in msg.args_float.iter().enumerate() {
                                if i < *arg_count {
                                    while last_args.len() <= i {
                                        last_args.push(0.0);
                                    }
                                    last_args[i] = val;
                                }
                            }
                            *last_args_text = msg.args_text.clone();
                        }
                    }
                }
            }
        }
    }

    /// Drain the manager's per-DmxIn-node receiver and stash the latest
    /// universe slice on the node so `evaluate` can surface it as port
    /// outputs. Mirrors `poll_osc_inputs` shape.
    pub(super) fn poll_dmx_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            let frames = self.dmx.poll_input(nid);
            if frames.is_empty() {
                continue;
            }
            // Use the most recent frame — DMX is sampled, old frames stale.
            if let Some(latest) = frames.into_iter().last() {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    if let NodeType::DmxIn {
                        last_values,
                        channel_count,
                        start_at,
                        ..
                    } = &mut node.node_type
                    {
                        let start_idx = (*start_at as usize).saturating_sub(1).min(511);
                        last_values.resize(*channel_count, 0);
                        for i in 0..*channel_count {
                            let ch = start_idx + i;
                            last_values[i] = latest.data.get(ch).copied().unwrap_or(0);
                        }
                    }
                }
            }
        }
    }

    /// Same as poll_dmx_inputs, but for Art-Net listener threads. Drops
    /// frames whose universe doesn't match when the filter is on.
    pub(super) fn poll_artnet_inputs(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            let frames = self.artnet.poll(nid);
            if frames.is_empty() {
                continue;
            }
            if let Some(node) = self.graph.nodes.get_mut(&nid) {
                if let NodeType::ArtNetIn {
                    last_values,
                    channel_count,
                    start_at,
                    universe,
                    universe_filter,
                    ..
                } = &mut node.node_type
                {
                    let want_univ = *universe;
                    let filter_on = *universe_filter;
                    // Pick the most recent matching frame.
                    let latest = frames
                        .into_iter()
                        .filter(|f| !filter_on || f.universe == want_univ)
                        .last();
                    if let Some(latest) = latest {
                        let start_idx = (*start_at as usize).saturating_sub(1).min(511);
                        last_values.resize(*channel_count, 0);
                        for i in 0..*channel_count {
                            let ch = start_idx + i;
                            last_values[i] = latest.data.get(ch).copied().unwrap_or(0);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn poll_network_events(&mut self) {
        let events = self.network.poll();
        for event in events {
            match event {
                crate::network::NetworkEvent::LinkReady {
                    node_id,
                    link,
                    secret_hex,
                } => {
                    if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                        if let NodeType::NetworkSend {
                            last_link,
                            persistent_secret,
                            identity_mode,
                            ..
                        } = &mut node.node_type
                        {
                            *last_link = Some(link);
                            if *identity_mode == 1 && !secret_hex.is_empty() {
                                *persistent_secret = Some(secret_hex);
                            }
                        }
                    }
                }
                crate::network::NetworkEvent::SendStatus { node_id, peers } => {
                    if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                        if let NodeType::NetworkSend {
                            last_peer_count, ..
                        } = &mut node.node_type
                        {
                            *last_peer_count = peers;
                        }
                    }
                }
                crate::network::NetworkEvent::Received {
                    node_id,
                    values,
                    sender_short_id,
                } => {
                    if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                        if let NodeType::NetworkReceive {
                            last_values,
                            last_values_text,
                            last_sender,
                            status,
                            connected,
                            imported_schema,
                            ..
                        } = &mut node.node_type
                        {
                            // Update values from received data
                            last_values.clear();
                            last_values_text.clear();
                            for val in values.iter() {
                                match val {
                                    crate::network::NetPort::Float(f) => {
                                        last_values.push(*f);
                                        last_values_text.push(format!("{:.4}", f));
                                    }
                                    crate::network::NetPort::Text(t) => {
                                        last_values.push(0.0);
                                        last_values_text.push(t.clone());
                                    }
                                    crate::network::NetPort::Image { .. } => {
                                        last_values.push(0.0);
                                        last_values_text.push("<image>".into());
                                    }
                                    crate::network::NetPort::None => {
                                        last_values.push(0.0);
                                        last_values_text.push(String::new());
                                    }
                                }
                            }
                            // Format sender short ID
                            *last_sender = sender_short_id
                                .iter()
                                .map(|b| format!("{:02x}", b))
                                .collect::<String>();
                            *connected = true;
                            let port_count = imported_schema.len();
                            *status = format!("Connected ({} ports)", port_count);
                        }
                    }
                }
                crate::network::NetworkEvent::Error { node_id, error } => {
                    if let Some(node) = self.graph.nodes.get_mut(&node_id) {
                        match &mut node.node_type {
                            NodeType::NetworkSend { .. } => {
                                eprintln!("Network send error for node {}: {}", node_id, error);
                            }
                            NodeType::NetworkReceive {
                                status, connected, ..
                            } => {
                                *status = format!("Error: {}", error);
                                *connected = false;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// Consume a pending gear-icon click from an AI generator node.
    ///
    /// The generator writes `(requester_node_id, ai_config_output_port)`
    /// into `egui::Memory` under the key `"ai_gear_click"`. This method
    /// drains the key each frame and:
    ///
    /// 1. If the generator's `Config` input is already wired, does nothing
    ///    (the wire already points to an AiConfig — a future iteration
    ///    will pan/flash that config).
    /// 2. Else if any `AiConfigNode` exists on the graph, creates a
    ///    connection from its matching output port → generator's Config input.
    /// 3. Else spawns a fresh `AiConfigNode` next to the generator and
    ///    wires it up.
    pub(super) fn process_ai_gear_click(&mut self, ctx: &egui::Context) {
        let key = egui::Id::new("ai_gear_click");
        let pending: Option<(NodeId, usize)> =
            ctx.data_mut(|d| d.remove_temp::<(NodeId, usize)>(key));
        let Some((requester, out_port)) = pending else {
            return;
        };

        let Some(req_node) = self.graph.nodes.get(&requester) else {
            return;
        };
        let req_pos = req_node.pos;

        let already_wired = self
            .graph
            .connections
            .iter()
            .any(|c| c.to_node == requester && c.to_port == 0);
        if already_wired {
            // TODO(gear-pan): find the upstream AiConfig and pan canvas to it + flash.
            return;
        }

        // Look for an existing AiConfig on the graph.
        let existing = self.graph.nodes.iter().find_map(|(nid, n)| {
            if let NodeType::Dynamic { inner } = &n.node_type {
                if inner.node.type_tag() == "ai_config" {
                    return Some(*nid);
                }
            }
            None
        });

        let config_id = if let Some(id) = existing {
            id
        } else {
            let cfg = NodeType::Dynamic {
                inner: crate::graph::DynNode {
                    node: Box::new(crate::nodes::ai_config_node::AiConfigNode::default()),
                },
            };
            // Place to the left so the wire flows left→right into the generator.
            let pos = [req_pos[0] - 320.0, req_pos[1]];
            self.graph.add_node(cfg, pos)
        };

        // Wire AiConfig's matching output port → requester's Config input (port 0).
        self.graph.add_connection(config_id, out_port, requester, 0);
    }

    pub(super) fn poll_http_responses(&mut self) {
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            if let Some(resp) = self.http.poll(nid) {
                if let Some(node) = self.graph.nodes.get_mut(&nid) {
                    match &mut node.node_type {
                        NodeType::HttpRequest {
                            response, status, ..
                        } => {
                            *status = format!("{}", resp.status);
                            *response = resp.body.clone();
                            // Surface non-2xx and network errors as node errors.
                            // status == 0 means the request never reached the server
                            // (DNS, connect, TLS, …); body holds the underlying message.
                            if resp.status == 0 {
                                crate::node_errors::report_for(
                                    nid,
                                    format!(
                                        "HTTP request failed: {}",
                                        truncate_for_error(&resp.body)
                                    ),
                                );
                            } else if !(200..300).contains(&resp.status) {
                                crate::node_errors::report_for(
                                    nid,
                                    format!(
                                        "HTTP {}: {}",
                                        resp.status,
                                        truncate_for_error(&resp.body)
                                    ),
                                );
                            } else {
                                crate::node_errors::clear(nid);
                            }
                        }
                        NodeType::AiRequest {
                            provider,
                            response,
                            status,
                            ..
                        } => {
                            if resp.status >= 200 && resp.status < 300 {
                                // Auto-detect provider from response if not set
                                let prov = if provider.is_empty() {
                                    if resp.body.contains("\"candidates\"") {
                                        "google"
                                    } else if resp.body.contains("\"content\":[{\"type\"") {
                                        "anthropic"
                                    } else {
                                        "openai"
                                    }
                                } else {
                                    provider.as_str()
                                };
                                *response =
                                    crate::nodes::ai_request::extract_ai_response(prov, &resp.body);
                                *status = "done".into();
                                crate::node_errors::clear(nid);
                            } else {
                                *response = resp.body.clone();
                                *status = format!("error: {}", resp.status);
                                let label = if resp.status == 0 {
                                    "AI request failed".to_string()
                                } else {
                                    format!("AI HTTP {}", resp.status)
                                };
                                crate::node_errors::report_for(
                                    nid,
                                    format!("{}: {}", label, truncate_for_error(&resp.body)),
                                );
                            }
                        }
                        NodeType::Dynamic { inner } => {
                            // Trait-based nodes (TextGen, ImageGen, CodeGen, …)
                            // handle their own response extraction via
                            // `apply_http_response`. Non-HTTP trait nodes have
                            // a no-op default impl, so this is safe for all.
                            inner
                                .node
                                .apply_http_response(resp.body.clone(), resp.status);
                            if !(200..300).contains(&resp.status) {
                                let label = if resp.status == 0 {
                                    "Request failed".to_string()
                                } else {
                                    format!("HTTP {}", resp.status)
                                };
                                crate::node_errors::report_for(
                                    nid,
                                    format!("{}: {}", label, truncate_for_error(&resp.body)),
                                );
                            } else {
                                crate::node_errors::clear(nid);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Poll ML inference results and dispatch new requests
    pub(super) fn poll_ml_inference(&mut self, ctx: &egui::Context) {
        // Receive completed results
        while let Ok(result) = self.ml_rx.try_recv() {
            let nid = result.node_id;
            // Mark this node as no longer running so next request can be dispatched
            self.ml_running.remove(&nid);
            if let Some(node) = self.graph.nodes.get_mut(&nid) {
                match &mut node.node_type {
                    NodeType::MlModel {
                        result_text,
                        result_json,
                        annotated_frame,
                        status,
                        ..
                    } => {
                        *result_text = result.result_text;
                        *result_json = result.result_json;
                        *annotated_frame = result.annotated_frame;
                        let s = result.status.clone();
                        *status = s;
                        if status.starts_with("Error") || status.starts_with("error") {
                            crate::node_errors::report_for(
                                nid,
                                format!("ML inference: {}", status),
                            );
                        } else {
                            crate::node_errors::clear(nid);
                        }
                    }
                    NodeType::Dynamic { inner, .. } => {
                        // Trait-based ML nodes — route result to the correct
                        // egui temp key so each node type reads its own results.
                        let key = match inner.node.type_tag() {
                            "face_detection" => "face_det_result",
                            "pose_detection" => "pose_det_result",
                            _ => "hand_det_result",
                        };
                        let result_id = egui::Id::new((key, nid));
                        ctx.data_mut(|d| d.insert_temp(result_id, result));
                    }
                    _ => {}
                }
            }
        }

        // Check for new inference requests (stored in egui temp data by ml_model::render).
        // Only dispatch if no inference is already running for this node — prevents
        // thread flooding when inference is slower than the frame rate.
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            if self.ml_running.contains(&nid) {
                // Still running — consume and discard any queued request so it
                // doesn't accumulate across frames.
                let inference_id = egui::Id::new(("ml_inference", nid));
                ctx.data_mut(|d| {
                    d.remove::<crate::nodes::ml_model::MlInferenceRequest>(inference_id)
                });
                continue;
            }
            let inference_id = egui::Id::new(("ml_inference", nid));
            if let Some(req) = ctx.data_mut(|d| {
                d.get_temp::<crate::nodes::ml_model::MlInferenceRequest>(inference_id)
            }) {
                ctx.data_mut(|d| {
                    d.remove::<crate::nodes::ml_model::MlInferenceRequest>(inference_id)
                });
                self.ml_running.insert(nid);
                let tx = self.ml_tx.clone();
                std::thread::spawn(move || {
                    let result = crate::nodes::ml_model::run_inference(&req);
                    let _ = tx.send(result);
                });
            }
        }
    }

    /// Poll TTS synthesis results and dispatch new requests.
    /// Mirrors poll_ml_inference — uses egui temp data for requests, mpsc for results.
    pub(super) fn poll_tts_synthesis(&mut self, ctx: &egui::Context) {
        // Receive completed results
        while let Ok(result) = self.tts_rx.try_recv() {
            let nid = result.node_id;
            self.audio.tts_running.remove(&nid);
            // Write status + done flag back into the node via egui temp data
            // (the node reads these in render_with_context on the next frame)
            ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new(("tts_result", nid)),
                    (result.status, result.success),
                );
            });
        }

        // Check for Play button presses — replay without re-synthesizing
        let node_ids_play: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids_play {
            let play_id = egui::Id::new(("tts_play", nid));
            if ctx.data_mut(|d| d.get_temp::<bool>(play_id)).is_some() {
                ctx.data_mut(|d| d.remove::<bool>(play_id));
                // Reset buffer read head so audio plays from the beginning
                if let Some(buf) = self.audio.tts_buffers.get(&nid) {
                    buf.replay();
                }
                // Re-send connect command so the engine is wired if it was cleaned up
                let outgoing: Vec<(NodeId, NodeId, usize)> = self
                    .graph
                    .connections
                    .iter()
                    .filter(|c| {
                        c.from_node == nid && self.audio.node_params.contains_key(&c.to_node)
                    })
                    .map(|c| (c.from_node, c.to_node, c.to_port))
                    .collect();
                for (fn_, tn, tp) in outgoing {
                    let engine_port = if matches!(
                        self.graph.nodes.get(&tn).map(|n| &n.node_type),
                        Some(crate::graph::NodeType::AudioMixer { .. })
                    ) {
                        tp / 2
                    } else {
                        tp
                    };
                    self.audio.connect_audio(fn_, tn, engine_port);
                }
            }
        }

        // Check for new synthesis requests stored by TtsNode::render_with_context
        let node_ids: Vec<NodeId> = self.graph.nodes.keys().copied().collect();
        for nid in node_ids {
            if self.audio.tts_running.contains(&nid) {
                // Still running — consume and discard queued request to avoid accumulation
                let req_id = egui::Id::new(("tts_request", nid));
                ctx.data_mut(|d| d.remove::<crate::nodes::tts_node::TtsSynthRequest>(req_id));
                continue;
            }
            let req_id = egui::Id::new(("tts_request", nid));
            if let Some(req) =
                ctx.data_mut(|d| d.get_temp::<crate::nodes::tts_node::TtsSynthRequest>(req_id))
            {
                ctx.data_mut(|d| d.remove::<crate::nodes::tts_node::TtsSynthRequest>(req_id));
                let tts_tx = self.tts_tx.clone();
                self.audio.start_tts_synthesis(nid, &req, &tts_tx);

                // Immediately wire TTS → downstream audio connections.
                // pending_connection_sync fires 1 frame later; doing it here ensures
                // the engine connection is live before the first audio callback reads.
                let outgoing: Vec<(NodeId, usize, NodeId, usize)> = self
                    .graph
                    .connections
                    .iter()
                    .filter(|c| {
                        c.from_node == nid && self.audio.node_params.contains_key(&c.to_node)
                    })
                    .map(|c| (c.from_node, c.from_port, c.to_node, c.to_port))
                    .collect();
                for (fn_, _fp, tn, tp) in outgoing {
                    let engine_port = if matches!(
                        self.graph.nodes.get(&tn).map(|n| &n.node_type),
                        Some(crate::graph::NodeType::AudioMixer { .. })
                    ) {
                        tp / 2
                    } else {
                        tp
                    };
                    self.audio.connect_audio(fn_, tn, engine_port);
                }
            }
        }
    }

    /// Process pending MCP commands from the MCP server thread
    pub(super) fn process_mcp_commands(&mut self, values: &HashMap<(NodeId, usize), PortValue>) {
        let rx = match &self.mcp_rx {
            Some(rx) => rx,
            None => return,
        };
        // Drain all pending requests (non-blocking)
        while let Ok(request) = rx.try_recv() {
            let result = crate::mcp::execute_command(request.command, &mut self.graph, values);
            let _ = request.response_tx.send(result);
        }
    }

    /// Save As for a custom-node tab: write the live graph as a new
    /// .pwnode and point this tab at it. The original file is not changed;
    /// the tab stays a custom-node document.
    pub(super) fn save_custom_node_as(&mut self) {
        use crate::document::DocumentKind;
        let document = self.documents.active();
        if document.kind != DocumentKind::CustomNode || document.read_only {
            return;
        }
        let current = document.path.clone();
        let stem = current
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| document.title.clone());

        let mut dialog = rfd::FileDialog::new()
            .set_title("Save Custom Node As")
            .add_filter("Patchwork Custom Node", &["pwnode"])
            .set_file_name(format!("{stem}.pwnode"));
        if let Some(dir) = current.as_ref().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(std::path::PathBuf::from(
                dir.to_string_lossy().trim_start_matches(r"\\?\"),
            ));
        }
        let Some(destination) = dialog.save_file() else {
            return;
        };
        let destination = crate::graph::group::with_pwnode_extension(destination);

        // Same file: an ordinary save.
        let canonical_destination = std::fs::canonicalize(&destination).ok();
        let canonical_current = current.as_ref().and_then(|p| std::fs::canonicalize(p).ok());
        if canonical_destination.is_some() && canonical_destination == canonical_current {
            self.save_project_quick();
            return;
        }
        if destination.exists() {
            let open_in_tab = canonical_destination.is_some()
                && self.documents.documents.iter().any(|doc| {
                    doc.path
                        .as_ref()
                        .and_then(|p| std::fs::canonicalize(p).ok())
                        == canonical_destination
                });
            if open_in_tab {
                self.toast_error(
                    "That file is open in another tab. Close it or choose another file.",
                );
                return;
            }
            let confirmed = rfd::MessageDialog::new()
                .set_title("Replace Custom Node?")
                .set_description(format!(
                    "{} already exists. Replacing it changes every instance that uses it. \
                     Replace it?",
                    destination.display()
                ))
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            if confirmed != rfd::MessageDialogResult::Yes {
                return;
            }
        }

        match crate::document::save_custom_node_as(&self.graph, &destination) {
            Ok(file) => {
                let location = std::fs::canonicalize(&destination).unwrap_or(destination);
                let document = self.documents.active_mut();
                if let Err(error) = document.set_path(Some(location.clone())) {
                    // Cannot happen for a .pwnode destination; keep the old
                    // location rather than an inconsistent one.
                    crate::system_log::error(format!("Save As: {error}"));
                    return;
                }
                document.title = file.name.clone();
                document.is_dirty = false;
                self.project_path = Some(location.to_string_lossy().into_owned());
                self.is_dirty = false;
                self.documents.clear_resolved_paths();
                // Instances of a replaced file pick up the new interface.
                self.refresh_custom_node_instances(&location, &file.inputs, &file.outputs);
                crate::system_log::log(format!("Saved custom node as {}", location.display()));
                self.toast_info(format!("Saved custom node as '{}'.", file.name));
            }
            Err(error) => {
                crate::system_log::error(format!("Save Custom Node As failed: {error}"));
                self.toast_error(format!("Save Custom Node As failed: {error}"));
            }
        }
    }

    /// Save to existing project path (Cmd+S). Falls back to Save As if no path set.
    pub(super) fn save_project_quick(&mut self) -> bool {
        if self.documents.active().kind == crate::document::DocumentKind::CustomNode {
            let Some(path) = self.documents.active().path.clone() else {
                crate::system_log::error(
                    "Cannot save custom node: no .pwnode path assigned.".to_string(),
                );
                return false;
            };
            // P4.1: published package versions are read-only. (The same
            // rule is enforced again inside `CustomNodeFile::save`.)
            if self.documents.active().read_only {
                self.toast_warn(
                    "This published Library version is read-only. Use File → Duplicate for Editing to make an editable copy.",
                );
                return false;
            }

            let existing = match crate::document::CustomNodeFile::load(&path) {
                Ok(file) => file,
                Err(error) => {
                    crate::system_log::error(format!("Cannot save custom node: {error}"));
                    return false;
                }
            };

            // Shared with P2 grouping so both build ports in the same order.
            let (inputs, outputs) = crate::document::custom_node_interface(&self.graph);

            let custom_node = crate::document::CustomNodeFile {
                version: existing.version,
                name: existing.name,
                graph: self.graph.clone(),
                inputs,
                outputs,
            };
            // Reject recursive dependencies before overwriting the .pwnode file.
            if let Err(error) =
                crate::graph::Graph::validate_custom_node_save(&custom_node.graph, &path)
            {
                crate::system_log::error(format!("Cannot save custom node: {error}"));
                return false;
            }
            match custom_node.save(&path) {
                Ok(()) => {
                    self.is_dirty = false;
                    self.documents.active_mut().is_dirty = false;
                    // P3: Update instances of this custom node in all open documents.
                    self.refresh_custom_node_instances(
                        &path,
                        &custom_node.inputs,
                        &custom_node.outputs,
                    );
                    return true;
                }
                Err(error) => {
                    crate::system_log::error(format!("Failed to save custom node: {error}"));
                    return false;
                }
            }
        }
        if let Some(dir_str) = self.project_path.clone() {
            self.save_to_dir(dir_str)
        } else {
            self.save_project()
        }
    }

    /// Save As — prompt for project name, then pick parent folder.
    pub(super) fn save_project(&mut self) -> bool {
        // Select the exact project folder.
        let Some(project_dir) = rfd::FileDialog::new()
            .set_title("Select Patchwork Project Folder")
            .pick_folder()
        else {
            return false;
        };

        // Do not create another nested my-project folder.
        let dir_str = project_dir.display().to_string();

        self.save_to_dir(dir_str)
    }
    /// Write project.json to the given directory. API keys are persisted to
    /// the per-user store at `~/.patchwork/keys.json` (see `save_user_keys`) —
    /// they never live in the project folder so they don't ride along when
    /// the project is shared. Any legacy in-project `api_keys.json` is
    /// migrated and removed.
    /// Asset paths are temporarily converted to relative for portability, then restored.
    /// Returns true on successful write — callers use this to update dirty/recent state.
    pub(super) fn save_to_dir(&mut self, dir_str: String) -> bool {
        // Stage 5: saving updates the live graph (package paths), so it waits
        // for a pending version-switch confirmation.
        if self.pending_version_switch.is_some() {
            self.toast_warn("Saving is paused while the version switch is being confirmed.");
            return false;
        }
        let dir = Path::new(&dir_str);
        // P3: Prevent different tabs from saving to the same project.
        let target_path = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());

        let active_id = self.documents.active().id;

        let already_open_in_another_tab = self.documents.documents.iter().any(|doc| {
            if doc.id == active_id {
                return false;
            }

            let Some(existing_path) = &doc.path else {
                return false;
            };

            let existing_path =
                std::fs::canonicalize(existing_path).unwrap_or_else(|_| existing_path.clone());

            existing_path == target_path
        });

        if already_open_in_another_tab {
            let message = "This project is already open in another tab.\n\n\
        Please select a different folder to save this project.";

            crate::system_log::error(message.to_string());

            rfd::MessageDialog::new()
                .set_title("Cannot Save Project")
                .set_description(message)
                .set_level(rfd::MessageLevel::Error)
                .set_buttons(rfd::MessageButtons::Ok)
                .show();

            return false;
        }
        // P3: Warn before overwriting an existing project on disk.
        let existing_project = dir.join("project.json");

        if existing_project.exists() {
            let active_path = self
                .project_path
                .as_ref()
                .and_then(|p| std::fs::canonicalize(p).ok());

            let target_path = std::fs::canonicalize(dir).ok();

            // Allow normal saving to the active project's own folder.
            let is_current_project = active_path.is_some() && active_path == target_path;

            if !is_current_project {
                let confirmed = rfd::MessageDialog::new()
                    .set_title("Project Already Exists")
                    .set_description(
                        "A Patchwork project already exists in this folder.\n\n\
                 Do you want to overwrite it?",
                    )
                    .set_level(rfd::MessageLevel::Warning)
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show();

                if confirmed != rfd::MessageDialogResult::Yes {
                    return false;
                }
            }
        }
        // P4.2 Stage 4: bundle the exact packages this project uses and
        // write project.json atomically with them. A failure leaves both
        // the files on disk and the live graph as they were.
        let source_project = match self.documents.active().kind {
            crate::document::DocumentKind::Project => {
                self.project_path.as_ref().map(std::path::PathBuf::from)
            }
            crate::document::DocumentKind::CustomNode => None,
        };
        let library_root = self.library_cache.root().ok();
        let project_file = dir.join("project.json");
        let ok = match save_project_files(
            &mut self.graph,
            &mut self.undo_history,
            self.pinned_nodes.iter().copied().collect(),
            [self.canvas_offset.x, self.canvas_offset.y],
            self.canvas_zoom,
            dir,
            source_project.as_deref(),
            library_root.as_deref(),
        ) {
            Ok(saved) => {
                crate::system_log::log(format!(
                    "Saved to {}. {}",
                    project_file.display(),
                    saved.portability.summary()
                ));
                for warning in &saved.plan.warnings {
                    crate::system_log::warn(format!("Portability: {warning}"));
                }
                let summary = saved.portability.summary();
                if saved.portability.custom_nodes_portable()
                    && saved.portability.external_assets == 0
                {
                    self.toast_info(summary);
                } else {
                    self.toast_warn(format!("{summary} See the System Log."));
                }
                true
            }
            Err(error) => {
                crate::system_log::error(format!("Save failed: {error}"));
                self.toast_error(format!("Save failed: {error}"));
                false
            }
        };
        // P0-2: API keys NEVER write into the project folder anymore — they
        // would ride along when a user zips/shares the project. Persist to the
        // per-user `~/.patchwork/keys.json` store on every save instead.
        if !self.api_keys.is_empty() {
            save_user_keys(&self.api_keys);
        }
        // Belt-and-suspenders: if a legacy `api_keys.json` is still present in
        // this project dir (from an older Patchwork version), delete it now so
        // the next zip/share doesn't leak. The keys are safely persisted to
        // the user store above.
        let legacy_keys_file = dir.join("api_keys.json");
        if legacy_keys_file.exists() {
            if let Err(e) = std::fs::remove_file(&legacy_keys_file) {
                crate::system_log::warn(format!(
                    "Failed to remove legacy api_keys.json from project folder: {}",
                    e
                ));
            } else {
                crate::system_log::log(
                    "Migrated: removed legacy api_keys.json from project folder \
                     (keys now stored at ~/.patchwork/keys.json)",
                );
            }
        }
        if ok {
            self.is_dirty = false;

            // P3: Associate the saved location with the active document.
            self.project_path = Some(dir_str.clone());

            // Update the active tab's graph, path, title and dirty state.
            self.sync_active_document();

            self.add_recent(dir_str);
        }
        ok
    }

    pub(super) fn load_project(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Patchwork", &["json"])
            .pick_file()
        {
            self.load_project_path(path);
        }
    }

    /// Load a project given a path to either `project.json` or its parent
    /// directory. Used by both the file dialog and the Open Recent submenu.
    pub(super) fn load_project_path(&mut self, path: std::path::PathBuf) {
        // Resolve to the project.json file. Recent entries are stored as
        // dirs; the dialog returns a file. Handle both shapes.
        let json_path = if path.is_dir() {
            path.join("project.json")
        } else {
            path
        };
        let dir = json_path.parent().map(|p| p.to_path_buf());
        let dir_str = dir
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        // P3: If this project is already open, switch to its existing tab.
        let requested_path = std::fs::canonicalize(&json_path).ok();

        if let Some(existing_id) = self.documents.documents.iter().find_map(|doc| {
            let open_path = doc.path.as_ref()?;

            // Document paths currently store the project directory.
            let open_json = if open_path.is_dir() {
                open_path.join("project.json")
            } else {
                open_path.clone()
            };

            let existing_path = std::fs::canonicalize(open_json).ok()?;

            if Some(existing_path) == requested_path {
                Some(doc.id)
            } else {
                None
            }
        }) {
            self.switch_document(existing_id);
            return;
        }

        if !json_path.exists() {
            crate::system_log::error(format!("Project not found: {}", json_path.display()));
            // Drop the broken entry from recents so the user doesn't keep
            // clicking a dead link.
            self.recent_projects.retain(|p| p != &dir_str);
            return;
        }

        let json = match std::fs::read_to_string(&json_path) {
            Ok(j) => j,
            Err(e) => {
                crate::system_log::error(format!("Load failed: {}", e));
                return;
            }
        };
        let pf: ProjectFile = match serde_json::from_str(&json) {
            Ok(pf) => pf,
            Err(e) => {
                crate::system_log::error(format!("Load failed: {}", e));
                return;
            }
        };
        let mut graph = pf.graph;
        graph.fix_next_id();
        absolutize_paths(&mut graph, &dir_str);

        // P3: Create a separate tab for the successfully loaded project.
        // Existing tabs retain their graphs and editor state. Never load
        // into the current tab if no new tab could be created.
        if !self.create_new_tab() {
            return;
        }
        // Always start with DSP, Camera, Mic off for safety —
        // prevents stale "active" state without actual streams running
        for node in graph.nodes.values_mut() {
            match &mut node.node_type {
                NodeType::AudioDevice { enabled, .. } => {
                    *enabled = false;
                }
                NodeType::Camera { active, .. } => {
                    *active = false;
                }
                NodeType::AudioInput { active, .. } => {
                    *active = false;
                }
                NodeType::VideoPlayer { playing, .. } => {
                    *playing = false;
                }
                _ => {}
            }
        }
        self.audio.stop_output();
        self.graph = graph;
        self.graph.audio_topology_dirty = true;
        // Restore UI state from project file
        self.pinned_nodes = pf.pinned_nodes.into_iter().collect();
        self.canvas_offset = egui::Vec2::new(pf.canvas_offset[0], pf.canvas_offset[1]);
        self.canvas_zoom = pf.canvas_zoom;
        self.target_zoom = pf.canvas_zoom;
        // Clear transient state
        self.port_positions.clear();
        self.node_rects.clear();
        self.undo_history.clear();
        self.selected_nodes.clear();
        self.selected_connection = None;
        self.wire_menu_conn = None;
        self.dragging_from = None;
        self.show_node_menu = false;
        self.show_context_menu = false;
        // The previous project's GPU textures and per-node
        // egui temp data must be wiped before any node from
        // the new project (whose ids restart at 1 after
        // `fix_next_id`) reads them back. Done at the top
        // of the next `update()` where ctx is in scope.
        self.caches_dirty = true;
        crate::system_log::log(format!("Loaded {}", json_path.display()));

        // P0-2: load api keys from the per-user store, NOT the project folder.
        // For backwards-compat, if a legacy `api_keys.json` is found in the
        // project folder, merge any unknown entries into the user store and
        // remove the legacy file (migration). User-store entries win on key
        // collisions — they're the more authoritative source.
        let user_keys = load_user_keys();
        if let Some(dir) = &dir {
            let legacy_keys_file = dir.join("api_keys.json");
            if let Ok(json) = std::fs::read_to_string(&legacy_keys_file) {
                if let Ok(legacy_keys) = serde_json::from_str::<HashMap<String, String>>(&json) {
                    let mut merged = legacy_keys;
                    merged.extend(user_keys);
                    save_user_keys(&merged);
                    self.api_keys = merged;
                    let _ = std::fs::remove_file(&legacy_keys_file);
                    crate::system_log::log(format!(
                        "Migrated api_keys.json from project folder → ~/.patchwork/keys.json \
                         and removed the project-local copy"
                    ));
                } else {
                    self.api_keys = user_keys;
                }
            } else {
                self.api_keys = user_keys;
            }
        } else {
            self.api_keys = user_keys;
        }
        self.project_path = Some(dir_str.clone());
        self.is_dirty = false;

        // P3: Store the loaded project's state in the active document.
        self.sync_active_document();

        self.add_recent(dir_str);
    }

    /// Push a project directory to the front of the recents list,
    /// dedup, and cap at 10 entries.
    pub(super) fn add_recent(&mut self, dir: String) {
        if dir.is_empty() {
            return;
        }
        self.recent_projects.retain(|p| p != &dir);
        self.recent_projects.insert(0, dir);
        self.recent_projects.truncate(10);
    }

    /// Push a `Title(...)` viewport command when the displayed title
    /// changes. Format is "PatchWork — {project} [*]"; the `*` shows
    /// when there are unsaved changes.
    pub(super) fn update_window_title(&mut self, ctx: &egui::Context) {
        let name = self
            .project_path
            .as_deref()
            .and_then(|p| Path::new(p).file_name())
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string());
        let title = if self.is_dirty {
            format!("PatchWork — {} *", name)
        } else {
            format!("PatchWork — {}", name)
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    /// If the OS asks the window to close and we have unsaved changes,
    /// cancel the close and pop the confirmation modal. Once the user
    /// has chosen Save / Don't Save, `force_quit` lets the next request
    /// through.
    pub(super) fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }

        if self.force_quit {
            return;
        }

        // P3: Check the active tab and all background tabs.
        // The active tab's latest dirty state lives in self.is_dirty.
        let has_unsaved_changes =
            self.is_dirty || self.documents.documents.iter().any(|doc| doc.is_dirty);

        if !has_unsaved_changes {
            return;
        }

        // Prevent quitting while any document has unsaved changes.
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.set_quit_dialog_open(true);
    }

    /// Render the "Save changes before quitting?" dialog when pending.
    /// Save & Continue → save the document listed as next (may show a
    /// dialog), then move to the next unsaved tab or close when none remain.
    /// Discard All & Quit → close without saving (B1: the session reopens
    /// the saved project from disk). Cancel → keep everything open.
    pub(super) fn render_quit_dialog(&mut self, ctx: &egui::Context) {
        if !self.pending_quit_dialog {
            return;
        }
        let unsaved = unsaved_documents(&self.documents, self.is_dirty);
        let mut choice = None;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} document(s) have unsaved changes:",
                    unsaved.len()
                ));
                ui.add_space(4.0);
                for doc in &unsaved {
                    let line = format!(
                        "{} {} ({}){}",
                        if doc.next { "→" } else { "•" },
                        doc.title,
                        doc.kind_label(),
                        if doc.next { " — saved next" } else { "" }
                    );
                    if doc.next {
                        ui.label(egui::RichText::new(line).strong());
                    } else {
                        ui.label(line);
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .button("Save & Continue")
                        .on_hover_text("Save the document marked → then move to the next one")
                        .clicked()
                    {
                        choice = Some(QuitChoice::SaveAndContinue);
                    }
                    if ui
                        .button("Discard All & Quit")
                        .on_hover_text(
                            "Quit without saving any open tab. The last saved project \
                             reopens from disk next time.",
                        )
                        .clicked()
                    {
                        choice = Some(QuitChoice::DiscardAll);
                    }
                    if ui
                        .button("Cancel")
                        .on_hover_text("Keep Patchwork open with all unsaved work")
                        .clicked()
                    {
                        choice = Some(QuitChoice::Cancel);
                    }
                });
            });
        let Some(choice) = choice else {
            return;
        };

        let (saved, next_dirty) = if choice == QuitChoice::SaveAndContinue {
            // Save the active document first (if it has changes).
            let saved = if self.is_dirty {
                Some(self.save_project_quick())
            } else {
                None
            };
            let next = if saved == Some(false) {
                None
            } else {
                // Update the active document's saved state, then find the
                // next unsaved document.
                self.sync_active_document();
                self.documents
                    .documents
                    .iter()
                    .find(|doc| doc.is_dirty)
                    .map(|doc| doc.id)
            };
            (saved, next)
        } else {
            (None, None)
        };

        match quit_step(choice, saved, next_dirty) {
            QuitStep::KeepOpen => {}
            QuitStep::SwitchTo(document_id) => {
                // The only tab switch allowed while documents are locked.
                self.switch_document_for_quit(document_id);
            }
            QuitStep::Close { discard } => {
                self.discard_on_exit = discard;
                self.set_quit_dialog_open(false);
                self.force_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            QuitStep::Dismiss => {
                // Cancel: normal tab interactions resume.
                self.set_quit_dialog_open(false);
            }
        }
    }

    #[allow(dead_code)]
    pub(super) fn project_dir(&self) -> Option<std::path::PathBuf> {
        self.project_path
            .as_ref()
            .map(|p| std::path::PathBuf::from(p))
    }

    #[allow(dead_code)]
    pub(super) fn load_api_keys(&mut self) {
        // P0-2: per-user store is the canonical source; project-folder copy
        // is legacy and (if present) migrated by load_from_dir.
        self.api_keys = load_user_keys();
    }

    pub(super) fn sync_console_messages(&mut self) {
        for node in self.graph.nodes.values_mut() {
            if let NodeType::Console { messages } = &mut node.node_type {
                *messages = self.console_messages.clone();
            }
        }
    }

    pub(super) fn apply_theme(&self, ctx: &egui::Context) {
        for node in self.graph.nodes.values() {
            if let NodeType::Theme {
                dark_mode,
                accent,
                font_size,
                bg_color,
                text_color,
                window_bg,
                window_alpha,
                grid_color: _,
                rounding,
                spacing,
                ..
            } = &node.node_type
            {
                // Publish theme colors to temp data so anywhere with a
                // ctx can read them without crawling the graph. Read via
                // `crate::nodes::theme::current_*(ctx)` helpers.
                ctx.data_mut(|d| {
                    d.insert_temp(egui::Id::new("theme_accent"), *accent);
                    d.insert_temp(egui::Id::new("theme_text"), *text_color);
                    d.insert_temp(egui::Id::new("theme_bg"), *bg_color);
                });
                nodes::theme::apply(
                    ctx,
                    *dark_mode,
                    *accent,
                    *font_size,
                    *bg_color,
                    *text_color,
                    *window_bg,
                    *window_alpha,
                    *rounding,
                    *spacing,
                );
                return;
            }
        }
        // No Theme node found — apply default Patchwork theme so the app
        // looks correct from the first frame without requiring a Theme node.
        // Each session gets a random accent hue for visual variety.
        let accent = self.session_accent;
        let bg_color: [u8; 3] = [20, 20, 20];
        let text_color: [u8; 3] = [220, 220, 220];
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("theme_accent"), accent);
            d.insert_temp(egui::Id::new("theme_text"), text_color);
            d.insert_temp(egui::Id::new("theme_bg"), bg_color);
        });
        nodes::theme::apply(
            ctx,
            true,         // dark_mode
            accent,       // accent
            14.0,         // font_size
            bg_color,     // bg_color
            text_color,   // text_color
            [24, 24, 24], // window_bg
            240,          // window_alpha
            16.0,         // rounding
            4.0,          // spacing
        );
    }

    #[allow(dead_code)]
    pub(super) fn log_message(&mut self, msg: String) {
        self.console_messages.push(msg);
        if self.console_messages.len() > 200 {
            self.console_messages.remove(0);
        }
    }

    pub(super) fn update_mouse_trackers(&mut self, _ctx: &egui::Context) {
        // MouseTracker is now trait-based — reads pointer position in render_ui()
    }

    pub(super) fn update_key_inputs(&mut self, _ctx: &egui::Context) {
        // KeyInput is now trait-based — reads key state in render_ui()
    }
}

/// Session-fix: the legacy single-document session only ever holds a
/// project, and restoring never turns a custom node into a project.
#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::document::{CustomNodeFile, DocumentKind, DocumentManager};
    use crate::graph::PortKind;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patchwork_session_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn project_graph(marker: f32) -> Graph {
        let mut graph = Graph::new();
        graph.add_node(NodeType::FileMenu, [marker, marker]);
        graph
    }

    fn write_custom_node(path: &Path) {
        let mut graph = Graph::new();
        graph.add_node(
            NodeType::CustomOutput {
                name: "out".into(),
                kind: PortKind::Number,
            },
            [0.0, 0.0],
        );
        CustomNodeFile {
            version: 1,
            name: "Node".into(),
            graph,
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
        .save(path)
        .unwrap();
    }

    fn snapshot(graph: Graph, path: Option<String>, dirty: bool) -> SessionSnapshot {
        SessionSnapshot {
            graph,
            canvas_offset: [1.0, 2.0],
            canvas_zoom: 1.5,
            pinned_nodes: Vec::new(),
            project_path: path,
            is_dirty: dirty,
        }
    }

    fn marker_of(graph: &Graph) -> f32 {
        graph
            .nodes
            .values()
            .next()
            .map(|n| n.pos[0])
            .unwrap_or(-1.0)
    }

    /// Serialize and parse like save_session/restore_session do.
    fn round_trip(state: &SessionState) -> SessionState {
        serde_json::from_str(&serde_json::to_string_pretty(state).unwrap()).unwrap()
    }

    // ── Shutdown ─────────────────────────────────────────────────────

    #[test]
    fn shutdown_with_project_tab_active_saves_that_project() {
        let tmp = TempDir::new("project_active");
        let project_dir = tmp.0.join("MyProject");
        std::fs::create_dir_all(&project_dir).unwrap();
        let documents = DocumentManager::new();
        let live = snapshot(
            project_graph(7.0),
            Some(project_dir.to_string_lossy().into_owned()),
            true,
        );
        let selected = select_session_project(&documents, live).unwrap();
        let state = round_trip(&session_state_from(
            Some(selected),
            HashMap::new(),
            vec!["recent".into()],
        ));
        assert!(!state.clean_start);
        assert_eq!(marker_of(&state.graph), 7.0);
        assert!(
            state.is_dirty,
            "unsaved project work is kept in the session"
        );
        assert_eq!(
            plan_session_restore(state.project_path.as_deref(), state.clean_start),
            SessionRestorePlan::Project {
                project_path: Some(project_dir.to_string_lossy().into_owned()),
                mark_dirty: false,
                note: None,
            }
        );
    }

    #[test]
    fn shutdown_with_custom_node_active_saves_the_open_project() {
        let tmp = TempDir::new("custom_active");
        let node = tmp.0.join("Node.pwnode");
        write_custom_node(&node);
        let older = tmp.0.join("Older");
        let newer = tmp.0.join("Newer");
        std::fs::create_dir_all(&older).unwrap();
        std::fs::create_dir_all(&newer).unwrap();

        let mut documents = DocumentManager::new();
        // Tab 1: a saved project.
        documents.active_mut().store_graph(&project_graph(1.0));
        documents
            .active_mut()
            .set_path(Some(older.clone()))
            .unwrap();
        // Tab 2: another project with unsaved work (most recently opened).
        let second = documents.new_document("Newer");
        {
            let doc = documents
                .documents
                .iter_mut()
                .find(|d| d.id == second)
                .unwrap();
            doc.store_graph(&project_graph(2.0));
            doc.set_path(Some(newer.clone())).unwrap();
            doc.is_dirty = true;
        }
        // Active: a custom-node tab with unsaved edits.
        let custom = documents.open_custom_node(node.clone()).unwrap();
        assert!(documents.activate(custom));
        assert_eq!(documents.active().kind, DocumentKind::CustomNode);
        let live = snapshot(
            Graph::new(),
            Some(node.to_string_lossy().into_owned()),
            true,
        );

        let selected = select_session_project(&documents, live).unwrap();
        let state = round_trip(&session_state_from(
            Some(selected),
            HashMap::new(),
            Vec::new(),
        ));
        assert!(!state.clean_start);
        assert_eq!(marker_of(&state.graph), 2.0, "most recently opened project");
        assert_eq!(
            state.project_path.as_deref(),
            Some(newer.to_string_lossy().as_ref())
        );
        assert!(state.is_dirty);
        assert!(
            !state
                .project_path
                .as_deref()
                .unwrap()
                .to_lowercase()
                .ends_with(".pwnode")
        );
    }

    #[test]
    fn shutdown_with_only_custom_node_tabs_starts_clean_next_time() {
        let tmp = TempDir::new("custom_only");
        let node = tmp.0.join("Node.pwnode");
        write_custom_node(&node);
        let mut documents = DocumentManager::new();
        let custom = documents.open_custom_node(node.clone()).unwrap();
        assert!(documents.activate(custom));
        assert!(documents.close_document(1), "close the untitled project");
        assert!(
            documents
                .documents
                .iter()
                .all(|d| d.kind == DocumentKind::CustomNode)
        );

        let live = snapshot(
            Graph::new(),
            Some(node.to_string_lossy().into_owned()),
            true,
        );
        assert!(select_session_project(&documents, live).is_none());
        let state = round_trip(&session_state_from(
            None,
            HashMap::from([("k".to_string(), "v".to_string())]),
            vec!["recent-project".into()],
        ));
        assert!(state.clean_start);
        assert!(state.project_path.is_none());
        assert!(state.graph.nodes.is_empty());
        assert!(!state.is_dirty);
        // Recent projects and keys survive the clean start.
        assert_eq!(state.recent_projects, vec!["recent-project".to_string()]);
        assert_eq!(state.api_keys.get("k").map(String::as_str), Some("v"));
        assert_eq!(
            plan_session_restore(state.project_path.as_deref(), state.clean_start),
            SessionRestorePlan::Clean { reason: None }
        );
    }

    // ── Startup ──────────────────────────────────────────────────────

    #[test]
    fn legacy_session_with_pwnode_path_starts_clean() {
        let tmp = TempDir::new("legacy_pwnode");
        let node = tmp.0.join("nested-test.pwnode");
        write_custom_node(&node);
        // A session file written by an older build (no clean_start field).
        let legacy = serde_json::json!({
            "graph": serde_json::to_value(Graph::new()).unwrap(),
            "canvas_offset": [0.0, 0.0],
            "canvas_zoom": 1.0,
            "pinned_nodes": [],
            "project_path": node.to_string_lossy(),
            "is_dirty": true,
            "recent_projects": ["a"]
        })
        .to_string();
        let state: SessionState = serde_json::from_str(&legacy).unwrap();
        assert!(!state.clean_start, "older files default to false");
        match plan_session_restore(state.project_path.as_deref(), state.clean_start) {
            SessionRestorePlan::Clean {
                reason: Some(reason),
            } => {
                assert!(reason.contains("custom node"), "{reason}");
                assert!(reason.contains("not changed"), "{reason}");
            }
            other => panic!("expected a clean start, got {other:?}"),
        }
        // Upper-case extension too.
        assert!(matches!(
            plan_session_restore(Some("C:/x/Node.PWNODE"), false),
            SessionRestorePlan::Clean { .. }
        ));
    }

    #[test]
    fn legacy_project_session_is_restored_unchanged() {
        let tmp = TempDir::new("legacy_project");
        let dir = tmp.0.join("Project");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = serde_json::json!({
            "graph": serde_json::to_value(project_graph(3.0)).unwrap(),
            "canvas_offset": [0.0, 0.0],
            "canvas_zoom": 1.0,
            "pinned_nodes": [],
            "project_path": dir.to_string_lossy(),
        })
        .to_string();
        let state: SessionState = serde_json::from_str(&legacy).unwrap();
        assert_eq!(marker_of(&state.graph), 3.0);
        assert_eq!(
            plan_session_restore(state.project_path.as_deref(), state.clean_start),
            SessionRestorePlan::Project {
                project_path: Some(dir.to_string_lossy().into_owned()),
                mark_dirty: false,
                note: None,
            }
        );
        // An untitled session (no path) also restores as before.
        assert_eq!(
            plan_session_restore(None, false),
            SessionRestorePlan::Project {
                project_path: None,
                mark_dirty: false,
                note: None,
            }
        );
    }

    #[test]
    fn missing_or_corrupted_session_paths() {
        let tmp = TempDir::new("bad_paths");
        // Missing project folder: keep the work, but as an unsaved Untitled
        // project so Ctrl+S cannot write to a vanished location.
        let missing = tmp.0.join("Gone");
        match plan_session_restore(Some(&missing.to_string_lossy()), false) {
            SessionRestorePlan::Project {
                project_path: None,
                mark_dirty: true,
                note: Some(note),
            } => assert!(note.contains("unavailable"), "{note}"),
            other => panic!("{other:?}"),
        }
        // A file where a project folder should be.
        let file = tmp.0.join("project.json");
        std::fs::write(&file, "{}").unwrap();
        assert!(matches!(
            plan_session_restore(Some(&file.to_string_lossy()), false),
            SessionRestorePlan::Project {
                project_path: None,
                mark_dirty: true,
                ..
            }
        ));
        // Empty / whitespace path: untitled, as before.
        assert!(matches!(
            plan_session_restore(Some("   "), false),
            SessionRestorePlan::Project {
                project_path: None,
                mark_dirty: false,
                ..
            }
        ));
        // A corrupted session file is not parsed at all (restore returns
        // false and the app starts clean).
        assert!(serde_json::from_str::<SessionState>("{ not json").is_err());
        assert!(serde_json::from_str::<SessionState>(r#"{"graph": 5}"#).is_err());
    }

    #[test]
    fn session_handling_never_modifies_project_or_custom_node_files() {
        let tmp = TempDir::new("no_writes");
        let project = tmp.0.join("Project");
        std::fs::create_dir_all(&project).unwrap();
        let project_json = project.join("project.json");
        std::fs::write(
            &project_json,
            r#"{"graph":{"nodes":{},"connections":[],"next_id":1}}"#,
        )
        .unwrap();
        let node = tmp.0.join("Node.pwnode");
        write_custom_node(&node);
        let watched = [project_json.clone(), node.clone()];
        let before: Vec<_> = watched
            .iter()
            .map(|p| {
                (
                    std::fs::read(p).unwrap(),
                    std::fs::metadata(p).unwrap().modified().unwrap(),
                )
            })
            .collect();

        let mut documents = DocumentManager::new();
        documents
            .active_mut()
            .set_path(Some(project.clone()))
            .unwrap();
        let custom = documents.open_custom_node(node.clone()).unwrap();
        documents.activate(custom);
        let live = snapshot(
            project_graph(9.0),
            Some(node.to_string_lossy().into_owned()),
            true,
        );
        let state = session_state_from(
            select_session_project(&documents, live),
            HashMap::new(),
            Vec::new(),
        );
        let session_file = tmp.0.join("last_session.json");
        std::fs::write(&session_file, serde_json::to_string_pretty(&state).unwrap()).unwrap();
        let reread: SessionState =
            serde_json::from_str(&std::fs::read_to_string(&session_file).unwrap()).unwrap();
        let _ = plan_session_restore(reread.project_path.as_deref(), reread.clean_start);
        let _ = plan_session_restore(Some(&node.to_string_lossy()), false);

        let after: Vec<_> = watched
            .iter()
            .map(|p| {
                (
                    std::fs::read(p).unwrap(),
                    std::fs::metadata(p).unwrap().modified().unwrap(),
                )
            })
            .collect();
        assert_eq!(before, after);
    }
}

/// B1 / B2 / R3: Discard All & Quit, the quit dialog list and decisions.
#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use crate::document::{CustomNodeFile, DocumentKind, DocumentManager};
    use crate::graph::PortKind;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patchwork_shutdown_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn graph_marked(marker: f32) -> Graph {
        let mut graph = Graph::new();
        graph.add_node(NodeType::FileMenu, [marker, marker]);
        graph
    }

    fn marker_of(graph: &Graph) -> f32 {
        graph
            .nodes
            .values()
            .next()
            .map(|n| n.pos[0])
            .unwrap_or(-1.0)
    }

    /// A saved project folder whose project.json holds `marker`.
    fn saved_project(root: &Path, name: &str, marker: f32) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let pf = ProjectFile {
            graph: graph_marked(marker),
            pinned_nodes: Vec::new(),
            canvas_offset: [0.0, 0.0],
            canvas_zoom: 1.0,
        };
        std::fs::write(
            dir.join("project.json"),
            serde_json::to_string_pretty(&pf).unwrap(),
        )
        .unwrap();
        dir
    }

    fn custom_node(path: &Path) {
        let mut graph = Graph::new();
        graph.add_node(
            NodeType::CustomOutput {
                name: "out".into(),
                kind: PortKind::Number,
            },
            [0.0, 0.0],
        );
        CustomNodeFile {
            version: 1,
            name: "Mixer".into(),
            graph,
            inputs: Vec::new(),
            outputs: Vec::new(),
        }
        .save(path)
        .unwrap();
    }

    fn live(graph: Graph, path: Option<&Path>, dirty: bool) -> SessionSnapshot {
        SessionSnapshot {
            graph,
            canvas_offset: [3.0, 4.0],
            canvas_zoom: 2.0,
            pinned_nodes: Vec::new(),
            project_path: path.map(|p| p.to_string_lossy().into_owned()),
            is_dirty: dirty,
        }
    }

    fn round_trip(state: &SessionState) -> SessionState {
        serde_json::from_str(&serde_json::to_string_pretty(state).unwrap()).unwrap()
    }

    fn plan(state: &SessionState) -> SessionRestorePlan {
        plan_session_restore_with(
            state.project_path.as_deref(),
            state.clean_start,
            state.reopen_from_disk,
        )
    }

    /// Mirrors restore_session: the graph that would be loaded, or None for
    /// a clean start.
    fn restored_graph(state: &SessionState) -> Option<Graph> {
        match plan(state) {
            SessionRestorePlan::Clean { .. } => None,
            SessionRestorePlan::ReopenFromDisk { project_path } => {
                read_project_file(Path::new(&project_path))
                    .ok()
                    .map(|pf| pf.graph)
            }
            SessionRestorePlan::Project { .. } => Some(state.graph.clone()),
        }
    }

    // 1 ──────────────────────────────────────────────────────────────────
    #[test]
    fn discarding_an_edited_saved_project_reopens_the_disk_version() {
        let tmp = TempDir::new("discard_saved");
        let dir = saved_project(&tmp.0, "Saved", 1.0);
        let mut documents = DocumentManager::new();
        documents.active_mut().set_path(Some(dir.clone())).unwrap();

        // Edited in memory (marker 9) and discarded.
        let state = round_trip(&discard_session_state(
            &documents,
            live(graph_marked(9.0), Some(&dir), true),
            HashMap::from([("key".to_string(), "v".to_string())]),
            vec!["recent".into()],
        ));
        assert!(state.reopen_from_disk);
        assert!(!state.clean_start);
        assert!(
            state.graph.nodes.is_empty(),
            "no discarded graph data stored"
        );
        assert!(!state.is_dirty);
        assert_eq!(state.recent_projects, vec!["recent".to_string()]);
        assert_eq!(state.api_keys.get("key").map(String::as_str), Some("v"));
        assert_eq!(
            plan(&state),
            SessionRestorePlan::ReopenFromDisk {
                project_path: dir.to_string_lossy().into_owned()
            }
        );
        assert_eq!(marker_of(&restored_graph(&state).unwrap()), 1.0);
    }

    // 2 ──────────────────────────────────────────────────────────────────
    #[test]
    fn discarding_with_a_custom_node_tab_active_keeps_no_project_edits() {
        let tmp = TempDir::new("discard_custom_active");
        let dir = saved_project(&tmp.0, "Saved", 1.0);
        let node = tmp.0.join("Mixer.pwnode");
        custom_node(&node);

        let mut documents = DocumentManager::new();
        {
            let project = documents.active_mut();
            project.set_path(Some(dir.clone())).unwrap();
            project.store_graph(&graph_marked(9.0)); // unsaved edits
            project.is_dirty = true;
        }
        let custom = documents.open_custom_node(node.clone()).unwrap();
        assert!(documents.activate(custom));
        assert_eq!(documents.active().kind, DocumentKind::CustomNode);

        let state = round_trip(&discard_session_state(
            &documents,
            live(Graph::new(), Some(&node), true),
            HashMap::new(),
            Vec::new(),
        ));
        assert!(state.reopen_from_disk);
        assert!(state.graph.nodes.is_empty());
        assert_eq!(
            state.project_path.as_deref(),
            Some(dir.to_string_lossy().as_ref())
        );
        assert_eq!(marker_of(&restored_graph(&state).unwrap()), 1.0);
    }

    #[test]
    fn discard_prefers_a_saved_project_over_an_untitled_one() {
        let tmp = TempDir::new("discard_where_possible");
        let dir = saved_project(&tmp.0, "Saved", 1.0);
        let mut documents = DocumentManager::new();
        documents.active_mut().set_path(Some(dir.clone())).unwrap();
        let untitled = documents.new_document("Untitled");
        assert!(documents.activate(untitled));
        // Active: untitled project with unsaved work.
        let state = discard_session_state(
            &documents,
            live(graph_marked(5.0), None, true),
            HashMap::new(),
            Vec::new(),
        );
        assert!(state.reopen_from_disk);
        assert_eq!(
            state.project_path.as_deref(),
            Some(dir.to_string_lossy().as_ref())
        );
        assert!(state.graph.nodes.is_empty());
    }

    // 3 ──────────────────────────────────────────────────────────────────
    #[test]
    fn discarding_an_unsaved_untitled_project_starts_clean() {
        let documents = DocumentManager::new();
        let state = round_trip(&discard_session_state(
            &documents,
            live(graph_marked(5.0), None, true),
            HashMap::new(),
            vec!["recent".into()],
        ));
        assert!(state.clean_start);
        assert!(!state.reopen_from_disk);
        assert!(state.graph.nodes.is_empty());
        assert_eq!(state.recent_projects, vec!["recent".to_string()]);
        assert_eq!(plan(&state), SessionRestorePlan::Clean { reason: None });
        assert!(restored_graph(&state).is_none());
    }

    // 4 ──────────────────────────────────────────────────────────────────
    #[test]
    fn discarding_never_modifies_project_or_custom_node_files() {
        let tmp = TempDir::new("discard_no_writes");
        let dir = saved_project(&tmp.0, "Saved", 1.0);
        let node = tmp.0.join("Mixer.pwnode");
        custom_node(&node);
        let watched = [dir.join("project.json"), node.clone()];
        let stamp = |paths: &[PathBuf]| -> Vec<(Vec<u8>, std::time::SystemTime)> {
            paths
                .iter()
                .map(|p| {
                    (
                        std::fs::read(p).unwrap(),
                        std::fs::metadata(p).unwrap().modified().unwrap(),
                    )
                })
                .collect()
        };
        let before = stamp(&watched);

        let mut documents = DocumentManager::new();
        documents.active_mut().set_path(Some(dir.clone())).unwrap();
        let custom = documents.open_custom_node(node.clone()).unwrap();
        documents.activate(custom);
        let state = discard_session_state(
            &documents,
            live(graph_marked(9.0), Some(&node), true),
            HashMap::new(),
            Vec::new(),
        );
        let session_file = tmp.0.join("last_session.json");
        std::fs::write(&session_file, serde_json::to_string(&state).unwrap()).unwrap();
        let reread: SessionState =
            serde_json::from_str(&std::fs::read_to_string(&session_file).unwrap()).unwrap();
        let _ = restored_graph(&reread);

        assert_eq!(stamp(&watched), before);
    }

    // 5 ──────────────────────────────────────────────────────────────────
    #[test]
    fn legacy_session_files_restore_as_before() {
        let tmp = TempDir::new("legacy");
        let dir = saved_project(&tmp.0, "Saved", 1.0);
        // Older file: no clean_start / reopen_from_disk, unsaved graph kept.
        let legacy = serde_json::json!({
            "graph": serde_json::to_value(graph_marked(7.0)).unwrap(),
            "canvas_offset": [0.0, 0.0],
            "canvas_zoom": 1.0,
            "pinned_nodes": [],
            "project_path": dir.to_string_lossy(),
            "is_dirty": true
        })
        .to_string();
        let state: SessionState = serde_json::from_str(&legacy).unwrap();
        assert!(!state.reopen_from_disk && !state.clean_start);
        assert!(matches!(plan(&state), SessionRestorePlan::Project { .. }));
        assert_eq!(
            marker_of(&restored_graph(&state).unwrap()),
            7.0,
            "a normal (non-discard) session still restores its graph"
        );
    }

    // 6 ──────────────────────────────────────────────────────────────────
    #[test]
    fn missing_or_corrupt_saved_project_never_resurrects_discarded_data() {
        let tmp = TempDir::new("discard_missing");
        // A discard session that (wrongly or from tampering) carries a graph.
        let mut state = session_state_from(
            Some(live(graph_marked(9.0), None, true)),
            HashMap::new(),
            Vec::new(),
        );
        state.reopen_from_disk = true;

        // Project folder gone.
        state.project_path = Some(tmp.0.join("Gone").to_string_lossy().into_owned());
        match plan(&state) {
            SessionRestorePlan::Clean {
                reason: Some(reason),
            } => {
                assert!(reason.contains("could not be reopened"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        assert!(restored_graph(&state).is_none());

        // Folder without project.json.
        let empty = tmp.0.join("Empty");
        std::fs::create_dir_all(&empty).unwrap();
        state.project_path = Some(empty.to_string_lossy().into_owned());
        assert!(matches!(plan(&state), SessionRestorePlan::Clean { .. }));

        // A custom-node path.
        state.project_path = Some(tmp.0.join("X.pwnode").to_string_lossy().into_owned());
        assert!(matches!(plan(&state), SessionRestorePlan::Clean { .. }));

        // No path at all.
        state.project_path = None;
        assert!(matches!(plan(&state), SessionRestorePlan::Clean { .. }));

        // Corrupt project.json: the plan reopens from disk, reading fails,
        // and restore starts clean (never uses the session graph).
        let corrupt = tmp.0.join("Corrupt");
        std::fs::create_dir_all(&corrupt).unwrap();
        std::fs::write(corrupt.join("project.json"), "{ not json").unwrap();
        state.project_path = Some(corrupt.to_string_lossy().into_owned());
        assert!(matches!(
            plan(&state),
            SessionRestorePlan::ReopenFromDisk { .. }
        ));
        assert!(read_project_file(&corrupt).is_err());
        assert!(restored_graph(&state).is_none());
    }

    // 7 ──────────────────────────────────────────────────────────────────
    #[test]
    fn quit_dialog_lists_every_unsaved_document_with_its_type() {
        let tmp = TempDir::new("quit_list");
        let node = tmp.0.join("Mixer.pwnode");
        custom_node(&node);
        let mut documents = DocumentManager::new(); // id 1 "Untitled", active
        documents.active_mut().title = "Song".into();
        let clean = documents.new_document("Clean");
        let custom = documents.open_custom_node(node).unwrap();
        documents
            .documents
            .iter_mut()
            .find(|d| d.id == custom)
            .unwrap()
            .is_dirty = true;
        let _ = clean;

        // Active project dirty: it is saved next.
        let list = unsaved_documents(&documents, true);
        let shown: Vec<_> = list
            .iter()
            .map(|d| (d.title.as_str(), d.kind_label(), d.next))
            .collect();
        assert_eq!(
            shown,
            vec![("Song", "Project", true), ("Mixer", "Custom Node", false)]
        );

        // Active project clean: the first unsaved tab is next.
        let list = unsaved_documents(&documents, false);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Mixer");
        assert_eq!(list[0].kind, DocumentKind::CustomNode);
        assert!(list[0].next);

        // A stale flag on the active document is ignored (live state wins).
        documents.active_mut().is_dirty = true;
        assert_eq!(unsaved_documents(&documents, false).len(), 1);
    }

    // 8 ──────────────────────────────────────────────────────────────────
    #[test]
    fn cancel_keeps_every_document_and_its_changes() {
        let mut documents = DocumentManager::new();
        let other = documents.new_document("Other");
        documents
            .documents
            .iter_mut()
            .find(|d| d.id == other)
            .unwrap()
            .is_dirty = true;
        let before = unsaved_documents(&documents, true);

        assert_eq!(quit_step(QuitChoice::Cancel, None, None), QuitStep::Dismiss);
        // Cancel never saves, closes or discards anything.
        assert_eq!(unsaved_documents(&documents, true), before);
        assert_eq!(documents.documents.len(), 2);
        assert!(documents.documents.iter().any(|d| d.is_dirty));
    }

    // 9 ──────────────────────────────────────────────────────────────────
    #[test]
    fn save_and_continue_keeps_the_tab_by_tab_behaviour() {
        // Save failed or was cancelled: stay in the dialog.
        assert_eq!(
            quit_step(QuitChoice::SaveAndContinue, Some(false), Some(2)),
            QuitStep::KeepOpen
        );
        // Saved, more unsaved tabs: move to the next one.
        assert_eq!(
            quit_step(QuitChoice::SaveAndContinue, Some(true), Some(2)),
            QuitStep::SwitchTo(2)
        );
        // Active had nothing to save: move on.
        assert_eq!(
            quit_step(QuitChoice::SaveAndContinue, None, Some(3)),
            QuitStep::SwitchTo(3)
        );
        // Everything saved: close normally (not a discard).
        assert_eq!(
            quit_step(QuitChoice::SaveAndContinue, Some(true), None),
            QuitStep::Close { discard: false }
        );
        // Discard All closes with the discard decision.
        assert_eq!(
            quit_step(QuitChoice::DiscardAll, None, Some(2)),
            QuitStep::Close { discard: true }
        );
    }
}

/// P4.2 Stage 2: package references through project save/open, undo and
/// tab switching.
#[cfg(test)]
mod package_reference_tests {
    use super::super::undo::{UndoHistory, UndoSnapshot};
    use super::*;
    use crate::document::{CustomNodeFile, CustomNodePort, DocumentManager};
    use crate::library::package::publish_package;
    use crate::library::package::tests::{TempDir, request};
    use crate::library::resolve::InstancePackageRef;
    use std::collections::HashSet;
    use std::path::PathBuf;

    /// Publish a one-port custom node; returns (library, reference, entry).
    fn published(tmp: &TempDir) -> (PathBuf, InstancePackageRef, PathBuf) {
        let library = tmp.0.join("library");
        let src = tmp.0.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let source = src.join("Gain.pwnode");
        CustomNodeFile {
            version: 1,
            name: "Gain".into(),
            graph: Graph::new(),
            inputs: Vec::new(),
            outputs: vec![CustomNodePort {
                name: "out".into(),
                kind: "Number".into(),
            }],
        }
        .save(&source)
        .unwrap();
        let version = publish_package(&library, &source, &request("Gain", "1.0.0")).unwrap();
        let reference = InstancePackageRef {
            id: version.manifest.id.clone(),
            version: version.version.to_string(),
            entry: version.manifest.entry.clone(),
        };
        (library, reference, version.entry_path)
    }

    fn packaged_graph(file: &str, reference: &InstancePackageRef) -> (Graph, NodeId) {
        let mut graph = Graph::new();
        let id = graph.add_node(
            NodeType::CustomNodeInstance {
                file: file.to_string(),
                inputs: Vec::new(),
                outputs: vec![CustomNodePort {
                    name: "out".into(),
                    kind: "Number".into(),
                }],
                package: Some(reference.clone()),
            },
            [0.0, 0.0],
        );
        (graph, id)
    }

    fn package_of(graph: &Graph, id: NodeId) -> Option<InstancePackageRef> {
        match &graph.nodes.get(&id)?.node_type {
            NodeType::CustomNodeInstance { package, .. } => package.clone(),
            _ => None,
        }
    }

    fn file_of(graph: &Graph, id: NodeId) -> String {
        match &graph.nodes[&id].node_type {
            NodeType::CustomNodeInstance { file, .. } => file.clone(),
            _ => unreachable!(),
        }
    }

    /// Evaluate like the editor does and report whether `id` loaded and
    /// whether evaluation produced interface changes (which mark a
    /// document modified).
    fn evaluate(
        graph: &mut Graph,
        project: Option<PathBuf>,
        library: &Path,
        id: NodeId,
    ) -> (bool, bool) {
        graph.set_package_environment(project, Some(library.to_path_buf()));
        graph.evaluate(0.0);
        let changed = graph.take_custom_port_events().changed;
        (
            graph
                .custom_instance_source(id)
                .is_some_and(|s| Path::new(&s).is_file()),
            changed,
        )
    }

    fn write_project(dir: &Path, graph: &Graph) {
        let mut saved = graph.clone();
        relativize_paths(&mut saved, &dir.to_string_lossy());
        let pf = ProjectFile {
            graph: saved,
            pinned_nodes: Vec::new(),
            canvas_offset: [0.0, 0.0],
            canvas_zoom: 1.0,
        };
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("project.json"),
            serde_json::to_string_pretty(&pf).unwrap(),
        )
        .unwrap();
    }

    fn open_project(dir: &Path) -> Graph {
        let mut pf = read_project_file(dir).unwrap();
        absolutize_paths(&mut pf.graph, &dir.to_string_lossy());
        pf.graph
    }

    #[test]
    fn project_save_and_open_preserve_package_references_without_rewriting() {
        let tmp = TempDir::new("s2_project");
        let (library, reference, entry) = published(&tmp);
        let project = tmp.0.join("project");
        let (graph, id) = packaged_graph(&entry.to_string_lossy(), &reference);
        write_project(&project, &graph);

        let json_path = project.join("project.json");
        let bytes = std::fs::read(&json_path).unwrap();
        let modified = std::fs::metadata(&json_path).unwrap().modified().unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("\"package\""));

        let mut opened = open_project(&project);
        assert_eq!(package_of(&opened, id), Some(reference.clone()));
        assert_eq!(PathBuf::from(file_of(&opened, id)), entry);
        let (loaded, changed) = evaluate(&mut opened, Some(project.clone()), &library, id);
        assert!(loaded);
        assert!(!changed, "opening does not mark the project modified");

        // Opening and evaluating never wrote the project file.
        assert_eq!(std::fs::read(&json_path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&json_path).unwrap().modified().unwrap(),
            modified
        );

        // Saving again keeps the reference.
        write_project(&project, &opened);
        let reopened = open_project(&project);
        assert_eq!(package_of(&reopened, id), Some(reference));
    }

    #[test]
    fn project_from_another_machine_resolves_by_package_reference() {
        let tmp = TempDir::new("s2_moved");
        let (library, reference, _) = published(&tmp);
        let project = tmp.0.join("project");
        let foreign = tmp.0.join("old-machine").join("Gain.pwnode");
        let (graph, id) = packaged_graph(&foreign.to_string_lossy(), &reference);
        write_project(&project, &graph);

        let mut opened = open_project(&project);
        let (loaded, changed) = evaluate(&mut opened, Some(project.clone()), &library, id);
        assert!(loaded, "resolved from the Library by id@version/entry");
        assert!(!changed);
        // The compatibility path is not rewritten by opening.
        assert_eq!(PathBuf::from(file_of(&opened, id)), foreign);
    }

    #[test]
    fn undo_and_redo_keep_package_references() {
        let tmp = TempDir::new("s2_undo");
        let (library, reference, entry) = published(&tmp);
        let (graph, id) = packaged_graph(&entry.to_string_lossy(), &reference);

        let mut history = UndoHistory::new(10);
        history.push(UndoSnapshot::new(graph.clone(), HashSet::new()));
        let mut edited = graph.clone();
        edited.remove_node(id);
        let undone = history
            .undo(UndoSnapshot::new(edited.clone(), HashSet::new()))
            .expect("undo");
        assert_eq!(package_of(&undone.graph, id), Some(reference.clone()));
        let redone = history
            .redo(UndoSnapshot::new(undone.graph.clone(), HashSet::new()))
            .expect("redo");
        assert!(!redone.graph.nodes.contains_key(&id));

        // A snapshot that differs only in its package reference is a
        // different state (not deduplicated).
        let mut unpackaged = graph.clone();
        if let Some(node) = unpackaged.nodes.get_mut(&id) {
            if let NodeType::CustomNodeInstance { package, .. } = &mut node.node_type {
                *package = None;
            }
        }
        assert!(
            !UndoSnapshot::new(unpackaged, HashSet::new())
                .has_same_content(&UndoSnapshot::new(graph.clone(), HashSet::new()))
        );

        // The restored graph still resolves and evaluates.
        let mut restored = undone.graph;
        assert!(evaluate(&mut restored, None, &library, id).0);
    }

    #[test]
    fn tab_switching_keeps_package_references() {
        let tmp = TempDir::new("s2_tabs");
        let (library, reference, entry) = published(&tmp);
        let (graph, id) = packaged_graph(&entry.to_string_lossy(), &reference);

        let mut documents = DocumentManager::new();
        let first = documents.active().id;
        assert!(documents.active_mut().store_graph(&graph));
        let second = documents.new_document("Second");
        assert!(documents.activate(second));
        assert!(documents.active().graph.nodes.is_empty());
        assert!(documents.activate(first));

        let mut restored = documents.active().graph.clone();
        assert_eq!(package_of(&restored, id), Some(reference));
        let (loaded, changed) = evaluate(&mut restored, None, &library, id);
        assert!(loaded);
        assert!(!changed);
    }
}

/// P4.2 Stage 4: portable project bundling (save, move, reopen).
#[cfg(test)]
mod project_bundle_tests {
    use super::*;
    use crate::document::CustomNodeFile;
    use crate::library::bundle::tests::{child, composite, files_under, leaf, publish, stored};
    use crate::library::package::tests::TempDir;
    use crate::library::package::{PACKAGES_DIR, VerifiedPackage};
    use crate::library::project::{
        FAIL_AT, FailPoint, SaveReport, commit_project_save, plan_project_packages,
    };
    use crate::library::resolve::InstancePackageRef;
    use std::path::PathBuf;

    /// Save like `save_to_dir` does: plan, relativize, point packaged
    /// instances at their project copies, commit atomically.
    fn save(
        graph: &Graph,
        dest: &Path,
        source: Option<&Path>,
        library: &Path,
    ) -> Result<SaveReport, String> {
        let plan = plan_project_packages(graph, dest, source, Some(library))?;
        let mut saved = graph.clone();
        relativize_paths(&mut saved, &dest.to_string_lossy());
        apply_project_package_paths(&mut saved, &plan);
        let pf = ProjectFile {
            graph: saved,
            pinned_nodes: Vec::new(),
            canvas_offset: [0.0, 0.0],
            canvas_zoom: 1.0,
        };
        commit_project_save(dest, &plan, &serde_json::to_string_pretty(&pf).unwrap())
    }

    fn open(dest: &Path) -> Graph {
        let mut pf = read_project_file(dest).unwrap();
        absolutize_paths(&mut pf.graph, &dest.to_string_lossy());
        pf.graph
    }

    /// A project graph with one instance per child, each feeding a Slider
    /// (so connections can be checked). Returns the instance ids.
    fn project_graph(children: &[(String, Option<InstancePackageRef>)]) -> (Graph, Vec<NodeId>) {
        let mut graph = Graph::new();
        let mut ids = Vec::new();
        for (index, (file, package)) in children.iter().enumerate() {
            let outputs = if file.is_empty() {
                vec![crate::document::CustomNodePort {
                    name: "out".into(),
                    kind: "Number".into(),
                }]
            } else {
                CustomNodeFile::load(Path::new(file)).unwrap().outputs
            };
            let id = graph.add_node(
                NodeType::CustomNodeInstance {
                    file: file.clone(),
                    inputs: Vec::new(),
                    outputs,
                    package: package.clone(),
                },
                [0.0, index as f32 * 100.0],
            );
            let slider = graph.add_node(
                NodeType::Slider {
                    value: 0.0,
                    min: 0.0,
                    max: 1.0,
                    step: 0.01,
                    slider_color: [0, 0, 0],
                    label: String::new(),
                },
                [300.0, index as f32 * 100.0],
            );
            graph.add_connection(id, 0, slider, 0);
            ids.push(id);
        }
        (graph, ids)
    }

    fn outputs(
        graph: &mut Graph,
        ids: &[NodeId],
        project: &Path,
        library: &Path,
    ) -> Vec<Option<f32>> {
        graph.set_package_environment(Some(project.to_path_buf()), Some(library.to_path_buf()));
        let values = graph.evaluate(0.0);
        ids.iter()
            .map(|id| match values.get(&(*id, 0)) {
                Some(PortValue::Float(v)) => Some(*v),
                _ => None,
            })
            .collect()
    }

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        files_under(dir)
            .into_iter()
            .map(|rel| {
                let bytes = std::fs::read(dir.join(&rel)).unwrap();
                (rel, bytes)
            })
            .collect()
    }

    fn copy_dir(from: &Path, to: &Path) {
        for rel in files_under(from) {
            let target = to.join(&rel);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(from.join(&rel), target).unwrap();
        }
    }

    fn package_ref(graph: &Graph, id: NodeId) -> Option<InstancePackageRef> {
        match &graph.nodes[&id].node_type {
            NodeType::CustomNodeInstance { package, .. } => package.clone(),
            _ => None,
        }
    }

    struct Fixture {
        tmp: TempDir,
        library: PathBuf,
        project: PathBuf,
    }

    fn fixture(tag: &str) -> Fixture {
        let tmp = TempDir::new(tag);
        let library = tmp.0.join("library");
        let project = tmp.0.join("project");
        std::fs::create_dir_all(&library).unwrap();
        Fixture {
            library,
            project,
            tmp,
        }
    }

    fn src(f: &Fixture) -> PathBuf {
        f.tmp.0.join("src")
    }

    fn pkg_dir(project: &Path, v: &VerifiedPackage) -> PathBuf {
        project
            .join(PACKAGES_DIR)
            .join(&v.manifest.id)
            .join(v.version.to_string())
    }

    // ── Saving ──────────────────────────────────────────────────────

    #[test]
    fn saves_a_project_with_one_package_portably() {
        let f = fixture("s4_one");
        let b = publish(&f.library, &leaf(&src(&f), "B", 0.25), "B", None, "1.0.0").unwrap();
        let library_bytes = snapshot(&b.dir);
        let (graph, ids) = project_graph(&[child(&b)]);
        let report = save(&graph, &f.project, None, &f.library).unwrap();
        assert_eq!(
            report,
            SaveReport {
                copied: 1,
                reused: 0
            }
        );

        // Exact copy, manifests and checksums preserved; source untouched.
        assert_eq!(snapshot(&pkg_dir(&f.project, &b)), library_bytes);
        assert_eq!(snapshot(&b.dir), library_bytes);

        // Relative compatibility path, no machine path, reference kept.
        let json = std::fs::read_to_string(f.project.join("project.json")).unwrap();
        assert!(
            json.contains("\"file\": \"packages/local.b/1.0.0/B.pwnode\""),
            "{json}"
        );
        assert!(!json.contains(&*f.library.to_string_lossy().replace('\\', "\\\\")));
        let reopened = open(&f.project);
        assert_eq!(package_ref(&reopened, ids[0]), Some(stored(&b)));
        assert_eq!(
            PathBuf::from(file_of(&reopened, ids[0])),
            f.project.join("packages/local.b/1.0.0/B.pwnode")
        );
    }

    fn file_of(graph: &Graph, id: NodeId) -> String {
        match &graph.nodes[&id].node_type {
            NodeType::CustomNodeInstance { file, .. } => file.clone(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn saves_nested_dependencies_and_multiple_versions() {
        let f = fixture("s4_nested");
        let s = src(&f);
        let c1 = publish(
            &f.library,
            &leaf(&s.join("v1"), "C", 0.25),
            "C",
            None,
            "1.0.0",
        )
        .unwrap();
        let c2 = publish(
            &f.library,
            &leaf(&s.join("v2"), "C", 0.75),
            "C",
            Some(&c1.manifest.id),
            "2.0.0",
        )
        .unwrap();
        let b = publish(
            &f.library,
            &composite(&s, "B", &[child(&c1)]),
            "B",
            None,
            "1.0.0",
        )
        .unwrap();
        let a = publish(
            &f.library,
            &composite(&s, "A", &[child(&b)]),
            "A",
            None,
            "1.0.0",
        )
        .unwrap();
        let (graph, _) = project_graph(&[child(&a), child(&c1), child(&c2)]);
        let report = save(&graph, &f.project, None, &f.library).unwrap();
        // A (with its complete bundle), C@1 and C@2 at the top level.
        assert_eq!(report.copied, 3);
        let a_copy = pkg_dir(&f.project, &a);
        assert!(
            a_copy
                .join("deps")
                .join(&b.manifest.id)
                .join("1.0.0")
                .is_dir()
        );
        assert!(
            a_copy
                .join("deps")
                .join(&c1.manifest.id)
                .join("1.0.0")
                .is_dir()
        );
        assert!(pkg_dir(&f.project, &c1).is_dir() && pkg_dir(&f.project, &c2).is_dir());
        assert!(crate::library::package::verify_version_dir(&a_copy, true).is_ok());
        assert!(
            !pkg_dir(&f.project, &b).exists(),
            "B travels inside A's bundle"
        );
    }

    #[test]
    fn packages_used_inside_local_custom_nodes_are_bundled() {
        // P2 grouping writes local .pwnode files into the project folder;
        // packages used inside them must travel too (the files are not
        // copied: they are already in the project).
        let f = fixture("s4_grouped");
        let b = publish(&f.library, &leaf(&src(&f), "B", 0.5), "B", None, "1.0.0").unwrap();
        std::fs::create_dir_all(&f.project).unwrap();
        let group = composite(&f.project, "Group", &[child(&b)]);
        let (graph, ids) = project_graph(&[(group.to_string_lossy().into_owned(), None)]);
        let plan = plan_project_packages(&graph, &f.project, None, Some(&f.library)).unwrap();
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        save(&graph, &f.project, None, &f.library).unwrap();
        assert!(pkg_dir(&f.project, &b).is_dir());

        // Works from the project alone.
        let moved = f.tmp.0.join("moved");
        copy_dir(&f.project, &moved);
        let empty = f.tmp.0.join("empty-library");
        std::fs::create_dir_all(&empty).unwrap();
        let mut reopened = open(&moved);
        assert_eq!(
            outputs(&mut reopened, &ids, &moved, &empty),
            vec![Some(0.5)]
        );
    }

    #[test]
    fn saving_twice_does_not_copy_again() {
        let f = fixture("s4_twice");
        let b = publish(&f.library, &leaf(&src(&f), "B", 0.25), "B", None, "1.0.0").unwrap();
        let (graph, _) = project_graph(&[child(&b)]);
        save(&graph, &f.project, None, &f.library).unwrap();
        let entry = pkg_dir(&f.project, &b).join("B.pwnode");
        let modified = std::fs::metadata(&entry).unwrap().modified().unwrap();
        let report = save(&graph, &f.project, None, &f.library).unwrap();
        assert_eq!(
            report,
            SaveReport {
                copied: 0,
                reused: 1
            }
        );
        assert_eq!(
            std::fs::metadata(&entry).unwrap().modified().unwrap(),
            modified
        );
        let names: Vec<String> = std::fs::read_dir(&f.project)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|n| n.starts_with(".patchwork-staging-")),
            "{names:?}"
        );
    }

    #[test]
    fn moved_project_runs_with_an_empty_library_and_identical_output() {
        let f = fixture("s4_moved");
        let s = src(&f);
        let c1 = publish(
            &f.library,
            &leaf(&s.join("v1"), "C", 0.125),
            "C",
            None,
            "1.0.0",
        )
        .unwrap();
        let c2 = publish(
            &f.library,
            &leaf(&s.join("v2"), "C", 0.875),
            "C",
            Some(&c1.manifest.id),
            "2.0.0",
        )
        .unwrap();
        let b = publish(
            &f.library,
            &composite(&s, "B", &[child(&c1)]),
            "B",
            None,
            "1.0.0",
        )
        .unwrap();
        let a = publish(
            &f.library,
            &composite(&s, "A", &[child(&b)]),
            "A",
            None,
            "1.0.0",
        )
        .unwrap();
        let (mut graph, ids) = project_graph(&[child(&a), child(&c2), child(&c1)]);
        let before = outputs(&mut graph, &ids, &f.tmp.0.join("not-saved"), &f.library);
        assert_eq!(before, vec![Some(0.125), Some(0.875), Some(0.125)]);
        save(&graph, &f.project, None, &f.library).unwrap();

        // Another computer: different folder, empty Library, old paths gone.
        let moved = f.tmp.0.join("elsewhere").join("Moved Project");
        copy_dir(&f.project, &moved);
        std::fs::remove_dir_all(&f.library).unwrap();
        std::fs::remove_dir_all(&f.project).unwrap();
        let empty = f.tmp.0.join("empty-library");
        std::fs::create_dir_all(&empty).unwrap();
        let json_before = std::fs::read(moved.join("project.json")).unwrap();

        let mut reopened = open(&moved);
        assert_eq!(outputs(&mut reopened, &ids, &moved, &empty), before);
        assert!(
            !reopened.take_custom_port_events().changed,
            "not marked modified"
        );
        assert_eq!(
            std::fs::read(moved.join("project.json")).unwrap(),
            json_before
        );

        // Saving it again there needs nothing from a Library.
        let report = save(&reopened, &moved, None, &empty).unwrap();
        assert_eq!(report.copied, 0);
    }

    // ── Failures leave the project intact ──────────────────────────

    #[test]
    fn missing_or_corrupted_package_leaves_the_saved_project_intact() {
        let f = fixture("s4_fail");
        let s = src(&f);
        let b = publish(&f.library, &leaf(&s, "B", 0.25), "B", None, "1.0.0").unwrap();
        let (graph, _) = project_graph(&[child(&b)]);
        save(&graph, &f.project, None, &f.library).unwrap();
        let saved = snapshot(&f.project);

        // A package installed nowhere.
        let missing = InstancePackageRef {
            id: "local.missing".into(),
            version: "1.0.0".into(),
            entry: "Missing.pwnode".into(),
        };
        let (graph2, _) = project_graph(&[child(&b), (String::new(), Some(missing))]);
        let error = save(&graph2, &f.project, None, &f.library).unwrap_err();
        assert!(
            error.contains("local.missing@1.0.0") && error.contains("not saved"),
            "{error}"
        );
        assert_eq!(snapshot(&f.project), saved);

        // A damaged Library package.
        let c = publish(&f.library, &leaf(&s, "C", 0.5), "C", None, "1.0.0").unwrap();
        std::fs::write(&c.entry_path, "{ damaged").unwrap();
        let (graph3, _) = project_graph(&[child(&b), (String::new(), Some(stored(&c)))]);
        let error = save(&graph3, &f.project, None, &f.library).unwrap_err();
        assert!(error.contains("damaged"), "{error}");
        assert_eq!(snapshot(&f.project), saved);
    }

    #[test]
    fn existing_project_copies_are_never_overwritten() {
        let f = fixture("s4_conflict");
        let s = src(&f);
        // The project holds local.x@1 (0.25); this Library has a different
        // local.x@1 (0.75), as on another machine.
        let other_library = f.tmp.0.join("other-library");
        let x_project = publish(
            &other_library,
            &leaf(&s.join("p"), "X", 0.25),
            "X",
            None,
            "1.0.0",
        )
        .unwrap();
        let x_library = publish(
            &f.library,
            &leaf(&s.join("l"), "X", 0.75),
            "X",
            None,
            "1.0.0",
        )
        .unwrap();
        copy_dir(&x_project.dir, &pkg_dir(&f.project, &x_project));
        let project_copy = snapshot(&pkg_dir(&f.project, &x_project));

        // The project copy has priority: reused, never replaced.
        let (mut graph, ids) = project_graph(&[child(&x_library)]);
        let report = save(&graph, &f.project, None, &f.library).unwrap();
        assert_eq!(
            report,
            SaveReport {
                copied: 0,
                reused: 1
            }
        );
        assert_eq!(snapshot(&pkg_dir(&f.project, &x_project)), project_copy);
        let mut reopened = open(&f.project);
        assert_eq!(
            outputs(&mut reopened, &ids, &f.project, &f.library),
            vec![Some(0.25)]
        );
        let _ = outputs(&mut graph, &ids, &f.project, &f.library);

        // A damaged project copy stops the save; it is not repaired from
        // the Library behind the user's back.
        let copy_entry = pkg_dir(&f.project, &x_project).join("X.pwnode");
        std::fs::write(&copy_entry, "{ damaged").unwrap();
        let damaged = snapshot(&f.project);
        let error = save(&graph, &f.project, None, &f.library).unwrap_err();
        assert!(error.contains("project copy"), "{error}");
        assert_eq!(snapshot(&f.project), damaged);
    }

    #[test]
    fn a_copy_appearing_during_the_save_is_not_overwritten() {
        let f = fixture("s4_race");
        let s = src(&f);
        let b = publish(&f.library, &leaf(&s, "B", 0.25), "B", None, "1.0.0").unwrap();
        let (graph, _) = project_graph(&[child(&b)]);
        std::fs::create_dir_all(&f.project).unwrap();
        std::fs::write(
            f.project.join("project.json"),
            "{\"graph\":{\"nodes\":{},\"connections\":[],\"next_id\":1}}",
        )
        .unwrap();
        let plan = plan_project_packages(&graph, &f.project, None, Some(&f.library)).unwrap();

        // Another writer adds a different local.b@1.0.0 before the commit.
        let other = f.tmp.0.join("other");
        let rival = publish(
            &other,
            &leaf(&s.join("rival"), "B", 0.9),
            "B",
            None,
            "1.0.0",
        )
        .unwrap();
        copy_dir(&rival.dir, &pkg_dir(&f.project, &b));
        let before = snapshot(&f.project);
        let error = commit_project_save(&f.project, &plan, "{}").unwrap_err();
        assert!(error.contains("not overwritten"), "{error}");
        assert_eq!(snapshot(&f.project), before);
    }

    #[test]
    fn conflicting_content_for_one_version_is_refused() {
        let f = fixture("s4_content");
        let s = src(&f);
        let lib1 = f.tmp.0.join("lib1");
        let c1 = publish(&lib1, &leaf(&s.join("c1"), "C", 0.5), "C", None, "1.0.0").unwrap();
        let a = publish(
            &lib1,
            &composite(&s, "A", &[child(&c1)]),
            "A",
            None,
            "1.0.0",
        )
        .unwrap();
        // This Library: A (bundling the first C@1) and a different C@1.
        copy_dir(
            &a.dir,
            &f.library
                .join(PACKAGES_DIR)
                .join(&a.manifest.id)
                .join("1.0.0"),
        );
        let c_other = publish(
            &f.library,
            &leaf(&s.join("c2"), "C", 0.9),
            "C",
            None,
            "1.0.0",
        )
        .unwrap();
        let (graph, _) = project_graph(&[
            (String::new(), Some(stored(&a))),
            (String::new(), Some(stored(&c_other))),
        ]);
        let error = save(&graph, &f.project, None, &f.library).unwrap_err();
        assert!(error.contains("different content"), "{error}");
        assert!(!f.project.join("project.json").exists());
    }

    #[test]
    fn interrupted_saves_leave_the_previous_project_untouched() {
        let f = fixture("s4_interrupt");
        let s = src(&f);
        let b = publish(&f.library, &leaf(&s, "B", 0.25), "B", None, "1.0.0").unwrap();
        let (graph, ids) = project_graph(&[child(&b)]);
        save(&graph, &f.project, None, &f.library).unwrap();
        let saved = snapshot(&f.project);

        let c = publish(&f.library, &leaf(&s, "C", 0.5), "C", None, "1.0.0").unwrap();
        let d = publish(&f.library, &leaf(&s, "D", 0.75), "D", None, "1.0.0").unwrap();
        let (bigger, _) = project_graph(&[child(&b), child(&c), child(&d)]);
        for point in [FailPoint::AfterStaging, FailPoint::AfterFirstPackage] {
            FAIL_AT.with(|cell| cell.set(Some(point)));
            let result = save(&bigger, &f.project, None, &f.library);
            FAIL_AT.with(|cell| cell.set(None));
            assert!(result.is_err(), "{point:?}");
            assert_eq!(snapshot(&f.project), saved, "{point:?}: nothing changed");
        }

        // A crash mid-staging leaves only a staging folder behind: the
        // project still opens unchanged, and the next save cleans it up.
        let stale = f.project.join(".patchwork-staging-1-1");
        std::fs::create_dir_all(stale.join("packages").join("half")).unwrap();
        std::fs::write(stale.join("project.json"), "{ partial").unwrap();
        let mut reopened = open(&f.project);
        assert_eq!(
            outputs(&mut reopened, &ids, &f.project, &f.library),
            vec![Some(0.25)]
        );
        save(&bigger, &f.project, None, &f.library).unwrap();
        assert!(!stale.exists());
        assert!(pkg_dir(&f.project, &c).is_dir() && pkg_dir(&f.project, &d).is_dir());
    }

    // ── Loading ─────────────────────────────────────────────────────

    #[test]
    fn missing_packages_keep_nodes_ports_and_connections() {
        let f = fixture("s4_missing_on_load");
        let b = publish(&f.library, &leaf(&src(&f), "B", 0.25), "B", None, "1.0.0").unwrap();
        let (graph, ids) = project_graph(&[child(&b)]);
        save(&graph, &f.project, None, &f.library).unwrap();
        std::fs::remove_dir_all(f.project.join(PACKAGES_DIR)).unwrap();
        let empty = f.tmp.0.join("empty-library");
        std::fs::create_dir_all(&empty).unwrap();

        let mut reopened = open(&f.project);
        let nodes = reopened.nodes.len();
        let connections = reopened.connections.len();
        assert_eq!(outputs(&mut reopened, &ids, &f.project, &empty), vec![None]);
        assert_eq!(reopened.nodes.len(), nodes);
        assert_eq!(reopened.connections.len(), connections);
        assert_eq!(package_ref(&reopened, ids[0]), Some(stored(&b)));
        assert!(!reopened.take_custom_port_events().changed);
        let error = reopened.custom_failure(ids[0]).unwrap();
        assert!(error.contains("not installed"), "{error}");
    }

    #[test]
    fn corrupted_project_copy_does_not_fall_back_to_the_library() {
        let f = fixture("s4_corrupt_load");
        let b = publish(&f.library, &leaf(&src(&f), "B", 0.25), "B", None, "1.0.0").unwrap();
        let (graph, ids) = project_graph(&[child(&b)]);
        save(&graph, &f.project, None, &f.library).unwrap();
        let copy_entry = pkg_dir(&f.project, &b).join("B.pwnode");
        let text = std::fs::read_to_string(&copy_entry)
            .unwrap()
            .replace("0.25", "0.95");
        std::fs::write(&copy_entry, text).unwrap();
        let mut reopened = open(&f.project);
        // The intact Library copy is not used instead.
        assert_eq!(
            outputs(&mut reopened, &ids, &f.project, &f.library),
            vec![None]
        );
    }

    #[test]
    fn old_projects_and_loose_nodes_still_load_and_warn_when_not_portable() {
        let f = fixture("s4_legacy");
        let s = src(&f);
        // An absolute-path project (P3) and a P4.0 unversioned Library node.
        let loose = leaf(&s, "Loose", 0.4);
        let unversioned = leaf(&f.library.join("nodes"), "Unversioned", 0.6);
        let (graph, ids) = project_graph(&[
            (loose.to_string_lossy().into_owned(), None),
            (unversioned.to_string_lossy().into_owned(), None),
        ]);
        std::fs::create_dir_all(&f.project).unwrap();
        let legacy = ProjectFile {
            graph: graph.clone(),
            pinned_nodes: Vec::new(),
            canvas_offset: [0.0, 0.0],
            canvas_zoom: 1.0,
        };
        std::fs::write(
            f.project.join("project.json"),
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .unwrap();
        let mut reopened = open(&f.project);
        assert_eq!(
            outputs(&mut reopened, &ids, &f.project, &f.library),
            vec![Some(0.4), Some(0.6)]
        );

        // Saving keeps their paths, copies no files and warns.
        let plan = plan_project_packages(&reopened, &f.project, None, Some(&f.library)).unwrap();
        assert!(plan.to_copy.is_empty());
        assert_eq!(plan.warnings.len(), 2, "{:?}", plan.warnings);
        assert!(plan.warnings[0].contains("outside the project folder"));
        save(&reopened, &f.project, None, &f.library).unwrap();
        assert!(!f.project.join(PACKAGES_DIR).exists());
        let again = open(&f.project);
        assert_eq!(PathBuf::from(file_of(&again, ids[0])), loose);
    }

    #[test]
    fn save_as_takes_packages_from_the_current_project() {
        let f = fixture("s4_save_as");
        let b = publish(&f.library, &leaf(&src(&f), "B", 0.25), "B", None, "1.0.0").unwrap();
        let (graph, ids) = project_graph(&[child(&b)]);
        save(&graph, &f.project, None, &f.library).unwrap();
        // On a machine without B in the Library: Save As to a new folder.
        let empty = f.tmp.0.join("empty-library");
        std::fs::create_dir_all(&empty).unwrap();
        let opened = open(&f.project);
        let copy = f.tmp.0.join("copy");
        let report = save(&opened, &copy, Some(&f.project), &empty).unwrap();
        assert_eq!(report.copied, 1);
        let mut reopened = open(&copy);
        assert_eq!(
            outputs(&mut reopened, &ids, &copy, &empty),
            vec![Some(0.25)]
        );
    }
}

/// P4.2 Stage 4 consistency: live state after a portable save, and the
/// portability report.
#[cfg(test)]
mod portable_save_sync_tests {
    use super::super::undo::{UndoHistory, UndoSnapshot};
    use super::*;
    use crate::document::{CustomNodeFile, DocumentManager};
    use crate::library::bundle::tests::{child, leaf, publish};
    use crate::library::package::tests::TempDir;
    use crate::library::project::{FAIL_AT, FailPoint, PortabilityReport};
    use crate::library::resolve::InstancePackageRef;
    use std::path::PathBuf;

    struct Env {
        tmp: TempDir,
        library: PathBuf,
        project: PathBuf,
    }

    fn env(tag: &str) -> Env {
        let tmp = TempDir::new(tag);
        let library = tmp.0.join("library");
        std::fs::create_dir_all(&library).unwrap();
        Env {
            library,
            project: tmp.0.join("project"),
            tmp,
        }
    }

    fn add_instance(graph: &mut Graph, file: &str, package: Option<InstancePackageRef>) -> NodeId {
        let outputs = if Path::new(file).is_file() {
            CustomNodeFile::load(Path::new(file)).unwrap().outputs
        } else {
            Vec::new()
        };
        graph.add_node(
            NodeType::CustomNodeInstance {
                file: file.to_string(),
                inputs: Vec::new(),
                outputs,
                package,
            },
            [0.0, 0.0],
        )
    }

    fn slider() -> NodeType {
        NodeType::Slider {
            value: 0.0,
            min: 0.0,
            max: 1.0,
            step: 0.01,
            slider_color: [0, 0, 0],
            label: String::new(),
        }
    }

    fn file_of(graph: &Graph, id: NodeId) -> String {
        match &graph.nodes[&id].node_type {
            NodeType::CustomNodeInstance { file, .. } => file.clone(),
            _ => unreachable!(),
        }
    }

    fn package_of(graph: &Graph, id: NodeId) -> Option<InstancePackageRef> {
        match &graph.nodes[&id].node_type {
            NodeType::CustomNodeInstance { package, .. } => package.clone(),
            _ => None,
        }
    }

    fn open(dir: &Path) -> Graph {
        let mut pf = read_project_file(dir).unwrap();
        absolutize_paths(&mut pf.graph, &dir.to_string_lossy());
        pf.graph
    }

    fn save(
        graph: &mut Graph,
        history: &mut UndoHistory,
        dir: &Path,
        source: Option<&Path>,
        library: &Path,
    ) -> Result<PortableSave, String> {
        save_project_files(
            graph,
            history,
            Vec::new(),
            [0.0, 0.0],
            1.0,
            dir,
            source,
            Some(library),
        )
    }

    fn json(graph: &Graph) -> String {
        serde_json::to_string(graph).unwrap()
    }

    fn snapshot_dir(dir: &Path) -> Vec<(String, Vec<u8>)> {
        crate::library::bundle::tests::files_under(dir)
            .into_iter()
            .map(|rel| {
                let bytes = std::fs::read(dir.join(&rel)).unwrap();
                (rel, bytes)
            })
            .collect()
    }

    #[test]
    fn live_graph_paths_match_the_saved_project_after_success() {
        let e = env("s4b_live");
        let b = publish(
            &e.library,
            &leaf(&e.tmp.0.join("src"), "B", 0.25),
            "B",
            None,
            "1.0.0",
        )
        .unwrap();
        let (file, package) = child(&b);
        let mut graph = Graph::new();
        let id = add_instance(&mut graph, &file, package.clone());
        graph.set_package_environment(None, Some(e.library.clone()));
        graph.evaluate(0.0);
        assert!(graph.has_custom_runtime(id));

        let mut history = UndoHistory::new(10);
        save(&mut graph, &mut history, &e.project, None, &e.library).unwrap();

        let reopened = open(&e.project);
        assert_eq!(file_of(&graph, id), file_of(&reopened, id), "live == saved");
        assert_eq!(
            PathBuf::from(file_of(&graph, id)),
            e.project.join("packages/local.b/1.0.0/B.pwnode")
        );
        assert_eq!(package_of(&graph, id), package, "reference kept");
        assert!(!graph.has_custom_runtime(id), "runtime invalidated");
        // Reloads from the project copy without reporting changes.
        graph.set_package_environment(Some(e.project.clone()), Some(e.library.clone()));
        graph.evaluate(0.0);
        assert!(graph.has_custom_runtime(id));
        assert!(!graph.take_custom_port_events().changed, "not modified");
        assert_eq!(history.depth(), (0, 0), "no undo step added");
    }

    #[test]
    fn failed_saves_leave_the_live_graph_and_history_untouched() {
        let e = env("s4b_failed");
        let b = publish(
            &e.library,
            &leaf(&e.tmp.0.join("src"), "B", 0.25),
            "B",
            None,
            "1.0.0",
        )
        .unwrap();
        let (file, package) = child(&b);
        let mut graph = Graph::new();
        add_instance(&mut graph, &file, package);
        let missing = InstancePackageRef {
            id: "local.missing".into(),
            version: "1.0.0".into(),
            entry: "Missing.pwnode".into(),
        };
        let mut history = UndoHistory::new(10);
        history.push(UndoSnapshot::new(graph.clone(), Default::default()));

        // Planning failure (missing package).
        let mut broken = graph.clone();
        add_instance(&mut broken, "", Some(missing));
        let before = json(&broken);
        assert!(save(&mut broken, &mut history, &e.project, None, &e.library).is_err());
        assert_eq!(json(&broken), before);
        assert!(!e.project.join("project.json").exists());

        // Commit failure (interrupted after staging).
        let before = json(&graph);
        FAIL_AT.with(|f| f.set(Some(FailPoint::AfterStaging)));
        let result = save(&mut graph, &mut history, &e.project, None, &e.library);
        FAIL_AT.with(|f| f.set(None));
        assert!(result.is_err());
        assert_eq!(json(&graph), before, "live graph unchanged");
        assert!(!e.project.join("project.json").exists(), "disk unchanged");
        assert_eq!(history.depth(), (1, 0));
        let undone = history
            .undo(UndoSnapshot::new(Graph::new(), Default::default()))
            .unwrap();
        assert_eq!(json(&undone.graph), before, "snapshots unchanged");
    }

    #[test]
    fn undo_and_redo_follow_the_portable_save() {
        let e = env("s4b_undo");
        let b = publish(
            &e.library,
            &leaf(&e.tmp.0.join("src"), "B", 0.25),
            "B",
            None,
            "1.0.0",
        )
        .unwrap();
        let (file, package) = child(&b);
        let mut graph = Graph::new();
        let id = add_instance(&mut graph, &file, package.clone());
        let mut history = UndoHistory::new(10);
        history.push(UndoSnapshot::new(graph.clone(), Default::default()));
        let extra = graph.add_node(slider(), [100.0, 0.0]);

        save(&mut graph, &mut history, &e.project, None, &e.library).unwrap();
        assert_eq!(history.depth(), (1, 0), "no extra undo step");
        let saved_file = file_of(&graph, id);

        let undone = history
            .undo(UndoSnapshot::new(graph.clone(), Default::default()))
            .expect("undo");
        assert!(!undone.graph.nodes.contains_key(&extra));
        assert_eq!(
            file_of(&undone.graph, id),
            saved_file,
            "history follows the save"
        );
        assert_eq!(package_of(&undone.graph, id), package);
        let redone = history
            .redo(UndoSnapshot::new(undone.graph.clone(), Default::default()))
            .expect("redo");
        assert!(redone.graph.nodes.contains_key(&extra));
        assert_eq!(file_of(&redone.graph, id), saved_file);
    }

    #[test]
    fn save_as_moves_live_paths_to_the_new_project() {
        let e = env("s4b_save_as");
        let b = publish(
            &e.library,
            &leaf(&e.tmp.0.join("src"), "B", 0.25),
            "B",
            None,
            "1.0.0",
        )
        .unwrap();
        let (file, package) = child(&b);
        let mut graph = Graph::new();
        let id = add_instance(&mut graph, &file, package);
        let mut history = UndoHistory::new(10);
        save(&mut graph, &mut history, &e.project, None, &e.library).unwrap();
        let first = snapshot_dir(&e.project);

        // Save As elsewhere, with no Library: the copy comes from the
        // current project, which is left as it was.
        let empty = e.tmp.0.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let second = e.tmp.0.join("second");
        let saved = save(&mut graph, &mut history, &second, Some(&e.project), &empty).unwrap();
        assert_eq!(saved.portability.copied, 1);
        assert_eq!(file_of(&graph, id), file_of(&open(&second), id));
        assert!(PathBuf::from(file_of(&graph, id)).starts_with(&second));
        assert_eq!(snapshot_dir(&e.project), first, "first project untouched");
    }

    #[test]
    fn tabs_and_session_keep_the_saved_paths() {
        let e = env("s4b_tabs");
        let b = publish(
            &e.library,
            &leaf(&e.tmp.0.join("src"), "B", 0.25),
            "B",
            None,
            "1.0.0",
        )
        .unwrap();
        let (file, package) = child(&b);
        let mut graph = Graph::new();
        let id = add_instance(&mut graph, &file, package.clone());
        let mut history = UndoHistory::new(10);
        save(&mut graph, &mut history, &e.project, None, &e.library).unwrap();
        let saved_file = file_of(&graph, id);

        // Tab switching stores and restores the synchronized graph.
        let mut documents = DocumentManager::new();
        let first = documents.active().id;
        assert!(documents.active_mut().store_graph(&graph));
        let second = documents.new_document("Second");
        assert!(documents.activate(second));
        assert!(documents.activate(first));
        assert_eq!(file_of(&documents.active().graph, id), saved_file);

        // The session written on exit carries the same paths.
        let state = session_state_from(
            Some(SessionSnapshot {
                graph: graph.clone(),
                canvas_offset: [0.0, 0.0],
                canvas_zoom: 1.0,
                pinned_nodes: Vec::new(),
                project_path: Some(e.project.to_string_lossy().into_owned()),
                is_dirty: false,
            }),
            HashMap::new(),
            Vec::new(),
        );
        let restored: SessionState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(file_of(&restored.graph, id), saved_file);
        assert_eq!(package_of(&restored.graph, id), package);
        assert!(!restored.is_dirty);
    }

    #[test]
    fn path_only_nodes_stay_unchanged_and_are_counted() {
        let e = env("s4b_report");
        let src = e.tmp.0.join("src");
        let b = publish(&e.library, &leaf(&src, "B", 0.25), "B", None, "1.0.0").unwrap();
        let c = publish(&e.library, &leaf(&src, "C", 0.5), "C", None, "1.0.0").unwrap();
        let outside = leaf(&src, "Outside", 0.1);
        std::fs::create_dir_all(&e.project).unwrap();
        let inside = leaf(&e.project, "Inside", 0.2);
        let missing = e.tmp.0.join("gone").join("Missing.pwnode");

        let mut graph = Graph::new();
        let (bf, bp) = child(&b);
        let (cf, cp) = child(&c);
        add_instance(&mut graph, &bf, bp);
        add_instance(&mut graph, &cf, cp);
        let loose = add_instance(&mut graph, &outside.to_string_lossy(), None);
        let by_path = add_instance(&mut graph, &b.entry_path.to_string_lossy(), None);
        let local = add_instance(&mut graph, &inside.to_string_lossy(), None);
        let gone = add_instance(&mut graph, &missing.to_string_lossy(), None);
        let path_only: Vec<(NodeId, String)> = [loose, by_path, local, gone]
            .iter()
            .map(|&id| (id, file_of(&graph, id)))
            .collect();

        let mut history = UndoHistory::new(10);
        let saved = save(&mut graph, &mut history, &e.project, None, &e.library).unwrap();
        // Path-only nodes are neither rewritten nor bundled.
        for (id, file) in &path_only {
            assert_eq!(&file_of(&graph, *id), file);
            assert_eq!(package_of(&graph, *id), None);
        }
        assert_eq!(
            saved.portability,
            PortabilityReport {
                bundled: 2,
                copied: 2,
                external_custom_nodes: 3,
                external_assets: 0,
            }
        );
        assert!(!saved.portability.custom_nodes_portable());
        let summary = saved.portability.summary();
        assert!(
            summary.contains("2 verified package(s) bundled (2 newly copied)"),
            "{summary}"
        );
        assert!(
            summary.contains("not portable, 3 path-only node(s)"),
            "{summary}"
        );

        // Saving again: nothing copied, same counts.
        let again = save(&mut graph, &mut history, &e.project, None, &e.library).unwrap();
        assert_eq!(again.portability.bundled, 2);
        assert_eq!(again.portability.copied, 0);
    }

    #[test]
    fn report_never_claims_whole_project_portability() {
        let portable = PortabilityReport {
            bundled: 1,
            copied: 0,
            external_custom_nodes: 0,
            external_assets: 0,
        };
        let text = portable.summary();
        assert!(portable.custom_nodes_portable());
        assert!(text.contains("Custom nodes: fully portable."), "{text}");
        assert!(text.contains("not checked"), "{text}");
        assert!(!text.to_lowercase().contains("project is fully portable"));
        let with_assets = PortabilityReport {
            external_assets: 2,
            ..portable
        };
        assert!(
            with_assets
                .summary()
                .contains("2 other file(s) are outside the project")
        );
    }

    #[test]
    fn external_asset_paths_are_counted() {
        let e = env("s4b_assets");
        std::fs::create_dir_all(&e.project).unwrap();
        let mut graph: Graph = serde_json::from_value(serde_json::json!({
            "nodes": {}, "connections": [], "next_id": 1
        }))
        .unwrap();
        let outside = e.tmp.0.join("media").join("clip.wav");
        let inside = e.project.join("clip.wav");
        for path in [&outside, &inside, &outside] {
            let node: NodeType = serde_json::from_value(serde_json::json!({
                "AudioPlayer": { "file_path": path.to_string_lossy() }
            }))
            .unwrap();
            graph.add_node(node, [0.0, 0.0]);
        }
        assert_eq!(
            count_external_assets(&graph, &e.project),
            1,
            "distinct outside paths"
        );
    }
}

/// P4.2 Stage 4: undo snapshots keep their own package version through
/// portable saves.
#[cfg(test)]
mod snapshot_version_tests {
    use super::super::undo::{UndoHistory, UndoSnapshot};
    use super::*;
    use crate::document::{CustomNodeFile, DocumentManager};
    use crate::library::bundle::tests::{leaf, publish, stored};
    use crate::library::package::VerifiedPackage;
    use crate::library::package::tests::TempDir;
    use crate::library::resolve::InstancePackageRef;
    use std::path::PathBuf;

    fn instance(graph: &Graph, id: NodeId) -> (String, Option<InstancePackageRef>) {
        match &graph.nodes[&id].node_type {
            NodeType::CustomNodeInstance { file, package, .. } => (file.clone(), package.clone()),
            _ => unreachable!(),
        }
    }

    /// Point node `id` at version `v` in place (same node id, as a future
    /// version switch would do).
    fn use_version(graph: &mut Graph, id: NodeId, v: &VerifiedPackage) {
        if let Some(node) = graph.nodes.get_mut(&id) {
            if let NodeType::CustomNodeInstance { file, package, .. } = &mut node.node_type {
                *file = v.entry_path.to_string_lossy().into_owned();
                *package = Some(stored(v));
            }
        }
        // No explicit invalidation: the cached v1 runtime is keyed by file
        // and package, so it must not be reused for v2.
    }

    fn value(graph: &mut Graph, id: NodeId, project: &Path, library: &Path) -> Option<f32> {
        graph.set_package_environment(Some(project.to_path_buf()), Some(library.to_path_buf()));
        match graph.evaluate(0.0).get(&(id, 0)) {
            Some(PortValue::Float(v)) => Some(*v),
            _ => None,
        }
    }

    fn save(graph: &mut Graph, history: &mut UndoHistory, dir: &Path, library: &Path) {
        save_project_files(
            graph,
            history,
            Vec::new(),
            [0.0, 0.0],
            1.0,
            dir,
            None,
            Some(library),
        )
        .unwrap();
    }

    fn snap(graph: &Graph) -> UndoSnapshot {
        UndoSnapshot::new(graph.clone(), Default::default())
    }

    #[test]
    fn snapshots_keep_their_own_version_and_path_across_saves() {
        let tmp = TempDir::new("s4c_versions");
        let library = tmp.0.join("library");
        let project = tmp.0.join("project");
        let v1 = publish(
            &library,
            &leaf(&tmp.0.join("v1"), "C", 0.25),
            "C",
            None,
            "1.0.0",
        )
        .unwrap();
        let v2 = publish(
            &library,
            &leaf(&tmp.0.join("v2"), "C", 0.75),
            "C",
            Some(&v1.manifest.id),
            "2.0.0",
        )
        .unwrap();
        let v1_project = project.join("packages/local.c/1.0.0/C.pwnode");
        let v2_project = project.join("packages/local.c/2.0.0/C.pwnode");

        // State 1: the node uses v1 (with a loaded v1 runtime).
        let mut graph = Graph::new();
        let outputs = CustomNodeFile::load(&v1.entry_path).unwrap().outputs;
        let id = graph.add_node(
            NodeType::CustomNodeInstance {
                file: v1.entry_path.to_string_lossy().into_owned(),
                inputs: Vec::new(),
                outputs,
                package: Some(stored(&v1)),
            },
            [0.0, 0.0],
        );
        assert_eq!(value(&mut graph, id, &project, &library), Some(0.25));
        let mut history = UndoHistory::new(10);
        history.push(snap(&graph));

        // State 2 (live): the same node now uses v2; no stale v1 runtime.
        use_version(&mut graph, id, &v2);
        assert_eq!(value(&mut graph, id, &project, &library), Some(0.75));

        // Save with v2 live: only v2 is bundled and normalized.
        save(&mut graph, &mut history, &project, &library);
        assert_eq!(
            instance(&graph, id),
            (v2_project.to_string_lossy().into_owned(), Some(stored(&v2)))
        );
        assert!(!v1_project.exists(), "v1 is not used by the saved state");
        assert_eq!(history.depth(), (1, 0));

        // Undo: the v1 snapshot was not rewritten to v2 (or to any project
        // path, since v1 was not bundled); it evaluates v1.
        let mut undone = history.undo(snap(&graph)).expect("undo").graph;
        assert_eq!(
            instance(&undone, id),
            (
                v1.entry_path.to_string_lossy().into_owned(),
                Some(stored(&v1))
            )
        );
        assert_eq!(value(&mut undone, id, &project, &library), Some(0.25));

        // Save in the v1 state: now v1 is bundled; the redo snapshot (v2)
        // keeps its own v2 project path, not v1's.
        save(&mut undone, &mut history, &project, &library);
        assert_eq!(
            PathBuf::from(instance(&undone, id).0),
            v1_project,
            "v1 normalized to its own project copy"
        );
        assert!(v2_project.exists(), "v2 copy kept");
        let mut redone = history.redo(snap(&undone)).expect("redo").graph;
        assert_eq!(
            instance(&redone, id),
            (v2_project.to_string_lossy().into_owned(), Some(stored(&v2)))
        );
        assert_eq!(value(&mut redone, id, &project, &library), Some(0.75));

        // And back again: undo restores v1 at its project path.
        let mut again = history.undo(snap(&redone)).expect("undo again").graph;
        assert_eq!(PathBuf::from(instance(&again, id).0), v1_project);
        assert_eq!(instance(&again, id).1, Some(stored(&v1)));
        assert_eq!(value(&mut again, id, &project, &library), Some(0.25));
        // Both work from the project alone.
        let empty = tmp.0.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(value(&mut again, id, &project, &empty), Some(0.25));
    }

    #[test]
    fn successful_and_failed_saves_keep_document_state_consistent() {
        let tmp = TempDir::new("s4c_documents");
        let library = tmp.0.join("library");
        let project = tmp.0.join("project");
        let v1 = publish(
            &library,
            &leaf(&tmp.0.join("src"), "C", 0.25),
            "C",
            None,
            "1.0.0",
        )
        .unwrap();
        let mut graph = Graph::new();
        let id = graph.add_node(
            NodeType::CustomNodeInstance {
                file: v1.entry_path.to_string_lossy().into_owned(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                package: Some(stored(&v1)),
            },
            [0.0, 0.0],
        );
        let mut history = UndoHistory::new(10);
        let mut documents = DocumentManager::new();
        assert!(documents.active_mut().store_graph(&graph));
        documents.active_mut().undo_history = history.clone();

        // Failed save (package removed from the Library): graph, history,
        // document and disk are all unchanged.
        let hidden = tmp.0.join("hidden");
        std::fs::rename(&library, &hidden).unwrap();
        std::fs::create_dir_all(&library).unwrap();
        let before = serde_json::to_string(&graph).unwrap();
        assert!(
            save_project_files(
                &mut graph,
                &mut history,
                Vec::new(),
                [0.0, 0.0],
                1.0,
                &project,
                None,
                Some(&library)
            )
            .is_err()
        );
        assert_eq!(serde_json::to_string(&graph).unwrap(), before);
        assert_eq!(
            serde_json::to_string(&documents.active().graph).unwrap(),
            before,
            "document untouched"
        );
        assert!(!project.join("project.json").exists());
        std::fs::remove_dir_all(&library).unwrap();
        std::fs::rename(&hidden, &library).unwrap();

        // Successful save, then the editor's post-save sync: live graph,
        // document and history agree with project.json.
        save(&mut graph, &mut history, &project, &library);
        assert!(documents.active_mut().store_graph(&graph));
        documents.active_mut().undo_history = history.clone();
        let mut pf = read_project_file(&project).unwrap();
        absolutize_paths(&mut pf.graph, &project.to_string_lossy());
        assert_eq!(instance(&pf.graph, id), instance(&graph, id));
        assert_eq!(
            instance(&documents.active().graph, id),
            instance(&graph, id)
        );
    }
}

/// P4.2 Stage 5: version switching with undo/redo, tabs and project saving.
#[cfg(test)]
mod version_switch_app_tests {
    use super::super::undo::{UndoHistory, UndoSnapshot};
    use super::super::version::describe_affected_ports;
    use super::*;
    use crate::document::{CustomNodeFile, CustomNodePort, DocumentManager};
    use crate::library::bundle::tests::{leaf, publish, stored};
    use crate::library::package::VerifiedPackage;
    use crate::library::package::tests::TempDir;
    use std::path::PathBuf;

    struct Versions {
        tmp: TempDir,
        library: PathBuf,
        v1: VerifiedPackage,
        v2: VerifiedPackage,
    }

    /// v1 (0.25) has an extra input `trim`; v2 (0.75) does not.
    fn versions(tag: &str) -> Versions {
        let tmp = TempDir::new(tag);
        let library = tmp.0.join("library");
        let with_trim = |dir: &Path, value: f32, trim: bool| {
            let path = leaf(dir, "Gain", value);
            let mut file = CustomNodeFile::load(&path).unwrap();
            if trim {
                file.inputs.push(CustomNodePort {
                    name: "trim".into(),
                    kind: "Number".into(),
                });
            }
            file.save(&path).unwrap();
            path
        };
        let v1 = publish(
            &library,
            &with_trim(&tmp.0.join("v1"), 0.25, true),
            "Gain",
            None,
            "1.0.0",
        )
        .unwrap();
        let v2 = publish(
            &library,
            &with_trim(&tmp.0.join("v2"), 0.75, false),
            "Gain",
            Some(&v1.manifest.id),
            "2.0.0",
        )
        .unwrap();
        Versions {
            tmp,
            library,
            v1,
            v2,
        }
    }

    fn slider() -> NodeType {
        NodeType::Slider {
            value: 0.5,
            min: 0.0,
            max: 1.0,
            step: 0.01,
            slider_color: [0, 0, 0],
            label: String::new(),
        }
    }

    /// Instance of v1 with `trim` wired from a slider and its output wired
    /// to another slider.
    fn wired(vs: &Versions) -> (Graph, NodeId) {
        let interface = CustomNodeFile::load(&vs.v1.entry_path).unwrap();
        let mut graph = Graph::new();
        graph.set_package_environment(None, Some(vs.library.clone()));
        let id = graph.add_node(
            NodeType::CustomNodeInstance {
                file: vs.v1.entry_path.to_string_lossy().into_owned(),
                inputs: interface.inputs,
                outputs: interface.outputs,
                package: Some(stored(&vs.v1)),
            },
            [0.0, 0.0],
        );
        let source = graph.add_node(slider(), [-200.0, 0.0]);
        let sink = graph.add_node(slider(), [200.0, 0.0]);
        graph.add_connection(source, 0, id, 0);
        graph.add_connection(id, 0, sink, 0);
        (graph, id)
    }

    fn value(graph: &mut Graph, id: NodeId) -> Option<f32> {
        match graph.evaluate(0.0).get(&(id, 0)) {
            Some(PortValue::Float(v)) => Some(*v),
            _ => None,
        }
    }

    fn package_of(
        graph: &Graph,
        id: NodeId,
    ) -> Option<crate::library::resolve::InstancePackageRef> {
        match &graph.nodes[&id].node_type {
            NodeType::CustomNodeInstance { package, .. } => package.clone(),
            _ => None,
        }
    }

    fn input_names(graph: &Graph, id: NodeId) -> Vec<String> {
        match &graph.nodes[&id].node_type {
            NodeType::CustomNodeInstance { inputs, .. } => {
                inputs.iter().map(|p| p.name.clone()).collect()
            }
            _ => unreachable!(),
        }
    }

    /// What the editor does on a confirmed switch: one undo step, apply.
    fn switch(graph: &mut Graph, history: &mut UndoHistory, id: NodeId, to: &VerifiedPackage) {
        let plan = graph.plan_version_switch(id, &stored(to)).unwrap();
        history.push(UndoSnapshot::new(graph.clone(), Default::default()));
        graph.apply_version_switch(&plan).unwrap();
    }

    #[test]
    fn one_switch_is_one_undo_step_and_undo_redo_restore_everything() {
        let vs = versions("s5a_undo");
        let (mut graph, id) = wired(&vs);
        assert_eq!(value(&mut graph, id), Some(0.25));
        let v1_state = serde_json::to_string(&graph).unwrap();
        let mut history = UndoHistory::new(10);

        switch(&mut graph, &mut history, id, &vs.v2);
        assert_eq!(history.depth(), (1, 0), "exactly one undo step");
        assert_eq!(graph.connections.len(), 1, "the trim wire was removed");
        assert_eq!(value(&mut graph, id), Some(0.75));
        let v2_state = serde_json::to_string(&graph).unwrap();

        let mut undone = history
            .undo(UndoSnapshot::new(graph.clone(), Default::default()))
            .unwrap()
            .graph;
        assert_eq!(serde_json::to_string(&undone).unwrap(), v1_state);
        assert_eq!(package_of(&undone, id), Some(stored(&vs.v1)));
        assert_eq!(input_names(&undone, id), vec!["trim"]);
        assert_eq!(undone.connections.len(), 2, "wires restored");
        assert_eq!(value(&mut undone, id), Some(0.25), "v1 evaluates again");

        let mut redone = history
            .redo(UndoSnapshot::new(undone.clone(), Default::default()))
            .unwrap()
            .graph;
        assert_eq!(serde_json::to_string(&redone).unwrap(), v2_state);
        assert_eq!(value(&mut redone, id), Some(0.75));
    }

    #[test]
    fn confirmation_lists_ports_and_a_changed_graph_is_planned_again() {
        let vs = versions("s5a_dialog");
        let (mut graph, id) = wired(&vs);
        let plan = graph.plan_version_switch(id, &stored(&vs.v2)).unwrap();
        assert!(plan.needs_confirmation());
        let source = graph
            .connections
            .iter()
            .find(|c| c.to_node == id)
            .map(|c| c.from_node)
            .unwrap();
        assert_eq!(
            describe_affected_ports(&plan, &graph),
            vec![
                "input 'trim': removed (1 connection removed)".to_string(),
                format!("    from Slider #{source}"),
            ],
            "grouped by port, with the exact wire"
        );
        // The `trim` wire is removed while the dialog is open: the confirmed
        // plan no longer describes the switch, so it is planned again.
        graph.connections.retain(|c| c.to_node != id);
        let fresh = graph.plan_version_switch(id, &stored(&vs.v2)).unwrap();
        assert!(!fresh.same_effect(&plan));
        assert_eq!(fresh.removed_connections, 0);
        assert!(fresh.same_effect(&graph.plan_version_switch(id, &stored(&vs.v2)).unwrap()));
    }

    #[test]
    fn switching_in_one_tab_leaves_other_tabs_alone() {
        let vs = versions("s5a_tabs");
        let (graph_a, id) = wired(&vs);
        let (graph_b, id_b) = wired(&vs);
        let mut documents = DocumentManager::new();
        let first = documents.active().id;
        assert!(documents.active_mut().store_graph(&graph_b));
        let second = documents.new_document("Second");
        assert!(documents.activate(second));

        let mut live = graph_a;
        let mut history = UndoHistory::new(10);
        switch(&mut live, &mut history, id, &vs.v2);
        assert!(documents.active_mut().store_graph(&live));

        assert!(documents.activate(first));
        assert_eq!(
            package_of(&documents.active().graph, id_b),
            Some(stored(&vs.v1)),
            "the other tab still uses v1"
        );
        assert!(documents.activate(second));
        assert_eq!(
            package_of(&documents.active().graph, id),
            Some(stored(&vs.v2))
        );
    }

    #[test]
    fn saving_after_switching_bundles_the_selected_version_and_reopens() {
        let vs = versions("s5a_save");
        let project = vs.tmp.0.join("project");
        let (mut graph, id) = wired(&vs);
        let mut history = UndoHistory::new(10);
        save_project_files(
            &mut graph,
            &mut history,
            Vec::new(),
            [0.0, 0.0],
            1.0,
            &project,
            None,
            Some(&vs.library),
        )
        .unwrap();
        let v1_copy = project
            .join("packages")
            .join(&vs.v1.manifest.id)
            .join("1.0.0");
        let v1_bytes: Vec<Vec<u8>> = crate::library::bundle::tests::files_under(&v1_copy)
            .iter()
            .map(|rel| std::fs::read(v1_copy.join(rel)).unwrap())
            .collect();

        graph.set_package_environment(Some(project.clone()), Some(vs.library.clone()));
        switch(&mut graph, &mut history, id, &vs.v2);
        let saved = save_project_files(
            &mut graph,
            &mut history,
            Vec::new(),
            [0.0, 0.0],
            1.0,
            &project,
            Some(&project),
            Some(&vs.library),
        )
        .unwrap();
        assert_eq!(saved.portability.copied, 1, "v2 bundled");
        let v2_copy = project
            .join("packages")
            .join(&vs.v1.manifest.id)
            .join("2.0.0");
        assert!(v2_copy.is_dir());
        let still: Vec<Vec<u8>> = crate::library::bundle::tests::files_under(&v1_copy)
            .iter()
            .map(|rel| std::fs::read(v1_copy.join(rel)).unwrap())
            .collect();
        assert_eq!(still, v1_bytes, "previous version kept, unchanged");

        // Reopen on a machine with an empty Library.
        let empty = vs.tmp.0.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let mut pf = read_project_file(&project).unwrap();
        absolutize_paths(&mut pf.graph, &project.to_string_lossy());
        let mut reopened = pf.graph;
        assert_eq!(package_of(&reopened, id), Some(stored(&vs.v2)));
        assert_eq!(input_names(&reopened, id), Vec::<String>::new());
        reopened.set_package_environment(Some(project.clone()), Some(empty));
        assert_eq!(value(&mut reopened, id), Some(0.75));
        assert!(!reopened.take_custom_port_events().changed);
    }
}

/// P4.2 Stage 5 correctness fixes: modal confirmation, node identity,
/// exact wires, re-planning and undo safety.
#[cfg(test)]
mod version_switch_fix_tests {
    use super::super::undo::UndoHistory;
    use super::super::version::{SwitchOutcome, confirm_version_switch, editing_paused};
    use super::*;
    use crate::document::{CustomNodeFile, CustomNodePort, DocumentManager};
    use crate::library::bundle::tests::{files_under, leaf, publish, stored};
    use crate::library::package::VerifiedPackage;
    use crate::library::package::tests::TempDir;
    use std::collections::HashSet;
    use std::path::PathBuf;

    struct Versions {
        tmp: TempDir,
        library: PathBuf,
        v1: VerifiedPackage,
        v2: VerifiedPackage,
    }

    fn gain(dir: &Path, value: f32, trim: bool) -> PathBuf {
        let path = leaf(dir, "Gain", value);
        let mut file = CustomNodeFile::load(&path).unwrap();
        if trim {
            file.inputs.push(CustomNodePort {
                name: "trim".into(),
                kind: "Number".into(),
            });
        }
        file.save(&path).unwrap();
        path
    }

    /// v1 (0.25) has an input `trim`; v2 (0.75) does not.
    fn versions(tag: &str) -> Versions {
        let tmp = TempDir::new(tag);
        let library = tmp.0.join("library");
        let v1 = publish(
            &library,
            &gain(&tmp.0.join("v1"), 0.25, true),
            "Gain",
            None,
            "1.0.0",
        )
        .unwrap();
        let v2 = publish(
            &library,
            &gain(&tmp.0.join("v2"), 0.75, false),
            "Gain",
            Some(&v1.manifest.id),
            "2.0.0",
        )
        .unwrap();
        Versions {
            tmp,
            library,
            v1,
            v2,
        }
    }

    fn slider() -> NodeType {
        NodeType::Slider {
            value: 0.5,
            min: 0.0,
            max: 1.0,
            step: 0.01,
            slider_color: [0, 0, 0],
            label: String::new(),
        }
    }

    fn v1_instance(vs: &Versions) -> NodeType {
        let interface = CustomNodeFile::load(&vs.v1.entry_path).unwrap();
        NodeType::CustomNodeInstance {
            file: vs.v1.entry_path.to_string_lossy().into_owned(),
            inputs: interface.inputs,
            outputs: interface.outputs,
            package: Some(stored(&vs.v1)),
        }
    }

    /// Two sliders (sources) and a sink, then the v1 instance with `trim`
    /// wired from `source_a` and its output wired to the sink.
    struct Scene {
        graph: Graph,
        id: NodeId,
        source_a: NodeId,
        source_b: NodeId,
    }

    fn scene(vs: &Versions) -> Scene {
        let mut graph = Graph::new();
        graph.set_package_environment(None, Some(vs.library.clone()));
        let source_a = graph.add_node(slider(), [-200.0, 0.0]);
        let source_b = graph.add_node(slider(), [-200.0, 100.0]);
        let sink = graph.add_node(slider(), [200.0, 0.0]);
        let id = graph.add_node(v1_instance(vs), [0.0, 0.0]);
        graph.add_connection(source_a, 0, id, 0);
        graph.add_connection(id, 0, sink, 0);
        Scene {
            graph,
            id,
            source_a,
            source_b,
        }
    }

    fn state(graph: &Graph) -> String {
        serde_json::to_string(graph).unwrap()
    }

    fn confirm(
        graph: &mut Graph,
        history: &mut UndoHistory,
        plan: &crate::graph::version::VersionSwitchPlan,
    ) -> SwitchOutcome {
        confirm_version_switch(graph, history, &HashSet::new(), plan)
    }

    fn value(graph: &mut Graph, id: NodeId) -> Option<f32> {
        match graph.evaluate(0.0).get(&(id, 0)) {
            Some(PortValue::Float(v)) => Some(*v),
            _ => None,
        }
    }

    #[test]
    fn normal_switching_still_works_with_one_undo_step() {
        let vs = versions("s5f_normal");
        let mut s = scene(&vs);
        let mut history = UndoHistory::new(10);
        let plan = s.graph.plan_version_switch(s.id, &stored(&vs.v2)).unwrap();
        assert!(plan.needs_confirmation());
        match confirm(&mut s.graph, &mut history, &plan) {
            SwitchOutcome::Applied { removed } => assert_eq!(removed, 1),
            other => panic!("{other:?}"),
        }
        assert_eq!(history.depth(), (1, 0));
        assert_eq!(value(&mut s.graph, s.id), Some(0.75));
    }

    #[test]
    fn deleted_node_and_reused_id_never_receive_a_pending_switch() {
        let vs = versions("s5f_reuse");
        // State before the instance existed (what undo would restore).
        let mut s = scene(&vs);
        let plan = s.graph.plan_version_switch(s.id, &stored(&vs.v2)).unwrap();
        let mut history = UndoHistory::new(10);

        // Deleted while pending: refused, nothing recorded.
        let mut deleted = s.graph.clone();
        deleted.remove_node(s.id);
        let before = state(&deleted);
        assert!(matches!(
            confirm(&mut deleted, &mut history, &plan),
            SwitchOutcome::Invalidated(_)
        ));
        assert_eq!(state(&deleted), before);
        assert_eq!(history.depth(), (0, 0));

        // Undo past its creation, then an identical node is inserted: it
        // gets the same id, type, position and wires, but is not the
        // previewed node.
        let mut recreated: Graph = s.graph.clone();
        recreated.remove_node(s.id);
        recreated.fix_next_id();
        let reused = recreated.add_node(v1_instance(&vs), [0.0, 0.0]);
        assert_eq!(reused, s.id, "id reused");
        let sink = s
            .graph
            .connections
            .iter()
            .find(|c| c.from_node == s.id)
            .unwrap()
            .to_node;
        recreated.add_connection(s.source_a, 0, reused, 0);
        recreated.add_connection(reused, 0, sink, 0);
        let fresh_context = recreated.switch_context(reused).unwrap();
        assert_eq!(
            fresh_context.node_state, plan.context.node_state,
            "byte-identical node"
        );
        assert_eq!(fresh_context.connections, plan.context.connections);
        let before = state(&recreated);
        assert!(matches!(
            confirm(&mut recreated, &mut history, &plan),
            SwitchOutcome::Invalidated(_)
        ));
        assert_eq!(state(&recreated), before, "the new node was not switched");
        assert_eq!(history.depth(), (0, 0));
        // The graph model refuses the stale plan as well.
        assert!(recreated.apply_version_switch(&plan).is_err());
        assert_eq!(state(&recreated), before);
        let _ = &mut s.graph;
    }

    #[test]
    fn different_wires_with_identical_counts_need_a_new_confirmation() {
        let vs = versions("s5f_wires");
        let mut s = scene(&vs);
        let plan = s.graph.plan_version_switch(s.id, &stored(&vs.v2)).unwrap();
        assert_eq!(plan.removed.len(), 1);
        assert_eq!(plan.removed[0].from_node, s.source_a);

        // Replace the trim wire with one from another source: same count.
        s.graph.connections.retain(|c| c.to_node != s.id);
        s.graph.add_connection(s.source_b, 0, s.id, 0);
        let fresh = s.graph.plan_version_switch(s.id, &stored(&vs.v2)).unwrap();
        assert_eq!(fresh.removed_connections, plan.removed_connections);
        assert!(
            !fresh.same_effect(&plan),
            "different wires are not equivalent"
        );

        let mut history = UndoHistory::new(10);
        let before = state(&s.graph);
        let renewed = match confirm(&mut s.graph, &mut history, &plan) {
            SwitchOutcome::Reconfirm(fresh) => *fresh,
            other => panic!("expected a new confirmation, got {other:?}"),
        };
        assert_eq!(state(&s.graph), before, "nothing applied");
        assert_eq!(history.depth(), (0, 0));
        assert_eq!(renewed.removed[0].from_node, s.source_b);
        // Confirming the renewed preview applies it.
        assert!(matches!(
            confirm(&mut s.graph, &mut history, &renewed),
            SwitchOutcome::Applied { removed: 1 }
        ));
    }

    #[test]
    fn port_changes_after_preview_are_never_applied_silently() {
        let vs = versions("s5f_ports");
        let mut s = scene(&vs);
        let plan = s.graph.plan_version_switch(s.id, &stored(&vs.v2)).unwrap();
        let mut history = UndoHistory::new(10);

        // An extra port appears on the instance (e.g. its file changed).
        let mut extra = s.graph.clone();
        if let Some(NodeType::CustomNodeInstance { inputs, .. }) =
            extra.nodes.get_mut(&s.id).map(|n| &mut n.node_type)
        {
            inputs.push(CustomNodePort {
                name: "extra".into(),
                kind: "Number".into(),
            });
        }
        let before = state(&extra);
        assert!(matches!(
            confirm(&mut extra, &mut history, &plan),
            SwitchOutcome::Reconfirm(_)
        ));
        assert_eq!(state(&extra), before);

        // The `trim` port and its wire disappear: the switch would now lose
        // nothing, but the node changed, so the preview is invalidated
        // instead of applied.
        let mut trimmed = s.graph.clone();
        trimmed.connections.retain(|c| c.to_node != s.id);
        if let Some(NodeType::CustomNodeInstance { inputs, .. }) =
            trimmed.nodes.get_mut(&s.id).map(|n| &mut n.node_type)
        {
            inputs.clear();
        }
        let before = state(&trimmed);
        assert!(matches!(
            confirm(&mut trimmed, &mut history, &plan),
            SwitchOutcome::Invalidated(_)
        ));
        assert_eq!(state(&trimmed), before);
        assert_eq!(history.depth(), (0, 0));
        let _ = &mut s.graph;
    }

    #[test]
    fn stale_preview_that_becomes_compatible_applies_directly() {
        let vs = versions("s5f_compatible");
        // The project gets its own copy of 2.0.0 (priority over the
        // Library) that still has `trim`: nothing would be lost any more.
        let project = vs.tmp.0.join("project");
        let mut s = scene(&vs);
        s.graph
            .set_package_environment(Some(project.clone()), Some(vs.library.clone()));
        let plan = s.graph.plan_version_switch(s.id, &stored(&vs.v2)).unwrap();
        assert!(plan.needs_confirmation());

        let elsewhere = vs.tmp.0.join("elsewhere");
        let first = publish(
            &elsewhere,
            &gain(&vs.tmp.0.join("o1"), 0.1, true),
            "Gain",
            None,
            "1.0.0",
        )
        .unwrap();
        let with_trim = publish(
            &elsewhere,
            &gain(&vs.tmp.0.join("o2"), 0.9, true),
            "Gain",
            Some(&first.manifest.id),
            "2.0.0",
        )
        .unwrap();
        let target = project
            .join(crate::library::package::PACKAGES_DIR)
            .join(&vs.v2.manifest.id)
            .join("2.0.0");
        for rel in files_under(&with_trim.dir) {
            let to = target.join(&rel);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(with_trim.dir.join(&rel), to).unwrap();
        }

        let mut history = UndoHistory::new(10);
        match confirm(&mut s.graph, &mut history, &plan) {
            SwitchOutcome::Applied { removed } => assert_eq!(removed, 0, "no wire removed"),
            other => panic!("{other:?}"),
        }
        assert_eq!(s.graph.connections.len(), 2);
        assert_eq!(history.depth(), (1, 0));
    }

    #[test]
    fn cancel_preserves_graph_and_undo_history() {
        let vs = versions("s5f_cancel");
        let mut s = scene(&vs);
        let mut history = UndoHistory::new(10);
        history.push(super::super::undo::UndoSnapshot::new(
            s.graph.clone(),
            Default::default(),
        ));
        let before = state(&s.graph);
        let plan = s.graph.plan_version_switch(s.id, &stored(&vs.v2)).unwrap();
        drop(plan); // Cancel: the plan is discarded, nothing is applied.
        assert_eq!(state(&s.graph), before);
        assert_eq!(history.depth(), (1, 0));
    }

    #[test]
    fn failed_application_records_no_undo_step() {
        let vs = versions("s5f_failed");
        let mut s = scene(&vs);
        assert_eq!(value(&mut s.graph, s.id), Some(0.25));
        let plan = s.graph.plan_version_switch(s.id, &stored(&vs.v2)).unwrap();
        std::fs::write(&vs.v2.entry_path, "{ damaged").unwrap();
        let mut history = UndoHistory::new(10);
        let before = state(&s.graph);
        assert!(matches!(
            confirm(&mut s.graph, &mut history, &plan),
            SwitchOutcome::Failed(_)
        ));
        assert_eq!(state(&s.graph), before);
        assert_eq!(history.depth(), (0, 0), "no empty undo step");
        assert_eq!(value(&mut s.graph, s.id), Some(0.25), "v1 keeps running");
    }

    #[test]
    fn the_confirmation_locks_documents_and_pauses_editing() {
        // The modal rule used by `active_is_editable`, the canvas gate, the
        // shortcut and system-action guards (undo/redo, insertion, grouping,
        // duplicate, automatic insertion all check it).
        assert!(!editing_paused(false, false));
        assert!(editing_paused(false, true));
        assert!(editing_paused(true, false));

        // The document lock the confirmation holds: no tab switching,
        // closing or opening while it is open.
        let vs = versions("s5f_lock");
        let node = vs.v1.entry_path.clone();
        let mut documents = DocumentManager::new();
        let first = documents.active().id;
        let second = documents.new_document("Second");
        assert!(documents.activate(first));
        documents.set_locked(true);
        assert!(!documents.activate(second), "no tab switching");
        assert!(!documents.close_document(first), "no closing");
        assert!(
            documents.open_custom_node(node.clone()).is_err(),
            "no opening"
        );
        assert_eq!(documents.active().id, first);
        documents.set_locked(false); // Cancel / Switch Version
        assert!(documents.activate(second));
    }
}

/// P4.2 Stage 5: runtime node identity tokens.
#[cfg(test)]
mod node_token_tests {
    use super::super::undo::{UndoHistory, UndoSnapshot};
    use super::*;
    use crate::document::{CustomNodeFile, custom_node_interface};
    use crate::graph::PortKind;
    use crate::library::package::tests::TempDir;
    use std::collections::HashSet;

    fn slider(value: f32) -> NodeType {
        NodeType::Slider {
            value,
            min: 0.0,
            max: 1.0,
            step: 0.01,
            slider_color: [0, 0, 0],
            label: String::new(),
        }
    }

    fn token(graph: &Graph, id: NodeId) -> u64 {
        graph.nodes[&id].token
    }

    fn tokens(graph: &Graph) -> HashSet<u64> {
        graph.nodes.values().map(|n| n.token).collect()
    }

    /// True if any object in `value` has a key named `key`.
    fn has_key(value: &serde_json::Value, key: &str) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                map.contains_key(key) || map.values().any(|v| has_key(v, key))
            }
            serde_json::Value::Array(items) => items.iter().any(|v| has_key(v, key)),
            _ => false,
        }
    }

    fn three_nodes() -> (Graph, Vec<NodeId>) {
        let mut graph = Graph::new();
        let ids = (0..3)
            .map(|i| graph.add_node(slider(i as f32 / 10.0), [i as f32 * 100.0, 0.0]))
            .collect();
        (graph, ids)
    }

    #[test]
    fn new_nodes_get_distinct_nonzero_tokens() {
        let (graph, ids) = three_nodes();
        let all = tokens(&graph);
        assert_eq!(all.len(), ids.len(), "every node has its own token");
        assert!(!all.contains(&0), "tokens are nonzero");
    }

    #[test]
    fn deserializing_assigns_distinct_fresh_tokens() {
        let (graph, _) = three_nodes();
        let text = serde_json::to_string(&graph).unwrap();
        let first: Graph = serde_json::from_str(&text).unwrap();
        let second: Graph = serde_json::from_str(&text).unwrap();
        for loaded in [&first, &second] {
            let all = tokens(loaded);
            assert_eq!(all.len(), 3, "distinct tokens after loading");
            assert!(!all.contains(&0));
            assert!(
                all.is_disjoint(&tokens(&graph)),
                "never the saved node's token"
            );
        }
        assert!(
            tokens(&first).is_disjoint(&tokens(&second)),
            "each load (reopen) gets new identities"
        );
    }

    /// Duplicate (`duplicate_selected`) and paste (the clipboard holds a
    /// `NodeType`) both copy only the node type and call `add_node`.
    #[test]
    fn duplicate_and_paste_create_new_identities() {
        let (mut graph, ids) = three_nodes();
        let source = ids[0];
        let clipboard = graph.nodes[&source].node_type.clone();
        let duplicate = graph.add_node(graph.nodes[&source].node_type.clone(), [30.0, 30.0]);
        let pasted = graph.add_node(clipboard.clone(), [60.0, 60.0]);
        let pasted_again = graph.add_node(clipboard, [90.0, 90.0]);
        let copies = [duplicate, pasted, pasted_again];
        for copy in copies {
            assert_ne!(copy, source);
            assert_ne!(token(&graph, copy), token(&graph, source));
            assert_ne!(token(&graph, copy), 0);
        }
        assert_eq!(tokens(&graph).len(), graph.nodes.len(), "no shared tokens");
    }

    #[test]
    fn clones_and_undo_snapshots_keep_identities() {
        let (mut graph, ids) = three_nodes();
        let before: Vec<u64> = ids.iter().map(|id| token(&graph, *id)).collect();

        let cloned = graph.clone();
        for (id, t) in ids.iter().zip(&before) {
            assert_eq!(token(&cloned, *id), *t, "a clone is the same logical node");
        }

        let mut history = UndoHistory::new(10);
        history.push(UndoSnapshot::new(graph.clone(), Default::default()));
        graph.remove_node(ids[1]);
        let restored = history
            .undo(UndoSnapshot::new(graph.clone(), Default::default()))
            .unwrap()
            .graph;
        for (id, t) in ids.iter().zip(&before) {
            assert_eq!(token(&restored, *id), *t, "undo restores the same node");
        }
        let redone = history
            .redo(UndoSnapshot::new(restored.clone(), Default::default()))
            .unwrap()
            .graph;
        assert!(!redone.nodes.contains_key(&ids[1]));
        assert_eq!(token(&redone, ids[0]), before[0]);
        assert_eq!(token(&redone, ids[2]), before[2]);
    }

    #[test]
    fn recreating_a_node_after_undo_gets_a_new_token() {
        let (mut graph, _) = three_nodes();
        let mut history = UndoHistory::new(10);
        history.push(UndoSnapshot::new(graph.clone(), Default::default()));
        let added = graph.add_node(slider(0.9), [0.0, 200.0]);
        let old_token = token(&graph, added);

        graph = history
            .undo(UndoSnapshot::new(graph.clone(), Default::default()))
            .unwrap()
            .graph;
        assert!(!graph.nodes.contains_key(&added));
        let recreated = graph.add_node(slider(0.9), [0.0, 200.0]);
        assert_eq!(recreated, added, "undo made the id reusable");
        assert_ne!(token(&graph, recreated), old_token, "but never the token");
        assert_eq!(tokens(&graph).len(), graph.nodes.len());
    }

    #[test]
    fn saved_project_and_custom_node_files_contain_no_tokens() {
        let tmp = TempDir::new("token_files");
        let (mut graph, _) = three_nodes();
        // Interface nodes so the graph is a valid custom node.
        graph.add_node(
            NodeType::CustomInput {
                name: "in".into(),
                kind: PortKind::Number,
            },
            [0.0, 100.0],
        );
        graph.add_node(
            NodeType::CustomOutput {
                name: "out".into(),
                kind: PortKind::Number,
            },
            [0.0, 200.0],
        );
        assert!(!has_key(&serde_json::to_value(&graph).unwrap(), "token"));

        let project = tmp.0.join("project");
        let mut history = UndoHistory::new(10);
        save_project_files(
            &mut graph,
            &mut history,
            Vec::new(),
            [0.0, 0.0],
            1.0,
            &project,
            None,
            None,
        )
        .unwrap();
        let text = std::fs::read_to_string(project.join("project.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(
            !has_key(&value, "token"),
            "project.json has no runtime tokens"
        );

        let (inputs, outputs) = custom_node_interface(&graph);
        let path = tmp.0.join("Tokens.pwnode");
        CustomNodeFile {
            version: 1,
            name: "Tokens".into(),
            graph: graph.clone(),
            inputs,
            outputs,
        }
        .save(&path)
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(!has_key(&value, "token"), ".pwnode has no runtime tokens");
        let loaded = CustomNodeFile::load(&path).unwrap();
        assert!(tokens(&loaded.graph).is_disjoint(&tokens(&graph)));
    }
}
