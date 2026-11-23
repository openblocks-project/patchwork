//! P6 Stage 2.1: Player manifest, first-run consent and isolation.

use super::consent::{ConsentRequest, Gate, GateInput, decide, run_if_permitted};
use super::manifest::{MANIFEST_FILE, check_relative_project_path, load_manifest, parse_manifest};
use super::*;
use crate::capability::{CapSet, Capability};
use crate::library::package::tests::TempDir;
use crate::library::trust::TrustStore;
use crate::library::trust::authorize::{BlockReason, Blocked, PackageSummary, TrustPolicy};
use crate::library::trust::content::{ContentDecision, ContentKind, ContentLedger, ContentState};

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn slider_graph(value: f32) -> Graph {
    let mut graph = Graph::new();
    graph.add_node(
        NodeType::Slider {
            value,
            min: 0.0,
            max: 1.0,
            step: 0.01,
            slider_color: [0, 0, 0],
            label: String::new(),
        },
        [0.0, 0.0],
    );
    graph
}

fn write_project(dir: &Path, graph: &Graph) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(PROJECT_FILE),
        serde_json::to_string_pretty(&serde_json::json!({ "graph": graph })).unwrap(),
    )
    .unwrap();
}

fn manifest_json(project: &str, digest: &str) -> String {
    serde_json::json!({
        "schema": "patchwork.player",
        "format": 1,
        "project": project,
        "output": 2,
        "title": "My Show",
        "patchwork_version": "0.0.10",
        "project_sha256": digest,
    })
    .to_string()
}

/// An export folder: `player.json` + `project/project.json`.
fn export(tmp: &TempDir, name: &str, graph: &Graph) -> PathBuf {
    let dir = tmp.0.join(name);
    write_project(&dir.join("project"), graph);
    std::fs::write(dir.join(MANIFEST_FILE), manifest_json("project", DIGEST)).unwrap();
    dir
}

/// Every file under `dir` (to show nothing was written).
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(files(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

// ── Manifest ────────────────────────────────────────────────────────────────

#[test]
fn a_valid_manifest_names_the_bundled_project() {
    let tmp = TempDir::new("s21_valid");
    let dir = export(&tmp, "app", &slider_graph(0.5));
    let exported = load_manifest(&dir, None).unwrap();
    let canonical = std::fs::canonicalize(&dir).unwrap();
    assert_eq!(exported.launch.project_dir, canonical.join("project"));
    assert_eq!(
        exported.launch.project_json,
        canonical.join("project").join(PROJECT_FILE)
    );
    assert_eq!(exported.launch.output, Some(2));
    let info = exported.launch.export.unwrap();
    assert_eq!(info.export_dir, canonical);
    assert_eq!(info.title, "My Show");
    assert_eq!(info.patchwork_version, "0.0.10");
    assert_eq!(info.expected_digest, DIGEST);
}

#[test]
fn manifest_fields_are_checked_before_anything_is_loaded() {
    let ok: serde_json::Value = serde_json::from_str(&manifest_json("project", DIGEST)).unwrap();
    let with = |key: &str, value: serde_json::Value| {
        let mut v = ok.clone();
        v[key] = value;
        v.to_string()
    };
    assert!(parse_manifest(&ok.to_string()).is_ok());
    for (text, expect) in [
        ("not json".to_string(), "not valid JSON"),
        (
            with("schema", "other".into()),
            "not a Patchwork Player manifest",
        ),
        (with("format", 2.into()), "format 2"),
        (with("format", 0.into()), "format 0"),
        (with("format", "1".into()), "no format"),
        (with("output", 0.into()), "output"),
        (with("output", (-1).into()), "not a valid manifest"),
        (with("title", "".into()), "title is empty"),
        (with("title", "a\u{7}b".into()), "title"),
        (with("patchwork_version", "".into()), "patchwork_version"),
        (with("project_sha256", "abc".into()), "project_sha256"),
        (
            with("project_sha256", DIGEST.to_uppercase().into()),
            "project_sha256",
        ),
        (with("unexpected", true.into()), "unknown field"),
    ] {
        let error = parse_manifest(&text).unwrap_err();
        assert!(error.contains(expect), "{text}: {error}");
    }
    let mut missing = ok.clone();
    missing.as_object_mut().unwrap().remove("project_sha256");
    assert!(parse_manifest(&missing.to_string()).is_err());
}

#[test]
fn unsupported_manifest_versions_are_refused() {
    for format in [0, 2, 99] {
        let text = manifest_json("project", DIGEST)
            .replace("\"format\":1", &format!("\"format\":{format}"));
        let error = parse_manifest(&text).unwrap_err();
        assert!(error.contains("reads format 1 only"), "{error}");
    }
}

#[test]
fn project_paths_cannot_leave_the_application_folder_by_spelling() {
    for good in ["project", "show/project", "a b/project-1"] {
        assert!(check_relative_project_path(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        "/project",
        "\\project",
        "C:\\project",
        "C:/project",
        "C:project",
        "\\\\server\\share\\project",
        "..",
        "../project",
        "..\\project",
        "a/../../project",
        "a/../b",
        ".",
        "./project",
        "a/./b",
        "a//b",
        "project/",
        r"a\.\b",
        r"a\\b",
        "project:stream",
        "project.",
        "project ",
    ] {
        assert!(check_relative_project_path(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn missing_or_malformed_projects_are_refused() {
    let tmp = TempDir::new("s21_missing");
    // No project folder.
    let dir = tmp.0.join("none");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(MANIFEST_FILE), manifest_json("project", DIGEST)).unwrap();
    assert!(
        load_manifest(&dir, None)
            .unwrap_err()
            .contains("was not found")
    );
    // A folder without project.json.
    std::fs::create_dir_all(dir.join("project")).unwrap();
    assert!(
        load_manifest(&dir, None)
            .unwrap_err()
            .contains("has no project.json")
    );
    // project.json that is a folder.
    std::fs::create_dir_all(dir.join("project").join(PROJECT_FILE)).unwrap();
    assert!(
        load_manifest(&dir, None)
            .unwrap_err()
            .contains("regular file")
    );
    // The project path names a file.
    let file = tmp.0.join("file");
    std::fs::create_dir_all(&file).unwrap();
    std::fs::write(file.join("project"), "x").unwrap();
    std::fs::write(file.join(MANIFEST_FILE), manifest_json("project", DIGEST)).unwrap();
    assert!(
        load_manifest(&file, None)
            .unwrap_err()
            .contains("not a folder")
    );
    // The manifest itself is a folder, or too large.
    let odd = tmp.0.join("odd");
    std::fs::create_dir_all(odd.join(MANIFEST_FILE)).unwrap();
    assert!(
        load_manifest(&odd, None)
            .unwrap_err()
            .contains("regular file")
    );
    let big = tmp.0.join("big");
    write_project(&big.join("project"), &slider_graph(0.1));
    std::fs::write(big.join(MANIFEST_FILE), " ".repeat(70 * 1024)).unwrap();
    assert!(load_manifest(&big, None).unwrap_err().contains("too large"));
}

/// Make a directory junction (no privileges needed on Windows).
#[cfg(windows)]
fn junction(link: &Path, target: &Path) -> bool {
    std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(windows)]
#[test]
fn junctions_cannot_lead_the_project_outside_the_export() {
    let tmp = TempDir::new("s21_junction");
    // The project folder itself is a junction to a project elsewhere.
    let outside = tmp.0.join("outside");
    write_project(&outside, &slider_graph(0.3));
    let dir = tmp.0.join("app");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(MANIFEST_FILE), manifest_json("project", DIGEST)).unwrap();
    assert!(junction(&dir.join("project"), &outside), "mklink /J failed");
    let error = load_manifest(&dir, None).unwrap_err();
    assert!(error.contains("link or junction"), "{error}");

    // A junction on the way (show/project where show is a junction).
    let dir2 = tmp.0.join("app2");
    std::fs::create_dir_all(&dir2).unwrap();
    std::fs::write(
        dir2.join(MANIFEST_FILE),
        manifest_json("show/project", DIGEST),
    )
    .unwrap();
    let show_target = tmp.0.join("show_target");
    write_project(&show_target.join("project"), &slider_graph(0.3));
    assert!(junction(&dir2.join("show"), &show_target));
    let error = load_manifest(&dir2, None).unwrap_err();
    assert!(error.contains("link or junction"), "{error}");

    // A junction inside the project (e.g. assets pointing elsewhere).
    let dir3 = export(&tmp, "app3", &slider_graph(0.3));
    assert!(junction(&dir3.join("project").join("assets"), &outside));
    let error = load_manifest(&dir3, None).unwrap_err();
    assert!(error.contains("link or junction"), "{error}");
}

#[cfg(windows)]
#[test]
fn symlinks_cannot_lead_the_project_outside_the_export() {
    let tmp = TempDir::new("s21_symlink");
    let outside = tmp.0.join("outside");
    write_project(&outside, &slider_graph(0.3));
    let dir = tmp.0.join("app");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(MANIFEST_FILE), manifest_json("project", DIGEST)).unwrap();
    // Creating symlinks needs Developer Mode or elevation on Windows.
    if std::os::windows::fs::symlink_dir(&outside, dir.join("project")).is_err() {
        eprintln!("symlinks unavailable here; junction test covers reparse points");
        return;
    }
    let error = load_manifest(&dir, None).unwrap_err();
    assert!(error.contains("link or junction"), "{error}");
}

#[test]
fn an_export_containing_this_computers_trust_records_is_refused() {
    let tmp = TempDir::new("s21_trust_inside");
    let dir = export(&tmp, "app", &slider_graph(0.5));
    let trust = dir.join(".patchwork");
    std::fs::create_dir_all(trust.join("trust")).unwrap();
    let error = load_manifest(&dir, Some(&trust)).unwrap_err();
    assert!(error.contains("Patchwork data"), "{error}");
    // Elsewhere: fine.
    let elsewhere = tmp.0.join("home").join(".patchwork");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert!(load_manifest(&dir, Some(&elsewhere)).is_ok());
}

// ── Launch: manifest mode ───────────────────────────────────────────────────

#[test]
fn without_a_manifest_the_command_line_decides_as_before() {
    let tmp = TempDir::new("s21_no_manifest");
    let exe_dir = tmp.0.join("bin");
    std::fs::create_dir_all(&exe_dir).unwrap();
    assert_eq!(launch_mode_at(&[], &exe_dir).unwrap(), LaunchMode::Editor);
    assert_eq!(
        launch_mode_at(&args(&["catalogue", "validate"]), &exe_dir).unwrap(),
        LaunchMode::Catalogue(args(&["validate"]))
    );
    let project = tmp.0.join("p");
    write_project(&project, &slider_graph(0.5));
    let LaunchMode::Player(launch) =
        launch_mode_at(&args(&["--player", &project.to_string_lossy()]), &exe_dir).unwrap()
    else {
        panic!("player");
    };
    assert!(launch.export.is_none());
    assert!(launch_mode_at(&args(&["x", "--player"]), &exe_dir).is_err());
}

#[test]
fn with_a_manifest_arguments_are_ignored_and_only_the_bundle_plays() {
    let tmp = TempDir::new("s21_args");
    let dir = export(&tmp, "app", &slider_graph(0.5));
    let other = tmp.0.join("other");
    write_project(&other, &slider_graph(0.9));
    let bundled = std::fs::canonicalize(&dir).unwrap().join("project");
    for argv in [
        vec![],
        args(&["catalogue", "validate", "--root", "x"]),
        args(&["catalogue", "publish-index"]),
        args(&["--player", &other.to_string_lossy()]),
        args(&["--player", &other.to_string_lossy(), "--output", "7"]),
        args(&[&other.to_string_lossy()]),
        args(&["--editor"]),
        args(&["--bogus", "x"]),
    ] {
        match launch_mode_at(&argv, &dir).unwrap() {
            LaunchMode::Exported(launch) => {
                assert_eq!(launch.project_dir, bundled, "{argv:?}");
                assert_eq!(launch.output, Some(2), "{argv:?}");
            }
            other => panic!("{argv:?} gave {other:?}"),
        }
    }
}

#[test]
fn an_invalid_manifest_never_falls_back_to_the_editor() {
    let tmp = TempDir::new("s21_invalid");
    let cases: Vec<(&str, String)> = vec![
        (
            "schema",
            manifest_json("project", DIGEST).replace("patchwork.player", "x"),
        ),
        (
            "format",
            manifest_json("project", DIGEST).replace("\"format\":1", "\"format\":2"),
        ),
        ("escape", manifest_json("../project", DIGEST)),
        ("missing", manifest_json("nowhere", DIGEST)),
        ("garbage", "{".to_string()),
        ("empty", String::new()),
    ];
    for (name, text) in cases {
        let dir = tmp.0.join(name);
        write_project(&dir.join("project"), &slider_graph(0.5));
        std::fs::write(dir.join(MANIFEST_FILE), text).unwrap();
        for argv in [
            vec![],
            args(&["catalogue", "validate"]),
            args(&["--player", "x"]),
        ] {
            match launch_mode_at(&argv, &dir) {
                Ok(LaunchMode::ExportInvalid { error, .. }) => assert!(!error.is_empty()),
                other => panic!("{name} {argv:?}: {other:?}"),
            }
        }
    }
}

// ── Consent decision ────────────────────────────────────────────────────────

fn input<'a>(
    content: &'a ContentState,
    digest: Option<&'a str>,
    expected: Option<&'a str>,
    blocked: &'a [Blocked],
) -> GateInput<'a> {
    GateInput {
        project_name: "Show",
        content,
        digest,
        expected,
        change_acknowledged: false,
        uses: CapSet::of(&[Capability::Devices, Capability::Scripting]),
        blocked,
    }
}

fn unverified() -> ContentState {
    ContentState::Unverified {
        kind: ContentKind::Project,
        digest: Some(DIGEST.to_string()),
        note: None,
    }
}

fn consent(gate: Gate) -> ConsentRequest {
    match gate {
        Gate::Consent(request) => request,
        other => panic!("expected consent, got {other:?}"),
    }
}

fn summary(name: &str, digest: Option<String>) -> PackageSummary {
    PackageSummary {
        dir: PathBuf::from(name),
        name: name.to_string(),
        label: name.to_string(),
        identity: None,
        provenance: None,
        declared: None,
        detected: Default::default(),
        restricted: Default::default(),
        unclassified: Default::default(),
        loose_digest: digest,
    }
}

fn blocked_package(name: &str, reason: BlockReason) -> Blocked {
    let digest: String = format!("{name:0>64}").chars().take(64).collect();
    Blocked {
        package: summary(name, Some(digest)),
        reason,
    }
}

#[test]
fn content_not_allowed_here_needs_consent_with_what_it_uses() {
    let state = unverified();
    let request = consent(decide(&input(&state, Some(DIGEST), None, &[])));
    assert!(request.project_decision);
    assert_eq!(request.project_name, "Show");
    assert_eq!(request.content_digest.as_deref(), Some(DIGEST));
    assert_eq!(
        request.permissions,
        CapSet::of(&[Capability::Devices, Capability::Scripting])
    );
    assert!(request.changed_from.is_none());
    // Pending (not yet resolved) is treated the same: never runs.
    let pending = ContentState::Pending {
        kind: ContentKind::Project,
        digest: DIGEST.to_string(),
    };
    assert!(consent(decide(&input(&pending, Some(DIGEST), None, &[]))).project_decision);
}

#[test]
fn approved_unchanged_content_runs_without_asking() {
    for state in [
        ContentState::Authored,
        ContentState::Allowed(CapSet::of(&[Capability::Devices])),
    ] {
        assert_eq!(decide(&input(&state, Some(DIGEST), None, &[])), Gate::Run);
        assert_eq!(
            decide(&input(&state, Some(DIGEST), Some(DIGEST), &[])),
            Gate::Run
        );
    }
}

#[test]
fn content_changed_since_export_is_pointed_out_and_needs_consent() {
    // Allowed content that is not what was exported: asked again.
    let allowed = ContentState::Allowed(CapSet::EMPTY);
    let request = consent(decide(&input(&allowed, Some(OTHER), Some(DIGEST), &[])));
    assert_eq!(request.changed_from.as_deref(), Some(DIGEST));
    assert!(!request.project_decision);
    // Acknowledged for this run: runs.
    let mut acknowledged = input(&allowed, Some(OTHER), Some(DIGEST), &[]);
    acknowledged.change_acknowledged = true;
    assert_eq!(decide(&acknowledged), Gate::Run);
    // Not allowed and changed: both.
    let state = unverified();
    let request = consent(decide(&input(&state, Some(OTHER), Some(DIGEST), &[])));
    assert!(request.project_decision);
    assert!(request.changed_from.is_some());
}

#[test]
fn denied_or_unidentifiable_content_is_refused_without_an_allow() {
    let denied = ContentState::Denied {
        kind: ContentKind::Project,
        digest: DIGEST.to_string(),
    };
    assert!(matches!(
        decide(&input(&denied, Some(DIGEST), None, &[])),
        Gate::Refused { .. }
    ));
    let unknown = ContentState::Unverified {
        kind: ContentKind::Project,
        digest: None,
        note: None,
    };
    assert!(matches!(
        decide(&input(&unknown, None, None, &[])),
        Gate::Refused { .. }
    ));
}

#[test]
fn packages_are_asked_for_separately_and_denied_ones_stay_off() {
    let needs = blocked_package("Synth", BlockReason::NeedsPermission);
    let denied = blocked_package("Spy", BlockReason::Denied);
    let wrapped = Blocked {
        package: summary("Outer", None),
        reason: BlockReason::Dependency(Box::new(needs.clone())),
    };
    let blocked = vec![needs.clone(), wrapped, denied.clone()];
    // An authored project with a package waiting: consent for the package
    // only (the project itself needs no decision).
    let request = consent(decide(&input(
        &ContentState::Authored,
        Some(DIGEST),
        None,
        &blocked,
    )));
    assert!(!request.project_decision);
    assert_eq!(
        request.packages,
        vec![needs.clone()],
        "deduplicated, innermost"
    );
    assert_eq!(request.unavailable, vec![denied.clone()]);
    // A denied package alone: nothing to decide, it simply stays off.
    assert_eq!(
        decide(&input(
            &ContentState::Authored,
            Some(DIGEST),
            None,
            &[denied]
        )),
        Gate::Run
    );
}

#[test]
fn evaluation_only_happens_when_the_gate_allows() {
    let mut calls = 0;
    for gate in [
        Gate::Consent(consent(decide(&input(
            &unverified(),
            Some(DIGEST),
            None,
            &[],
        )))),
        Gate::Refused {
            title: String::new(),
            detail: String::new(),
        },
    ] {
        assert!(run_if_permitted(&gate, || calls += 1).is_none());
    }
    assert_eq!(calls, 0);
    assert_eq!(run_if_permitted(&Gate::Run, || 7), Some(7));
}

// ── With real trust records (temporary, not this computer's) ────────────────

/// Load `dir` like the Player, resolve against `ledger`, decide.
fn player_decision(dir: &Path, ledger: &ContentLedger, expected: Option<&str>) -> (Graph, Gate) {
    let launch = parse_player_args(&args(&[&dir.to_string_lossy()])).unwrap();
    let mut graph = load_player_graph(&launch).unwrap();
    let digest = super::loaded_digest(&graph);
    crate::app::provenance::resolve_graph_with(&mut graph, Some(ledger));
    let uses = crate::app::provenance::graph_uses(&graph);
    let gate = decide(&GateInput {
        project_name: "Show",
        content: &graph.content,
        digest: digest.as_deref(),
        expected,
        change_acknowledged: false,
        uses,
        blocked: &[],
    });
    (graph, gate)
}

#[test]
fn first_run_consent_records_exactly_the_approved_content_and_nothing_before() {
    let tmp = TempDir::new("s21_first_run");
    let project = tmp.0.join("project");
    write_project(&project, &slider_graph(0.25));
    let trust_dir = tmp.0.join("home").join("trust");
    let ledger = ContentLedger::at(trust_dir.join("content.json"));

    // First run: consent; evaluation is not reached; nothing is written.
    let (mut graph, gate) = player_decision(&project, &ledger, None);
    let request = consent(gate.clone());
    assert!(request.project_decision);
    assert!(run_if_permitted(&gate, || graph.evaluate(0.0)).is_none());
    assert!(
        files(&tmp.0.join("home")).is_empty(),
        "no trust writes before approval"
    );

    // The user allows it: the existing ledger records exactly this content
    // and the capabilities it uses (what decide_active_content records).
    let digest = request.content_digest.clone().unwrap();
    ledger
        .record(
            ContentKind::Project,
            &digest,
            ContentDecision::Allowed(request.permissions),
            true,
        )
        .unwrap();
    assert_eq!(
        ledger.decision(ContentKind::Project, &digest),
        Ok(Some(ContentDecision::Allowed(request.permissions)))
    );

    // Next launch: unchanged and approved, it runs and evaluates.
    let (mut graph, gate) = player_decision(&project, &ledger, None);
    assert_eq!(gate, Gate::Run);
    assert_eq!(graph.content, ContentState::Allowed(request.permissions));
    let values = run_if_permitted(&gate, || graph.evaluate(0.0)).unwrap();
    assert!(!values.is_empty(), "the graph was evaluated");

    // The project is modified: a new identity, consent again, not run.
    write_project(&project, &slider_graph(0.75));
    let (mut graph, gate) = player_decision(&project, &ledger, None);
    let again = consent(gate.clone());
    assert!(again.project_decision);
    assert_ne!(again.content_digest.as_deref(), Some(digest.as_str()));
    assert!(run_if_permitted(&gate, || graph.evaluate(0.0)).is_none());
}

#[test]
fn denied_content_never_runs() {
    let tmp = TempDir::new("s21_denied");
    let project = tmp.0.join("project");
    write_project(&project, &slider_graph(0.25));
    let ledger = ContentLedger::at(tmp.0.join("trust").join("content.json"));
    let (_, gate) = player_decision(&project, &ledger, None);
    let digest = consent(gate).content_digest.unwrap();
    ledger
        .record(ContentKind::Project, &digest, ContentDecision::Denied, true)
        .unwrap();
    let (mut graph, gate) = player_decision(&project, &ledger, None);
    assert!(matches!(gate, Gate::Refused { .. }), "{gate:?}");
    assert!(run_if_permitted(&gate, || graph.evaluate(0.0)).is_none());
}

#[test]
fn an_exported_digest_mismatch_is_detected_even_for_allowed_content() {
    let tmp = TempDir::new("s21_mismatch");
    let project = tmp.0.join("project");
    write_project(&project, &slider_graph(0.25));
    let ledger = ContentLedger::at(tmp.0.join("trust").join("content.json"));
    let (graph, _) = player_decision(&project, &ledger, None);
    let digest = super::loaded_digest(&graph).unwrap();
    ledger
        .record(
            ContentKind::Project,
            &digest,
            ContentDecision::Authored,
            true,
        )
        .unwrap();
    // The manifest recorded this digest: runs.
    assert_eq!(
        player_decision(&project, &ledger, Some(&digest)).1,
        Gate::Run
    );
    // The manifest recorded another digest: warned, not run until confirmed.
    let request = consent(player_decision(&project, &ledger, Some(OTHER)).1);
    assert_eq!(request.changed_from.as_deref(), Some(OTHER));
}

#[test]
fn trust_files_in_the_export_folder_are_never_consulted() {
    let tmp = TempDir::new("s21_bundled_trust");
    let dir = export(&tmp, "app", &slider_graph(0.4));
    let project = dir.join("project");
    // Plant "allowed" records for this exact content inside the export, in
    // every place a ledger might be looked for.
    let launch = parse_player_args(&args(&[&project.to_string_lossy()])).unwrap();
    let digest = super::loaded_digest(&load_player_graph(&launch).unwrap()).unwrap();
    for planted in [
        dir.join("trust").join("content.json"),
        dir.join(".patchwork").join("trust").join("content.json"),
        project.join("trust").join("content.json"),
    ] {
        ContentLedger::at(&planted)
            .record(
                ContentKind::Project,
                &digest,
                ContentDecision::Allowed(CapSet::ALL),
                true,
            )
            .unwrap();
    }
    // This computer's own (empty) records decide: consent is required.
    let home = ContentLedger::at(tmp.0.join("home").join("trust").join("content.json"));
    let (_, gate) = player_decision(&project, &home, Some(DIGEST));
    assert!(consent(gate).project_decision);
    // And an export holding the per-user data folder is refused outright.
    assert!(load_manifest(&dir, Some(&dir.join(".patchwork"))).is_err());
}

#[test]
fn packages_need_their_own_approval_recorded_separately() {
    use crate::document::{CustomNodeFile, CustomNodePort};
    use crate::library::package::publish_package;
    use crate::library::package::tests::request as publish_request;
    use crate::library::resolve::InstancePackageRef;

    let tmp = TempDir::new("s21_packages");
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
    let version = publish_package(&library, &source, &publish_request("Gain", "1.0.0")).unwrap();
    let mut graph = Graph::new();
    graph.add_node(
        NodeType::CustomNodeInstance {
            file: version.entry_path.to_string_lossy().into_owned(),
            inputs: Vec::new(),
            outputs: vec![CustomNodePort {
                name: "out".into(),
                kind: "Number".into(),
            }],
            package: Some(InstancePackageRef {
                id: version.manifest.id.clone(),
                version: version.version.to_string(),
                entry: version.manifest.entry.clone(),
            }),
        },
        [0.0, 0.0],
    );
    let trust_dir = tmp.0.join("home").join("trust");
    let store = TrustStore::at(trust_dir.join("packages.json"));
    let policy = TrustPolicy::Store(store.clone());
    graph.set_package_environment(None, Some(library.clone()));
    graph.set_trust_policy(policy.clone());

    // Found without loading anything; nothing written.
    let blocked = graph.preflight_custom_nodes();
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert_eq!(blocked[0].reason, BlockReason::NeedsPermission);
    assert!(!graph.has_custom_runtime(*graph.nodes.keys().next().unwrap()));
    assert!(
        files(&tmp.0.join("home")).is_empty(),
        "no trust writes before approval"
    );

    // Allowing the project does not allow the package.
    let request = consent(decide(&input(
        &ContentState::Authored,
        Some(DIGEST),
        None,
        &blocked,
    )));
    assert!(!request.project_decision);
    assert_eq!(request.packages.len(), 1);
    store
        .content_ledger()
        .record(
            ContentKind::Project,
            DIGEST,
            ContentDecision::Allowed(CapSet::ALL),
            true,
        )
        .unwrap();
    assert_eq!(
        graph.preflight_custom_nodes().len(),
        1,
        "still needs its own approval"
    );

    // The package's own approval, by the editor's mechanism.
    crate::app::permissions::decide_package(&policy, &request.packages[0].package, true).unwrap();
    assert!(graph.preflight_custom_nodes().is_empty());
    assert_eq!(
        decide(&input(
            &ContentState::Authored,
            Some(DIGEST),
            None,
            &graph.preflight_custom_nodes()
        )),
        Gate::Run
    );
}

#[test]
fn a_launch_from_the_command_line_has_no_export_and_uses_the_library() {
    let tmp = TempDir::new("s21_cli");
    let project = tmp.0.join("p");
    write_project(&project, &slider_graph(0.5));
    let launch = parse_player_args(&args(&[&project.to_string_lossy()])).unwrap();
    assert!(launch.export.is_none());
}
