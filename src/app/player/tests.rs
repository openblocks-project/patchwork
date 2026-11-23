//! P6 Stage 1: Player launch, loading, output selection and read-only
//! state. The window itself (drawing, fullscreen, audio, devices) needs
//! manual testing.

use super::*;
use crate::graph::{DynNode, NodeType};
use crate::library::package::tests::TempDir;
use crate::library::trust::content::ContentState;
use std::collections::HashMap;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

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

fn video_out(sink: &str) -> NodeType {
    let node = crate::node_trait::NODE_REGISTRY
        .lock()
        .unwrap()
        .create(
            VIDEO_OUT_TAG,
            &serde_json::json!({ "sink": sink, "destination": "", "fullscreen": true }),
        )
        .expect("video_out is registered");
    assert_eq!(
        node.save_state().get("sink").and_then(|s| s.as_str()),
        Some(sink)
    );
    NodeType::Dynamic {
        inner: DynNode { node },
    }
}

/// A project folder holding `graph` as its project.json.
fn project(tmp: &TempDir, name: &str, graph: &Graph) -> PathBuf {
    let dir = tmp.0.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(PROJECT_FILE),
        serde_json::to_string_pretty(&serde_json::json!({ "graph": graph })).unwrap(),
    )
    .unwrap();
    dir
}

fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    out.sort();
    out
}

#[test]
fn the_command_line_selects_editor_catalogue_or_player() {
    // No arguments, or arguments the editor always ignored: the editor.
    assert_eq!(launch_mode(&[]).unwrap(), LaunchMode::Editor);
    assert_eq!(
        launch_mode(&args(&["something"])).unwrap(),
        LaunchMode::Editor
    );
    // The catalogue tool keeps exactly its arguments.
    let catalogue = args(&[
        "catalogue",
        "validate",
        "--root",
        "x",
        "--repository",
        "a/b",
    ]);
    assert_eq!(
        launch_mode(&catalogue).unwrap(),
        LaunchMode::Catalogue(args(&["validate", "--root", "x", "--repository", "a/b"]))
    );
    // `catalogue` with `--player` among its own arguments is still the
    // catalogue tool (it reports its own usage error).
    assert!(matches!(
        launch_mode(&args(&["catalogue", "--player"])).unwrap(),
        LaunchMode::Catalogue(_)
    ));
    // The player flag anywhere but first is an error, not the editor.
    for misplaced in [&["x", "--player", "y"][..], &["--output", "3"][..]] {
        let error = launch_mode(&args(misplaced)).unwrap_err();
        assert!(error.contains("must be the first argument"), "{error}");
    }
    // The player.
    let tmp = TempDir::new("player_cli");
    let dir = project(&tmp, "show", &Graph::new());
    let mode = launch_mode(&args(&["--player", &dir.to_string_lossy()])).unwrap();
    let LaunchMode::Player(launch) = mode else {
        panic!("player expected");
    };
    assert_eq!(launch.output, None);
    assert!(launch.project_json.ends_with(PROJECT_FILE));
}

#[test]
fn the_catalogue_tool_still_runs_from_the_dispatched_arguments() {
    let LaunchMode::Catalogue(rest) = launch_mode(&args(&["catalogue"])).unwrap() else {
        panic!("catalogue expected");
    };
    // Same exit status as before: no command is a usage error.
    assert_eq!(crate::library::marketplace::repository::cli::run(rest), 2);
    let LaunchMode::Catalogue(rest) =
        launch_mode(&args(&["catalogue", "validate", "--root", "x"])).unwrap()
    else {
        panic!("catalogue expected");
    };
    assert_eq!(
        crate::library::marketplace::repository::cli::run(rest),
        2,
        "no --repository"
    );
}

#[test]
fn player_options_are_parsed_strictly() {
    let tmp = TempDir::new("player_options");
    let dir = project(&tmp, "show", &Graph::new());
    let path = dir.to_string_lossy().into_owned();
    // --output before or after the path.
    for list in [
        args(&[&path, "--output", "7"]),
        args(&["--output", "7", &path]),
    ] {
        assert_eq!(parse_player_args(&list).unwrap().output, Some(7));
    }
    for (list, needle) in [
        (args(&[]), "No project given"),
        (args(&["--output", "7"]), "No project given"),
        (args(&[&path, "--output"]), "needs a node id"),
        (args(&[&path, "--output", "seven"]), "must be a node id"),
        (args(&[&path, "--output", "0"]), "must be a node id"),
        (args(&[&path, "--output", "-1"]), "must be a node id"),
        (
            args(&[&path, "--output", "1", "--output", "2"]),
            "given twice",
        ),
        (args(&[&path, "--fullscreen"]), "Unknown option"),
        (args(&[&path, &path]), "Only one project"),
    ] {
        let error = parse_player_args(&list).unwrap_err();
        assert!(error.contains(needle), "{list:?}: {error}");
    }
}

#[test]
fn project_paths_are_folders_with_project_json_or_that_file() {
    let tmp = TempDir::new("player_paths");
    let dir = project(&tmp, "show", &Graph::new());
    // The folder, or its project.json, give the same absolute locations.
    let from_dir = resolve_project_path(&dir).unwrap();
    let from_file = resolve_project_path(&dir.join(PROJECT_FILE)).unwrap();
    assert_eq!(from_dir, from_file);
    assert!(from_dir.0.is_absolute() && from_dir.1.is_absolute());
    assert_eq!(from_dir.1, from_dir.0.join(PROJECT_FILE));

    // A folder without project.json, a missing path, other files.
    let empty = tmp.0.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let node = tmp.0.join("Gain.pwnode");
    std::fs::write(&node, "{}").unwrap();
    let other_json = dir.join("settings.json");
    std::fs::write(&other_json, "{}").unwrap();
    for (path, needle) in [
        (empty.clone(), "has no project.json"),
        (tmp.0.join("missing"), "Project not found"),
        (node, "is not a project"),
        (other_json, "is not a project"),
        (PathBuf::new(), "No project given"),
    ] {
        let error = resolve_project_path(&path).unwrap_err();
        assert!(error.contains(needle), "{}: {error}", path.display());
    }
    // A folder containing a *directory* named project.json is not a project.
    let odd = tmp.0.join("odd");
    std::fs::create_dir_all(odd.join(PROJECT_FILE)).unwrap();
    assert!(resolve_project_path(&odd).is_err());
}

#[test]
fn loading_reads_the_project_without_writing_or_authorizing_it() {
    let tmp = TempDir::new("player_load");
    let mut graph = Graph::new();
    graph.add_node(slider(0.25), [0.0, 0.0]);
    let dir = project(&tmp, "show", &graph);
    // A legacy keys file the editor's interactive loader would migrate
    // and delete: the Player leaves it alone.
    std::fs::write(dir.join("api_keys.json"), r#"{"x":"y"}"#).unwrap();
    let before = listing(&dir);

    let launch = parse_player_args(&args(&[&dir.to_string_lossy()])).unwrap();
    let loaded = load_player_graph(&launch).unwrap();
    assert_eq!(loaded.nodes.len(), 1);
    assert_eq!(listing(&dir), before, "nothing in the project changed");

    // Identified, but not allowed: it cannot run until the ledger says so.
    assert!(matches!(loaded.content, ContentState::Pending { .. }));
    assert!(!loaded.content.runnable());
}

#[test]
fn loading_failures_are_reported() {
    let tmp = TempDir::new("player_load_fail");
    let dir = tmp.0.join("broken");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(PROJECT_FILE), "{ not json").unwrap();
    let launch = parse_player_args(&args(&[&dir.to_string_lossy()])).unwrap();
    let error = load_player_graph(&launch).unwrap_err();
    assert!(error.starts_with("Cannot load"), "{error}");
    assert!(error.contains(PROJECT_FILE), "{error}");
    // The project file is still what it was.
    assert_eq!(
        std::fs::read_to_string(dir.join(PROJECT_FILE)).unwrap(),
        "{ not json"
    );
}

#[test]
fn the_output_is_the_single_window_video_out_or_the_one_chosen() {
    // No outputs at all.
    let mut graph = Graph::new();
    graph.add_node(slider(0.5), [0.0, 0.0]);
    let error = select_output(&graph, None).unwrap_err();
    assert!(error.contains("no visual output"), "{error}");

    // A Video Out that sends elsewhere is not a window output.
    let elsewhere = graph.add_node(video_out("Syphon"), [0.0, 0.0]);
    assert!(window_outputs(&graph).is_empty());
    assert!(select_output(&graph, Some(elsewhere)).is_err());

    // Exactly one window output is chosen without asking.
    let first = graph.add_node(video_out("Window"), [0.0, 0.0]);
    assert_eq!(select_output(&graph, None), Ok(first));

    // Several: never guessed; the ids are listed; --output chooses.
    let second = graph.add_node(video_out("Window"), [0.0, 0.0]);
    assert_eq!(window_outputs(&graph), vec![first, second]);
    let error = select_output(&graph, None).unwrap_err();
    assert!(
        error.contains(&format!("#{first}"))
            && error.contains(&format!("#{second}"))
            && error.contains(OUTPUT_FLAG),
        "{error}"
    );
    assert_eq!(select_output(&graph, Some(second)), Ok(second));
    let error = select_output(&graph, Some(elsewhere)).unwrap_err();
    assert!(error.contains("Choose one of"), "{error}");
    assert!(select_output(&graph, Some(9999)).is_err());
}

#[test]
fn a_saved_project_keeps_its_output_choice_after_loading() {
    let tmp = TempDir::new("player_output_load");
    let mut graph = Graph::new();
    let out = graph.add_node(video_out("Window"), [0.0, 0.0]);
    let dir = project(&tmp, "show", &graph);
    let launch = parse_player_args(&args(&[&dir.to_string_lossy()])).unwrap();
    let loaded = load_player_graph(&launch).unwrap();
    assert_eq!(select_output(&loaded, None), Ok(out));
}

#[test]
fn the_output_shows_the_image_wired_into_it() {
    let mut graph = Graph::new();
    let source = graph.add_node(slider(0.5), [0.0, 0.0]);
    let out = graph.add_node(video_out("Window"), [0.0, 0.0]);
    let mut values: HashMap<(NodeId, usize), PortValue> = HashMap::new();
    // Nothing wired yet.
    assert!(output_image(&graph, &values, out).is_none());
    graph.add_connection(source, 0, out, 0);
    // Wired, but not an image this frame.
    values.insert((source, 0), PortValue::Float(0.5));
    assert!(output_image(&graph, &values, out).is_none());
    // An image: shown with its source.
    let image = Arc::new(ImageData {
        width: 4,
        height: 2,
        pixels: vec![0; 4 * 2 * 4],
    });
    values.insert((source, 0), PortValue::Image(image.clone()));
    let (shown, gpu) = output_image(&graph, &values, out).unwrap();
    assert!(Arc::ptr_eq(&shown, &image));
    assert_eq!(gpu, Some((source, 0)));
}

#[test]
fn the_player_document_is_read_only() {
    let mut manager = crate::document::DocumentManager::new();
    assert!(manager.active().is_editable());
    player_document(manager.active_mut());
    let document = manager.active();
    assert!(!document.is_editable());
    assert!(document.read_only && !document.is_dirty);
    assert_eq!(document.kind, crate::document::DocumentKind::Project);
}

#[test]
fn a_loaded_project_runs_on_the_existing_engine() {
    let tmp = TempDir::new("player_engine");
    let mut graph = Graph::new();
    let a = graph.add_node(slider(0.25), [0.0, 0.0]);
    let dir = project(&tmp, "show", &graph);
    let launch = parse_player_args(&args(&[&dir.to_string_lossy()])).unwrap();
    let mut loaded = load_player_graph(&launch).unwrap();
    // The same evaluation the editor's frame runs (`run_graph_frame`).
    let values = loaded.evaluate(0.0);
    match values.get(&(a, 0)) {
        Some(PortValue::Float(v)) => assert!((v - 0.25).abs() < 1e-6),
        other => panic!("slider output expected, got {other:?}"),
    }
}

// ── P6.1: reliability fixes ────────────────────────────────────────────────

fn port(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

#[test]
fn midi_out_connects_its_saved_port_once_and_retries_slowly() {
    let mut reconnect = MidiOutReconnect::default();
    let out = |connected| vec![(7, "Synth".to_string(), connected)];
    let connect = MidiOutStep::Connect {
        node_id: 7,
        port_name: "Synth".to_string(),
    };
    // Attached and not connected: one attempt.
    assert_eq!(
        reconnect.plan(&out(false), &port("Synth"), 0.0),
        vec![connect.clone()]
    );
    // Still not connected (the port refused, or permission denied): not
    // retried every frame.
    for t in [0.016, 1.0, 4.9] {
        assert!(reconnect.plan(&out(false), &port("Synth"), t).is_empty());
    }
    // Retried after the interval.
    assert_eq!(
        reconnect.plan(&out(false), &port("Synth"), MIDI_OUT_RETRY_SECS + 0.1),
        vec![connect]
    );
    // Connected and attached: nothing to do.
    assert!(reconnect.plan(&out(true), &port("Synth"), 20.0).is_empty());
}

#[test]
fn midi_out_waits_for_a_missing_port_and_drops_a_vanished_one() {
    let mut reconnect = MidiOutReconnect::default();
    let out = |connected| vec![(3, "Desk".to_string(), connected)];
    // Missing: no connection attempt until it is attached.
    for t in [0.0, 10.0, 100.0] {
        assert!(reconnect.plan(&out(false), &port("Other"), t).is_empty());
        assert!(reconnect.plan(&out(false), &[], t).is_empty());
    }
    // A connected port that disappears is dropped so it can reconnect.
    assert_eq!(
        reconnect.plan(&out(true), &port("Other"), 101.0),
        vec![MidiOutStep::Disconnect { node_id: 3 }]
    );
    // It comes back: connected at once.
    assert_eq!(
        reconnect.plan(&out(false), &port("Desk"), 102.0),
        vec![MidiOutStep::Connect {
            node_id: 3,
            port_name: "Desk".to_string()
        }]
    );
    // A node without a saved port is left alone.
    assert!(
        reconnect
            .plan(&[(4, String::new(), false)], &port("Desk"), 200.0)
            .is_empty()
    );
}

#[test]
fn escape_only_leaves_fullscreen() {
    assert!(escape_leaves_fullscreen(true, Some(true)));
    // Windowed, unknown, or no Escape: nothing (Escape never closes).
    assert!(!escape_leaves_fullscreen(true, Some(false)));
    assert!(!escape_leaves_fullscreen(true, None));
    assert!(!escape_leaves_fullscreen(false, Some(true)));
}

fn display(id: &str, primary: bool) -> crate::display::Display {
    crate::display::Display {
        id: id.to_string(),
        name: id.to_string(),
        origin: (0, 0),
        size: (1920, 1080),
        is_primary: primary,
    }
}

#[test]
fn the_saved_display_is_used_else_the_primary() {
    let displays = vec![
        display("Left", false),
        display("Main", true),
        display("TV", false),
    ];
    // An attached saved display is kept.
    assert_eq!(choose_display(&displays, "TV").unwrap().id, "TV");
    // Unplugged or never chosen: the primary.
    assert_eq!(choose_display(&displays, "Gone").unwrap().id, "Main");
    assert_eq!(choose_display(&displays, "").unwrap().id, "Main");
    // No primary reported: the first.
    let no_primary = vec![display("A", false), display("B", false)];
    assert_eq!(choose_display(&no_primary, "Gone").unwrap().id, "A");
    // None enumerated: the OS places the window.
    assert!(choose_display(&[], "TV").is_none());
}

#[test]
fn display_fallback_does_not_change_the_saved_project() {
    let tmp = TempDir::new("p6_display");
    let node = crate::node_trait::NODE_REGISTRY
        .lock()
        .unwrap()
        .create(
            VIDEO_OUT_TAG,
            &serde_json::json!({ "sink": "Window", "destination": "Unplugged", "fullscreen": true }),
        )
        .expect("video_out is registered");
    let mut graph = Graph::new();
    let id = graph.add_node(
        NodeType::Dynamic {
            inner: DynNode { node },
        },
        [0.0, 0.0],
    );
    let dir = project(&tmp, "fallback", &graph);
    let before = listing(&dir);
    let launch = parse_player_args(&args(&[dir.to_str().unwrap()])).unwrap();
    let loaded = load_player_graph(&launch).unwrap();
    let NodeType::Dynamic { inner } = &loaded.nodes[&id].node_type else {
        panic!("video out");
    };
    // The saved choice is read as saved; the fallback is only a choice.
    assert_eq!(inner.node.save_state()["destination"], "Unplugged");
    assert_eq!(
        choose_display(&[display("Main", true)], "Unplugged")
            .unwrap()
            .id,
        "Main"
    );
    assert_eq!(listing(&dir), before);
}

/// No implicit allow on this thread (as in production).
struct Enforce;

impl Enforce {
    fn new() -> Self {
        crate::library::trust::authorize::TEST_DEFAULT_ALLOW.with(|a| a.set(false));
        Self
    }
}

impl Drop for Enforce {
    fn drop(&mut self) {
        crate::library::trust::authorize::TEST_DEFAULT_ALLOW.with(|a| a.set(true));
    }
}

#[test]
fn player_output_needs_the_right_the_video_out_node_checks() {
    use crate::capability::{CapSet, Capability, Context};
    let _enforce = Enforce::new();
    // The Video Out node's Window sink checks Devices in the document's
    // context. The Player's own output must agree in every context, and
    // the Player never grants anything.
    let node_check = || crate::capability::check(Capability::Devices).is_ok();

    // No context: denied.
    assert!(!output_permitted());

    // Content allowed without device access: denied (the P6 bypass).
    let without = Context::new(CapSet::of(&[Capability::Network]), "no devices");
    {
        let _in = without.enter();
        assert!(!output_permitted());
        assert!(!node_check());
        assert!(!output_permitted());
    }

    // Content allowed with device access: shown.
    let with = Context::new(CapSet::of(&[Capability::Devices]), "devices");
    {
        let _in = with.enter();
        assert!(output_permitted());
        assert!(node_check());
    }

    // Permission withdrawn while playing: stops at once.
    let revoked = Context::new(CapSet::ALL, "revoked");
    {
        let _in = revoked.enter();
        assert!(output_permitted());
        revoked.revoke();
        assert!(!output_permitted());
    }
}

#[test]
fn video_in_starts_in_the_player_only_when_set_to() {
    use crate::node_trait::NodeBehavior;
    use crate::nodes::video_in_node::{VideoInNode, VideoSource};
    // Older projects (no setting) never start by themselves.
    let mut old = VideoInNode::default();
    old.load_state(&serde_json::json!({ "source": "Camera", "device_id": "0" }));
    assert!(!old.start_on_play);
    assert!(!old.take_player_start(true));

    // The setting is saved, loaded and kept by duplication.
    let mut set = VideoInNode::default();
    set.start_on_play = true;
    let mut loaded = VideoInNode::default();
    loaded.load_state(&set.save_state());
    assert!(loaded.start_on_play);
    assert!(loaded.clone().start_on_play);

    // Never in the editor.
    assert!(!loaded.take_player_start(false));
    // In the Player: once; an unavailable camera is not retried each frame.
    assert!(loaded.take_player_start(true));
    assert!(!loaded.take_player_start(true));

    // P6 Stage 2.8: ...but retried after growing pauses (a camera that did
    // not start, or stopped).
    let mut retried = VideoInNode::default();
    retried.start_on_play = true;
    let t0 = std::time::Instant::now();
    let step = VideoInNode::PLAYER_RETRY_STEP;
    let mut at = t0;
    let mut starts = 0;
    for attempt in 1..=VideoInNode::PLAYER_START_ATTEMPTS {
        assert!(retried.take_player_start_at(true, at), "attempt {attempt}");
        starts += 1;
        let next = at + step * u32::from(attempt);
        assert!(
            !retried.take_player_start_at(true, next - std::time::Duration::from_millis(1)),
            "not before its pause"
        );
        at = next;
    }
    assert_eq!(starts, VideoInNode::PLAYER_START_ATTEMPTS);
    // P6 Stage 3.4: then, instead of giving up, every 10 s for as long as
    // the camera is required.
    let every = VideoInNode::PLAYER_RECOVERY_INTERVAL;
    for _ in 0..30 {
        assert!(retried.take_player_start_at(true, at));
        assert!(
            !retried.take_player_start_at(true, at + every - std::time::Duration::from_millis(1))
        );
        at += every;
    }

    // Only camera sources.
    let mut file = VideoInNode::default();
    file.start_on_play = true;
    file.source = VideoSource::File;
    assert!(!file.take_player_start(true));
}

#[test]
fn the_autostart_signal_is_off_unless_the_player_sets_it() {
    let ctx = egui::Context::default();
    assert!(!player_autostart(&ctx));
    set_player_autostart(&ctx, true);
    assert!(player_autostart(&ctx));
}
