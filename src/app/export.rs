//! P6 Stage 2.2: Windows portable export.
//!
//! [`export_portable`] turns a saved project into a folder that runs on
//! another Windows computer: the Patchwork runtime renamed to the app, its
//! required DLLs, `player.json` (Stage 2.1) and a copy of the project with
//! its packages and assets. The exported program starts straight into the
//! Player through the manifest, asks for explicit consent the first time
//! on each computer, and never carries trust records.
//!
//! It reuses Patchwork's own mechanisms: project loading
//! (`io::load_project_graph`), content trust (`provenance`), package
//! planning, bundling and verification (`library::project`,
//! `PackageResolver`), node classification (`marketplace::scan`) and the
//! Player's output selection and manifest validation.
//!
//! **Fail closed.** Every check runs before anything is written, and all
//! problems are reported together. The export is built in a staging folder
//! next to the destination, verified there (packages, asset checksums,
//! the manifest launch, no secrets or links), and renamed into place only
//! when complete; on any failure the staging folder is removed. The source
//! project is only read.
//!
//! P6 Stage 2.3: the runtime is the Player build (`patchwork-player.exe`,
//! GUI subsystem, no editor), checked from its own bytes; native
//! dependencies are approved by [`runtime::DependencyRules`]; the source
//! commit the runtime records goes into `licenses/SOURCE.txt`; and
//! [`ExportJob`] runs an export on its own thread with progress and
//! cancellation, one at a time (File → Export as App, `export_dialog`).

pub mod pe;
pub mod runtime;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::graph::{Graph, NodeId, NodeType};
use crate::library::trust::authorize::TrustPolicy;
use crate::library::trust::content::ContentLedger;

use super::launch;
use super::player::manifest::{MANIFEST_FILE, MANIFEST_FORMAT, MANIFEST_SCHEMA};

/// The exported project's folder (relative to the export folder).
pub const PROJECT_DIR: &str = "project";
/// External assets are copied here, per kind.
pub const ASSETS_DIR: &str = "assets";
const PROJECT_FILE: &str = "project.json";
const LICENSES_DIR: &str = "licenses";
const THIRD_PARTY_DIR: &str = "third-party";
const MAX_APP_NAME: usize = 64;
/// The Patchwork licence, shipped with every export (GPL-3.0).
const PATCHWORK_LICENSE: &str = include_str!("../../LICENSE");
const SOURCE_URL: &str = "https://github.com/openblocks-project/patchwork";
/// Files that must never be in an export (trust, credentials, sessions).
const FORBIDDEN_NAMES: &[&str] = &[
    "keys.json",
    "api_keys.json",
    "last_session.json",
    "content.json",
    "packages.json",
    "trusted_plugins.json",
    "settings.json",
];

/// What to export.
#[derive(Debug, Clone)]
pub struct ExportRequest {
    /// The saved project folder (with `project.json`).
    pub source_project: PathBuf,
    /// The export folder to create (absolute; must not exist unless
    /// `overwrite`).
    pub destination: PathBuf,
    /// The application's name: window title and `<name>.exe`.
    pub app_name: String,
    /// The Video Out (Window / Display) node; required when the project
    /// has several.
    pub output: Option<NodeId>,
    pub runtime: RuntimeSource,
    /// ffmpeg to bundle, when the project needs it.
    pub ffmpeg: Option<FfmpegSource>,
    /// Replace an existing export at `destination` (explicit only).
    pub overwrite: bool,
    /// P6 Stage 2.6: distributable, or an internal test build.
    pub purpose: ExportPurpose,
}

/// What an export is for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExportPurpose {
    /// A build that may be given to others: every embedded component must
    /// be cleared for redistribution, or the export is refused.
    #[default]
    Distribution,
    /// P6 Stage 2.6: for internal development and testing only. Exported
    /// despite uncleared components, and labelled
    /// [`runtime::INTERNAL_TEST_LABEL`] in its marker files and notices
    /// (the Player also labels its own window). Never treated as cleared.
    InternalTest,
}

/// Where the runtime comes from.
#[derive(Debug, Clone)]
pub struct RuntimeSource {
    /// A release build of Patchwork.
    pub executable: PathBuf,
    /// Folders holding the DLLs it needs (besides its own folder). System
    /// folders are refused: dependencies are never taken from Windows.
    pub dependency_dirs: Vec<PathBuf>,
}

/// A redistributable ffmpeg build and its licence notices.
#[derive(Debug, Clone)]
pub struct FfmpegSource {
    pub executable: PathBuf,
    /// Licence texts / notices to ship with it (at least one).
    pub licence_files: Vec<PathBuf>,
}

/// This computer's records and folders the export consults.
#[derive(Debug, Clone)]
pub struct ExportEnvironment {
    /// Content trust (is the project allowed to run here?).
    pub ledger: Option<ContentLedger>,
    /// Package trust.
    pub trust: TrustPolicy,
    /// The Library (packages are copied from it into the export).
    pub library_root: Option<PathBuf>,
    /// Folders an export may not be written into.
    pub protected: Vec<PathBuf>,
    /// `~/.patchwork` (never copied; an export may not contain it).
    pub user_data: Option<PathBuf>,
    /// P6 Stage 2.3: which native dependencies may be bundled.
    pub dependencies: runtime::DependencyRules,
    /// P6 Stage 2.4: embedded components not cleared for redistribution
    /// (component, why). Any entry refuses a distributable export.
    pub unresolved: Vec<(String, String)>,
    /// P6 Stage 2.4: the source the runtime must be built from (commit,
    /// changed), so the notices compiled into this exporter describe it.
    pub runtime_source: Option<(String, Option<bool>)>,
}

impl ExportEnvironment {
    /// The editor's own records and this computer's protected folders.
    pub fn this_computer() -> Self {
        let mut protected: Vec<PathBuf> = [
            "SystemRoot",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramData",
        ]
        .iter()
        .filter_map(|var| std::env::var_os(var).map(PathBuf::from))
        .collect();
        let user_data = super::player::manifest::trust_dir();
        protected.extend(user_data.clone());
        Self {
            ledger: super::provenance::editor_ledger(),
            trust: super::permissions::editor_trust_policy(),
            library_root: crate::library::library_root().ok(),
            protected,
            user_data,
            dependencies: runtime::DependencyRules::windows(),
            unresolved: runtime::UNRESOLVED_COMPONENTS
                .iter()
                .map(|(what, why)| (what.to_string(), why.to_string()))
                .collect(),
            runtime_source: Some((
                pe::SOURCE_COMMIT.to_string(),
                match pe::SOURCE_MODIFIED {
                    "0" => Some(false),
                    "1" => Some(true),
                    _ => None,
                },
            )),
        }
    }
}

/// The outcome: what was (or would have been) exported, and why not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportReport {
    /// The finished export (only when it succeeded).
    pub destination: Option<PathBuf>,
    /// Files in the export (relative, `/`-separated, sorted).
    pub files: Vec<String>,
    /// Runtime dependencies and bundled tools included.
    pub dependencies: Vec<String>,
    pub warnings: Vec<String>,
    /// Blocking errors: nothing was exported.
    pub errors: Vec<String>,
    /// The exported project's content digest (recorded in `player.json`;
    /// an identity, not a signature).
    pub project_digest: Option<String>,
    /// The export was cancelled (nothing was left behind).
    pub cancelled: bool,
    /// P6 Stage 2.6: the export is an internal test build, NOT for
    /// distribution.
    pub internal_test_build: bool,
}

impl ExportReport {
    pub fn succeeded(&self) -> bool {
        self.errors.is_empty() && self.destination.is_some()
    }
}

/// A file to copy into the export: source → path relative to the export.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CopyItem {
    from: PathBuf,
    to: String,
}

/// Everything decided before writing.
struct ExportPlan {
    graph: Graph,
    output: NodeId,
    packages: crate::library::project::ProjectPackagePlan,
    /// Files copied into the project folder (relative to it).
    project_files: Vec<CopyItem>,
    /// Runtime files copied into the export root.
    runtime_files: Vec<CopyItem>,
    dependencies: Vec<String>,
    warnings: Vec<String>,
    ffmpeg_licences: Vec<CopyItem>,
    /// The runtime's recorded build (source provenance).
    build: Option<pe::BuildInfo>,
    /// Licence notices of the bundled dependencies: file name -> text.
    dependency_notices: BTreeMap<String, String>,
    /// Internal test builds: the marker text (why it may not be
    /// distributed). `None`: a distributable build.
    not_for_distribution: Option<String>,
}

/// Export `request.source_project` as a Windows portable app.
#[cfg_attr(not(test), allow(dead_code))]
pub fn export_portable(request: &ExportRequest, env: &ExportEnvironment) -> ExportReport {
    export_portable_with(request, env, &ExportControl::default())
}

/// [`export_portable`], reporting progress to and stoppable through
/// `control` (from another thread). A cancelled or failed export leaves
/// nothing at the destination: the staging folder is removed, and an
/// earlier export being replaced stays as it was.
pub fn export_portable_with(
    request: &ExportRequest,
    env: &ExportEnvironment,
    control: &ExportControl,
) -> ExportReport {
    let mut report = ExportReport::default();
    control.set_step("Checking the project", 0, 0);
    let plan = match preflight(request, env) {
        Ok(plan) => plan,
        Err((errors, warnings)) => {
            report.errors = errors;
            report.warnings = warnings;
            return report;
        }
    };
    report.warnings = plan.warnings.clone();
    report.dependencies = plan.dependencies.clone();
    report.internal_test_build = plan.not_for_distribution.is_some();
    if control.cancelled() {
        report.cancelled = true;
        report.errors.push(CANCELLED.to_string());
        return report;
    }
    let staging = staging_dir(&request.destination);
    let written = write_export(request, env, &plan, &staging, control).and_then(|digest| {
        if control.cancelled() {
            return Err(CANCELLED.to_string());
        }
        control.set_step("Moving the export into place", 0, 0);
        finish(&staging, &request.destination, request.overwrite).map(|()| digest)
    });
    match written {
        Ok(digest) => {
            report.files = list_files(&request.destination);
            report.project_digest = Some(digest);
            report.destination = Some(request.destination.clone());
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            report.cancelled = error == CANCELLED;
            report.errors.push(error);
        }
    }
    control.set_step("Done", 0, 0);
    report
}

const CANCELLED: &str = "The export was cancelled; nothing was written.";

/// What a running export is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportProgress {
    pub step: String,
    /// Bytes copied so far, of `total_bytes` (0 when not copying).
    pub done_bytes: u64,
    pub total_bytes: u64,
}

/// Shared with the thread running an export: its progress, and a request
/// to stop.
#[derive(Debug, Default)]
pub struct ExportControl {
    cancel: std::sync::atomic::AtomicBool,
    progress: std::sync::Mutex<ExportProgress>,
}

impl ExportControl {
    /// Ask the export to stop (honoured until it starts moving into place).
    pub fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn progress(&self) -> ExportProgress {
        self.progress.lock().map(|p| p.clone()).unwrap_or_default()
    }

    fn set_step(&self, step: &str, done_bytes: u64, total_bytes: u64) {
        if let Ok(mut p) = self.progress.lock() {
            *p = ExportProgress {
                step: step.to_string(),
                done_bytes,
                total_bytes,
            };
        }
    }

    fn add_bytes(&self, n: u64) {
        if let Ok(mut p) = self.progress.lock() {
            p.done_bytes = p.done_bytes.saturating_add(n);
        }
    }
}

// ── Background exports ──────────────────────────────────────────────────────

static EXPORT_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The right to run the one export allowed at a time (released on drop).
#[derive(Debug)]
pub struct ExportSlot(());

impl ExportSlot {
    /// `None` while another export is running.
    pub fn claim() -> Option<Self> {
        EXPORT_RUNNING
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .ok()
            .map(|_| ExportSlot(()))
    }
}

impl Drop for ExportSlot {
    fn drop(&mut self) {
        EXPORT_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// An export running on its own thread (file copies never block the UI).
pub struct ExportJob {
    control: std::sync::Arc<ExportControl>,
    result: std::sync::mpsc::Receiver<ExportReport>,
    finished: Option<ExportReport>,
}

impl ExportJob {
    /// Start exporting in the background. Refused while another export is
    /// running.
    pub fn start(request: ExportRequest, env: ExportEnvironment) -> Result<Self, String> {
        let slot = ExportSlot::claim().ok_or("Another export is already running.")?;
        let control = std::sync::Arc::new(ExportControl::default());
        let (send, result) = std::sync::mpsc::channel();
        let shared = control.clone();
        std::thread::Builder::new()
            .name("patchwork-export".to_string())
            .spawn(move || {
                let _slot = slot;
                let report = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    export_portable_with(&request, &env, &shared)
                }))
                .unwrap_or_else(|_| ExportReport {
                    errors: vec![
                        "The export stopped unexpectedly; nothing was exported.".to_string(),
                    ],
                    ..ExportReport::default()
                });
                let _ = send.send(report);
            })
            .map_err(|e| format!("The export cannot start: {e}"))?;
        Ok(Self {
            control,
            result,
            finished: None,
        })
    }

    pub fn progress(&self) -> ExportProgress {
        self.control.progress()
    }

    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// The report, once the export has finished.
    pub fn poll(&mut self) -> Option<&ExportReport> {
        if self.finished.is_none() {
            match self.result.try_recv() {
                Ok(report) => self.finished = Some(report),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.finished = Some(ExportReport {
                        errors: vec!["The export stopped unexpectedly.".to_string()],
                        ..ExportReport::default()
                    });
                }
            }
        }
        self.finished.as_ref()
    }

    /// Wait for the report (tests and shutdown).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn wait(mut self) -> ExportReport {
        if let Some(report) = self.finished.take() {
            return report;
        }
        self.result.recv().unwrap_or_else(|_| ExportReport {
            errors: vec!["The export stopped unexpectedly.".to_string()],
            ..ExportReport::default()
        })
    }
}

/// Whether the project needs ffmpeg (video playback or capture), so the
/// export dialog can ask for it.
pub fn needs_ffmpeg(graph: &Graph) -> bool {
    check_nodes(graph, &mut Vec::new())
}

// ── Preflight ───────────────────────────────────────────────────────────────

type Problems = (Vec<String>, Vec<String>);

fn preflight(request: &ExportRequest, env: &ExportEnvironment) -> Result<ExportPlan, Problems> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if let Err(e) = check_app_name(&request.app_name) {
        errors.push(e);
    }
    let source = match std::fs::canonicalize(&request.source_project) {
        Ok(dir) if dir.join(PROJECT_FILE).is_file() => dir,
        _ => {
            errors.push(format!(
                "{} is not a saved Patchwork project (no {PROJECT_FILE}).",
                request.source_project.display()
            ));
            return Err((errors, warnings));
        }
    };
    if let Err(e) = check_destination(request, env, &source) {
        errors.push(e);
    }

    // The project loads, and may run on this computer.
    let mut graph = match super::io::load_project_graph(&source.join(PROJECT_FILE)) {
        Ok(graph) => graph,
        Err(e) => {
            errors.push(format!("The project cannot be loaded: {e}"));
            return Err((errors, warnings));
        }
    };
    super::provenance::resolve_graph_with(&mut graph, env.ledger.as_ref());
    if !graph.content.runnable() {
        errors.push(format!(
            "The project is not allowed to run on this computer ({}). Open it in Patchwork \
             and allow it (or save it as your own work) before exporting.",
            graph.content.label()
        ));
    }

    // Exactly one visual output.
    let output = match super::player::select_output(&graph, request.output) {
        Ok(id) => Some(id),
        Err(e) => {
            errors.push(e);
            None
        }
    };

    // Node types: restricted, credentials, external tools.
    let needs_ffmpeg = check_nodes(&graph, &mut errors);

    // Custom nodes: packages bundled by the save planner; loose files must
    // be inside the project; everything allowed here and intact.
    let staged_project_hint = request.destination.join(PROJECT_DIR);
    let packages = match crate::library::project::plan_project_packages(
        &graph,
        &staged_project_hint,
        Some(&source),
        env.library_root.as_deref(),
    ) {
        Ok(mut plan) => {
            // The planner judges path-only files against the export; those
            // inside the source project are copied with it (below), so only
            // files outside the source project are a problem.
            let inside: Vec<String> = plan
                .external_custom_nodes
                .iter()
                .filter(|f| crate::library::project::is_within(Path::new(f.as_str()), &source))
                .cloned()
                .collect();
            plan.external_custom_nodes.retain(|f| !inside.contains(f));
            plan.warnings
                .retain(|w| !inside.iter().any(|f| w.contains(f.as_str())));
            for external in &plan.external_custom_nodes {
                errors.push(format!(
                    "Custom node {external} is outside the project and not a package. Move it \
                     into the project folder or publish it to the Library, then export again."
                ));
            }
            Some(plan)
        }
        Err(e) => {
            errors.push(e);
            None
        }
    };
    graph.set_package_environment(Some(source.clone()), env.library_root.clone());
    graph.set_trust_policy(env.trust.clone());
    for blocked in graph.preflight_custom_nodes() {
        let inner = blocked.innermost();
        errors.push(format!(
            "Custom node {} cannot be exported: {}",
            inner.package.label,
            super::player::consent::package_reason(inner)
        ));
    }
    let mut project_files = Vec::new();
    let loose = collect_loose_custom_nodes(&graph, &source, &mut errors);
    project_files.extend(loose.iter().cloned());
    if let Some(plan) = &packages {
        for dir in plan.to_copy.iter().map(|c| c.source_dir.clone()) {
            for file in pwnode_files(&dir) {
                check_custom_node_paths(&file, &mut errors);
            }
        }
    }
    for item in &loose {
        check_custom_node_paths(&item.from, &mut errors);
    }

    // Assets: copy and rewrite (in the export copy only).
    let mut export_graph = graph.clone();
    let staged_project = staged_project_hint.clone();
    project_files.extend(plan_assets(
        &mut export_graph,
        &source,
        &staged_project,
        &mut errors,
    ));
    let mut probe = export_graph.clone();
    super::io::relativize_paths(&mut probe, &staged_project.to_string_lossy());
    if let Some(plan) = &packages {
        super::io::apply_project_package_paths(&mut probe, plan);
    }
    for path in absolute_strings(&serde_json::to_value(&probe).unwrap_or_default()) {
        errors.push(format!(
            "The project still refers to {path}, which an exported app cannot reach. Move the \
             file into the project folder or clear that setting, then export again."
        ));
    }

    // Licensing: a distributable build needs everything the runtime embeds
    // cleared; an internal test build is exported, labelled as such.
    match request.purpose {
        ExportPurpose::Distribution => {
            for (what, why) in &env.unresolved {
                errors.push(format!(
                    "Patchwork embeds {what} in every app, and {why}. A distributable app \
                     cannot be exported until this is resolved (see \
                     licenses/third-party/README.md); an internal test build can."
                ));
            }
        }
        ExportPurpose::InternalTest => warnings.push(format!(
            "{}: this export is for internal development and testing only and must not be \
             distributed ({}).",
            runtime::INTERNAL_TEST_LABEL,
            runtime::INTERNAL_TEST_MARKER
        )),
    }

    // Runtime.
    let RuntimeCheck {
        files: runtime_files,
        mut dependencies,
        build,
        notices: dependency_notices,
    } = check_runtime(request, env, &mut errors);
    if let Some(build) = &build {
        if build.exact_commit().is_none() {
            warnings.push(format!(
                "The runtime was built from {} rather than an unmodified commit. The \
                 exported app's notices say so; whoever distributes it must also provide the \
                 exact corresponding source code (GPL-3.0).",
                match (build.modified, build.commit.as_str()) {
                    (_, "unknown") => "source of unknown version".to_string(),
                    (Some(true), commit) => format!("commit {commit} with local changes"),
                    (_, commit) => format!("commit {commit} (changes unknown)"),
                }
            ));
        }
    }
    let mut ffmpeg_licences = Vec::new();
    let mut runtime_files = runtime_files;
    match (&request.ffmpeg, needs_ffmpeg) {
        (Some(ffmpeg), true) => match check_ffmpeg(ffmpeg) {
            Ok((exe, licences)) => {
                runtime_files.push(exe);
                ffmpeg_licences = licences;
                dependencies.push("ffmpeg.exe (provided)".to_string());
            }
            Err(e) => errors.push(e),
        },
        (None, true) => errors.push(
            "The project plays or captures video, which needs ffmpeg. Provide a redistributable \
             ffmpeg.exe and its licence notices; none is downloaded or taken from this computer."
                .to_string(),
        ),
        (Some(_), false) => warnings.push(
            "ffmpeg was provided but the project does not need it; not included.".to_string(),
        ),
        (None, false) => {}
    }

    if let Some(plan) = &packages {
        warnings.extend(plan.warnings.iter().cloned());
    }
    if !errors.is_empty() {
        errors.sort();
        errors.dedup();
        return Err((errors, warnings));
    }
    Ok(ExportPlan {
        graph: export_graph,
        output: output.expect("checked"),
        packages: packages.expect("checked"),
        project_files,
        runtime_files,
        dependencies,
        warnings,
        ffmpeg_licences,
        build,
        dependency_notices,
        not_for_distribution: (request.purpose == ExportPurpose::InternalTest)
            .then(|| runtime::not_for_distribution_text(&request.app_name, &env.unresolved)),
    })
}

/// The app name becomes `<name>.exe` and the title: a plain file name.
pub fn check_app_name(name: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("The application name '{name}' {why}."));
    if name.trim().is_empty() {
        return bad("is empty");
    }
    if name.chars().count() > MAX_APP_NAME {
        return bad(&format!("is longer than {MAX_APP_NAME} characters"));
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return bad("contains characters not allowed in a file name");
    }
    if name.ends_with('.') || name.ends_with(' ') || name.starts_with(' ') {
        return bad("starts or ends with a space or dot");
    }
    if is_reserved_name(name) {
        return bad("is reserved by Windows");
    }
    Ok(())
}

fn is_reserved_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
}

fn is_link(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink() || crate::library::bundle::is_reparse_point(meta)
}

/// The destination: absolute, new (or an export being replaced on
/// request), in an existing ordinary folder, outside the project, the
/// runtime's folder and protected folders.
fn check_destination(
    request: &ExportRequest,
    env: &ExportEnvironment,
    source: &Path,
) -> Result<(), String> {
    let dest = &request.destination;
    if !dest.is_absolute() {
        return Err("The export destination must be an absolute path.".to_string());
    }
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or("The export destination has no folder name.")?;
    check_app_name(&name)
        .map_err(|_| format!("The export folder name '{name}' is not acceptable."))?;
    let parent = dest
        .parent()
        .ok_or("The export destination has no parent folder.")?;
    let parent_meta = std::fs::symlink_metadata(parent)
        .map_err(|_| format!("The folder {} does not exist.", parent.display()))?;
    if is_link(&parent_meta) || !parent_meta.is_dir() {
        return Err(format!(
            "{} must be an ordinary folder (not a link).",
            parent.display()
        ));
    }
    let parent = std::fs::canonicalize(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let target = parent.join(&name);
    if target.starts_with(source) || source.starts_with(&target) {
        return Err("The export cannot be inside the project, or contain it.".to_string());
    }
    let mut protected: Vec<PathBuf> = env.protected.clone();
    if let Some(dir) = request.runtime.executable.parent() {
        protected.push(dir.to_path_buf());
    }
    protected.extend(request.runtime.dependency_dirs.iter().cloned());
    for dir in protected {
        if let Ok(dir) = std::fs::canonicalize(&dir) {
            if target.starts_with(&dir) {
                return Err(format!(
                    "Exports cannot be written into {} (a protected or application folder).",
                    dir.display()
                ));
            }
        }
    }
    match std::fs::symlink_metadata(&target) {
        Err(_) => Ok(()),
        Ok(meta) if !request.overwrite => {
            let _ = meta;
            Err(format!(
                "{} already exists. Choose another folder, or confirm replacing the earlier export.",
                target.display()
            ))
        }
        Ok(meta) => {
            if is_link(&meta) || !meta.is_dir() || !target.join(MANIFEST_FILE).is_file() {
                Err(format!(
                    "{} exists and is not an earlier Patchwork export; it will not be replaced.",
                    target.display()
                ))
            } else {
                Ok(())
            }
        }
    }
}

/// Reject node types an exported app cannot or may not run. Returns
/// whether ffmpeg is needed.
fn check_nodes(graph: &Graph, errors: &mut Vec<String>) -> bool {
    use crate::library::marketplace::Capability;
    use crate::library::marketplace::scan::{NodeClass, classify};
    let mut needs_ffmpeg = false;
    let mut ids: Vec<&NodeId> = graph.nodes.keys().collect();
    ids.sort_unstable();
    for id in ids {
        let node = &graph.nodes[id];
        let value = serde_json::to_value(&node.node_type).unwrap_or_default();
        let name = match classify(&value) {
            Ok((name, NodeClass::Forbidden(why))) => {
                errors.push(format!(
                    "Node #{id} ({name}) cannot be exported: it {why}. Exported apps never \
                     include native plugins or editor tools."
                ));
                continue;
            }
            Ok((name, NodeClass::Uses(caps))) if caps.contains(&Capability::AiCredentials) => {
                errors.push(format!(
                    "Node #{id} ({name}) needs private API credentials, which are never \
                     exported. Remove it to export this project."
                ));
                continue;
            }
            Ok((name, _)) => name,
            Err(why) => {
                errors.push(format!("Node #{id} cannot be exported: {why}."));
                continue;
            }
        };
        let state = match &node.node_type {
            NodeType::Dynamic { inner } => inner.node.save_state(),
            _ => serde_json::Value::Null,
        };
        match name.as_str() {
            "VideoPlayer" | "Camera" | "web_app" | "web_stream" => needs_ffmpeg = true,
            "video_in" => {
                needs_ffmpeg = true;
                if state.get("source").and_then(|s| s.as_str()) == Some("Url") {
                    errors.push(format!(
                        "Node #{id} (Video In) streams from a URL, which needs yt-dlp; exported \
                         apps cannot include it. Cache the video into the project or use a file."
                    ));
                }
            }
            "video_out" => {
                let sink = state
                    .get("sink")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default();
                if sink == "FileRecord" {
                    errors.push(format!(
                        "Node #{id} (Video Out) records to a file; exported apps do not record."
                    ));
                }
            }
            "tts" => errors.push(format!(
                "Node #{id} (Text to Speech) needs speech tools installed on the computer; \
                 exported apps cannot include them."
            )),
            _ => {}
        }
    }
    needs_ffmpeg
}

/// Path-only custom nodes (and the path-only nodes they use): copied with
/// the project, so they must be inside it.
fn collect_loose_custom_nodes(
    graph: &Graph,
    source: &Path,
    errors: &mut Vec<String>,
) -> Vec<CopyItem> {
    let mut out: BTreeMap<String, CopyItem> = BTreeMap::new();
    let mut pending: Vec<(PathBuf, usize)> = graph
        .nodes
        .values()
        .filter_map(|node| match &node.node_type {
            NodeType::CustomNodeInstance {
                file,
                package: None,
                ..
            } if !file.is_empty()
                && !crate::library::package::is_inside_published_package(Path::new(file)) =>
            {
                Some((PathBuf::from(file), 0))
            }
            _ => None,
        })
        .collect();
    while let Some((file, depth)) = pending.pop() {
        if depth > crate::library::bundle::MAX_DEPENDENCY_DEPTH {
            errors.push(format!("Custom node {} nests too deeply.", file.display()));
            continue;
        }
        let Ok(canonical) = std::fs::canonicalize(&file) else {
            errors.push(format!("Custom node {} was not found.", file.display()));
            continue;
        };
        let Ok(relative) = canonical.strip_prefix(source) else {
            continue; // reported by the package planner (external_custom_nodes)
        };
        let to = relative.to_string_lossy().replace('\\', "/");
        if out.contains_key(&to) {
            continue;
        }
        out.insert(
            to.clone(),
            CopyItem {
                from: canonical.clone(),
                to,
            },
        );
        // Nested path-only references, relative to this file.
        let Ok(text) = std::fs::read_to_string(&canonical) else {
            continue;
        };
        let parent = canonical.parent().unwrap_or(source).to_path_buf();
        for (nested, packaged) in nested_custom_nodes(&text) {
            if packaged || nested.is_empty() {
                continue;
            }
            let path = Path::new(&nested);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                parent.join(path)
            };
            pending.push((path, depth + 1));
        }
    }
    out.into_values().collect()
}

/// Nested custom-node references in a `.pwnode` text: (file, packaged).
pub(crate) fn nested_custom_nodes(text: &str) -> Vec<(String, bool)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let Some(nodes) = value.pointer("/graph/nodes").and_then(|n| n.as_object()) else {
        return Vec::new();
    };
    nodes
        .values()
        .filter_map(|node| node.pointer("/node_type/CustomNodeInstance"))
        .map(|instance| {
            (
                instance
                    .get("file")
                    .and_then(|f| f.as_str())
                    .unwrap_or_default()
                    .to_string(),
                instance.get("package").is_some_and(|p| !p.is_null()),
            )
        })
        .collect()
}

/// Asset paths inside custom nodes have no project-relative resolution:
/// an exported app could not find them, so they must not be set.
fn check_custom_node_paths(file: &Path, errors: &mut Vec<String>) {
    let Ok(text) = std::fs::read_to_string(file) else {
        errors.push(format!("Custom node {} cannot be read.", file.display()));
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        errors.push(format!("Custom node {} is not valid JSON.", file.display()));
        return;
    };
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for path in absolute_strings(&value) {
        errors.push(format!(
            "Custom node {name} refers to {path}; files used inside custom nodes are not \
             exported yet."
        ));
    }
    if let Some(graph) = value.get("graph") {
        if let Ok(graph) = serde_json::from_value::<Graph>(graph.clone()) {
            for (field, path) in asset_fields(&graph) {
                if !path.is_empty() {
                    errors.push(format!(
                        "Custom node {name} uses a file ({field}: {path}); files used inside \
                         custom nodes are not exported yet."
                    ));
                }
            }
        }
    }
}

/// Every `.pwnode` file under `dir`.
fn pwnode_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("pwnode"))
            {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Strings anywhere in `value` that are absolute Windows paths (drive or
/// UNC). Exported projects must contain none.
fn absolute_strings(value: &serde_json::Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    fn walk(value: &serde_json::Value, out: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::String(s) => {
                if is_absolute_windows_path(s) {
                    out.insert(s.clone());
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            serde_json::Value::Object(map) => map.values().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    walk(value, &mut out);
    out.into_iter().collect()
}

fn is_absolute_windows_path(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || s.starts_with("\\\\")
        || s.starts_with("//")
}

/// The asset paths a graph uses, by field (the fields saved relative to
/// the project and resolved against it on load).
fn asset_fields(graph: &Graph) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for node in graph.nodes.values() {
        match &node.node_type {
            NodeType::ImageNode {
                path, save_path, ..
            } => {
                out.push(("image", path.clone()));
                out.push(("image save path", save_path.clone()));
            }
            NodeType::AudioPlayer { file_path, .. } => out.push(("audio", file_path.clone())),
            NodeType::VideoPlayer { path, .. } => out.push(("video", path.clone())),
            NodeType::MlModel {
                model_path,
                labels_path,
                ..
            } => {
                out.push(("model", model_path.clone()));
                out.push(("model labels", labels_path.clone()));
            }
            _ => {}
        }
    }
    out
}

/// Plan the asset copies and point the export graph's fields at their
/// place in the staged project (made relative when written). Files inside
/// the project keep their place; outside files go to `assets/<kind>/`.
fn plan_assets(
    graph: &mut Graph,
    source: &Path,
    staged_project: &Path,
    errors: &mut Vec<String>,
) -> Vec<CopyItem> {
    let mut targets: HashMap<PathBuf, String> = HashMap::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut items = Vec::new();
    let mut cached_copies = Vec::new();
    let mut assign = |path: &mut String, kind: &str, errors: &mut Vec<String>| {
        if path.trim().is_empty() {
            return;
        }
        let Some(from) = asset_source(path, errors) else {
            return;
        };
        let to = if let Some(to) = targets.get(&from) {
            to.clone()
        } else {
            let to = match from.strip_prefix(source) {
                Ok(inside) => inside.to_string_lossy().replace('\\', "/"),
                Err(_) => unique_asset_name(kind, &from, &taken),
            };
            taken.insert(to.to_lowercase());
            targets.insert(from.clone(), to.clone());
            items.push(CopyItem {
                from: from.clone(),
                to: to.clone(),
            });
            to
        };
        *path = staged_project.join(&to).to_string_lossy().into_owned();
    };
    let mut ids: Vec<NodeId> = graph.nodes.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let Some(node) = graph.nodes.get_mut(&id) else {
            continue;
        };
        match &mut node.node_type {
            NodeType::ImageNode {
                path,
                save_path,
                cached_file,
                ..
            } => {
                assign(path, "images", errors);
                if !save_path.trim().is_empty() {
                    errors.push(format!(
                        "Node #{id} (Image) saves images to {save_path}; exported apps do not \
                         write files. Clear the save path to export."
                    ));
                }
                // Per-user cache: never exported (rebuilt from `path`).
                *cached_file = String::new();
            }
            NodeType::AudioPlayer { file_path, .. } => assign(file_path, "audio", errors),
            NodeType::VideoPlayer { path, .. } => assign(path, "video", errors),
            NodeType::MlModel {
                model_path,
                labels_path,
                ..
            } => {
                assign(model_path, "models", errors);
                assign(labels_path, "models", errors);
            }
            // Path-only custom nodes inside the project are copied to the
            // same place (see `collect_loose_custom_nodes`).
            NodeType::CustomNodeInstance {
                file,
                package: None,
                ..
            } if !file.is_empty() => {
                if let Some(inside) = std::fs::canonicalize(&*file)
                    .ok()
                    .and_then(|p| p.strip_prefix(source).ok().map(Path::to_path_buf))
                {
                    *file = staged_project.join(inside).to_string_lossy().into_owned();
                }
            }
            NodeType::Dynamic { inner } if inner.node.type_tag() == "video_in" => {
                // A cached copy inside the project (`assets/video_in/…`).
                let state = inner.node.save_state();
                if let Some(cached) = state.get("cached_path").and_then(|c| c.as_str()) {
                    if !cached.is_empty() {
                        match crate::app::player::manifest::check_relative_project_path(cached)
                            .ok()
                            .and_then(|()| std::fs::canonicalize(source.join(cached)).ok())
                            .filter(|p| p.starts_with(source) && p.is_file())
                        {
                            Some(from) => cached_copies.push(CopyItem {
                                from,
                                to: cached.replace('\\', "/"),
                            }),
                            None => errors.push(format!(
                                "Node #{id} (Video In) cached file {cached} is missing."
                            )),
                        }
                    }
                }
            }
            _ => {}
        }
    }
    drop(assign);
    items.extend(cached_copies);
    items
}

/// An asset path that must exist as an ordinary file (not a link).
fn asset_source(path: &str, errors: &mut Vec<String>) -> Option<PathBuf> {
    let p = Path::new(path);
    if !p.is_absolute() {
        errors.push(format!("The file {path} cannot be located."));
        return None;
    }
    match std::fs::symlink_metadata(p) {
        Ok(meta) if is_link(&meta) => {
            errors.push(format!(
                "{path} is a link; exports only copy ordinary files."
            ));
            None
        }
        Ok(meta) if meta.is_file() => std::fs::canonicalize(p).ok(),
        _ => {
            errors.push(format!("The file {path} used by the project is missing."));
            None
        }
    }
}

/// `assets/<kind>/<name>`, with `-2`, `-3`… when another file already has
/// that name (case-insensitively, as on Windows).
fn unique_asset_name(kind: &str, from: &Path, taken: &BTreeSet<String>) -> String {
    let name = sanitize_file_name(
        &from
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (name[..dot].to_string(), name[dot..].to_string()),
        _ => (name.clone(), String::new()),
    };
    let mut candidate = format!("{ASSETS_DIR}/{kind}/{stem}{ext}");
    let mut n = 2;
    while taken.contains(&candidate.to_lowercase()) {
        candidate = format!("{ASSETS_DIR}/{kind}/{stem}-{n}{ext}");
        n += 1;
    }
    candidate
}

/// A file name safe on Windows (Unicode kept).
fn sanitize_file_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() {
        out = "file".to_string();
    }
    if is_reserved_name(&out) {
        out.insert(0, '_');
    }
    out
}

struct RuntimeCheck {
    files: Vec<CopyItem>,
    dependencies: Vec<String>,
    build: Option<pe::BuildInfo>,
    notices: BTreeMap<String, String>,
}

/// The runtime: the release Player build (GUI subsystem: no console), and
/// every DLL it, or a DLL it needs, imports that Windows does not provide,
/// approved by the dependency rules ([`runtime`]); never from system
/// folders.
fn check_runtime(
    request: &ExportRequest,
    env: &ExportEnvironment,
    errors: &mut Vec<String>,
) -> RuntimeCheck {
    let mut check = RuntimeCheck {
        files: Vec::new(),
        dependencies: Vec::new(),
        build: None,
        notices: BTreeMap::new(),
    };
    let exe = &request.runtime.executable;
    let bytes = match std::fs::symlink_metadata(exe) {
        Ok(meta) if meta.is_file() && !is_link(&meta) => match std::fs::read(exe) {
            Ok(bytes) => bytes,
            Err(e) => {
                errors.push(format!("The runtime {} cannot be read: {e}", exe.display()));
                return check;
            }
        },
        _ => {
            errors.push(format!("The runtime {} was not found.", exe.display()));
            return check;
        }
    };
    match pe::build_profile(&bytes) {
        Ok(pe::BuildProfile::Release) => {}
        Ok(pe::BuildProfile::Debug) => errors.push(format!(
            "The runtime {} is a debug build; only a release build can be exported.",
            exe.display()
        )),
        Err(why) => errors.push(format!(
            "The runtime {} cannot be used: {why}.",
            exe.display()
        )),
    }
    match pe::build_info(&bytes) {
        Ok(info) if info.flavor == launch::Binary::Player.flavor() => {
            if let Some((commit, modified)) = &env.runtime_source {
                if info.commit != *commit || info.modified != *modified {
                    errors.push(format!(
                        "The runtime {} was built from different source ({}) than this \
                         Patchwork ({}), so the licence notices would not describe it. Build \
                         both together (`cargo build --release`) and export again.",
                        exe.display(),
                        info.commit,
                        commit
                    ));
                }
            }
            check.build = Some(info)
        }
        Ok(info) => errors.push(format!(
            "The runtime {} is the Patchwork {} build; exports use the Player runtime \
             ({}), which has no editor and opens no console window.",
            exe.display(),
            info.flavor,
            runtime::PLAYER_EXE
        )),
        Err(why) => errors.push(format!(
            "The runtime {} cannot be used: {why}.",
            exe.display()
        )),
    }
    match pe::subsystem(&bytes) {
        Ok(pe::SUBSYSTEM_GUI) => {}
        Ok(_) => errors.push(format!(
            "The runtime {} is a console program; an exported app must not open a console \
             window.",
            exe.display()
        )),
        Err(why) => errors.push(format!(
            "The runtime {} cannot be used: {why}.",
            exe.display()
        )),
    }
    let imports = match pe::imported_dlls(&bytes) {
        Ok(imports) => imports,
        Err(why) => {
            errors.push(format!(
                "The runtime {} cannot be used: {why}.",
                exe.display()
            ));
            return check;
        }
    };
    let system_root = std::env::var_os("SystemRoot").and_then(|s| std::fs::canonicalize(s).ok());
    let mut dirs: Vec<PathBuf> = exe.parent().map(Path::to_path_buf).into_iter().collect();
    dirs.extend(request.runtime.dependency_dirs.iter().cloned());
    for dir in &dirs {
        if let (Some(root), Ok(dir)) = (&system_root, std::fs::canonicalize(dir)) {
            if dir.starts_with(root) {
                errors.push(format!(
                    "{} is a Windows system folder; dependencies are never copied from it.",
                    dir.display()
                ));
            }
        }
    }
    // Every native dependency, including those of bundled DLLs.
    let mut seen = BTreeSet::new();
    let mut pending: Vec<(String, String)> = imports
        .into_iter()
        .rev()
        .map(|dll| (dll, "The runtime".to_string()))
        .collect();
    let mut vc_redist: Option<PathBuf> = None;
    while let Some((dll, needed_by)) = pending.pop() {
        if pe::is_system_dll(&dll) || !seen.insert(dll.to_ascii_lowercase()) {
            continue;
        }
        let Some(found) = find_file(&dirs, &dll) else {
            errors.push(format!(
                "{needed_by} needs {dll}, which was not found next to the runtime or in the \
                 dependency folders. {}",
                if runtime::is_vc_runtime(&dll) {
                    "Install Visual Studio's C++ redistributable files and export again."
                } else {
                    "Rebuild Patchwork so its own copy is beside it, and export again."
                }
            ));
            continue;
        };
        match runtime::approve(&env.dependencies, &dll, &found) {
            Ok(approval) => {
                match &approval {
                    runtime::Approval::Pinned(index) => {
                        let pinned = &env.dependencies.pinned[*index];
                        check
                            .notices
                            .insert(pinned.notice_file.clone(), pinned.notice.clone());
                    }
                    runtime::Approval::VcRuntime { redist } => {
                        if vc_redist.as_ref().is_some_and(|r| r != redist) {
                            errors.push(
                                "The Visual C++ runtime files must all come from one \
                                 redistributable folder."
                                    .to_string(),
                            );
                        }
                        vc_redist = Some(redist.clone());
                    }
                }
                match std::fs::read(&found)
                    .map_err(|e| e.to_string())
                    .and_then(|b| pe::imported_dlls(&b))
                {
                    Ok(more) => pending.extend(more.into_iter().rev().map(|d| (d, dll.clone()))),
                    Err(why) => errors.push(format!("{}: {why}", found.display())),
                }
                // Keep the file's own name (Windows matches it either way).
                let name = found
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dll.clone());
                check.files.push(CopyItem {
                    from: found,
                    to: name,
                });
                check.dependencies.push(dll);
            }
            Err(why) => errors.push(why),
        }
    }
    if let Some(redist) = vc_redist {
        check.notices.insert(
            "VCRuntime-NOTICE.txt".to_string(),
            format!(
                "{}\r\nSource folder: {}\r\n",
                runtime::VC_RUNTIME_NOTICE,
                redist.display()
            ),
        );
    }
    check.files.sort_by(|a, b| a.to.cmp(&b.to));
    check.dependencies.sort_by_key(|d| d.to_ascii_lowercase());
    check.files.insert(
        0,
        CopyItem {
            from: exe.clone(),
            to: format!("{}.exe", request.app_name),
        },
    );
    check.dependencies.insert(
        0,
        format!("{}.exe (Patchwork Player runtime)", request.app_name),
    );
    check
}

/// `name` in the first of `dirs` that has it (case-insensitively), as an
/// ordinary file.
fn find_file(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(name)
            {
                if let Ok(meta) = std::fs::symlink_metadata(entry.path()) {
                    if meta.is_file() && !is_link(&meta) {
                        return Some(entry.path());
                    }
                }
            }
        }
    }
    None
}

fn check_ffmpeg(ffmpeg: &FfmpegSource) -> Result<(CopyItem, Vec<CopyItem>), String> {
    let meta = std::fs::symlink_metadata(&ffmpeg.executable)
        .map_err(|_| format!("ffmpeg {} was not found.", ffmpeg.executable.display()))?;
    if !meta.is_file() || is_link(&meta) {
        return Err(format!(
            "ffmpeg {} is not an ordinary file.",
            ffmpeg.executable.display()
        ));
    }
    let bytes = std::fs::read(&ffmpeg.executable).map_err(|e| format!("ffmpeg: {e}"))?;
    pe::imported_dlls(&bytes)
        .map_err(|why| format!("ffmpeg {}: {why}", ffmpeg.executable.display()))?;
    if ffmpeg.licence_files.is_empty() {
        return Err("ffmpeg can only be included with its licence notices.".to_string());
    }
    let mut licences = Vec::new();
    for file in &ffmpeg.licence_files {
        match std::fs::symlink_metadata(file) {
            Ok(meta) if meta.is_file() && !is_link(&meta) => licences.push(CopyItem {
                from: file.clone(),
                to: format!(
                    "{LICENSES_DIR}/ffmpeg/{}",
                    sanitize_file_name(&file.file_name().unwrap_or_default().to_string_lossy())
                ),
            }),
            _ => {
                return Err(format!(
                    "ffmpeg licence notice {} was not found.",
                    file.display()
                ));
            }
        }
    }
    Ok((
        CopyItem {
            from: ffmpeg.executable.clone(),
            to: "ffmpeg.exe".to_string(),
        },
        licences,
    ))
}

// ── Writing ─────────────────────────────────────────────────────────────────

/// A hidden staging folder beside the destination (same volume, so the
/// final rename is atomic).
fn staging_dir(destination: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    destination.with_file_name(format!(".{name}.patchwork-export-{nanos}"))
}

fn copy_into(root: &Path, item: &CopyItem, control: &ExportControl) -> Result<(), String> {
    use std::io::{Read, Write};
    let target = root.join(&item.to);
    if !target.starts_with(root) || item.to.split('/').any(|s| s == ".." || s.is_empty()) {
        return Err(format!("Refusing to write outside the export: {}", item.to));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let failed = |e: std::io::Error| format!("Copying {} failed: {e}", item.from.display());
    let mut from = std::fs::File::open(&item.from).map_err(failed)?;
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(failed)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        if control.cancelled() {
            return Err(CANCELLED.to_string());
        }
        let n = from.read(&mut buffer).map_err(failed)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        to.write_all(&buffer[..n]).map_err(failed)?;
        control.add_bytes(n as u64);
    }
    to.sync_all().map_err(failed)?;
    drop(to);
    let copied: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    // What was read is what was written, and the source did not change.
    if copied != sha256_file(&target)? || copied != sha256_file(&item.from)? {
        return Err(format!(
            "{} changed while it was copied.",
            item.from.display()
        ));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
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

/// Build the export in `staging` and verify it. Returns the digest.
fn write_export(
    request: &ExportRequest,
    env: &ExportEnvironment,
    plan: &ExportPlan,
    staging: &Path,
    control: &ExportControl,
) -> Result<String, String> {
    let total: u64 = plan
        .project_files
        .iter()
        .chain(&plan.runtime_files)
        .chain(&plan.ffmpeg_licences)
        .filter_map(|item| std::fs::metadata(&item.from).ok())
        .map(|meta| meta.len())
        .sum();
    control.set_step("Copying files", 0, total);
    std::fs::create_dir(staging).map_err(|e| format!("Cannot create the staging folder: {e}"))?;
    let project = staging.join(PROJECT_DIR);
    std::fs::create_dir(&project).map_err(|e| format!("Cannot create the project folder: {e}"))?;

    // Assets and path-only custom nodes, then the project itself through
    // the save pipeline (packages copied and their paths applied).
    for item in &plan.project_files {
        copy_into(&project, item, control)?;
    }
    let staged_hint = request.destination.join(PROJECT_DIR);
    let mut graph = plan.graph.clone();
    super::io::relativize_paths(&mut graph, &staged_hint.to_string_lossy());
    super::io::apply_project_package_paths(&mut graph, &plan.packages);
    let json = serde_json::to_string_pretty(&super::io::ProjectFile::exported(graph))
        .map_err(|e| format!("The project cannot be written: {e}"))?;
    // The plan was made for a destination that did not exist, so every
    // package is copied (from the source project or the Library).
    if !plan.packages.reused.is_empty() {
        return Err("The package plan expected packages already in the export.".to_string());
    }
    crate::library::project::commit_project_save(&project, &plan.packages, &json)?;

    for item in &plan.runtime_files {
        copy_into(staging, item, control)?;
    }
    for item in &plan.ffmpeg_licences {
        copy_into(staging, item, control)?;
    }
    write_notices(staging, request, plan)?;
    if control.cancelled() {
        return Err(CANCELLED.to_string());
    }

    #[cfg(test)]
    if FAIL_BEFORE_MANIFEST.with(|f| f.get()) {
        return Err("injected failure (test)".to_string());
    }

    // The digest of exactly what was written, then the manifest.
    let text = std::fs::read_to_string(project.join(PROJECT_FILE))
        .map_err(|e| format!("The exported project cannot be read: {e}"))?;
    let digest = crate::library::trust::content::project_digest(&text)?;
    let manifest = serde_json::json!({
        "schema": MANIFEST_SCHEMA,
        "format": MANIFEST_FORMAT,
        "project": PROJECT_DIR,
        "output": plan.output,
        "title": request.app_name,
        "patchwork_version": env!("CARGO_PKG_VERSION"),
        "project_sha256": digest,
    });
    std::fs::write(
        staging.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    )
    .map_err(|e| format!("player.json cannot be written: {e}"))?;

    let progress = control.progress();
    control.set_step(
        "Verifying the export",
        progress.done_bytes,
        progress.total_bytes,
    );
    verify_export(staging, env, plan, &digest)?;
    Ok(digest)
}

fn write_notices(staging: &Path, request: &ExportRequest, plan: &ExportPlan) -> Result<(), String> {
    let licenses = staging.join(LICENSES_DIR);
    std::fs::create_dir_all(&licenses).map_err(|e| format!("{e}"))?;
    let write = |name: &str, text: &str| {
        std::fs::write(licenses.join(name), text).map_err(|e| format!("{name}: {e}"))
    };
    write("Patchwork-LICENSE.txt", PATCHWORK_LICENSE)?;
    write(
        "SOURCE.txt",
        &source_notice(&request.app_name, plan.build.as_ref()),
    )?;
    for (name, text) in &plan.dependency_notices {
        write(name, &text.replace("\r\n", "\n").replace('\n', "\r\n"))?;
    }
    std::fs::create_dir_all(licenses.join(THIRD_PARTY_DIR)).map_err(|e| format!("{e}"))?;
    for (name, text) in runtime::THIRD_PARTY_FILES {
        write(&format!("{THIRD_PARTY_DIR}/{name}"), text)?;
    }
    let mut notices = String::new();
    if let Some(text) = &plan.not_for_distribution {
        write(runtime::NOT_FOR_DISTRIBUTION_NOTICE, text)?;
        std::fs::write(staging.join(runtime::INTERNAL_TEST_MARKER), text)
            .map_err(|e| format!("{}: {e}", runtime::INTERNAL_TEST_MARKER))?;
        notices.push_str(&format!(
            "*** {} ***\r\nThis app must not be distributed: see {} (and ../{}).\r\n\r\n",
            runtime::INTERNAL_TEST_LABEL,
            runtime::NOT_FOR_DISTRIBUTION_NOTICE,
            runtime::INTERNAL_TEST_MARKER
        ));
    }
    notices.push_str(&format!(
        "{name} is a project played by the Patchwork {version} Player runtime.\r\n\r\n\
         Patchwork is free software under the GNU General Public License v3 \
         (Patchwork-LICENSE.txt). Where to get its corresponding source: SOURCE.txt.\r\n\r\n\
         This folder also contains, unmodified:\r\n",
        name = request.app_name,
        version = env!("CARGO_PKG_VERSION"),
    ));
    for dep in &plan.dependencies {
        notices.push_str(&format!("  - {dep}\r\n"));
    }
    notices.push_str("\r\nTheir licence terms:\r\n");
    for name in plan.dependency_notices.keys() {
        notices.push_str(&format!("  - {name}\r\n"));
    }
    notices.push_str(
        "\r\nThe runtime itself also contains, compiled in: Rust crates, ONNX Runtime, \
         libopus, HIDAPI, ring and fonts. Their notices are in third-party/ (index: \
         third-party/README.txt):\r\n",
    );
    for (name, _) in runtime::THIRD_PARTY_FILES {
        notices.push_str(&format!("  - third-party/{name}\r\n"));
    }
    if !plan.ffmpeg_licences.is_empty() {
        notices.push_str("  - ffmpeg/ (the notices provided with the bundled ffmpeg)\r\n");
    }
    notices.push_str(
        "\r\nThe project content is identified by player.json's project_sha256. This is \
         an identity, not a signature: it does not say who made the project. The first \
         time it runs on a computer, the Player asks before running it.\r\n",
    );
    write("THIRD-PARTY-NOTICES.txt", &notices)
}

/// `licenses/SOURCE.txt`: exactly which source the runtime was built from.
pub fn source_notice(app_name: &str, build: Option<&pe::BuildInfo>) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut text = format!(
        "Corresponding source of {app_name}'s runtime\r\n\r\n\
         {app_name}.exe is the Patchwork {version} Player runtime, licensed under the GNU \
         General Public License v3 (Patchwork-LICENSE.txt).\r\n\r\n\
         Source repository: {SOURCE_URL}\r\n"
    );
    match build {
        Some(info) if info.exact_commit().is_some() => text.push_str(&format!(
            "Source commit: {}\r\n\r\n\
             The runtime was built from exactly this commit, without changes.\r\n",
            info.commit
        )),
        Some(info) if info.commit != "unknown" => text.push_str(&format!(
            "Source commit: {} ({})\r\n\r\n\
             The runtime was NOT built from this commit alone: its source had changes that \
             are not in the repository. That commit is therefore not its corresponding \
             source; the exact source used must be provided with this app.\r\n",
            info.commit,
            if info.modified == Some(true) {
                "with local changes"
            } else {
                "changes unknown"
            }
        )),
        _ => text.push_str(
            "Source commit: unknown\r\n\r\n\
             The runtime does not record which source it was built from; the exact source \
             used must be provided with this app.\r\n",
        ),
    }
    text.push_str(
        "\r\nA repository link alone does not meet every obligation of the GPL. Whoever \
         distributes this app must make its complete corresponding source available as \
         section 6 of the licence requires (for example, alongside the app, or with a \
         written offer), for as long as they distribute it.\r\n",
    );
    text
}

/// Check the staged export as the Player will see it.
fn verify_export(
    staging: &Path,
    env: &ExportEnvironment,
    plan: &ExportPlan,
    digest: &str,
) -> Result<(), String> {
    // The manifest launch (structure, containment, no links).
    let launch = super::player::manifest::load_manifest(staging, env.user_data.as_deref())
        .map_err(|e| format!("The export does not launch: {e}"))?;
    // The project loads from its new place, and is the content recorded.
    let graph = super::io::load_project_graph(&launch.launch.project_json)
        .map_err(|e| format!("The exported project does not load: {e}"))?;
    let text = std::fs::read_to_string(&launch.launch.project_json).map_err(|e| format!("{e}"))?;
    if crate::library::trust::content::project_digest(&text)? != digest {
        return Err("The exported project changed while it was written.".to_string());
    }
    if super::player::select_output(&graph, Some(plan.output)).is_err() {
        return Err("The exported project lost its output.".to_string());
    }
    // Every asset resolves inside the exported project.
    for (field, path) in asset_fields(&graph) {
        if !path.is_empty() && !Path::new(&path).starts_with(&launch.launch.project_dir) {
            return Err(format!(
                "The exported {field} path {path} is outside the export."
            ));
        }
        if !path.is_empty() && !Path::new(&path).is_file() {
            return Err(format!("The exported {field} {path} is missing."));
        }
    }
    // Packages resolve from the export alone and pass verification.
    let mut resolver = crate::library::resolve::PackageResolver::new(
        Some(launch.launch.project_dir.clone()),
        None,
    );
    for node in graph.nodes.values() {
        if let NodeType::CustomNodeInstance {
            file,
            package: Some(package),
            ..
        } = &node.node_type
        {
            resolver
                .resolve_instance(file, Some(package))
                .map_err(|e| format!("Exported package {package} does not verify: {e}"))?;
        }
    }
    // The program is the Player runtime: no console window, no editor.
    let exe = plan
        .runtime_files
        .first()
        .ok_or("The export has no program.")?;
    let bytes = std::fs::read(staging.join(&exe.to)).map_err(|e| format!("{}: {e}", exe.to))?;
    if pe::subsystem(&bytes) != Ok(pe::SUBSYSTEM_GUI)
        || pe::build_info(&bytes).map(|info| info.flavor).as_deref()
            != Ok(launch::Binary::Player.flavor())
    {
        return Err(format!("{} is not the Player runtime.", exe.to));
    }
    for notice in [
        "Patchwork-LICENSE.txt",
        "SOURCE.txt",
        "THIRD-PARTY-NOTICES.txt",
        "third-party/rust-crates.txt",
        "third-party/onnxruntime-ThirdPartyNotices.txt",
    ] {
        if !staging.join(LICENSES_DIR).join(notice).is_file() {
            return Err(format!("The export is missing {LICENSES_DIR}/{notice}."));
        }
    }
    // P6 Stage 2.6: labelled exactly as what it is. A distributable build
    // embeds nothing uncleared and carries no internal marker; an internal
    // test build carries both markers.
    let markers = [
        staging.join(runtime::INTERNAL_TEST_MARKER),
        staging
            .join(LICENSES_DIR)
            .join(runtime::NOT_FOR_DISTRIBUTION_NOTICE),
    ];
    match &plan.not_for_distribution {
        None if !env.unresolved.is_empty() => {
            return Err("Uncleared components would be in a distributable build.".to_string());
        }
        None if markers.iter().any(|m| m.exists()) => {
            return Err("A distributable build carries an internal test marker.".to_string());
        }
        Some(_) if !markers.iter().all(|m| m.is_file()) => {
            return Err(format!(
                "The internal test build is missing its {} label.",
                runtime::INTERNAL_TEST_MARKER
            ));
        }
        _ => {}
    }
    // Nothing secret, personal or trust-related.
    for file in list_files(staging) {
        let name = file
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if FORBIDDEN_NAMES.contains(&name.as_str()) {
            return Err(format!("The export would contain {file}; refused."));
        }
    }
    Ok(())
}

/// Move the staged export into place (replacing an earlier export only
/// when that was confirmed).
fn finish(staging: &Path, destination: &Path, overwrite: bool) -> Result<(), String> {
    if destination.exists() {
        if !overwrite {
            return Err(format!(
                "{} appeared during the export.",
                destination.display()
            ));
        }
        let old = staging.with_extension("previous");
        std::fs::rename(destination, &old)
            .map_err(|e| format!("The earlier export cannot be replaced: {e}"))?;
        if let Err(e) = std::fs::rename(staging, destination) {
            let _ = std::fs::rename(&old, destination);
            return Err(format!("The export cannot be moved into place: {e}"));
        }
        let _ = std::fs::remove_dir_all(&old);
        return Ok(());
    }
    std::fs::rename(staging, destination)
        .map_err(|e| format!("The export cannot be moved into place: {e}"))
}

/// Files under `root`, relative, `/`-separated, sorted.
pub fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
thread_local! {
    /// Test hook: fail late in `write_export` (after most files are staged).
    pub(crate) static FAIL_BEFORE_MANIFEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests;
