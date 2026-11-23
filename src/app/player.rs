//! P6 Stage 1: Player mode.
//!
//! `patchwork --player <project folder | project.json> [--output <node id>]`
//! runs a saved project without the editor:
//!
//! - **Loading** uses [`super::io::load_project_graph`], the loader that
//!   reads and identifies a project without writing anything. Player mode
//!   never saves the project, the session, recents or keys.
//! - **Running** uses the editor's own frame
//!   ([`super::PatchworkApp::run_graph_frame`]) and the same per-frame
//!   content resolution and rights. A project that this computer has not
//!   authored or allowed does not run until the user explicitly allows it
//!   on the consent screen (P6 Stage 2.1, [`consent`]); the Player never
//!   allows anything by itself. Package permissions apply as in the editor.
//! - **Editing** is impossible: the document is read-only, and none of the
//!   editor's chrome, canvas input, shortcuts, file drop, MCP or system-node
//!   actions run. Node bodies are still drawn each frame (part of their
//!   runtime work, e.g. audio sources, happens there) but far outside the
//!   window, disabled by the read-only document.
//! - **Visual output** is the image feeding the project's Video Out node
//!   with the Window / Display destination: the output the project
//!   declares for a screen. With none, or several and no `--output`, the
//!   Player shows why instead of guessing.
//!
//! P6.1 (reliability):
//! - The output is shown only with the Devices right, the right the Video
//!   Out node's Window / Display sink checks ([`output_permitted`]).
//! - The window goes to the node's saved display, else the primary
//!   ([`choose_display`]); Escape leaves fullscreen.
//! - MIDI Out nodes connect to their saved ports ([`MidiOutReconnect`]), and
//!   Video In cameras set to "Start when played" start ([`player_autostart`]).
//!   Both go through the usual managers and permission checks.
//!
//! P6 Stage 2.1 (exported apps):
//! - A `player.json` beside the executable ([`manifest`]) makes the program
//!   that project's Player: arguments are ignored, an invalid manifest is
//!   shown (never the editor), and packages come from the bundle only.
//! - Every launch decides before running ([`consent::decide`]): approved,
//!   unchanged content runs; otherwise the consent screen asks, and only an
//!   explicit "Allow to run" records approval, with the editor's mechanisms.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;

use crate::graph::{Graph, ImageData, NodeId, NodeType, PortValue};

pub mod consent;
pub mod diagnostics;
pub mod manifest;

/// The flag selecting player mode (first argument).
pub const PLAYER_FLAG: &str = "--player";
/// Chooses the Video Out node when a project has several.
pub const OUTPUT_FLAG: &str = "--output";

const PROJECT_FILE: &str = "project.json";
const VIDEO_OUT_TAG: &str = "video_out";
const WINDOW_SINK: &str = "Window";
/// Node bodies are drawn here, far outside any window (see module docs).
const OFFSCREEN: egui::Vec2 = egui::vec2(-1.0e7, -1.0e7);

pub const USAGE: &str =
    "usage: patchwork --player <project folder | project.json> [--output <node id>]";

/// A validated player launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerLaunch {
    /// The project folder (absolute).
    pub project_dir: PathBuf,
    /// Its `project.json` (absolute).
    pub project_json: PathBuf,
    /// The Video Out node chosen with `--output` (or by the manifest).
    pub output: Option<NodeId>,
    /// P6 Stage 2.1: set when launched from a `player.json` beside the
    /// executable (an exported app).
    pub export: Option<manifest::ExportInfo>,
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMode {
    /// `patchwork catalogue …` (P4.3.4), unchanged.
    Catalogue(Vec<String>),
    Player(PlayerLaunch),
    /// P6 Stage 2.1: a valid `player.json` beside the executable.
    Exported(PlayerLaunch),
    /// P6 Stage 2.1: a `player.json` that cannot be used. The Player shows
    /// why; the editor is never started instead.
    ExportInvalid {
        export_dir: PathBuf,
        error: String,
    },
    Editor,
}

/// What the Player starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerStart {
    Launch(PlayerLaunch),
    /// An exported app whose manifest cannot be used.
    Invalid {
        export_dir: PathBuf,
        error: String,
    },
}

/// P6 Stage 2.1: the launch mode for an executable in `exe_dir`. A
/// `player.json` there makes this the exported app: its arguments are
/// ignored (no other project, no catalogue tool, no editor). Without one,
/// [`launch_mode`] decides as before.
pub fn launch_mode_at(args: &[String], exe_dir: &Path) -> Result<LaunchMode, String> {
    if !manifest::manifest_present(exe_dir) {
        return launch_mode(args);
    }
    if !args.is_empty() {
        crate::system_log::warn(format!(
            "Player: {} command-line argument(s) ignored: this application plays its \
             bundled project only.",
            args.len()
        ));
    }
    Ok(
        match manifest::load_manifest(exe_dir, manifest::trust_dir().as_deref()) {
            Ok(exported) => LaunchMode::Exported(exported.launch),
            Err(error) => LaunchMode::ExportInvalid {
                export_dir: exe_dir.to_path_buf(),
                error,
            },
        },
    )
}

/// Decide the launch mode from the arguments after the program name.
/// Without `catalogue` or `--player` the editor starts exactly as before
/// (other arguments are ignored, as they always were).
pub fn launch_mode(args: &[String]) -> Result<LaunchMode, String> {
    match args.first().map(String::as_str) {
        Some("catalogue") => Ok(LaunchMode::Catalogue(args[1..].to_vec())),
        Some(PLAYER_FLAG) => parse_player_args(&args[1..]).map(LaunchMode::Player),
        _ if args.iter().any(|a| a == PLAYER_FLAG || a == OUTPUT_FLAG) => Err(format!(
            "{PLAYER_FLAG} must be the first argument.\n{USAGE}"
        )),
        _ => Ok(LaunchMode::Editor),
    }
}

/// Parse `<path> [--output <node id>]` (in either order).
pub fn parse_player_args(args: &[String]) -> Result<PlayerLaunch, String> {
    let mut path: Option<&str> = None;
    let mut output: Option<NodeId> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == OUTPUT_FLAG {
            if output.is_some() {
                return Err(format!("{OUTPUT_FLAG} given twice.\n{USAGE}"));
            }
            let value = iter
                .next()
                .ok_or_else(|| format!("{OUTPUT_FLAG} needs a node id.\n{USAGE}"))?;
            output = Some(
                value
                    .parse::<NodeId>()
                    .ok()
                    .filter(|id| *id > 0)
                    .ok_or_else(|| format!("{OUTPUT_FLAG} must be a node id, not '{value}'."))?,
            );
        } else if arg.starts_with("--") {
            return Err(format!("Unknown option '{arg}'.\n{USAGE}"));
        } else if path.is_some() {
            return Err(format!("Only one project can be played.\n{USAGE}"));
        } else {
            path = Some(arg);
        }
    }
    let path = path.ok_or_else(|| format!("No project given.\n{USAGE}"))?;
    let (project_dir, project_json) = resolve_project_path(Path::new(path))?;
    Ok(PlayerLaunch {
        project_dir,
        project_json,
        output,
        export: None,
    })
}

/// A project is a folder containing `project.json`. Accept the folder or
/// that file itself; refuse anything else with a reason.
pub fn resolve_project_path(path: &Path) -> Result<(PathBuf, PathBuf), String> {
    if path.as_os_str().is_empty() {
        return Err("No project given.".to_string());
    }
    let absolute = std::path::absolute(path)
        .map_err(|e| format!("{} is not a usable path: {e}", path.display()))?;
    let meta = std::fs::metadata(&absolute)
        .map_err(|_| format!("Project not found: {}", absolute.display()))?;
    let json = if meta.is_dir() {
        let json = absolute.join(PROJECT_FILE);
        if !json.is_file() {
            return Err(format!(
                "{} is not a Patchwork project: it has no {PROJECT_FILE}.",
                absolute.display()
            ));
        }
        json
    } else {
        let is_project_file = absolute
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(PROJECT_FILE));
        if !is_project_file {
            return Err(format!(
                "{} is not a project. Give a project folder or its {PROJECT_FILE}.",
                absolute.display()
            ));
        }
        absolute
    };
    let dir = json
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no project folder.", json.display()))?;
    Ok((dir, json))
}

/// Load the project without writing anything (the editor's interactive
/// loader also migrates legacy files and records recents; the Player does
/// neither). The graph's content state is `Pending` until the ledger is
/// consulted, so it cannot run by being loaded.
pub fn load_player_graph(launch: &PlayerLaunch) -> Result<Graph, String> {
    super::io::load_project_graph(&launch.project_json)
        .map_err(|e| format!("Cannot load {}: {e}", launch.project_json.display()))
}

/// The Video Out nodes sending to a window / display, by id.
pub fn window_outputs(graph: &Graph) -> Vec<NodeId> {
    let mut ids: Vec<NodeId> = graph
        .nodes
        .iter()
        .filter(|(_, node)| match &node.node_type {
            NodeType::Dynamic { inner } => {
                inner.node.type_tag() == VIDEO_OUT_TAG
                    && inner.node.save_state().get("sink").and_then(|s| s.as_str())
                        == Some(WINDOW_SINK)
            }
            _ => false,
        })
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Choose the output to show. Never guesses between several.
pub fn select_output(graph: &Graph, requested: Option<NodeId>) -> Result<NodeId, String> {
    let outputs = window_outputs(graph);
    let list = || {
        outputs
            .iter()
            .map(|id| format!("#{id}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match requested {
        Some(id) if outputs.contains(&id) => Ok(id),
        Some(id) => Err(if outputs.is_empty() {
            format!(
                "Node #{id} is not a Video Out node with the Window / Display destination, \
                 and the project has none."
            )
        } else {
            format!(
                "Node #{id} is not a Video Out node with the Window / Display destination. \
                 Choose one of: {}.",
                list()
            )
        }),
        None => match outputs.as_slice() {
            [only] => Ok(*only),
            [] => Err(
                "This project has no visual output for the Player. Add a Video Out node \
                 with the Window / Display destination and connect the image to show."
                    .to_string(),
            ),
            _ => Err(format!(
                "This project has several Video Out (Window / Display) nodes: {}. Choose one \
                 with {OUTPUT_FLAG} <node id>.",
                list()
            )),
        },
    }
}

/// The image feeding `output` this frame, with its GPU source when there
/// is one: the same resolution the Video Out window sink uses.
pub fn output_image(
    graph: &Graph,
    values: &std::collections::HashMap<(NodeId, usize), PortValue>,
    output: NodeId,
) -> Option<(Arc<ImageData>, Option<(NodeId, usize)>)> {
    let conn = graph
        .connections
        .iter()
        .find(|c| c.to_node == output && c.to_port == 0)?;
    match values.get(&(conn.from_node, conn.from_port))? {
        PortValue::Image(img) => Some((img.clone(), Some((conn.from_node, conn.from_port)))),
        PortValue::GpuImage(handle) => Some((
            Arc::new(ImageData {
                width: handle.width,
                height: handle.height,
                pixels: Vec::new(),
            }),
            Some((handle.node_id, handle.port)),
        )),
        _ => None,
    }
}

/// P6.1: whether the Player may show its output on a display. The Video
/// Out node's Window / Display sink requires the Devices capability (it
/// drives a display); the Player's own window is that same output, so it
/// is held to the same check, in the same context (the document's rights,
/// entered for the frame). Nothing is granted here: without Devices the
/// output is not shown.
pub fn output_permitted() -> bool {
    crate::capability::still_allowed(crate::capability::Capability::Devices)
}

/// P6.1: the display a Video Out node's `destination` names, falling back
/// to the primary display (then the first) when it is empty or no longer
/// attached: the rule the Video Out node uses itself. The project's saved
/// setting is never changed.
pub fn choose_display<'a>(
    displays: &'a [crate::display::Display],
    destination: &str,
) -> Option<&'a crate::display::Display> {
    crate::display::choose(displays, destination)
}

/// P6.1: Escape leaves fullscreen. It does nothing else (in particular it
/// never closes the Player; closing the window and Alt+F4 still do).
pub fn escape_leaves_fullscreen(escape_pressed: bool, fullscreen: Option<bool>) -> bool {
    escape_pressed && fullscreen == Some(true)
}

/// Seconds between attempts to reconnect one MIDI Out node's port.
pub const MIDI_OUT_RETRY_SECS: f64 = 5.0;

/// One step the MIDI Out reconnect asks the MIDI manager to take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MidiOutStep {
    Connect {
        node_id: NodeId,
        port_name: String,
    },
    /// The port is no longer attached: drop the stale connection so the
    /// node reconnects when the device comes back.
    Disconnect {
        node_id: NodeId,
    },
}

/// P6.1: MIDI Out nodes connect to their saved port in the Player.
///
/// In the editor a MIDI Out connects when the user picks its port or
/// presses Connect (unchanged). The Player has no such controls, so it
/// connects saved ports itself, like MIDI In does in both. Unlike MIDI In
/// it does not try every frame: only when the port is attached (the port
/// list is refreshed in the background), and at most once every
/// [`MIDI_OUT_RETRY_SECS`] per node. A missing port is reported once and
/// waited for. Connecting still goes through the MIDI manager, which checks
/// the Devices capability.
#[derive(Debug, Default)]
pub struct MidiOutReconnect {
    /// Last attempt per node: the port and when.
    attempts: std::collections::HashMap<NodeId, (String, f64)>,
    /// Nodes whose missing port has been reported.
    reported_missing: std::collections::HashSet<NodeId>,
}

impl MidiOutReconnect {
    /// Plan this frame's steps. `outputs` lists each MIDI Out node's saved
    /// port and whether it is connected; `available` is the attached ports.
    pub fn plan(
        &mut self,
        outputs: &[(NodeId, String, bool)],
        available: &[String],
        now: f64,
    ) -> Vec<MidiOutStep> {
        let mut steps = Vec::new();
        self.attempts
            .retain(|id, _| outputs.iter().any(|(node, _, _)| node == id));
        self.reported_missing
            .retain(|id| outputs.iter().any(|(node, _, _)| node == id));
        for (node_id, port_name, connected) in outputs {
            if port_name.is_empty() {
                continue;
            }
            let attached = available.iter().any(|p| p == port_name);
            if !attached {
                if *connected {
                    steps.push(MidiOutStep::Disconnect { node_id: *node_id });
                }
                if self.reported_missing.insert(*node_id) {
                    crate::system_log::warn(format!(
                        "Player: MIDI Out #{node_id}: port '{port_name}' is not attached; \
                         it will connect when it appears."
                    ));
                }
                continue;
            }
            self.reported_missing.remove(node_id);
            if *connected {
                continue;
            }
            let recent = self.attempts.get(node_id).is_some_and(|(port, at)| {
                port == port_name && now - at < MIDI_OUT_RETRY_SECS && now >= *at
            });
            if recent {
                continue;
            }
            self.attempts.insert(*node_id, (port_name.clone(), now));
            steps.push(MidiOutStep::Connect {
                node_id: *node_id,
                port_name: port_name.clone(),
            });
        }
        steps
    }
}

/// egui memory key telling nodes the Player is running a project, so nodes
/// configured to start when played (Video In "Start when played") start.
/// Set by the Player each frame; never set in the editor.
const PLAYER_AUTOSTART_KEY: &str = "patchwork_player_autostart";

/// Whether nodes should honour their "start when played" setting now.
pub(crate) fn player_autostart(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::new(PLAYER_AUTOSTART_KEY)))
        .unwrap_or(false)
}

fn set_player_autostart(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(PLAYER_AUTOSTART_KEY), on));
}

/// The Player's document: a read-only project. Every graph-changing entry
/// point checks this (`active_is_editable` / `require_editable`), node
/// bodies are disabled, and the document never becomes dirty.
pub(super) fn player_document(document: &mut crate::document::Document) {
    document.kind = crate::document::DocumentKind::Project;
    document.read_only = true;
    document.is_dirty = false;
}

/// Player state kept by the app.
pub(super) struct PlayerState {
    pub(super) launch: PlayerLaunch,
    /// The output to show, or why the project cannot be played.
    pub(super) output: Result<NodeId, String>,
    /// Window title and placement applied.
    configured: bool,
    /// MIDI Out ports connected by the Player (P6.1).
    midi_out: MidiOutReconnect,
    /// P6 Stage 2.1: the digest the project was loaded as (its content
    /// identity), for consent and the export check.
    content_digest: Option<String>,
    /// The user allowed running despite a change since export (this run).
    change_acknowledged: bool,
    /// Package refusals found without loading anything, per trust
    /// generation (recomputed after any decision).
    preflight: Option<(u64, Vec<crate::library::trust::authorize::Blocked>)>,
    /// Consent screen: the "I trust this content" tick, and the last error.
    consent_ticked: bool,
    consent_error: Option<String>,
    /// P6 Stage 2.8: model preloading, provider and rate diagnostics.
    diagnostics: diagnostics::PlayerDiagnostics,
}

impl PlayerState {
    pub(super) fn new(launch: PlayerLaunch, output: Result<NodeId, String>) -> Self {
        Self {
            launch,
            output,
            configured: false,
            midi_out: MidiOutReconnect::default(),
            content_digest: None,
            change_acknowledged: false,
            preflight: None,
            consent_ticked: false,
            consent_error: None,
            diagnostics: Default::default(),
        }
    }

    /// The name shown for the project: the export's title, else its folder.
    fn project_name(&self) -> String {
        match &self.launch.export {
            Some(export) => export.title.clone(),
            None => self
                .launch
                .project_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

/// The content digest a freshly loaded graph was identified as.
fn loaded_digest(graph: &Graph) -> Option<String> {
    use crate::library::trust::content::ContentState;
    match &graph.content {
        ContentState::Pending { digest, .. } | ContentState::Denied { digest, .. } => {
            Some(digest.clone())
        }
        ContentState::Unverified { digest, .. } => digest.clone(),
        ContentState::Authored | ContentState::Allowed(_) => None,
    }
}

impl super::PatchworkApp {
    /// P6 Stage 2.1: whether this is an exported app (bundled project only).
    pub(super) fn player_isolated(&self) -> bool {
        self.player
            .as_ref()
            .is_some_and(|p| p.launch.export.is_some())
    }

    /// Set up player mode: a read-only project document and the loaded
    /// graph, or (an unusable export) only the reason. Nothing is written;
    /// failures are kept and shown.
    pub(super) fn start_player(&mut self, start: PlayerStart) {
        let launch = match start {
            PlayerStart::Launch(launch) => launch,
            PlayerStart::Invalid { export_dir, error } => {
                eprintln!("patchwork: {error}");
                crate::system_log::error(format!("Player: {error}"));
                player_document(self.documents.active_mut());
                self.sync_active_document();
                let launch = PlayerLaunch {
                    project_json: export_dir.join(manifest::MANIFEST_FILE),
                    project_dir: export_dir,
                    output: None,
                    export: None,
                };
                self.player = Some(PlayerState::new(
                    launch,
                    Err(format!("This application cannot start: {error}")),
                ));
                return;
            }
        };
        let dir = launch.project_dir.display().to_string();
        let mut content_digest = None;
        let output = match load_player_graph(&launch) {
            Ok(graph) => {
                let selected = select_output(&graph, launch.output);
                content_digest = loaded_digest(&graph);
                self.graph = graph;
                self.graph.audio_topology_dirty = true;
                selected
            }
            Err(error) => Err(error),
        };
        if let Err(error) = &output {
            eprintln!("patchwork {PLAYER_FLAG}: {error}");
            crate::system_log::error(format!("Player: {error}"));
        } else {
            crate::system_log::log(format!("Player: playing {}", launch.project_json.display()));
        }
        self.project_path = Some(dir);
        // Keys are read from the per-user store only (never migrated).
        self.api_keys = super::io::load_user_keys();
        self.pinned_nodes.clear();
        self.canvas_offset = OFFSCREEN;
        self.canvas_zoom = 1.0;
        self.target_zoom = 1.0;
        self.is_dirty = false;
        self.caches_dirty = true;
        player_document(self.documents.active_mut());
        self.sync_active_document();
        let mut state = PlayerState::new(launch, output);
        state.content_digest = content_digest;
        self.player = Some(state);
    }

    /// Package refusals for the current trust generation (computed without
    /// loading or running anything).
    fn player_preflight(&mut self) -> Vec<crate::library::trust::authorize::Blocked> {
        let generation = crate::library::trust::generation();
        if let Some((cached, blocked)) = self.player.as_ref().and_then(|p| p.preflight.as_ref()) {
            if *cached == generation {
                return blocked.clone();
            }
        }
        let blocked = self.graph.preflight_custom_nodes();
        if let Some(state) = self.player.as_mut() {
            state.preflight = Some((generation, blocked.clone()));
        }
        blocked
    }

    /// The run decision for this frame (see [`consent::decide`]).
    fn player_gate(&mut self) -> consent::Gate {
        super::provenance::resolve_graph(&mut self.graph);
        self.set_graph_package_environment();
        let blocked = self.player_preflight();
        let uses = super::provenance::graph_uses(&self.graph);
        let Some(state) = self.player.as_ref() else {
            return consent::Gate::Refused {
                title: String::new(),
                detail: String::new(),
            };
        };
        let name = state.project_name();
        consent::decide(&consent::GateInput {
            project_name: &name,
            content: &self.graph.content,
            digest: state.content_digest.as_deref(),
            expected: state
                .launch
                .export
                .as_ref()
                .map(|e| e.expected_digest.as_str()),
            change_acknowledged: state.change_acknowledged,
            uses,
            blocked: &blocked,
        })
    }

    /// Record the user's explicit approval, with Patchwork's own trust
    /// mechanisms: the project's exact content and the capabilities it uses
    /// (as the editor's Permissions window does), and each package by its
    /// own content. Nothing else is granted.
    fn apply_player_consent(&mut self, request: &consent::ConsentRequest) -> Result<(), String> {
        if request.project_decision {
            self.decide_active_content(true)?;
            crate::system_log::log(format!(
                "Player: this project was allowed ({}).",
                request.permissions.describe()
            ));
        }
        let policy = super::permissions::editor_trust_policy();
        for blocked in &request.packages {
            let what = super::permissions::decide_package(&policy, &blocked.package, true)?;
            crate::system_log::log(format!("Player: {what} allowed."));
        }
        if let Some(state) = self.player.as_mut() {
            if request.changed_from.is_some() {
                state.change_acknowledged = true;
            }
            state.preflight = None;
            state.consent_ticked = false;
            state.consent_error = None;
        }
        Ok(())
    }

    /// The consent screen (and nothing else runs meanwhile).
    fn player_consent_screen(&mut self, ctx: &egui::Context, request: &consent::ConsentRequest) {
        let Some(state) = self.player.as_mut() else {
            return;
        };
        let mut ticked = state.consent_ticked;
        let error = state.consent_error.clone();
        let mut allow = false;
        let mut quit = false;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(egui::Color32::from_gray(20))
                    .inner_margin(24.0),
            )
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(egui::Color32::from_gray(220));
                egui::ScrollArea::vertical().show(ui, |ui| {
                    consent_body(ui, request);
                    ui.add_space(16.0);
                    ui.checkbox(&mut ticked, "I trust this content and allow it to run");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        allow = ui
                            .add_enabled(ticked, egui::Button::new("Allow to run"))
                            .clicked();
                        quit = ui.button("Quit").clicked();
                    });
                    if let Some(error) = &error {
                        ui.add_space(8.0);
                        ui.colored_label(egui::Color32::from_rgb(255, 120, 120), error);
                    }
                });
            });
        if let Some(state) = self.player.as_mut() {
            state.consent_ticked = ticked;
        }
        if quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if allow && ticked {
            if let Err(why) = self.apply_player_consent(request) {
                crate::system_log::error(format!("Player: {why}"));
                if let Some(state) = self.player.as_mut() {
                    state.consent_error = Some(why);
                }
            }
        }
    }

    /// While the Player is not running the project: every subsystem sees
    /// an empty graph (as the editor parks a document that may not run),
    /// so anything opened earlier closes and nothing of the project runs.
    fn park_player_frame(&mut self, ctx: &egui::Context) {
        let graph = std::mem::replace(&mut self.graph, Graph::new());
        let _ = self.run_graph_frame(ctx);
        self.sync_audio_engine();
        self.graph = graph;
    }

    /// One Player frame. Replaces the editor frame entirely.
    pub(super) fn player_frame(&mut self, ctx: &egui::Context) {
        let Some(state) = self.player.as_mut() else {
            return;
        };
        let output = state.output.clone();
        if !state.configured {
            state.configured = true;
            let title = match &state.launch.export {
                Some(export) => export.title.clone(),
                None => format!("Patchwork Player — {}", state.project_name()),
            };
            let title = player_title(title, super::export::runtime::internal_test_label());
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
            if let Ok(node) = output {
                self.place_player_window(ctx, node);
            }
        }
        // P6 Stage 2.6: a runtime that embeds uncleared components says so
        // on screen too (the title is hidden in fullscreen).
        if let Some(label) = super::export::runtime::internal_test_label() {
            internal_test_badge(ctx, label);
        }
        // P6.1: Escape leaves fullscreen (and does nothing else).
        let (escape, fullscreen) =
            ctx.input(|i| (i.key_pressed(egui::Key::Escape), i.viewport().fullscreen));
        if escape_leaves_fullscreen(escape, fullscreen) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        let node = match output {
            Ok(node) => node,
            Err(error) => {
                player_message(ctx, "This project cannot be played", &error);
                return;
            }
        };

        // P6 Stage 2.1: decide first (this computer's records, the export
        // check, packages), then run only what was allowed. Content that is
        // not allowed never reaches graph evaluation.
        let gate = self.player_gate();
        let _document_rights = self.document_rights().enter();
        let Some(values) = consent::run_if_permitted(&gate, || self.run_graph_frame(ctx)) else {
            self.park_player_frame(ctx);
            match &gate {
                consent::Gate::Consent(request) => self.player_consent_screen(ctx, request),
                consent::Gate::Refused { title, detail } => player_message(ctx, title, detail),
                consent::Gate::Run => {}
            }
            return;
        };
        // Allowed, but the frame found it may not run after all (e.g. a
        // decision changed meanwhile): it was parked; show nothing of it.
        if !self.graph.content.runnable() {
            self.stop_player_audio();
            return;
        }

        // P6 Stage 2.8: allowed to run: build the tracking models it uses in
        // the background now (with this document's rights), instead of one
        // after another as the first frames reach each node.
        if let Some(state) = self.player.as_mut() {
            if !state.diagnostics.preloaded {
                state.diagnostics.preloaded = true;
                for tag in diagnostics::tracking_types(&self.graph) {
                    crate::ml::session_cache::preload_bundled(crate::ml::bundled::tracking_models(
                        tag,
                    ));
                }
            }
        }

        // P6.1: MIDI Out nodes connect to their saved ports (Player only).
        self.reconnect_midi_outputs(ctx.input(|i| i.time));

        // P6.1: the output is shown only with the right the Video Out node
        // itself needs (checked in this document's context).
        let permitted = output_permitted();
        let image = output_image(&self.graph, &values, node);
        let blocked = self.graph.blocked_packages().len();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                if !permitted {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.4);
                        ui.label(
                            egui::RichText::new(
                                "This project has not been allowed to use devices, so its \
                                 video output is not shown. Allow it in Patchwork.",
                            )
                            .color(egui::Color32::from_rgb(255, 180, 80)),
                        );
                    });
                } else if let Some((img, source)) = image.as_ref() {
                    show_fitted(ui, node, img, *source);
                }
                if blocked > 0 {
                    ui.put(
                        egui::Rect::from_min_size(
                            ui.max_rect().left_bottom() + egui::vec2(12.0, -32.0),
                            egui::vec2(ui.max_rect().width() - 24.0, 24.0),
                        ),
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "{blocked} package(s) need permission and are not running. \
                                 Allow them in Patchwork."
                            ))
                            .color(egui::Color32::from_rgb(255, 180, 80)),
                        ),
                    );
                }
            });

        // P6 Stage 2.8: loading / CPU-fallback notes, and details on F12.
        let camera = diagnostics::camera_frame(&self.graph, &values);
        let cameras = diagnostics::camera_states(ctx, &self.graph);
        if let Some(state) = self.player.as_mut() {
            state.diagnostics.show(
                ctx,
                image.as_ref().filter(|_| permitted).map(|(img, _)| img),
                camera.as_ref(),
                &cameras,
            );
        }

        // Node bodies: their runtime work, drawn outside the window and
        // disabled (read-only document). Nothing is pinned in a Player.
        self.canvas_offset = OFFSCREEN;
        self.canvas_zoom = 1.0;
        self.target_zoom = 1.0;
        self.node_double_click_consumed = false;
        // Nodes set to start when played (e.g. Video In) start now.
        set_player_autostart(ctx, true);
        self.render_nodes_filtered(ctx, &values, false);
        // Same audio sync as the editor (after node drawing).
        self.sync_audio_engine();
        self.sync_console_messages();
        ctx.request_repaint();
    }

    /// The audio engine sees no nodes: chains stop, nothing starts.
    fn stop_player_audio(&mut self) {
        let graph = std::mem::replace(&mut self.graph, Graph::new());
        self.sync_audio_engine();
        self.graph = graph;
    }

    /// Connect MIDI Out nodes to their saved ports (see [`MidiOutReconnect`]).
    fn reconnect_midi_outputs(&mut self, now: f64) {
        let outputs: Vec<(NodeId, String, bool)> = self
            .graph
            .nodes
            .iter()
            .filter_map(|(id, node)| match &node.node_type {
                NodeType::MidiOut { port_name, .. } => {
                    Some((*id, port_name.clone(), self.midi.is_output_connected(*id)))
                }
                _ => None,
            })
            .collect();
        if outputs.is_empty() {
            return;
        }
        let available = self.midi.cached_output_ports.clone();
        let Some(state) = self.player.as_mut() else {
            return;
        };
        let actions: Vec<crate::midi::MidiAction> = state
            .midi_out
            .plan(&outputs, &available, now)
            .into_iter()
            .map(|step| match step {
                MidiOutStep::Connect { node_id, port_name } => {
                    crate::midi::MidiAction::ConnectOutput { node_id, port_name }
                }
                MidiOutStep::Disconnect { node_id } => {
                    crate::midi::MidiAction::DisconnectOutput { node_id }
                }
            })
            .collect();
        if !actions.is_empty() {
            self.midi.process(actions);
        }
    }

    /// Put the window on the output's chosen display, fullscreen when the
    /// Video Out node asks for it (its saved settings).
    fn place_player_window(&mut self, ctx: &egui::Context, node: NodeId) {
        let Some(NodeType::Dynamic { inner }) = self.graph.nodes.get(&node).map(|n| &n.node_type)
        else {
            return;
        };
        let state = inner.node.save_state();
        let destination = state
            .get("destination")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string();
        let fullscreen = state
            .get("fullscreen")
            .and_then(|f| f.as_bool())
            .unwrap_or(false);
        // P6.1: the saved display, else the primary (as the node does).
        // With no displays enumerated (e.g. a headless session) the OS
        // places the window, which is the primary display.
        let displays = crate::display::enumerate_displays();
        if let Some(display) = choose_display(&displays, &destination) {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                display.origin.0 as f32,
                display.origin.1 as f32,
            )));
        }
        if fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
        }
    }
}

/// Draw `img` as large as fits, centred, keeping its aspect ratio.
fn show_fitted(
    ui: &mut egui::Ui,
    node: NodeId,
    img: &Arc<ImageData>,
    source: Option<(NodeId, usize)>,
) {
    if img.width == 0 || img.height == 0 {
        return;
    }
    let avail = ui.available_size();
    let img_aspect = img.width as f32 / img.height.max(1) as f32;
    let screen_aspect = avail.x / avail.y.max(1.0);
    let (draw_w, draw_h) = if img_aspect > screen_aspect {
        (avail.x, avail.x / img_aspect)
    } else {
        (avail.y * img_aspect, avail.y)
    };
    let pad_y = ((avail.y - draw_h) / 2.0).max(0.0);
    let pad_x = ((avail.x - draw_w) / 2.0).max(0.0);
    ui.add_space(pad_y);
    ui.horizontal(|ui| {
        ui.add_space(pad_x);
        crate::nodes::image_node::show_image_gpu(ui, node, img, draw_w, source);
    });
}

/// What the consent screen shows (everything the approval covers).
fn consent_body(ui: &mut egui::Ui, request: &consent::ConsentRequest) {
    let warn = egui::Color32::from_rgb(255, 180, 80);
    ui.heading(egui::RichText::new("Allow this project to run?").color(egui::Color32::WHITE));
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(&request.project_name)
            .strong()
            .size(16.0),
    );
    if let Some(digest) = &request.content_digest {
        ui.label("Content identity (SHA-256 of the project):");
        ui.monospace(digest);
    }
    ui.small(
        "This identifies the exact content. It is not a signature: it does not say who \
         made it.",
    );
    if let Some(expected) = &request.changed_from {
        ui.add_space(8.0);
        ui.colored_label(
            warn,
            "Warning: this project's content differs from the content recorded when it \
             was exported. It may have been modified.",
        );
        ui.monospace(format!("Recorded at export: {expected}"));
    }
    ui.add_space(12.0);
    if request.project_decision {
        ui.strong("This project has not been allowed to run on this computer.");
        ui.label(format!(
            "Allowing it approves exactly these permissions: {}.",
            request.permissions.describe()
        ));
    } else if request.changed_from.is_some() {
        ui.label("This content is already allowed on this computer.");
    }
    if !request.packages.is_empty() {
        ui.add_space(12.0);
        ui.strong("Packages it uses (each is allowed separately, by its own content):");
        for blocked in &request.packages {
            package_entry(ui, blocked);
        }
    }
    if !request.unavailable.is_empty() {
        ui.add_space(12.0);
        ui.strong("Not available (these will not run):");
        for blocked in &request.unavailable {
            package_entry(ui, blocked);
        }
    }
    ui.add_space(12.0);
    ui.colored_label(warn, super::permissions::NOT_SANDBOXED);
}

fn package_entry(ui: &mut egui::Ui, blocked: &crate::library::trust::authorize::Blocked) {
    let package = &blocked.package;
    ui.group(|ui| {
        ui.label(format!("{} — {}", package.name, package.label));
        ui.small(consent::package_reason(blocked));
        if let Some(identity) = &package.identity {
            ui.monospace(format!("Content: {}", identity.tree_sha256));
        } else if let Some(digest) = &package.loose_digest {
            ui.monospace(format!("Content: {digest}"));
        }
        let detected: Vec<String> = package.detected.iter().map(|c| format!("{c:?}")).collect();
        ui.small(format!(
            "Permissions: {}",
            if detected.is_empty() {
                "none".to_string()
            } else {
                detected.join(", ")
            }
        ));
    });
}

/// P6 Stage 2.6: the window title, labelled when this runtime is an
/// internal test build. The label comes from the binary itself, never from
/// the export's files, so it cannot be edited away.
pub(super) fn player_title(title: String, internal_test: Option<&str>) -> String {
    match internal_test {
        Some(label) => format!("{title} — {label}"),
        None => title,
    }
}

/// P6 Stage 2.6: a small, always-visible label in the corner.
fn internal_test_badge(ctx: &egui::Context, label: &str) {
    egui::Area::new(egui::Id::new("player_internal_test_badge"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::LEFT_BOTTOM, [8.0, -8.0])
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 170))
                .inner_margin(egui::Margin::symmetric(6, 3))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(label)
                            .size(11.0)
                            .color(egui::Color32::from_rgb(240, 170, 60)),
                    );
                });
        });
}

/// A full-window message instead of output.
fn player_message(ctx: &egui::Context, title: &str, detail: &str) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::from_gray(20)))
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.heading(egui::RichText::new(title).color(egui::Color32::WHITE));
                ui.add_space(12.0);
                ui.label(egui::RichText::new(detail).color(egui::Color32::LIGHT_GRAY));
            });
        });
}

#[cfg(test)]
mod stage21_tests;
#[cfg(test)]
mod stage28_tests;
#[cfg(test)]
mod stage29_tests;
#[cfg(test)]
mod tests;
