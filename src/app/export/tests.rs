//! P6 Stage 2.2: Windows portable export.

use super::pe::{BuildProfile, build_profile, imported_dlls, profile_marker};
use super::runtime::{DependencyRules, PinnedDll};
use super::*;
use crate::graph::DynNode;
use crate::library::package::tests::TempDir;
use crate::library::trust::content::{ContentDecision, ContentKind};

// ── Fixtures ────────────────────────────────────────────────────────────────

/// The commit the fake runtimes record.
const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A minimal release-style Player runtime (GUI subsystem, Player build
/// information from an exact commit) importing `dlls`, carrying `profile`'s
/// marker.
fn fake_exe(profile: Option<BuildProfile>, dlls: &[&str]) -> Vec<u8> {
    fake_exe_as(
        profile,
        dlls,
        super::pe::SUBSYSTEM_GUI,
        Some(super::pe::build_info_bytes("player", TEST_COMMIT, "0")),
    )
}

/// A DLL importing `dlls` (no Patchwork markers).
fn fake_dll(dlls: &[&str]) -> Vec<u8> {
    fake_exe_as(None, dlls, super::pe::SUBSYSTEM_GUI, None)
}

/// A minimal PE32+ executable importing `dlls`.
fn fake_exe_as(
    profile: Option<BuildProfile>,
    dlls: &[&str],
    subsystem: u16,
    info: Option<Vec<u8>>,
) -> Vec<u8> {
    let mut b = vec![0u8; 0x400];
    b[0] = b'M';
    b[1] = b'Z';
    b[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    b[0x40..0x44].copy_from_slice(b"PE\0\0");
    let coff = 0x44;
    b[coff..coff + 2].copy_from_slice(&0x8664u16.to_le_bytes());
    b[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes()); // sections
    b[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes()); // optional size
    let opt = coff + 20;
    b[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    b[opt + 68..opt + 70].copy_from_slice(&subsystem.to_le_bytes());
    b[opt + 108..opt + 112].copy_from_slice(&16u32.to_le_bytes());
    let names_offset = 20 * (dlls.len() + 1);
    let mut section = vec![0u8; names_offset];
    for (i, dll) in dlls.iter().enumerate() {
        let name_rva = 0x1000 + section.len() as u32;
        section[i * 20 + 12..i * 20 + 16].copy_from_slice(&name_rva.to_le_bytes());
        section.extend_from_slice(dll.as_bytes());
        section.push(0);
    }
    section.resize(section.len().max(0x200), 0);
    let dirs = opt + 112;
    b[dirs + 8..dirs + 12].copy_from_slice(&0x1000u32.to_le_bytes());
    b[dirs + 12..dirs + 16].copy_from_slice(&(names_offset as u32).to_le_bytes());
    let sh = opt + 240;
    b[sh..sh + 5].copy_from_slice(b".idat");
    b[sh + 8..sh + 12].copy_from_slice(&(section.len() as u32).to_le_bytes());
    b[sh + 12..sh + 16].copy_from_slice(&0x1000u32.to_le_bytes());
    b[sh + 16..sh + 20].copy_from_slice(&(section.len() as u32).to_le_bytes());
    b[sh + 20..sh + 24].copy_from_slice(&0x400u32.to_le_bytes());
    b.extend_from_slice(&section);
    if let Some(profile) = profile {
        b.extend_from_slice(&profile_marker(profile));
    }
    if let Some(info) = info {
        b.extend_from_slice(&info);
    }
    b
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn signature_ok(_: &Path) -> Result<(), String> {
    Ok(())
}

fn signature_bad(_: &Path) -> Result<(), String> {
    Err("not signed".to_string())
}

const RUNTIME_IMPORTS: &[&str] = &[
    "KERNEL32.dll",
    "directml.dll",
    "VCRUNTIME140.dll",
    "MSVCP140.dll",
    "api-ms-win-crt-heap-l1-1-0.dll",
    "USER32.dll",
];

struct Fx {
    tmp: TempDir,
    project: PathBuf,
    library: PathBuf,
    runtime: PathBuf,
    /// A Visual Studio-shaped VC++ redistributable folder.
    redist: PathBuf,
    out: PathBuf,
    home: PathBuf,
}

impl Fx {
    fn new(tag: &str) -> Self {
        let tmp = TempDir::new(tag);
        let fx = Fx {
            project: tmp.0.join("My Project"),
            library: tmp.0.join("library"),
            runtime: tmp.0.join("runtime"),
            redist: tmp
                .0
                .join("VS")
                .join("VC")
                .join("Redist")
                .join("MSVC")
                .join("14.44.35112")
                .join("x64")
                .join("Microsoft.VC143.CRT"),
            out: tmp.0.join("exports"),
            home: tmp.0.join("home").join(".patchwork"),
            tmp,
        };
        for dir in [
            &fx.project,
            &fx.library,
            &fx.runtime,
            &fx.redist,
            &fx.out,
            &fx.home,
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            fx.runtime.join("patchwork-player.exe"),
            fake_exe(Some(BuildProfile::Release), RUNTIME_IMPORTS),
        )
        .unwrap();
        std::fs::write(
            fx.runtime.join("DirectML.dll"),
            fake_dll(&["KERNEL32.dll", "d3d12.dll"]),
        )
        .unwrap();
        std::fs::write(
            fx.redist.join("VCRUNTIME140.dll"),
            fake_dll(&["KERNEL32.dll"]),
        )
        .unwrap();
        // MSVCP140 needs VCRUNTIME140 itself (found once, bundled once).
        std::fs::write(
            fx.redist.join("MSVCP140.dll"),
            fake_dll(&["VCRUNTIME140.dll", "KERNEL32.dll"]),
        )
        .unwrap();
        fx
    }

    /// The rules for this fixture: its DirectML is the pinned one, and VC++
    /// signatures are accepted.
    fn rules(&self) -> DependencyRules {
        let directml = std::fs::read(self.runtime.join("DirectML.dll")).unwrap();
        DependencyRules {
            pinned: vec![PinnedDll {
                name: "directml.dll".to_string(),
                sha256: sha256_hex(&directml),
                component: "DirectML (test)".to_string(),
                notice_file: "DirectML-NOTICE.txt".to_string(),
                notice: "DirectML test notice\n".to_string(),
            }],
            verify_signature: signature_ok,
        }
    }

    fn ledger(&self) -> ContentLedger {
        ContentLedger::at(self.home.join("trust").join("content.json"))
    }

    fn env(&self) -> ExportEnvironment {
        ExportEnvironment {
            ledger: Some(self.ledger()),
            trust: TrustPolicy::AllowAll,
            library_root: Some(self.library.clone()),
            protected: Vec::new(),
            user_data: Some(self.home.clone()),
            dependencies: self.rules(),
            unresolved: Vec::new(),
            runtime_source: None,
        }
    }

    /// Write `graph` as the project and record it as authored here.
    fn save(&self, graph: &Graph) {
        let text = serde_json::to_string_pretty(&serde_json::json!({ "graph": graph })).unwrap();
        std::fs::write(self.project.join(PROJECT_FILE), &text).unwrap();
        let digest = crate::library::trust::content::project_digest(&text).unwrap();
        self.ledger()
            .record(
                ContentKind::Project,
                &digest,
                ContentDecision::Authored,
                true,
            )
            .unwrap();
    }

    fn request(&self, name: &str) -> ExportRequest {
        ExportRequest {
            source_project: self.project.clone(),
            destination: self.out.join(name),
            app_name: name.to_string(),
            output: None,
            runtime: RuntimeSource {
                executable: self.runtime.join("patchwork-player.exe"),
                dependency_dirs: vec![self.redist.clone()],
            },
            ffmpeg: None,
            overwrite: false,
            purpose: super::ExportPurpose::Distribution,
        }
    }
}

fn video_out() -> NodeType {
    let node = crate::node_trait::NODE_REGISTRY
        .lock()
        .unwrap()
        .create(
            "video_out",
            &serde_json::json!({ "sink": "Window", "destination": "", "fullscreen": false }),
        )
        .unwrap();
    NodeType::Dynamic {
        inner: DynNode { node },
    }
}

fn dynamic(tag: &str, state: serde_json::Value) -> NodeType {
    let node = crate::node_trait::NODE_REGISTRY
        .lock()
        .unwrap()
        .create(tag, &state)
        .unwrap_or_else(|| panic!("{tag} is registered"));
    NodeType::Dynamic {
        inner: DynNode { node },
    }
}

fn node(json: serde_json::Value) -> NodeType {
    serde_json::from_value(json).unwrap()
}

/// Slider + Video Out (Window): the smallest playable project.
fn base_graph() -> (Graph, NodeId) {
    let mut graph = Graph::new();
    graph.add_node(
        NodeType::Slider {
            value: 0.5,
            min: 0.0,
            max: 1.0,
            step: 0.01,
            slider_color: [0, 0, 0],
            label: String::new(),
        },
        [0.0, 0.0],
    );
    let out = graph.add_node(video_out(), [200.0, 0.0]);
    (graph, out)
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    list_files(dir)
        .into_iter()
        .map(|rel| {
            let bytes = std::fs::read(dir.join(&rel)).unwrap();
            (rel, bytes)
        })
        .collect()
}

fn assert_failed(report: &ExportReport, expect: &str) {
    assert!(!report.succeeded(), "expected failure, got {report:?}");
    assert!(
        report.errors.iter().any(|e| e.contains(expect)),
        "no error containing {expect:?}: {:#?}",
        report.errors
    );
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// ── Runtime inspection ──────────────────────────────────────────────────────

#[test]
fn imports_and_build_profile_are_read_from_executables() {
    let exe = fake_exe(
        Some(BuildProfile::Release),
        &["KERNEL32.dll", "directml.dll"],
    );
    assert_eq!(
        imported_dlls(&exe).unwrap(),
        vec!["KERNEL32.dll", "directml.dll"]
    );
    assert_eq!(build_profile(&exe), Ok(BuildProfile::Release));
    let debug = fake_exe(Some(BuildProfile::Debug), &[]);
    assert_eq!(build_profile(&debug), Ok(BuildProfile::Debug));
    assert!(build_profile(&fake_exe(None, &[])).is_err());
    assert!(imported_dlls(b"not an exe").is_err());
    // A real executable: this test binary (a build of Patchwork).
    let real = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let imports = imported_dlls(&real).unwrap();
    assert!(
        imports
            .iter()
            .any(|d| d.eq_ignore_ascii_case("kernel32.dll")),
        "{imports:?}"
    );
    super::pe::touch_profile_marker();
    // This test binary's own marker: debug under `cargo test`, release
    // under `cargo test --release`.
    let expected = if cfg!(debug_assertions) {
        BuildProfile::Debug
    } else {
        BuildProfile::Release
    };
    assert_eq!(build_profile(&real), Ok(expected));
    assert!(super::pe::is_system_dll("KERNEL32.dll"));
    assert!(super::pe::is_system_dll("api-ms-win-crt-heap-l1-1-0.dll"));
    assert!(!super::pe::is_system_dll("VCRUNTIME140.dll"));
    assert!(!super::pe::is_system_dll("directml.dll"));
}

// ── Successful exports ──────────────────────────────────────────────────────

#[test]
fn a_minimal_project_exports_to_a_launchable_folder() {
    let fx = Fx::new("x_minimal");
    let (graph, output) = base_graph();
    fx.save(&graph);
    // Things that must never be exported, placed in the source project.
    for secret in [
        "keys.json",
        "api_keys.json",
        "last_session.json",
        "notes.txt",
    ] {
        std::fs::write(fx.project.join(secret), "secret").unwrap();
    }
    std::fs::create_dir_all(fx.project.join("trust")).unwrap();
    std::fs::write(fx.project.join("trust").join("content.json"), "{}").unwrap();
    let before = snapshot(&fx.project);

    let report = export_portable(&fx.request("My Show"), &fx.env());
    assert!(report.succeeded(), "{report:#?}");
    let dest = fx.out.join("My Show");
    assert_eq!(report.destination.as_deref(), Some(dest.as_path()));
    let mut expected: Vec<String> = [
        "DirectML.dll",
        "MSVCP140.dll",
        "My Show.exe",
        "VCRUNTIME140.dll",
        "licenses/DirectML-NOTICE.txt",
        "licenses/Patchwork-LICENSE.txt",
        "licenses/SOURCE.txt",
        "licenses/THIRD-PARTY-NOTICES.txt",
        "licenses/VCRuntime-NOTICE.txt",
        "player.json",
        "project/project.json",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    expected.extend(
        super::runtime::THIRD_PARTY_FILES
            .iter()
            .map(|(name, _)| format!("licenses/third-party/{name}")),
    );
    expected.sort();
    assert_eq!(report.files, expected);
    // The bundled notices are exactly the reviewed texts.
    for (name, text) in super::runtime::THIRD_PARTY_FILES {
        let written =
            std::fs::read_to_string(dest.join("licenses").join("third-party").join(name)).unwrap();
        assert_eq!(&written, text, "{name}");
    }
    assert!(report.dependencies.iter().any(|d| d == "directml.dll"));
    // An exact commit: no provenance warning; SOURCE.txt names it.
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let source = std::fs::read_to_string(dest.join("licenses").join("SOURCE.txt")).unwrap();
    assert!(
        source.contains(&format!("Source commit: {TEST_COMMIT}")),
        "{source}"
    );
    assert!(source.contains("exactly this commit"), "{source}");
    let vc = std::fs::read_to_string(dest.join("licenses").join("VCRuntime-NOTICE.txt")).unwrap();
    assert!(vc.contains(&fx.redist.display().to_string()), "{vc}");
    // The program is the Player runtime: no console window.
    let exe = std::fs::read(dest.join("My Show.exe")).unwrap();
    assert_eq!(super::pe::subsystem(&exe), Ok(super::pe::SUBSYSTEM_GUI));
    // The source is byte-identical, and nothing secret travelled.
    assert_eq!(snapshot(&fx.project), before);
    // No staging folder left behind.
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 1);

    // player.json: Stage 2.1 format, digest of the exported project.
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dest.join(MANIFEST_FILE)).unwrap()).unwrap();
    let text = std::fs::read_to_string(dest.join("project").join(PROJECT_FILE)).unwrap();
    let digest = crate::library::trust::content::project_digest(&text).unwrap();
    assert_eq!(manifest["schema"], "patchwork.player");
    assert_eq!(manifest["format"], 1);
    assert_eq!(manifest["project"], "project");
    assert_eq!(manifest["output"], output);
    assert_eq!(manifest["title"], "My Show");
    assert_eq!(manifest["patchwork_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["project_sha256"], digest.as_str());
    assert_eq!(report.project_digest.as_deref(), Some(digest.as_str()));

    // It launches as that app, whatever the arguments.
    let other = fx.tmp.0.join("elsewhere");
    match crate::app::player::launch_mode_at(
        &args(&["--player", &other.to_string_lossy(), "catalogue"]),
        &dest,
    )
    .unwrap()
    {
        crate::app::player::LaunchMode::Exported(launch) => {
            assert_eq!(launch.output, Some(output));
            assert!(launch.project_dir.ends_with("project"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_export_still_launches_after_the_folder_is_moved() {
    let fx = Fx::new("x_moved");
    let (mut graph, _) = base_graph();
    let picture = fx.tmp.0.join("outside").join("pic.png");
    std::fs::create_dir_all(picture.parent().unwrap()).unwrap();
    std::fs::write(&picture, b"png bytes").unwrap();
    graph.add_node(
        node(serde_json::json!({ "ImageNode": { "path": picture } })),
        [0.0, 200.0],
    );
    fx.save(&graph);
    let report = export_portable(&fx.request("Show"), &fx.env());
    assert!(report.succeeded(), "{report:#?}");

    let moved = fx.tmp.0.join("another disk").join("Show copy");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(fx.out.join("Show"), &moved).unwrap();
    let launch = crate::app::player::manifest::load_manifest(&moved, None).unwrap();
    let loaded = crate::app::io::load_project_graph(&launch.launch.project_json).unwrap();
    let paths: Vec<String> = asset_fields(&loaded)
        .into_iter()
        .map(|(_, p)| p)
        .filter(|p| !p.is_empty())
        .collect();
    assert_eq!(paths.len(), 1);
    let resolved = PathBuf::from(&paths[0]);
    assert!(
        resolved.starts_with(std::fs::canonicalize(&moved).unwrap()),
        "{resolved:?}"
    );
    assert_eq!(std::fs::read(resolved).unwrap(), b"png bytes");
}

#[test]
fn external_assets_are_copied_with_unique_names_and_relative_paths() {
    let fx = Fx::new("x_assets");
    let (mut graph, _) = base_graph();
    let outside = fx.tmp.0.join("media library");
    let a = outside.join("a").join("pic.png");
    let b = outside.join("b").join("PIC.png");
    let unicode = outside.join("Café photo é.png");
    let sound = outside.join("sound.wav");
    let inside = fx.project.join("media").join("inside.png");
    for (path, bytes) in [
        (&a, &b"A"[..]),
        (&b, b"B"),
        (&unicode, b"U"),
        (&sound, b"S"),
        (&inside, b"I"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    for path in [&a, &b, &unicode, &a] {
        graph.add_node(
            node(serde_json::json!({ "ImageNode": { "path": path } })),
            [0.0, 0.0],
        );
    }
    graph.add_node(
        node(serde_json::json!({ "AudioPlayer": { "file_path": sound } })),
        [0.0, 0.0],
    );
    // Saved relative, as the editor does for files inside the project.
    graph.add_node(
        node(serde_json::json!({ "ImageNode": { "path": "media/inside.png", "cached_file": "C:\\Users\\x\\.patchwork\\image_cache\\abc.png" } })),
        [0.0, 0.0],
    );
    fx.save(&graph);
    let report = export_portable(&fx.request("Gallery"), &fx.env());
    assert!(report.succeeded(), "{report:#?}");
    let project = fx.out.join("Gallery").join("project");
    let read = |rel: &str| std::fs::read(project.join(rel)).unwrap();
    assert_eq!(read("assets/images/pic.png"), b"A");
    assert_eq!(
        read("assets/images/PIC-2.png"),
        b"B",
        "same name, another file"
    );
    assert_eq!(read("assets/images/Café photo é.png"), b"U");
    assert_eq!(read("assets/audio/sound.wav"), b"S");
    assert_eq!(read("media/inside.png"), b"I");
    // The same file used twice is copied once.
    let images: Vec<String> = report
        .files
        .iter()
        .filter(|f| f.starts_with("project/assets/images/"))
        .cloned()
        .collect();
    assert_eq!(images.len(), 3, "{images:?}");
    // The exported project refers to files by relative path only, and the
    // per-user image cache is not referenced.
    let text = std::fs::read_to_string(project.join(PROJECT_FILE)).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(
        absolute_strings(&value).is_empty(),
        "{:?}",
        absolute_strings(&value)
    );
    assert!(!text.contains("image_cache"));
}

#[test]
fn packages_and_nested_dependencies_are_bundled_and_verified() {
    use crate::library::bundle::tests::{child, composite, leaf, publish};
    let fx = Fx::new("x_packages");
    let src = fx.tmp.0.join("src");
    let c = publish(
        &fx.library,
        &leaf(&src.join("c"), "C", 0.25),
        "C",
        None,
        "1.0.0",
    )
    .unwrap();
    let b = publish(
        &fx.library,
        &composite(&src.join("b"), "B", &[child(&c)]),
        "B",
        None,
        "1.0.0",
    )
    .unwrap();
    let (mut graph, _) = base_graph();
    let (file, package) = child(&b);
    graph.add_node(
        NodeType::CustomNodeInstance {
            file,
            inputs: Vec::new(),
            outputs: vec![crate::document::CustomNodePort {
                name: "out0".into(),
                kind: "Number".into(),
            }],
            package,
        },
        [0.0, 300.0],
    );
    fx.save(&graph);
    let report = export_portable(&fx.request("Packaged"), &fx.env());
    assert!(report.succeeded(), "{report:#?}");
    let packaged: Vec<&String> = report
        .files
        .iter()
        .filter(|f| f.starts_with("project/packages/"))
        .collect();
    assert!(
        packaged.iter().any(|f| f.contains(&b.manifest.id)),
        "{packaged:?}"
    );
    assert!(
        packaged
            .iter()
            .any(|f| f.contains("/deps/") && f.contains(&c.manifest.id)),
        "the nested dependency travels inside its parent: {packaged:?}"
    );
    // It runs from the export alone (no Library): identical output.
    let dest = fx.out.join("Packaged");
    let launch = crate::app::player::manifest::load_manifest(&dest, None).unwrap();
    let mut loaded = crate::app::io::load_project_graph(&launch.launch.project_json).unwrap();
    loaded.set_package_environment(Some(launch.launch.project_dir.clone()), None);
    let values = loaded.evaluate(0.0);
    let instance = loaded
        .nodes
        .iter()
        .find(|(_, n)| matches!(n.node_type, NodeType::CustomNodeInstance { .. }))
        .map(|(id, _)| *id)
        .unwrap();
    assert!(
        matches!(values.get(&(instance, 0)), Some(crate::graph::PortValue::Float(v)) if (*v - 0.25).abs() < 1e-6),
        "{:?}",
        values.get(&(instance, 0))
    );
}

#[test]
fn path_only_custom_nodes_inside_the_project_travel_with_it() {
    use crate::library::bundle::tests::leaf;
    let fx = Fx::new("x_loose");
    let inner = leaf(&fx.project.join("nodes").join("inner"), "Inner", 0.5);
    let (mut graph, _) = base_graph();
    graph.add_node(
        NodeType::CustomNodeInstance {
            file: inner.to_string_lossy().into_owned(),
            inputs: Vec::new(),
            outputs: vec![crate::document::CustomNodePort {
                name: "out".into(),
                kind: "Number".into(),
            }],
            package: None,
        },
        [0.0, 0.0],
    );
    fx.save(&graph);
    let report = export_portable(&fx.request("Loose"), &fx.env());
    assert!(report.succeeded(), "{report:#?}");
    assert!(
        report
            .files
            .contains(&"project/nodes/inner/Inner.pwnode".to_string()),
        "{:?}",
        report.files
    );

    // Outside the project: refused, nothing written.
    let fx = Fx::new("x_loose_outside");
    let outside = leaf(&fx.tmp.0.join("elsewhere"), "Far", 0.5);
    let (mut graph, _) = base_graph();
    graph.add_node(
        NodeType::CustomNodeInstance {
            file: outside.to_string_lossy().into_owned(),
            inputs: Vec::new(),
            outputs: vec![crate::document::CustomNodePort {
                name: "out".into(),
                kind: "Number".into(),
            }],
            package: None,
        },
        [0.0, 0.0],
    );
    fx.save(&graph);
    let report = export_portable(&fx.request("Loose"), &fx.env());
    assert_failed(&report, "outside the project");
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 0);
}

#[test]
fn nested_path_only_custom_nodes_are_found_before_anything_runs() {
    use crate::library::bundle::tests::{composite, leaf};
    let tmp = TempDir::new("x_nested_preflight");
    let dir = tmp.0.join("project");
    let inner = leaf(&dir.join("inner"), "Inner", 0.5);
    let outer = composite(
        &dir,
        "Outer",
        &[(inner.to_string_lossy().into_owned(), None)],
    );
    let mut graph = Graph::new();
    graph.add_node(
        NodeType::CustomNodeInstance {
            file: outer.to_string_lossy().into_owned(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            package: None,
        },
        [0.0, 0.0],
    );
    let store = crate::library::trust::TrustStore::at(tmp.0.join("trust").join("packages.json"));
    graph.set_package_environment(Some(dir.clone()), None);
    graph.set_trust_policy(TrustPolicy::Store(store));
    let blocked = graph.preflight_custom_nodes();
    let names: Vec<String> = blocked
        .iter()
        .map(|b| b.package.dir.to_string_lossy().into_owned())
        .collect();
    assert_eq!(blocked.len(), 2, "outer and its nested file: {names:?}");
    assert!(names.iter().any(|n| n.ends_with("Outer.pwnode")));
    assert!(names.iter().any(|n| n.ends_with("Inner.pwnode")));
    assert!(!tmp.0.join("trust").exists(), "nothing recorded");
}

#[test]
fn ffmpeg_is_included_only_when_needed_and_with_its_notices() {
    let fx = Fx::new("x_ffmpeg");
    let (mut graph, _) = base_graph();
    let clip = fx.project.join("clip.mp4");
    std::fs::write(&clip, b"mp4").unwrap();
    graph.add_node(
        node(serde_json::json!({ "VideoPlayer": { "path": "clip.mp4" } })),
        [0.0, 0.0],
    );
    fx.save(&graph);
    assert_failed(
        &export_portable(&fx.request("Video"), &fx.env()),
        "needs ffmpeg",
    );

    let ffmpeg = fx.tmp.0.join("ffmpeg");
    std::fs::create_dir_all(&ffmpeg).unwrap();
    std::fs::write(ffmpeg.join("ffmpeg.exe"), fake_exe(None, &["KERNEL32.dll"])).unwrap();
    let mut request = fx.request("Video");
    request.ffmpeg = Some(FfmpegSource {
        executable: ffmpeg.join("ffmpeg.exe"),
        licence_files: Vec::new(),
    });
    assert_failed(&export_portable(&request, &fx.env()), "licence notices");

    std::fs::write(ffmpeg.join("LICENSE.txt"), "LGPL notice").unwrap();
    request.ffmpeg.as_mut().unwrap().licence_files = vec![ffmpeg.join("LICENSE.txt")];
    let report = export_portable(&request, &fx.env());
    assert!(report.succeeded(), "{report:#?}");
    assert!(report.files.contains(&"ffmpeg.exe".to_string()));
    assert!(
        report
            .files
            .contains(&"licenses/ffmpeg/LICENSE.txt".to_string())
    );
    assert!(report.files.contains(&"project/clip.mp4".to_string()));

    // Not needed: not included (warned).
    let fx2 = Fx::new("x_no_ffmpeg");
    fx2.save(&base_graph().0);
    let mut request = fx2.request("Plain");
    request.ffmpeg = Some(FfmpegSource {
        executable: ffmpeg.join("ffmpeg.exe"),
        licence_files: vec![ffmpeg.join("LICENSE.txt")],
    });
    let report = export_portable(&request, &fx2.env());
    assert!(report.succeeded(), "{report:#?}");
    assert!(!report.files.contains(&"ffmpeg.exe".to_string()));
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("does not need it"))
    );
}

// ── Refusals (nothing is written) ───────────────────────────────────────────

#[test]
fn unsupported_paths_are_refused_not_left_broken() {
    let fx = Fx::new("x_paths");
    let (mut graph, _) = base_graph();
    let track = fx.tmp.0.join("music").join("song.mp3");
    std::fs::create_dir_all(track.parent().unwrap()).unwrap();
    std::fs::write(&track, b"mp3").unwrap();
    graph.add_node(
        node(serde_json::json!({ "AudioPlaylist": { "tracks": [track] } })),
        [0.0, 0.0],
    );
    graph.add_node(
        node(
            serde_json::json!({ "ImageNode": { "path": "", "save_path": "D:\\renders\\out.png" } }),
        ),
        [0.0, 0.0],
    );
    graph.add_node(
        node(serde_json::json!({ "ImageNode": { "path": fx.tmp.0.join("missing.png") } })),
        [0.0, 0.0],
    );
    fx.save(&graph);
    let report = export_portable(&fx.request("Paths"), &fx.env());
    assert_failed(&report, "song.mp3");
    assert_failed(&report, "saves images to");
    assert_failed(&report, "missing");
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 0);
}

#[test]
fn restricted_credentialed_and_tool_dependent_nodes_are_refused() {
    let fx = Fx::new("x_nodes");
    let (mut graph, _) = base_graph();
    graph.add_node(NodeType::McpServer, [0.0, 0.0]);
    graph.add_node(dynamic("terminal", serde_json::json!({})), [0.0, 0.0]);
    graph.add_node(dynamic("text_gen", serde_json::json!({})), [0.0, 0.0]);
    graph.add_node(dynamic("tts", serde_json::json!({})), [0.0, 0.0]);
    graph.add_node(
        dynamic(
            "video_in",
            serde_json::json!({ "source": "Url", "device_id": "https://youtube.com/watch?v=x" }),
        ),
        [0.0, 0.0],
    );
    let mut recorder = video_out();
    if let NodeType::Dynamic { inner } = &mut recorder {
        inner
            .node
            .load_state(&serde_json::json!({ "sink": "FileRecord" }));
    }
    graph.add_node(recorder, [0.0, 0.0]);
    fx.save(&graph);
    let report = export_portable(&fx.request("Nodes"), &fx.env());
    for expect in [
        "(McpServer) cannot be exported",
        "(terminal) cannot be exported",
        "(text_gen) needs private API credentials",
        "Text to Speech",
        "yt-dlp",
        "records to a file",
    ] {
        assert_failed(&report, expect);
    }
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 0);
}

#[test]
fn a_project_not_allowed_here_or_without_one_output_is_refused() {
    let fx = Fx::new("x_not_allowed");
    let (graph, _) = base_graph();
    let text = serde_json::to_string_pretty(&serde_json::json!({ "graph": graph })).unwrap();
    std::fs::write(fx.project.join(PROJECT_FILE), text).unwrap();
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "not allowed to run on this computer",
    );

    let fx = Fx::new("x_outputs");
    let (mut graph, first) = base_graph();
    let second = graph.add_node(video_out(), [0.0, 0.0]);
    fx.save(&graph);
    assert_failed(
        &export_portable(&fx.request("Two"), &fx.env()),
        "several Video Out",
    );
    let mut request = fx.request("Two");
    request.output = Some(second);
    let report = export_portable(&request, &fx.env());
    assert!(report.succeeded(), "{report:#?}");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fx.out.join("Two").join(MANIFEST_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["output"], second);
    assert_ne!(first, second);
}

#[test]
fn corrupt_or_unauthorized_packages_are_refused() {
    use crate::library::bundle::tests::{child, leaf, publish};
    let fx = Fx::new("x_corrupt");
    let c = publish(
        &fx.library,
        &leaf(&fx.tmp.0.join("src"), "C", 0.25),
        "C",
        None,
        "1.0.0",
    )
    .unwrap();
    let (mut graph, _) = base_graph();
    let (file, package) = child(&c);
    graph.add_node(
        NodeType::CustomNodeInstance {
            file,
            inputs: Vec::new(),
            outputs: Vec::new(),
            package,
        },
        [0.0, 0.0],
    );
    fx.save(&graph);
    // Not allowed on this computer (real trust store, no record).
    let mut env = fx.env();
    env.trust = TrustPolicy::Store(crate::library::trust::TrustStore::at(
        fx.home.join("trust").join("packages.json"),
    ));
    assert_failed(
        &export_portable(&fx.request("P"), &env),
        "cannot be exported",
    );
    // Tampered after publishing.
    let mut text = std::fs::read_to_string(&c.entry_path).unwrap();
    text.push(' ');
    std::fs::write(&c.entry_path, text).unwrap();
    let report = export_portable(&fx.request("P"), &fx.env());
    assert!(!report.succeeded(), "{report:#?}");
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 0);
}

#[test]
fn runtime_problems_are_refused() {
    // A missing Visual C++ runtime file.
    let fx = Fx::new("x_dll");
    fx.save(&base_graph().0);
    std::fs::remove_file(fx.redist.join("VCRUNTIME140.dll")).unwrap();
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "VCRUNTIME140.dll",
    );
    // A copy beside the runtime (a hand-assembled folder) is not accepted.
    std::fs::write(
        fx.runtime.join("VCRUNTIME140.dll"),
        fake_dll(&["KERNEL32.dll"]),
    )
    .unwrap();
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "installed Visual Studio redistributable folder",
    );
    std::fs::remove_file(fx.runtime.join("VCRUNTIME140.dll")).unwrap();
    std::fs::write(
        fx.redist.join("VCRUNTIME140.dll"),
        fake_dll(&["KERNEL32.dll"]),
    )
    .unwrap();
    assert!(export_portable(&fx.request("A"), &fx.env()).succeeded());
    // Unsigned Visual C++ files are refused.
    let mut env = fx.env();
    env.dependencies.verify_signature = signature_bad;
    assert_failed(
        &export_portable(&fx.request("B"), &env),
        "does not have a valid signature",
    );
    // A dependency's own dependency must be there too.
    std::fs::write(
        fx.redist.join("MSVCP140.dll"),
        fake_dll(&["VCRUNTIME140_1.dll", "KERNEL32.dll"]),
    )
    .unwrap();
    assert_failed(
        &export_portable(&fx.request("B"), &fx.env()),
        "MSVCP140.dll needs VCRUNTIME140_1.dll",
    );

    // A different DirectML, or a DLL nobody approved.
    let fx = Fx::new("x_pinned");
    fx.save(&base_graph().0);
    let env = fx.env();
    std::fs::write(
        fx.runtime.join("DirectML.dll"),
        fake_dll(&["KERNEL32.dll", "USER32.dll"]),
    )
    .unwrap();
    assert_failed(&export_portable(&fx.request("A"), &env), "not the verified");
    let fx = Fx::new("x_unknown_dll");
    fx.save(&base_graph().0);
    let mut imports = RUNTIME_IMPORTS.to_vec();
    imports.push("helper.dll");
    std::fs::write(
        fx.runtime.join("patchwork-player.exe"),
        fake_exe(Some(BuildProfile::Release), &imports),
    )
    .unwrap();
    std::fs::write(fx.runtime.join("helper.dll"), fake_dll(&["KERNEL32.dll"])).unwrap();
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "helper.dll is not an approved redistributable",
    );

    // A debug build, or not Patchwork at all.
    let fx = Fx::new("x_debug");
    fx.save(&base_graph().0);
    let exe = fx.runtime.join("patchwork-player.exe");
    std::fs::write(&exe, fake_exe(Some(BuildProfile::Debug), RUNTIME_IMPORTS)).unwrap();
    assert_failed(&export_portable(&fx.request("A"), &fx.env()), "debug build");
    std::fs::write(&exe, fake_exe(None, RUNTIME_IMPORTS)).unwrap();
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "not a Patchwork runtime",
    );
    std::fs::write(&exe, "MZ nonsense").unwrap();
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "not a valid Windows executable",
    );

    // The console editor build, a console program, or a build that does not
    // say what it is.
    let release = Some(BuildProfile::Release);
    let player_info = || Some(super::pe::build_info_bytes("player", TEST_COMMIT, "0"));
    std::fs::write(
        &exe,
        fake_exe_as(
            release,
            RUNTIME_IMPORTS,
            super::pe::SUBSYSTEM_CONSOLE,
            Some(super::pe::build_info_bytes("editor", TEST_COMMIT, "0")),
        ),
    )
    .unwrap();
    let report = export_portable(&fx.request("A"), &fx.env());
    assert_failed(&report, "Patchwork editor build");
    assert_failed(&report, "console program");
    std::fs::write(
        &exe,
        fake_exe_as(
            release,
            RUNTIME_IMPORTS,
            super::pe::SUBSYSTEM_CONSOLE,
            player_info(),
        ),
    )
    .unwrap();
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "must not open a console window",
    );
    std::fs::write(
        &exe,
        fake_exe_as(release, RUNTIME_IMPORTS, super::pe::SUBSYSTEM_GUI, None),
    )
    .unwrap();
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "no Patchwork build information",
    );
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 0);

    // Dependencies are never taken from Windows' own folders.
    if let Some(root) = std::env::var_os("SystemRoot") {
        let fx = Fx::new("x_system");
        fx.save(&base_graph().0);
        let mut request = fx.request("A");
        request.runtime.dependency_dirs = vec![PathBuf::from(root).join("System32")];
        assert_failed(&export_portable(&request, &fx.env()), "system folder");
    }
}

#[test]
fn subsystem_and_build_information_are_read_from_executables() {
    use super::pe::{BuildInfo, SUBSYSTEM_CONSOLE, SUBSYSTEM_GUI, build_info, subsystem};
    let gui = fake_exe(Some(BuildProfile::Release), &[]);
    assert_eq!(subsystem(&gui), Ok(SUBSYSTEM_GUI));
    assert_eq!(
        build_info(&gui),
        Ok(BuildInfo {
            flavor: "player".to_string(),
            commit: TEST_COMMIT.to_string(),
            modified: Some(false),
        })
    );
    assert_eq!(build_info(&gui).unwrap().exact_commit(), Some(TEST_COMMIT));
    let modified = fake_exe_as(
        None,
        &[],
        SUBSYSTEM_CONSOLE,
        Some(super::pe::build_info_bytes("player", TEST_COMMIT, "1")),
    );
    assert_eq!(subsystem(&modified), Ok(SUBSYSTEM_CONSOLE));
    assert_eq!(build_info(&modified).unwrap().modified, Some(true));
    assert_eq!(build_info(&modified).unwrap().exact_commit(), None);
    let mut twice = gui.clone();
    twice.extend_from_slice(&super::pe::build_info_bytes("editor", "unknown", "unknown"));
    assert!(build_info(&twice).is_err());
    assert!(subsystem(b"nonsense").is_err());

    // This test binary is the editor build: a console program recording the
    // source it was built from.
    super::pe::touch_profile_marker();
    let real = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    assert_eq!(subsystem(&real), Ok(SUBSYSTEM_CONSOLE));
    let info = build_info(&real).unwrap();
    assert_eq!(info.flavor, "editor");
    assert_eq!(info.commit, super::pe::SOURCE_COMMIT);
    if info.commit != "unknown" {
        assert_eq!(info.commit.len(), 40);
        assert!(info.commit.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

#[test]
fn source_provenance_is_exact_or_says_it_is_not() {
    // Built from a commit with local changes: exported, with a warning and
    // a SOURCE.txt that does not pretend the commit is the source.
    let fx = Fx::new("x_provenance");
    fx.save(&base_graph().0);
    let exe = fx.runtime.join("patchwork-player.exe");
    std::fs::write(
        &exe,
        fake_exe_as(
            Some(BuildProfile::Release),
            RUNTIME_IMPORTS,
            super::pe::SUBSYSTEM_GUI,
            Some(super::pe::build_info_bytes("player", TEST_COMMIT, "1")),
        ),
    )
    .unwrap();
    let report = export_portable(&fx.request("Changed"), &fx.env());
    assert!(report.succeeded(), "{report:#?}");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("with local changes") && w.contains("corresponding source")),
        "{:?}",
        report.warnings
    );
    let text = std::fs::read_to_string(fx.out.join("Changed").join("licenses").join("SOURCE.txt"))
        .unwrap();
    assert!(text.contains(TEST_COMMIT), "{text}");
    assert!(text.contains("NOT built from this commit alone"), "{text}");
    assert!(
        text.contains("A repository link alone does not meet"),
        "{text}"
    );

    // Unknown source.
    let unknown = super::pe::BuildInfo {
        flavor: "player".to_string(),
        commit: "unknown".to_string(),
        modified: None,
    };
    let text = source_notice("X", Some(&unknown));
    assert!(text.contains("Source commit: unknown"), "{text}");
    assert!(!text.contains("exactly this commit"));
}

#[test]
fn visual_studio_redistributables_are_found_and_recognised() {
    use super::runtime::{find_vc_redist, is_vc_redist_dir, is_vc_runtime};
    let tmp = TempDir::new("x_vc_find");
    let pf = tmp.0.join("Program Files");
    let msvc = pf
        .join("Microsoft Visual Studio")
        .join("2022")
        .join("BuildTools")
        .join("VC")
        .join("Redist")
        .join("MSVC");
    let crt = |version: &str, variant: &[&str]| {
        let mut dir = msvc.join(version);
        for part in variant {
            dir = dir.join(part);
        }
        let dir = dir.join("x64").join("Microsoft.VC143.CRT");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vcruntime140.dll"), "x").unwrap();
        dir
    };
    let old = crt("14.38.33130", &[]);
    let new = crt("14.44.35112", &[]);
    crt("14.50.1", &["onecore"]);
    crt("14.51.1", &["debug_nonredist"]);
    assert_eq!(find_vc_redist(std::slice::from_ref(&pf)), Some(new.clone()));
    assert!(is_vc_redist_dir(&old));
    assert!(!is_vc_redist_dir(
        &msvc
            .join("14.44.35112")
            .join("debug_nonredist")
            .join("x64")
            .join("Microsoft.VC143.DebugCRT")
    ));
    assert!(!is_vc_redist_dir(&msvc.join("14.44.35112").join("x64")));
    assert!(!is_vc_redist_dir(&tmp.0));
    assert_eq!(find_vc_redist(&[tmp.0.join("nothing")]), None);
    for name in [
        "VCRUNTIME140.dll",
        "vcruntime140_1.dll",
        "MSVCP140.dll",
        "msvcp140_atomic_wait.dll",
        "concrt140.dll",
    ] {
        assert!(is_vc_runtime(name), "{name}");
    }
    for name in ["DirectML.dll", "msvcrt.dll", "vcruntime140d.dl"] {
        assert!(!is_vc_runtime(name), "{name}");
    }
}

#[cfg(windows)]
#[test]
fn authenticode_signatures_are_checked() {
    let tmp = TempDir::new("x_authenticode");
    let unsigned = tmp.0.join("unsigned.dll");
    std::fs::write(&unsigned, fake_dll(&["KERNEL32.dll"])).unwrap();
    assert!(super::runtime::verify_authenticode(&unsigned).is_err());
    // A real Visual C++ runtime file, when Visual Studio is installed.
    let program_files: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
        .collect();
    if let Some(redist) = super::runtime::find_vc_redist(&program_files) {
        let file = redist.join("vcruntime140.dll");
        assert_eq!(super::runtime::verify_authenticode(&file), Ok(()));
        // The same bytes, changed by one byte, no longer verify.
        let mut bytes = std::fs::read(&file).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        let tampered = tmp.0.join("vcruntime140.dll");
        std::fs::write(&tampered, bytes).unwrap();
        assert!(super::runtime::verify_authenticode(&tampered).is_err());
    }
}

// ── Background exports ──────────────────────────────────────────────────────

/// Background-export tests share the one-export-at-a-time slot.
static JOB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn job_lock() -> std::sync::MutexGuard<'static, ()> {
    JOB_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn exports_run_in_the_background_one_at_a_time() {
    let _lock = job_lock();
    let fx = Fx::new("x_job");
    let (mut graph, _) = base_graph();
    // A sizeable asset so the copy takes a moment.
    std::fs::write(fx.project.join("big.png"), vec![7u8; 8 << 20]).unwrap();
    graph.add_node(
        node(serde_json::json!({ "ImageNode": { "path": "big.png" } })),
        [0.0, 0.0],
    );
    fx.save(&graph);
    let before = snapshot(&fx.project);

    let job = ExportJob::start(fx.request("Background"), fx.env()).unwrap();
    // While it runs, a second export is refused (it can only start once the
    // first has finished and released the slot).
    match ExportJob::start(fx.request("Second"), fx.env()) {
        Err(e) => assert!(e.contains("already running"), "{e}"),
        Ok(second) => assert!(second.wait().succeeded()),
    }
    let report = job.wait();
    assert!(report.succeeded(), "{report:#?}");
    assert!(
        report.files.contains(&"project/big.png".to_string()),
        "{:?}",
        report.files
    );
    assert_eq!(snapshot(&fx.project), before, "the source is only read");
    // The slot is free again.
    let again = ExportJob::start(fx.request("Again"), fx.env()).unwrap();
    assert!(again.wait().succeeded());
}

#[test]
fn a_cancelled_export_leaves_nothing() {
    let _lock = job_lock();
    let fx = Fx::new("x_cancel");
    let (mut graph, _) = base_graph();
    std::fs::write(fx.project.join("huge.png"), vec![3u8; 96 << 20]).unwrap();
    graph.add_node(
        node(serde_json::json!({ "ImageNode": { "path": "huge.png" } })),
        [0.0, 0.0],
    );
    fx.save(&graph);
    let before = snapshot(&fx.project);

    // Cancelled while copying.
    let mut job = ExportJob::start(fx.request("Cancelled"), fx.env()).unwrap();
    let started = std::time::Instant::now();
    while job.progress().done_bytes == 0 && job.poll().is_none() {
        assert!(started.elapsed() < std::time::Duration::from_secs(60));
        std::thread::yield_now();
    }
    job.cancel();
    let report = job.wait();
    assert!(report.cancelled, "{report:#?}");
    assert!(!report.succeeded());
    assert_eq!(
        std::fs::read_dir(&fx.out).unwrap().count(),
        0,
        "no export and no staging folder"
    );

    // Cancelled before anything is written.
    let control = ExportControl::default();
    control.cancel();
    let report = export_portable_with(&fx.request("Early"), &fx.env(), &control);
    assert!(report.cancelled);
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 0);

    // Replacing an earlier export, cancelled: the earlier one is kept.
    assert!(export_portable(&fx.request("Kept"), &fx.env()).succeeded());
    let earlier = snapshot(&fx.out.join("Kept"));
    let mut replace = fx.request("Kept");
    replace.overwrite = true;
    let mut job = ExportJob::start(replace, fx.env()).unwrap();
    while job.progress().done_bytes == 0 && job.poll().is_none() {
        std::thread::yield_now();
    }
    job.cancel();
    let report = job.wait();
    assert!(report.cancelled, "{report:#?}");
    assert_eq!(snapshot(&fx.out.join("Kept")), earlier);
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 1);
    assert_eq!(snapshot(&fx.project), before, "the source is only read");
}

#[test]
fn unsafe_destinations_are_refused() {
    let fx = Fx::new("x_dest");
    fx.save(&base_graph().0);
    let try_dest = |dest: PathBuf, env: &ExportEnvironment| {
        let mut request = fx.request("A");
        request.destination = dest;
        export_portable(&request, env)
    };
    assert_failed(
        &try_dest(PathBuf::from("relative\\A"), &fx.env()),
        "absolute path",
    );
    assert_failed(
        &try_dest(fx.project.join("A"), &fx.env()),
        "inside the project",
    );
    assert_failed(
        &try_dest(fx.tmp.0.join("no such folder").join("A"), &fx.env()),
        "does not exist",
    );
    assert_failed(
        &try_dest(fx.runtime.join("A"), &fx.env()),
        "protected or application folder",
    );
    let mut env = fx.env();
    env.protected = vec![fx.out.clone()];
    assert_failed(
        &try_dest(fx.out.join("A"), &env),
        "protected or application folder",
    );
    assert_failed(&try_dest(fx.out.join("CON"), &fx.env()), "not acceptable");
    // None of the refusals wrote anything.
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 0);
    assert!(!fx.project.join("A").exists());
}

#[test]
fn existing_folders_are_never_replaced_without_confirmation() {
    let fx = Fx::new("x_overwrite");
    fx.save(&base_graph().0);
    // Some unrelated folder: never replaced, even when confirmed.
    let other = fx.out.join("A");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("mine.txt"), "keep").unwrap();
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "already exists",
    );
    let mut confirmed = fx.request("A");
    confirmed.overwrite = true;
    assert_failed(
        &export_portable(&confirmed, &fx.env()),
        "not an earlier Patchwork export",
    );
    assert_eq!(
        std::fs::read_to_string(other.join("mine.txt")).unwrap(),
        "keep"
    );

    // An earlier export: replaced only when confirmed.
    std::fs::remove_dir_all(&other).unwrap();
    assert!(export_portable(&fx.request("A"), &fx.env()).succeeded());
    assert_failed(
        &export_portable(&fx.request("A"), &fx.env()),
        "already exists",
    );
    let report = export_portable(&confirmed, &fx.env());
    assert!(report.succeeded(), "{report:#?}");
    assert_eq!(
        std::fs::read_dir(&fx.out).unwrap().count(),
        1,
        "no leftovers"
    );
}

#[cfg(windows)]
#[test]
fn a_junction_on_the_way_to_the_destination_is_refused() {
    let fx = Fx::new("x_dest_junction");
    fx.save(&base_graph().0);
    let target = fx.tmp.0.join("real");
    std::fs::create_dir_all(&target).unwrap();
    let link = fx.out.join("linked");
    let ok = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(ok, "mklink /J failed");
    let mut request = fx.request("A");
    request.destination = link.join("A");
    assert_failed(&export_portable(&request, &fx.env()), "not a link");
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
}

#[test]
fn a_failure_while_writing_leaves_no_partial_export() {
    let fx = Fx::new("x_atomic");
    fx.save(&base_graph().0);
    FAIL_BEFORE_MANIFEST.with(|f| f.set(true));
    let report = export_portable(&fx.request("A"), &fx.env());
    FAIL_BEFORE_MANIFEST.with(|f| f.set(false));
    assert_failed(&report, "injected failure");
    assert_eq!(
        std::fs::read_dir(&fx.out).unwrap().count(),
        0,
        "staging removed, nothing final"
    );

    // Replacing an earlier export that then fails keeps the earlier one.
    assert!(export_portable(&fx.request("A"), &fx.env()).succeeded());
    let before = snapshot(&fx.out.join("A"));
    let mut confirmed = fx.request("A");
    confirmed.overwrite = true;
    FAIL_BEFORE_MANIFEST.with(|f| f.set(true));
    let report = export_portable(&confirmed, &fx.env());
    FAIL_BEFORE_MANIFEST.with(|f| f.set(false));
    assert_failed(&report, "injected failure");
    assert_eq!(snapshot(&fx.out.join("A")), before);
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 1);
}

#[test]
fn application_names_must_be_plain_file_names() {
    for good in ["My Show", "Ausstellung – Raum 2", "展示", "a"] {
        assert!(check_app_name(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        " ",
        "a/b",
        "a\\b",
        "a:b",
        "x?",
        "trailing.",
        "trailing ",
        "CON",
        "com1",
        "LPT9",
        &"x".repeat(65),
    ] {
        assert!(check_app_name(bad).is_err(), "{bad:?}");
    }
}

/// Manual: a real export with this computer's records and real runtime
/// discovery (never in the normal suite).
/// `cargo test --release -- --ignored manual_real_export --nocapture` with
/// PATCHWORK_EXPORT_SOURCE, PATCHWORK_EXPORT_DEST, PATCHWORK_EXPORT_EDITOR
/// (the release `patchwork.exe`; `patchwork-player.exe` and the Visual
/// Studio redistributables are found from it as the dialog does) and
/// optionally PATCHWORK_EXPORT_NAME.
#[test]
#[ignore = "manual: real export using this computer's trust records"]
fn manual_real_export() {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let (Some(source), Some(dest), Some(editor)) = (
        var("PATCHWORK_EXPORT_SOURCE"),
        var("PATCHWORK_EXPORT_DEST"),
        var("PATCHWORK_EXPORT_EDITOR"),
    ) else {
        eprintln!("set PATCHWORK_EXPORT_SOURCE, _DEST and _EDITOR");
        return;
    };
    let name = std::env::var("PATCHWORK_EXPORT_NAME").unwrap_or_else(|_| {
        dest.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let runtime = super::runtime::discover(&editor).expect("runtime");
    eprintln!("{runtime:#?}");
    let request = ExportRequest {
        source_project: source,
        destination: dest,
        app_name: name,
        output: None,
        runtime,
        ffmpeg: None,
        overwrite: false,
        // PATCHWORK_EXPORT_INTERNAL_TEST=1: an internal test build (as the
        // dialog's Build choice), labelled NOT FOR DISTRIBUTION.
        purpose: if std::env::var("PATCHWORK_EXPORT_INTERNAL_TEST").as_deref() == Ok("1") {
            super::ExportPurpose::InternalTest
        } else {
            super::ExportPurpose::Distribution
        },
    };
    let env = ExportEnvironment::this_computer();
    let report = export_portable(&request, &env);
    eprintln!("{report:#?}");
    assert!(report.succeeded());
}

/// Manual (P6 Stage 2.6/2.7): a MediaPipe webcam project exported with the
/// real runtime: refused as a distributable build, exported as a labelled
/// internal test build. The project: Video In (camera 0, "Start when
/// played") -> Hand Tracking -> Face Tracking -> Pose Tracking (each draws
/// on the previous one's annotated frame) -> Video Out (Window).
/// `cargo test --release -- --ignored manual_mediapipe_internal_test_export
/// --nocapture` with PATCHWORK_EXPORT_WORK (an empty scratch folder),
/// PATCHWORK_EXPORT_EDITOR, PATCHWORK_EXPORT_FFMPEG (camera capture runs
/// ffmpeg) and PATCHWORK_EXPORT_FFMPEG_LICENCES (`;`-separated). The
/// project is recorded as authored in a ledger inside the scratch folder
/// only; this computer's records are not touched.
#[test]
#[ignore = "manual: real export with the real runtime"]
fn manual_mediapipe_internal_test_export() {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let (Some(work), Some(editor), Some(ffmpeg), Some(licences)) = (
        var("PATCHWORK_EXPORT_WORK"),
        var("PATCHWORK_EXPORT_EDITOR"),
        var("PATCHWORK_EXPORT_FFMPEG"),
        std::env::var("PATCHWORK_EXPORT_FFMPEG_LICENCES").ok(),
    ) else {
        eprintln!("set PATCHWORK_EXPORT_WORK, _EDITOR, _FFMPEG and _FFMPEG_LICENCES");
        return;
    };
    let project = work.join("MediaPipe Show");
    let exports = work.join("exports");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&exports).unwrap();
    let mut graph = Graph::new();
    let mut add = |kind: &str, state: serde_json::Value, x: f32| {
        let node = crate::node_trait::NODE_REGISTRY
            .lock()
            .unwrap()
            .create(kind, &state)
            .unwrap();
        graph.add_node(
            NodeType::Dynamic {
                inner: DynNode { node },
            },
            [x, 0.0],
        )
    };
    let camera = add(
        "video_in",
        serde_json::json!({
            "source": "Camera", "device_id": "0", "res_w": 640, "res_h": 480,
            "start_on_play": true
        }),
        0.0,
    );
    let hand = add("hand_detection", serde_json::json!({}), 300.0);
    let face = add("face_detection", serde_json::json!({}), 600.0);
    let pose = add("pose_detection", serde_json::json!({}), 900.0);
    let out = graph.add_node(video_out(), [1200.0, 0.0]);
    for (from, to) in [(camera, hand), (hand, face), (face, pose), (pose, out)] {
        graph.add_connection(from, 0, to, 0);
    }
    assert_eq!(graph.connections.len(), 4, "the chain must be wired");
    let text = serde_json::to_string_pretty(&serde_json::json!({ "graph": graph })).unwrap();
    std::fs::write(project.join(PROJECT_FILE), &text).unwrap();
    let ledger = ContentLedger::at(work.join("ledger").join("content.json"));
    let digest = crate::library::trust::content::project_digest(&text).unwrap();
    ledger
        .record(
            ContentKind::Project,
            &digest,
            ContentDecision::Authored,
            true,
        )
        .unwrap();
    let mut env = ExportEnvironment::this_computer();
    env.ledger = Some(ledger);
    let runtime = super::runtime::discover(&editor).expect("runtime");
    let mut request = ExportRequest {
        source_project: project.clone(),
        destination: exports.join("MediaPipe Show"),
        app_name: "MediaPipe Show".to_string(),
        output: None,
        runtime,
        ffmpeg: Some(FfmpegSource {
            executable: ffmpeg,
            licence_files: licences.split(';').map(PathBuf::from).collect(),
        }),
        overwrite: false,
        purpose: super::ExportPurpose::Distribution,
    };
    let refused = export_portable(&request, &env);
    eprintln!("DISTRIBUTION: {:#?}", refused.errors);
    assert!(!refused.succeeded());
    assert!(!request.destination.exists());
    request.purpose = super::ExportPurpose::InternalTest;
    request.destination = exports.join("MediaPipe Show (INTERNAL TEST - NOT FOR DISTRIBUTION)");
    let report = export_portable(&request, &env);
    eprintln!("INTERNAL TEST: {report:#?}");
    assert!(report.succeeded());
    assert!(report.internal_test_build);
}

/// Manual (P6 Stage 2.7): the three MediaPipe pipelines (the code and the
/// six embedded models every Player carries) on still images, with timings
/// and whether DirectML was loaded. `cargo test --release -- --ignored
/// manual_mediapipe_inference_on_images --nocapture` with
/// PATCHWORK_ML_IMAGES (a folder of .jpg/.png); the annotated frames are
/// written to its `annotated` subfolder for inspection.
#[test]
#[ignore = "manual: MediaPipe inference on sample images"]
fn manual_mediapipe_inference_on_images() {
    use crate::graph::{ImageData, MlPreset};
    use crate::nodes::ml_model::{
        BUNDLED_FACE_SENTINEL, BUNDLED_HAND_SENTINEL, BUNDLED_POSE_SENTINEL, MlInferenceRequest,
        run_inference,
    };
    let Some(dir) = std::env::var_os("PATCHWORK_ML_IMAGES").map(PathBuf::from) else {
        eprintln!("set PATCHWORK_ML_IMAGES");
        return;
    };
    let out = dir.join("annotated");
    std::fs::create_dir_all(&out).unwrap();
    let mut images: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("png"))
        })
        .collect();
    images.sort();
    let pipelines = [
        ("hand", BUNDLED_HAND_SENTINEL, MlPreset::HandDetection),
        ("face", BUNDLED_FACE_SENTINEL, MlPreset::FaceDetection),
        ("pose", BUNDLED_POSE_SENTINEL, MlPreset::PoseEstimation),
    ];
    for path in &images {
        let rgba = image::open(path).unwrap().to_rgba8();
        let (width, height) = rgba.dimensions();
        let img = std::sync::Arc::new(ImageData::new(width, height, rgba.into_raw()));
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        for (label, sentinel, preset) in &pipelines {
            let request = MlInferenceRequest {
                model_path: sentinel.to_string(),
                labels_path: String::new(),
                confidence: 0.5,
                preset: preset.clone(),
                image: img.clone(),
                node_id: 1,
            };
            let mut times = Vec::new();
            let mut last = None;
            for _ in 0..11 {
                let start = std::time::Instant::now();
                let result = run_inference(&request);
                times.push(start.elapsed().as_secs_f64() * 1000.0);
                last = Some(result);
            }
            let result = last.unwrap();
            let cold = times[0];
            let mut warm = times[1..].to_vec();
            warm.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let json: serde_json::Value =
                serde_json::from_str(&result.result_json).unwrap_or(serde_json::Value::Null);
            let summary = match &json {
                serde_json::Value::Array(items) => format!("{} result(s)", items.len()),
                other => other.to_string().chars().take(160).collect(),
            };
            println!(
                "RESULT {name} {width}x{height} {label}: status={:?} first={cold:.1}ms \
                 warm median={:.1}ms min={:.1}ms | {summary}",
                result.status,
                warm[warm.len() / 2],
                warm[0],
            );
            if let Some(frame) = &result.annotated_frame {
                image::save_buffer(
                    out.join(format!("{name}_{label}.png")),
                    &frame.pixels,
                    frame.width,
                    frame.height,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
            std::fs::write(
                out.join(format!("{name}_{label}.json")),
                &result.result_json,
            )
            .unwrap();
        }
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
        }
        let loaded = |dll: &str| {
            let wide: Vec<u16> = dll.encode_utf16().chain([0]).collect();
            // SAFETY: a NUL-terminated UTF-16 name; only queried.
            !unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null()
        };
        println!(
            "PROVIDERS DirectML.dll loaded={} onnxruntime_providers_cuda.dll loaded={}",
            loaded("DirectML.dll"),
            loaded("onnxruntime_providers_cuda.dll")
        );
    }
}

// ── Licensing (P6 Stage 2.4) ────────────────────────────────────────────────

/// The Rust crate notices were generated for exactly this Cargo.lock.
#[test]
fn rust_crate_notices_match_cargo_lock() {
    let lock = include_str!("../../../Cargo.lock").replace("\r\n", "\n");
    let expected = sha256_hex(lock.as_bytes());
    let notices = super::runtime::THIRD_PARTY_FILES
        .iter()
        .find(|(name, _)| *name == "rust-crates.txt")
        .map(|(_, text)| *text)
        .unwrap();
    let first = notices.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        format!("Cargo.lock SHA-256 (LF): {expected}"),
        "Cargo.lock changed: run licenses\\third-party\\regenerate.ps1"
    );
    // Every crate in Cargo.lock that the notices name is at its locked
    // version (no stale entries), and the crates with bundled C code are
    // covered.
    let locked: std::collections::BTreeSet<(String, String)> = lock
        .split("[[package]]")
        .filter_map(|block| {
            let field = |key: &str| {
                block.lines().find_map(|l| {
                    l.strip_prefix(&format!("{key} = \""))
                        .and_then(|v| v.strip_suffix('"'))
                        .map(str::to_string)
                })
            };
            Some((field("name")?, field("version")?))
        })
        .collect();
    let mut named = 0;
    for line in notices.lines() {
        if let Some(entry) = line.strip_prefix("  - ") {
            let mut parts = entry.split_whitespace();
            let (Some(name), Some(version)) = (parts.next(), parts.next()) else {
                continue;
            };
            assert!(
                locked.contains(&(name.to_string(), version.to_string())),
                "{name} {version} is not in Cargo.lock"
            );
            named += 1;
        }
    }
    assert!(named > 500, "{named}");
    for native in [
        "ring 0.17.14",
        "audiopus_sys 0.2.2",
        "hidapi 2.6.5",
        "ort-sys 2.0.0-rc.12",
    ] {
        assert!(notices.contains(&format!("  - {native}")), "{native}");
    }
    // Every bundled notice file has content, and the full DirectML terms
    // travel with DirectML.dll.
    for (name, text) in super::runtime::THIRD_PARTY_FILES {
        assert!(text.len() > 200, "{name} is empty");
    }
    let rules = super::runtime::DependencyRules::windows();
    assert!(
        rules.pinned[0]
            .notice
            .contains("MICROSOFT SOFTWARE LICENSE TERMS")
    );
}

#[test]
fn components_without_cleared_licences_refuse_the_export() {
    let fx = Fx::new("x_unresolved");
    fx.save(&base_graph().0);
    let mut env = fx.env();
    env.unresolved = vec![("a font".to_string(), "its terms are unknown".to_string())];
    let report = export_portable(&fx.request("Gated"), &env);
    assert_failed(&report, "Patchwork embeds a font");
    assert_eq!(std::fs::read_dir(&fx.out).unwrap().count(), 0);

    // P6 Stage 2.6: an internal test build is exported, and labelled as
    // such everywhere (folder root, licences, notices, report).
    let mut request = fx.request("Internal");
    request.purpose = super::ExportPurpose::InternalTest;
    let report = export_portable(&request, &env);
    assert!(report.succeeded(), "{report:#?}");
    assert!(report.internal_test_build);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("INTERNAL TEST BUILD — NOT FOR DISTRIBUTION"))
    );
    let dir = fx.out.join("Internal");
    let root = std::fs::read_to_string(dir.join(super::runtime::INTERNAL_TEST_MARKER)).unwrap();
    let licences =
        std::fs::read_to_string(dir.join("licenses").join("NOT-FOR-DISTRIBUTION.txt")).unwrap();
    assert_eq!(root, licences);
    assert!(root.starts_with("INTERNAL TEST BUILD — NOT FOR DISTRIBUTION"));
    assert!(root.contains("a font: its terms are unknown"), "{root}");
    assert!(root.contains("must not be distributed"));
    for (file, sha) in super::runtime::UNRESOLVED_MODEL_SHA256 {
        assert!(root.contains(&format!("{file}  {sha}")), "{root}");
    }
    let notices =
        std::fs::read_to_string(dir.join("licenses").join("THIRD-PARTY-NOTICES.txt")).unwrap();
    assert!(notices.starts_with("*** INTERNAL TEST BUILD — NOT FOR DISTRIBUTION ***"));
    assert!(
        report
            .files
            .contains(&super::runtime::INTERNAL_TEST_MARKER.to_string())
    );

    // Exporting the internal build did not make the components cleared: a
    // distributable export is still refused.
    let report = export_portable(&fx.request("Gated"), &env);
    assert_failed(&report, "Patchwork embeds a font");
    assert!(!report.internal_test_build);

    // With nothing uncleared, a distributable build carries no label.
    env.unresolved.clear();
    let report = export_portable(&fx.request("Clear"), &env);
    assert!(report.succeeded(), "{report:#?}");
    assert!(!report.internal_test_build);
    let dir = fx.out.join("Clear");
    assert!(!dir.join(super::runtime::INTERNAL_TEST_MARKER).exists());
    assert!(
        !dir.join("licenses")
            .join("NOT-FOR-DISTRIBUTION.txt")
            .exists()
    );
    let notices =
        std::fs::read_to_string(dir.join("licenses").join("THIRD-PARTY-NOTICES.txt")).unwrap();
    assert!(!notices.contains("NOT FOR DISTRIBUTION"));

    // The real list is what the app uses, and the review documents it.
    let real = ExportEnvironment::this_computer();
    assert_eq!(
        real.unresolved.len(),
        super::runtime::UNRESOLVED_COMPONENTS.len()
    );
    let readme = super::runtime::THIRD_PARTY_FILES[0].1;
    assert!(readme.contains("Qualcomm AI Hub Proprietary License") && readme.contains("MediaPipe"));
}

#[test]
fn the_runtime_must_be_built_from_the_exporters_source() {
    let fx = Fx::new("x_same_source");
    fx.save(&base_graph().0);
    let mut env = fx.env();
    env.runtime_source = Some(("f".repeat(40), Some(false)));
    assert_failed(
        &export_portable(&fx.request("Other"), &env),
        "different source",
    );
    env.runtime_source = Some((TEST_COMMIT.to_string(), Some(true)));
    assert_failed(
        &export_portable(&fx.request("Other"), &env),
        "different source",
    );
    env.runtime_source = Some((TEST_COMMIT.to_string(), Some(false)));
    assert!(export_portable(&fx.request("Same"), &env).succeeded());
}

/// P6 Stage 2.5: every model Patchwork embeds is either cleared or keeps the
/// export blocked, and the Satoshi font is no longer embedded.
#[test]
fn embedded_assets_are_cleared_or_block_the_export() {
    let bundled = include_str!("../../ml/bundled.rs");
    let unresolved: Vec<&str> = super::runtime::UNRESOLVED_COMPONENTS
        .iter()
        .map(|(what, _)| *what)
        .collect();
    let mut models = 0;
    for line in bundled.lines() {
        if let Some(rest) = line.split("assets/presets/ml/").nth(1) {
            let file = rest.split('"').next().unwrap();
            models += 1;
            assert!(
                unresolved.iter().any(|u| u.starts_with(file)),
                "{file} is embedded but neither cleared nor listed as unresolved"
            );
        }
    }
    assert_eq!(models, 6);
    // P6 Stage 2.6: the exact files are recorded (and documented), so every
    // internal test build names precisely what it contains.
    let readme = super::runtime::THIRD_PARTY_FILES[0].1;
    let ml = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("presets")
        .join("ml");
    assert_eq!(super::runtime::UNRESOLVED_MODEL_SHA256.len(), models);
    for (file, sha) in super::runtime::UNRESOLVED_MODEL_SHA256 {
        assert!(
            bundled.contains(&format!("assets/presets/ml/{file}")),
            "{file}"
        );
        assert!(unresolved.iter().any(|u| u.starts_with(file)), "{file}");
        let bytes = std::fs::read(ml.join(file)).unwrap();
        assert_eq!(sha256_hex(&bytes), *sha, "{file} changed");
        assert!(readme.contains(sha), "{file}'s hash is not documented");
    }
    // The Player labels itself while it embeds any of them.
    assert_eq!(
        super::runtime::internal_test_label(),
        Some("INTERNAL TEST BUILD — NOT FOR DISTRIBUTION")
    );
    assert_eq!(
        crate::app::player::player_title(
            "Light Show".to_string(),
            super::runtime::internal_test_label()
        ),
        "Light Show — INTERNAL TEST BUILD — NOT FOR DISTRIBUTION"
    );
    assert_eq!(
        crate::app::player::player_title("Light Show".to_string(), None),
        "Light Show"
    );
    for source in [
        include_str!("../../icons.rs"),
        include_str!("../../nodes/text_to_image_node.rs"),
    ] {
        assert!(!source.contains("Satoshi-Regular.ttf"));
        assert!(source.contains("Outfit-Regular.ttf"));
    }
    assert!(
        super::runtime::THIRD_PARTY_FILES
            .iter()
            .any(|(name, text)| *name == "Outfit-OFL.txt"
                && text.contains("SIL OPEN FONT LICENSE Version 1.1"))
    );
}
