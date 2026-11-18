//! P4.3.1B installation tests. Artifacts are built from real published
//! packages (P4.1/P4.2 publishing), renamed into the public `acme`
//! namespace with their checksums recomputed, so they exercise the same
//! verification as real Library packages.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::*;
use crate::library::bundle::tests::{child, composite, files_under, leaf, publish};
use crate::library::index::scan_packages;
use crate::library::marketplace::browse::{InstallState, InstalledIndex};
use crate::library::marketplace::fetch::fixture::{Route, Server};
use crate::library::marketplace::fetch::{HttpTransport, Origin, Timeouts};
use crate::library::marketplace::limits::MAX_ARTIFACT_FILE_BYTES;
use crate::library::marketplace::source::CatalogueLocation;
use crate::library::marketplace::{MARKETPLACE_SOURCE, parse_catalogue};
use crate::library::package::tests::TempDir;
use crate::library::package::{MANIFEST_FILE, is_inside_published_package};

const COMMIT: &str = "3f2a9c1e7b4d5a6f8e0c2b1a9d7e6f5c4b3a2918";

fn app() -> semver::Version {
    semver::Version::new(0, 0, 10)
}

fn sha(text: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

type Files = BTreeMap<String, String>;

/// A published version folder as marketplace files: `local.` → `acme.`,
/// `source: "marketplace"`. `edit` runs before checksums are recomputed.
fn files_from(dir: &Path, edit: impl FnOnce(&mut Files)) -> Files {
    let mut files: Files = files_under(dir)
        .into_iter()
        .map(|rel| {
            let text = std::fs::read_to_string(dir.join(&rel)).unwrap();
            (
                rel.replace("local.", "acme."),
                text.replace("local.", "acme."),
            )
        })
        .collect();
    edit(&mut files);
    let mut manifest: Value = serde_json::from_str(&files[MANIFEST_FILE]).unwrap();
    manifest["source"] = json!(MARKETPLACE_SOURCE);
    let entry = manifest["entry"].as_str().unwrap().to_string();
    manifest["sha256"] = json!(sha(files[&entry].as_bytes()));
    // `get_mut`: indexing a missing key would insert `null`.
    if let Some(map) = manifest.get_mut("files").and_then(Value::as_object_mut) {
        for (path, digest) in map.iter_mut() {
            if let Some(text) = files.get(path) {
                *digest = json!(sha(text.as_bytes()));
            }
        }
    }
    files.insert(
        MANIFEST_FILE.to_string(),
        serde_json::to_string_pretty(&manifest).unwrap(),
    );
    files
}

fn envelope(id: &str, version: &str, files: &Files) -> Vec<u8> {
    let list: Vec<Value> = files
        .iter()
        .map(|(path, text)| json!({"path": path, "text": text}))
        .collect();
    serde_json::to_vec(&json!({"pwpkg": 1, "id": id, "version": version, "files": list})).unwrap()
}

/// Catalogue entry describing exactly `bytes`.
fn listing(
    id: &str,
    version: &str,
    entry: &str,
    bundle_format: u32,
    capabilities: &[&str],
    bytes: &[u8],
    files: usize,
) -> (CataloguePackage, CatalogueVersion) {
    let value = json!({
        "kind": "patchwork-catalogue",
        "catalogue_format": 1,
        "publishers": [{"namespace": "acme", "github": "acme", "github_id": 7, "display_name": "Acme"}],
        "packages": [{
            "id": id, "name": "Pkg", "author": "Acme",
            "versions": [{
                "version": version, "entry": entry,
                "manifest_format": 1, "pwnode_version": 1, "bundle_format": bundle_format,
                "min_patchwork": "0.0.1",
                "capabilities": capabilities,
                "artifact": {
                    "format": 1,
                    "url": format!("https://raw.githubusercontent.com/patchwork-nodes/catalogue/{COMMIT}/artifacts/{id}/{version}/{id}-{version}.pwpkg"),
                    "sha256": sha(bytes),
                    "size": bytes.len().max(1),
                    "files": files.clamp(2, 256),
                }
            }]
        }]
    });
    let source = CatalogueLocation::new("patchwork-nodes", "catalogue", "main").unwrap();
    let catalogue = parse_catalogue(&serde_json::to_vec(&value).unwrap(), source.source())
        .unwrap_or_else(|e| panic!("test catalogue invalid: {e}"));
    let package = catalogue.packages[id].clone();
    let version = package.versions.values().next().unwrap().clone();
    (package, version)
}

struct Fx {
    _tmp: TempDir,
    work: PathBuf,
    /// The user's Library the marketplace installs into.
    library: PathBuf,
}

fn fx(tag: &str) -> Fx {
    let tmp = TempDir::new(tag);
    let work = tmp.0.join("work");
    let library = tmp.0.join("library");
    std::fs::create_dir_all(&library).unwrap();
    Fx {
        work,
        library,
        _tmp: tmp,
    }
}

/// A fresh folder name for each published fixture.
fn unique(f: &Fx, tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    f.work.join(format!("{tag}-{n}"))
}

/// `acme.gain` (standalone) as marketplace files.
fn gain_files(f: &Fx, version: &str, edit: impl FnOnce(&mut Files)) -> Files {
    let base = unique(f, "gain");
    let v = publish(
        &base.join("lib"),
        &leaf(&base.join("src"), "Gain", 0.5),
        "Gain",
        None,
        version,
    )
    .unwrap();
    files_from(&v.dir, edit)
}

/// `acme.chain` 1.0.0 bundling `acme.gain` 1.0.0.
fn chain_files(f: &Fx, edit: impl FnOnce(&mut Files)) -> Files {
    let base = unique(f, "chain");
    let lib = base.join("lib");
    let gain = publish(
        &lib,
        &leaf(&base.join("g"), "Gain", 0.5),
        "Gain",
        None,
        "1.0.0",
    )
    .unwrap();
    let chain = publish(
        &lib,
        &composite(&base.join("c"), "Chain", &[child(&gain)]),
        "Chain",
        None,
        "1.0.0",
    )
    .unwrap();
    files_from(&chain.dir, edit)
}

/// Install `files` as `id@version` with a catalogue that matches them.
fn install_files(
    f: &Fx,
    id: &str,
    version: &str,
    files: &Files,
    capabilities: &[&str],
) -> Result<Installed, InstallError> {
    let bytes = envelope(id, version, files);
    // The catalogue describes what the publisher's manifest says.
    let manifest: Value = serde_json::from_str(&files[MANIFEST_FILE]).unwrap();
    let bundle = manifest["bundle_format"].as_u64().unwrap_or(0) as u32;
    let entry = if id == "acme.chain" {
        "Chain.pwnode"
    } else {
        "Gain.pwnode"
    };
    let (package, v) = listing(
        id,
        version,
        entry,
        bundle,
        capabilities,
        &bytes,
        files.len(),
    );
    install_artifact(
        &f.library,
        &package,
        &v,
        &app(),
        &bytes,
        &AtomicBool::new(false),
    )
}

/// Inject a node (raw JSON) into a .pwnode's graph.
fn inject(files: &mut Files, path: &str, node_type: Value) {
    let mut graph: Value = serde_json::from_str(&files[path]).unwrap();
    graph["graph"]["nodes"]["9001"] =
        json!({"id": 9001, "node_type": node_type, "pos": [0.0, 0.0]});
    files.insert(path.to_string(), serde_json::to_string(&graph).unwrap());
}

fn rejected(result: Result<Installed, InstallError>, needle: &str) {
    match result {
        Err(InstallError::Rejected(why)) => {
            assert!(why.contains(needle), "{needle:?} not in {why:?}")
        }
        other => panic!("expected rejection containing {needle:?}, got {other:?}"),
    }
}

/// Nothing at all was added to the Library (no package folder, no staging).
fn library_untouched(f: &Fx) {
    let packages = f.library.join("packages");
    let leftovers: Vec<String> = if packages.is_dir() {
        files_under(&packages)
    } else {
        Vec::new()
    };
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    if packages.is_dir() {
        let dirs: Vec<_> = std::fs::read_dir(&packages).unwrap().collect();
        assert!(dirs.is_empty(), "left a package folder behind");
    }
}

// ── Success and registration ────────────────────────────────────────────

#[test]
fn installs_a_verified_package_and_registers_it_in_the_library() {
    let f = fx("mk_install_ok");
    let files = gain_files(&f, "1.0.0", |_| {});
    let installed = install_files(&f, "acme.gain", "1.0.0", &files, &[]).unwrap();
    assert!(installed.capabilities.is_empty());
    let dir = f.library.join("packages").join("acme.gain").join("1.0.0");
    assert_eq!(installed.package.dir, dir);
    assert_eq!(installed.package.manifest.source, MARKETPLACE_SOURCE);
    // Byte-for-byte what the artifact contained, and nothing else.
    let mut on_disk = files_under(&dir);
    on_disk.sort();
    assert_eq!(on_disk, files.keys().cloned().collect::<Vec<_>>());
    for (path, text) in &files {
        assert_eq!(&std::fs::read_to_string(dir.join(path)).unwrap(), text);
    }
    // Registered: the Library scan lists it as verified and marketplace.
    let scan = scan_packages(&f.library);
    assert!(scan.problems.is_empty(), "{:?}", scan.problems);
    let index = InstalledIndex::from_packages(&scan.packages);
    assert_eq!(
        index.state("acme.gain", &semver::Version::new(1, 0, 0)),
        Some(&InstallState::Installed {
            from_marketplace: true
        })
    );
    // Read-only like every published version; no staging left behind.
    assert!(is_inside_published_package(&installed.package.entry_path));
    let names: Vec<String> = std::fs::read_dir(f.library.join("packages").join("acme.gain"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["1.0.0"]);
}

#[test]
fn installs_a_complete_dependency_bundle() {
    let f = fx("mk_install_bundle");
    let files = chain_files(&f, |_| {});
    assert!(files.keys().any(|k| k.starts_with("deps/acme.gain/1.0.0/")));
    let installed = install_files(&f, "acme.chain", "1.0.0", &files, &[]).unwrap();
    assert_eq!(installed.package.manifest.dependencies.len(), 1);
    assert_eq!(installed.package.manifest.dependencies[0].id, "acme.gain");
    // Only the root is a Library package; the dependency stays bundled.
    assert!(!f.library.join("packages").join("acme.gain").exists());
}

// ── Download and checksums ──────────────────────────────────────────────

#[test]
fn artifact_hash_and_size_must_match_the_catalogue() {
    let f = fx("mk_hash");
    let files = gain_files(&f, "1.0.0", |_| {});
    let bytes = envelope("acme.gain", "1.0.0", &files);
    let (package, version) = listing(
        "acme.gain",
        "1.0.0",
        "Gain.pwnode",
        0,
        &[],
        &bytes,
        files.len(),
    );
    let cancel = AtomicBool::new(false);

    let mut tampered = bytes.clone();
    let at = tampered.len() / 2;
    tampered[at] ^= 0x01;
    rejected(
        install_artifact(&f.library, &package, &version, &app(), &tampered, &cancel),
        "checksum",
    );
    let mut longer = bytes.clone();
    longer.push(b' ');
    rejected(
        install_artifact(&f.library, &package, &version, &app(), &longer, &cancel),
        "bytes",
    );
    library_untouched(&f);
    assert!(install_artifact(&f.library, &package, &version, &app(), &bytes, &cancel).is_ok());
}

/// Routes the catalogue's https URL to a local fixture server.
struct Local<'a>(&'a HttpTransport, &'a Server);
impl Transport for Local<'_> {
    fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        max: usize,
    ) -> Result<FetchResponse, FetchError> {
        let path = url.trim_start_matches("https://raw.githubusercontent.com");
        self.0.fetch(&self.1.url(path), etag, max)
    }
}

#[test]
fn downloads_only_the_catalogue_url_within_the_declared_size() {
    let f = fx("mk_download");
    let files = gain_files(&f, "1.0.0", |_| {});
    let bytes = envelope("acme.gain", "1.0.0", &files);
    let (package, version) = listing(
        "acme.gain",
        "1.0.0",
        "Gain.pwnode",
        0,
        &[],
        &bytes,
        files.len(),
    );
    let server = Server::start();
    let path = version
        .artifact
        .url
        .trim_start_matches("https://raw.githubusercontent.com")
        .to_string();
    let http = HttpTransport::new(Origin::local_http(server.port), Timeouts::default()).unwrap();
    let local = Local(&http, &server);
    let cancel = AtomicBool::new(false);

    // Server sends more than the catalogue declares: cut off, nothing written.
    let mut padded = bytes.clone();
    padded.extend_from_slice(&[b' '; 64]);
    server.route(
        &path,
        Route::Ok {
            body: padded,
            etag: None,
        },
    );
    assert!(matches!(
        install(&local, &f.library, &package, &version, &app(), &cancel),
        Err(InstallError::Download(FetchError::TooLarge { .. }))
    ));
    // Missing artifact.
    server.route(&path, Route::Status(404));
    assert_eq!(
        install(&local, &f.library, &package, &version, &app(), &cancel).unwrap_err(),
        InstallError::Download(FetchError::Status(404))
    );
    // Redirect elsewhere.
    server.route(
        &path,
        Route::Redirect(format!("http://localhost:{}/x", server.port)),
    );
    assert!(matches!(
        install(&local, &f.library, &package, &version, &app(), &cancel),
        Err(InstallError::Download(FetchError::UnsafeRedirect(_)))
    ));
    library_untouched(&f);

    server.route(
        &path,
        Route::Ok {
            body: bytes.clone(),
            etag: None,
        },
    );
    install(&local, &f.library, &package, &version, &app(), &cancel).unwrap();
    let requested: Vec<String> = server
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    assert!(
        requested.iter().all(|p| *p == path.to_lowercase()),
        "{requested:?}"
    );
}

struct Offline;
impl Transport for Offline {
    fn fetch(&self, _: &str, _: Option<&str>, _: usize) -> Result<FetchResponse, FetchError> {
        Err(FetchError::Network("offline".into()))
    }
}

#[test]
fn offline_installs_fail_cleanly() {
    let f = fx("mk_offline");
    let files = gain_files(&f, "1.0.0", |_| {});
    let bytes = envelope("acme.gain", "1.0.0", &files);
    let (package, version) = listing(
        "acme.gain",
        "1.0.0",
        "Gain.pwnode",
        0,
        &[],
        &bytes,
        files.len(),
    );
    assert_eq!(
        install(
            &Offline,
            &f.library,
            &package,
            &version,
            &app(),
            &AtomicBool::new(false)
        )
        .unwrap_err(),
        InstallError::Download(FetchError::Network("offline".into()))
    );
    library_untouched(&f);
}

// ── Malformed artifacts ─────────────────────────────────────────────────

/// Install arbitrary bytes with a catalogue entry that matches them, so the
/// failure comes from parsing, not the checksum.
fn install_raw(f: &Fx, bytes: &[u8], files: usize) -> Result<Installed, InstallError> {
    let (package, version) = listing("acme.gain", "1.0.0", "Gain.pwnode", 0, &[], bytes, files);
    install_artifact(
        &f.library,
        &package,
        &version,
        &app(),
        bytes,
        &AtomicBool::new(false),
    )
}

#[test]
fn malformed_and_oversized_artifacts_are_rejected() {
    let f = fx("mk_malformed");
    let files = gain_files(&f, "1.0.0", |_| {});
    let good: Value = serde_json::from_slice(&envelope("acme.gain", "1.0.0", &files)).unwrap();
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        edit(&mut v);
        serde_json::to_vec(&v).unwrap()
    };
    let n = files.len();
    let cases: Vec<(&str, Vec<u8>, usize)> = vec![
        ("not valid", b"PK\x03\x04 zip".to_vec(), n),
        ("UTF-8", vec![0xff, 0xfe, b'{', b'}'], n),
        (
            "not valid",
            with(&|v| v["hooks"] = json!(["postinstall.sh"])),
            n,
        ),
        (
            "not valid",
            with(&|v| v["files"][0]["mode"] = json!(493)),
            n,
        ),
        (
            "not valid",
            with(&|v| v["files"][0]["symlink"] = json!("/etc/passwd")),
            n,
        ),
        ("format 2", with(&|v| v["pwpkg"] = json!(2)), n),
        (
            "not acme.gain@1.0.0",
            with(&|v| v["id"] = json!("acme.other")),
            n,
        ),
        (
            "not acme.gain@1.0.0",
            with(&|v| v["version"] = json!("1.0.1")),
            n,
        ),
        ("catalogue says", good.to_string().into_bytes(), n + 1),
        (
            "nested too deeply",
            format!(
                "{{\"pwpkg\":1,\"x\":{}{}}}",
                "[".repeat(200),
                "]".repeat(200)
            )
            .into_bytes(),
            n,
        ),
    ];
    for (needle, bytes, count) in cases {
        rejected(install_raw(&f, &bytes, count), needle);
    }
    // One file over the per-file limit.
    let big = with(&|v| {
        v["files"][0]["text"] = json!(" ".repeat(MAX_ARTIFACT_FILE_BYTES as usize + 1));
    });
    rejected(install_raw(&f, &big, n), "larger than");
    // A file nested too deeply.
    let deep = with(&|v| {
        let i = v["files"]
            .as_array()
            .unwrap()
            .iter()
            .position(|x| x["path"] == "Gain.pwnode")
            .unwrap();
        v["files"][i]["text"] = json!(format!("{}{}", "[".repeat(100), "]".repeat(100)));
    });
    rejected(install_raw(&f, &deep, n), "nested too deeply");
    // Duplicate JSON keys in the envelope.
    let text = good
        .to_string()
        .replacen("\"pwpkg\":1", "\"pwpkg\":1,\"pwpkg\":1", 1);
    rejected(install_raw(&f, text.as_bytes(), n), "duplicate");
    library_untouched(&f);
}

#[test]
fn duplicate_and_unsafe_paths_are_rejected() {
    let f = fx("mk_paths");
    let files = gain_files(&f, "1.0.0", |_| {});
    let base: Vec<(String, String)> = files.clone().into_iter().collect();
    let extra = |path: &str, text: &str| {
        let mut list = base.clone();
        list.push((path.to_string(), text.to_string()));
        list
    };
    let cases = vec![
        ("more than once", extra("package.json", "{}")),
        ("more than once", extra("PACKAGE.JSON", "{}")),
        ("'..'", extra("../evil.pwnode", "{}")),
        ("plain relative", extra("/abs.pwnode", "{}")),
        ("plain relative", extra("C:/x.pwnode", "{}")),
        (
            "plain relative",
            extra("deps\\acme.x\\1.0.0\\X.pwnode", "{}"),
        ),
        ("'..'", extra("deps/acme.x/1.0.0/../../../x.pwnode", "{}")),
        ("package version layout", extra("install.sh", "rm -rf /")),
        ("package version layout", extra("README.md", "hi")),
        ("not valid", extra("deps/acme.x/1.0.0/NUL.pwnode", "{}")),
    ];
    for (needle, list) in cases {
        let entries: Vec<Value> = list
            .iter()
            .map(|(p, t)| json!({"path": p, "text": t}))
            .collect();
        let bytes = serde_json::to_vec(
            &json!({"pwpkg": 1, "id": "acme.gain", "version": "1.0.0", "files": entries}),
        )
        .unwrap();
        let result = install_raw(&f, &bytes, list.len());
        match result {
            Err(InstallError::Rejected(why)) => assert!(
                why.contains(needle) || why.contains("must be"),
                "{needle:?}: {why}"
            ),
            other => panic!("{needle}: {other:?}"),
        }
    }
    library_untouched(&f);
}

// ── Manifests and dependencies ──────────────────────────────────────────

#[test]
fn invalid_and_incompatible_manifests_are_rejected() {
    let f = fx("mk_manifest");
    let set = |key: &'static str, value: Value| {
        move |files: &mut Files| {
            let mut m: Value = serde_json::from_str(&files[MANIFEST_FILE]).unwrap();
            m[key] = value;
            files.insert(MANIFEST_FILE.into(), m.to_string());
        }
    };
    rejected(
        install_files(
            &f,
            "acme.gain",
            "1.0.0",
            &gain_files(&f, "1.0.0", set("id", json!("acme.other"))),
            &[],
        ),
        "different id",
    );
    rejected(
        install_files(
            &f,
            "acme.gain",
            "1.0.0",
            &gain_files(&f, "1.0.0", set("min_app_version", json!("9.0.0"))),
            &[],
        ),
        "needs Patchwork 9.0.0",
    );
    rejected(
        install_files(
            &f,
            "acme.gain",
            "1.0.0",
            &gain_files(&f, "1.0.0", set("format", json!(2))),
            &[],
        ),
        "format",
    );
    // source is forced to "marketplace" by the builder; undo it afterwards.
    let mut local = gain_files(&f, "1.0.0", |_| {});
    let mut m: Value = serde_json::from_str(&local[MANIFEST_FILE]).unwrap();
    m["source"] = json!("local");
    local.insert(MANIFEST_FILE.into(), m.to_string());
    rejected(
        install_files(&f, "acme.gain", "1.0.0", &local, &[]),
        "source",
    );
    // Entry changed after its checksum was recorded.
    let mut tampered = gain_files(&f, "1.0.0", |_| {});
    tampered.insert(
        "Gain.pwnode".into(),
        tampered["Gain.pwnode"].replace("0.5", "0.6"),
    );
    rejected(
        install_files(&f, "acme.gain", "1.0.0", &tampered, &[]),
        "checksum",
    );
    library_untouched(&f);

    // Not offered here: needs a newer Patchwork, or withdrawn.
    let files = gain_files(&f, "1.0.0", |_| {});
    let bytes = envelope("acme.gain", "1.0.0", &files);
    let (package, mut version) = listing(
        "acme.gain",
        "1.0.0",
        "Gain.pwnode",
        0,
        &[],
        &bytes,
        files.len(),
    );
    let cancel = AtomicBool::new(false);
    version.min_patchwork = semver::Version::new(9, 0, 0);
    assert!(matches!(
        install_artifact(&f.library, &package, &version, &app(), &bytes, &cancel),
        Err(InstallError::NotInstallable(_))
    ));
    version.min_patchwork = semver::Version::new(0, 0, 1);
    version.yanked = Some("broken".into());
    assert!(matches!(
        install_artifact(&f.library, &package, &version, &app(), &bytes, &cancel),
        Err(InstallError::NotInstallable(why)) if why.contains("Withdrawn")
    ));
    library_untouched(&f);
}

#[test]
fn bundled_dependencies_must_be_complete_and_exact() {
    let f = fx("mk_deps");
    let dep_entry = "deps/acme.gain/1.0.0/Gain.pwnode";
    let dep_manifest = "deps/acme.gain/1.0.0/package.json";

    // Dependency files removed (still declared).
    let missing = chain_files(&f, |files| {
        files.remove(dep_entry);
        files.remove(dep_manifest);
    });
    rejected(
        install_files(&f, "acme.chain", "1.0.0", &missing, &[]),
        "missing",
    );

    // A bundled file the manifest does not declare.
    let mut undeclared = chain_files(&f, |_| {});
    let mut m: Value = serde_json::from_str(&undeclared[MANIFEST_FILE]).unwrap();
    m["files"].as_object_mut().unwrap().remove(dep_entry);
    undeclared.insert(MANIFEST_FILE.into(), m.to_string());
    rejected(
        install_files(&f, "acme.chain", "1.0.0", &undeclared, &[]),
        "not in package.json",
    );

    // Substitution: the bundled dependency's code differs from what the
    // manifest recorded.
    let mut substituted = chain_files(&f, |_| {});
    substituted.insert(
        dep_entry.into(),
        substituted[dep_entry].replace("0.5", "0.9"),
    );
    rejected(
        install_files(&f, "acme.chain", "1.0.0", &substituted, &[]),
        "checksum",
    );

    // Substitution: a different version bundled than declared.
    let other_version = chain_files(&f, |files| {
        let moved: Files = std::mem::take(files)
            .into_iter()
            .map(|(k, v)| (k.replace("acme.gain/1.0.0", "acme.gain/1.0.1"), v))
            .collect();
        *files = moved;
    });
    assert!(install_files(&f, "acme.chain", "1.0.0", &other_version, &[]).is_err());
    library_untouched(&f);
}

// ── Capabilities and node types ─────────────────────────────────────────

#[test]
fn capability_under_declaration_is_refused() {
    let f = fx("mk_caps");
    let files = gain_files(&f, "1.0.0", |files| {
        inject(
            files,
            "Gain.pwnode",
            json!({"HttpRequest": {"url": "http://example.com"}}),
        );
    });
    rejected(
        install_files(&f, "acme.gain", "1.0.0", &files, &[]),
        "network",
    );
    rejected(
        install_files(&f, "acme.gain", "1.0.0", &files, &["filesystem"]),
        "network",
    );
    library_untouched(&f);
    let installed = install_files(&f, "acme.gain", "1.0.0", &files, &["network"]).unwrap();
    assert_eq!(
        installed.capabilities.into_iter().collect::<Vec<_>>(),
        vec![Capability::Network]
    );

    // Capabilities in a bundled dependency count too.
    let f = fx("mk_caps_dep");
    let files = chain_files(&f, |files| {
        inject(
            files,
            "deps/acme.gain/1.0.0/Gain.pwnode",
            json!({"Camera": {}}),
        );
    });
    rejected(
        install_files(&f, "acme.chain", "1.0.0", &files, &[]),
        "media_capture",
    );
}

#[test]
fn unsupported_unknown_and_native_nodes_are_refused() {
    let f = fx("mk_nodes");
    for (node, path, needle) in [
        (
            json!({"Dynamic": {"inner": {"type_tag": "evil", "state": {}}}}),
            "Gain.pwnode",
            "unsupported node type 'evil'",
        ),
        (
            json!({"Dynamic": {"inner": {"state": {}}}}),
            "Gain.pwnode",
            "no type_tag",
        ),
        (
            json!({"FutureNode": {}}),
            "Gain.pwnode",
            "unsupported node type 'FutureNode'",
        ),
        (
            json!({"RustPlugin": {"code": "fn main(){}"}}),
            "Gain.pwnode",
            "native code",
        ),
        (json!({"ClapPlugin": {}}), "Gain.pwnode", "native code"),
        (
            json!({"Dynamic": {"inner": {"type_tag": "terminal", "state": {}}}}),
            "Gain.pwnode",
            "system commands",
        ),
    ] {
        let files = gain_files(&f, "1.0.0", |files| inject(files, path, node.clone()));
        rejected(
            install_files(
                &f,
                "acme.gain",
                "1.0.0",
                &files,
                &[
                    "network",
                    "filesystem",
                    "devices",
                    "media_capture",
                    "scripting",
                ],
            ),
            needle,
        );
    }
    // Inside a bundled dependency.
    let files = chain_files(&f, |files| {
        inject(
            files,
            "deps/acme.gain/1.0.0/Gain.pwnode",
            json!({"RustPlugin": {}}),
        );
    });
    rejected(
        install_files(&f, "acme.chain", "1.0.0", &files, &[]),
        "native code",
    );
    library_untouched(&f);
}

#[test]
fn nothing_is_executed_during_installation() {
    let f = fx("mk_noexec");
    // Nodes that would contact this listener if they were evaluated.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let files = gain_files(&f, "1.0.0", |files| {
        inject(
            files,
            "Gain.pwnode",
            json!({"HttpRequest": {"url": format!("http://127.0.0.1:{port}/fired"), "method": "GET", "auto_send": true}}),
        );
        let mut graph: Value = serde_json::from_str(&files["Gain.pwnode"]).unwrap();
        graph["graph"]["nodes"]["9002"] = json!({"id": 9002, "pos": [0.0, 0.0],
            "node_type": {"Dynamic": {"inner": {"type_tag": "websocket", "state": {"url": format!("ws://127.0.0.1:{port}/fired")}}}}});
        files.insert("Gain.pwnode".into(), graph.to_string());
    });
    let built = crate::node_trait::CONSTRUCTED.with(|c| c.get());
    install_files(&f, "acme.gain", "1.0.0", &files, &["network"]).unwrap();
    // The Library rescans after installing; that must not run them either.
    let scan = scan_packages(&f.library);
    assert_eq!(scan.packages.len(), 1);
    // P4.3.2A: verification is static: not even constructors ran.
    assert_eq!(crate::node_trait::CONSTRUCTED.with(|c| c.get()), built);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "a node contacted the network during installation"
    );
}

// ── Cancellation, interruption and conflicts ────────────────────────────

#[test]
fn cancelled_installations_change_nothing() {
    let f = fx("mk_cancel");
    let files = gain_files(&f, "1.0.0", |_| {});
    let bytes = envelope("acme.gain", "1.0.0", &files);
    let (package, version) = listing(
        "acme.gain",
        "1.0.0",
        "Gain.pwnode",
        0,
        &[],
        &bytes,
        files.len(),
    );

    let cancelled = AtomicBool::new(true);
    assert_eq!(
        install_artifact(&f.library, &package, &version, &app(), &bytes, &cancelled).unwrap_err(),
        InstallError::Cancelled
    );
    // Cancelled while downloading.
    struct CancelDuring<'a>(&'a AtomicBool, Vec<u8>);
    impl Transport for CancelDuring<'_> {
        fn fetch(&self, _: &str, _: Option<&str>, _: usize) -> Result<FetchResponse, FetchError> {
            self.0.store(true, Ordering::SeqCst);
            Ok(FetchResponse::Body {
                bytes: self.1.clone(),
                etag: None,
            })
        }
    }
    let flag = AtomicBool::new(false);
    assert_eq!(
        install(
            &CancelDuring(&flag, bytes.clone()),
            &f.library,
            &package,
            &version,
            &app(),
            &flag
        )
        .unwrap_err(),
        InstallError::Cancelled
    );
    library_untouched(&f);
}

#[test]
fn interrupted_installations_leave_the_library_unchanged() {
    let f = fx("mk_interrupt");
    // 1.0.0 is installed; 1.1.0 is interrupted at every step.
    let v1 = gain_files(&f, "1.0.0", |_| {});
    install_files(&f, "acme.gain", "1.0.0", &v1, &[]).unwrap();
    let v1_dir = f.library.join("packages").join("acme.gain").join("1.0.0");
    let before: Vec<(String, Vec<u8>)> = files_under(&v1_dir)
        .into_iter()
        .map(|rel| (rel.clone(), std::fs::read(v1_dir.join(rel)).unwrap()))
        .collect();
    let v2 = gain_files(&f, "1.1.0", |_| {});
    for point in [
        FailPoint::Writing(0),
        FailPoint::Writing(1),
        FailPoint::BeforeVerify,
        FailPoint::BeforeCommit,
        FailPoint::AfterCommit,
    ] {
        FAIL_AT.with(|p| p.set(Some(point)));
        let result = install_files(&f, "acme.gain", "1.1.0", &v2, &[]);
        FAIL_AT.with(|p| p.set(None));
        assert!(
            matches!(result, Err(InstallError::Io(_))),
            "{point:?}: {result:?}"
        );
        let names: Vec<String> = std::fs::read_dir(f.library.join("packages").join("acme.gain"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["1.0.0"], "{point:?}: nothing new left behind");
        let after: Vec<(String, Vec<u8>)> = files_under(&v1_dir)
            .into_iter()
            .map(|rel| (rel.clone(), std::fs::read(v1_dir.join(rel)).unwrap()))
            .collect();
        assert_eq!(after, before, "{point:?}: installed version untouched");
    }
    // A first install interrupted removes the package folder it created.
    let g = fx("mk_interrupt_first");
    let files = gain_files(&g, "1.0.0", |_| {});
    FAIL_AT.with(|p| p.set(Some(FailPoint::BeforeCommit)));
    assert!(install_files(&g, "acme.gain", "1.0.0", &files, &[]).is_err());
    FAIL_AT.with(|p| p.set(None));
    library_untouched(&g);

    // A staging folder left by a crash is ignored by the Library and does
    // not block a later install.
    let crashed = g
        .library
        .join("packages")
        .join("acme.gain")
        .join(".staging-crash");
    std::fs::create_dir_all(&crashed).unwrap();
    std::fs::write(crashed.join("package.json"), "{}").unwrap();
    assert!(scan_packages(&g.library).packages.is_empty());
    install_files(&g, "acme.gain", "1.0.0", &files, &[]).unwrap();
    assert_eq!(scan_packages(&g.library).packages.len(), 1);
}

#[test]
fn existing_versions_are_never_replaced() {
    let f = fx("mk_conflict");
    let files = gain_files(&f, "1.0.0", |_| {});
    install_files(&f, "acme.gain", "1.0.0", &files, &[]).unwrap();
    assert!(matches!(
        install_files(&f, "acme.gain", "1.0.0", &files, &[]),
        Err(InstallError::AlreadyInstalled(_))
    ));

    // A folder already at the target (placed by hand, even damaged) is kept.
    let g = fx("mk_conflict_foreign");
    let target = g.library.join("packages").join("acme.gain").join("1.0.0");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("Gain.pwnode"), "mine").unwrap();
    assert!(matches!(
        install_files(&g, "acme.gain", "1.0.0", &files, &[]),
        Err(InstallError::AlreadyInstalled(_))
    ));
    assert_eq!(
        std::fs::read_to_string(target.join("Gain.pwnode")).unwrap(),
        "mine"
    );
    assert_eq!(files_under(&target), vec!["Gain.pwnode".to_string()]);

    // Locally published packages live under local.*, which no marketplace
    // id can use, so they can never be targeted.
    let local = publish(
        &g.library,
        &leaf(&g.work.join("l"), "Gain", 0.1),
        "Gain",
        None,
        "1.0.0",
    )
    .unwrap();
    assert_eq!(local.manifest.id, "local.gain");
    assert!(crate::library::marketplace::split_public_id("local.gain").is_err());
}

#[test]
fn packages_carrying_secrets_are_refused() {
    let f = fx("mk_secret");
    let files = gain_files(&f, "1.0.0", |files| {
        inject(
            files,
            "Gain.pwnode",
            json!({"AiRequest": {"api_key": format!("sk-ant-{}", "x".repeat(40))}}),
        );
    });
    rejected(
        install_files(
            &f,
            "acme.gain",
            "1.0.0",
            &files,
            &["network", "ai_credentials", "filesystem"],
        ),
        "secret",
    );
    library_untouched(&f);
}

// ── P4.3.2C: provenance ──────────────────────────────────────────────────

mod provenance {
    use super::*;
    use crate::library::trust::{
        Grant, MarketplaceOrigin, Provenance, StoreLoad, TRUST_FILE, TrustStatus, TrustStore,
        digest::{digest_of_files, read_tree},
        identify, install_and_record,
    };

    /// Serves one artifact; optionally cancels while "downloading".
    struct Serve {
        bytes: Vec<u8>,
        cancel: Option<std::sync::Arc<AtomicBool>>,
    }

    impl Transport for Serve {
        fn fetch(&self, _: &str, _: Option<&str>, _: usize) -> Result<FetchResponse, FetchError> {
            if let Some(flag) = &self.cancel {
                flag.store(true, Ordering::SeqCst);
            }
            Ok(FetchResponse::Body {
                bytes: self.bytes.clone(),
                etag: None,
            })
        }
    }

    fn origin() -> MarketplaceOrigin {
        MarketplaceOrigin {
            location: CatalogueLocation::new("patchwork-nodes", "catalogue", "main").unwrap(),
            generated_at: Some(1_700_000_000),
            catalogue_sha256: "c".repeat(64),
        }
    }

    fn store(f: &Fx) -> TrustStore {
        TrustStore::at(f.work.join("trust").join(TRUST_FILE))
    }

    #[test]
    fn marketplace_installs_record_verified_provenance() {
        let f = fx("mk_prov_ok");
        let files = chain_files(&f, |_| {});
        let bytes = envelope("acme.chain", "1.0.0", &files);
        let (package, version) = listing(
            "acme.chain",
            "1.0.0",
            "Chain.pwnode",
            1,
            &["network"],
            &bytes,
            files.len(),
        );
        let trust = store(&f);
        let (installed, outcome) = install_and_record(
            &trust,
            &Serve {
                bytes: bytes.clone(),
                cancel: None,
            },
            &f.library,
            &package,
            &version,
            &origin(),
            &app(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let record = outcome.unwrap();
        // Bound to the exact verified files: the in-memory artifact, the
        // folder on disk and the record agree.
        let expected =
            digest_of_files(files.iter().map(|(p, t)| (p.as_str(), t.as_bytes()))).unwrap();
        assert_eq!(record.identity.tree_sha256, expected);
        assert_eq!(read_tree(&installed.package.dir).unwrap().digest, expected);
        assert_eq!(
            record.identity,
            identify(&installed.package.dir, true).unwrap().identity
        );
        assert_eq!(record.identity.id, "acme.chain");
        assert_eq!(
            record.provenance,
            Provenance::Marketplace {
                owner: "patchwork-nodes".into(),
                repo: "catalogue".into(),
                branch: "main".into(),
                catalogue_generated_at: Some(1_700_000_000),
                catalogue_sha256: "c".repeat(64),
                artifact_url: version.artifact.url.clone(),
                artifact_sha256: sha(&bytes),
            }
        );
        assert_eq!(record.declared, Some([Capability::Network].into()));
        assert!(record.detected.is_empty());
        assert_eq!(record.grant, Grant::NotGranted, "installing grants nothing");
        assert!(
            trust
                .load()
                .status(&record.identity)
                .has_verified_provenance()
        );
    }

    #[test]
    fn failed_and_cancelled_installs_record_nothing() {
        let f = fx("mk_prov_fail");
        let trust = store(&f);
        let files = gain_files(&f, "1.0.0", |_| {});
        let bytes = envelope("acme.gain", "1.0.0", &files);
        let (package, version) = listing(
            "acme.gain",
            "1.0.0",
            "Gain.pwnode",
            0,
            &[],
            &bytes,
            files.len(),
        );
        let run = |served: Vec<u8>, cancel: Option<std::sync::Arc<AtomicBool>>| {
            let flag = cancel.clone().unwrap_or_default();
            install_and_record(
                &trust,
                &Serve {
                    bytes: served,
                    cancel,
                },
                &f.library,
                &package,
                &version,
                &origin(),
                &app(),
                &flag,
            )
        };
        // Tampered download.
        let mut tampered = bytes.clone();
        tampered[10] ^= 1;
        assert!(matches!(
            run(tampered, None),
            Err(InstallError::Rejected(_))
        ));
        // Cancelled during the download.
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        assert_eq!(
            run(bytes.clone(), Some(cancel)).unwrap_err(),
            InstallError::Cancelled
        );
        // Interrupted before the commit.
        FAIL_AT.with(|p| p.set(Some(FailPoint::BeforeCommit)));
        let interrupted = run(bytes.clone(), None);
        FAIL_AT.with(|p| p.set(None));
        assert!(interrupted.is_err());
        // Undeclared capability.
        let mut net = files.clone();
        inject(&mut net, "Gain.pwnode", json!({"OscOut": {}}));
        let net_bytes = envelope("acme.gain", "1.0.0", &net);
        let (net_package, net_version) = listing(
            "acme.gain",
            "1.0.0",
            "Gain.pwnode",
            0,
            &[],
            &net_bytes,
            net.len(),
        );
        assert!(
            install_and_record(
                &trust,
                &Serve {
                    bytes: net_bytes,
                    cancel: None
                },
                &f.library,
                &net_package,
                &net_version,
                &origin(),
                &app(),
                &AtomicBool::new(false),
            )
            .is_err()
        );
        assert_eq!(trust.load(), StoreLoad::Missing, "no record of any kind");
        library_untouched(&f);

        // An existing version is never replaced, nor is its record.
        run(bytes.clone(), None).unwrap().1.unwrap();
        let before = std::fs::read(f.work.join("trust").join(TRUST_FILE)).unwrap();
        assert!(matches!(
            run(bytes, None),
            Err(InstallError::AlreadyInstalled(_))
        ));
        assert_eq!(
            std::fs::read(f.work.join("trust").join(TRUST_FILE)).unwrap(),
            before
        );
    }

    #[test]
    fn installed_content_changed_later_loses_its_record() {
        let f = fx("mk_prov_changed");
        let trust = store(&f);
        let files = gain_files(&f, "1.0.0", |_| {});
        let bytes = envelope("acme.gain", "1.0.0", &files);
        let (package, version) = listing(
            "acme.gain",
            "1.0.0",
            "Gain.pwnode",
            0,
            &[],
            &bytes,
            files.len(),
        );
        let (installed, outcome) = install_and_record(
            &trust,
            &Serve {
                bytes,
                cancel: None,
            },
            &f.library,
            &package,
            &version,
            &origin(),
            &app(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let record = outcome.unwrap();
        // Something edits the installed folder afterwards.
        std::fs::write(installed.package.dir.join("extra.txt"), "x").unwrap();
        let now = identify(&installed.package.dir, true).unwrap().identity;
        assert_eq!(now.id, record.identity.id);
        assert_eq!(trust.load().status(&now), TrustStatus::ContentChanged);
        // A receipt can no longer be made for it either.
        assert!(installed.receipt(&origin()).is_err());
    }

    /// P4.3.2F: installing never runs anything, and a grant for a version
    /// the marketplace later withdraws (or stops listing) is flagged.
    #[test]
    fn installs_never_run_until_granted_and_withdrawn_grants_are_flagged() {
        use crate::library::trust::authorize::{BlockReason, TrustPolicy, authorize_entry};
        let f = fx("mk_withdrawn");
        let files = chain_files(&f, |_| {});
        let bytes = envelope("acme.chain", "1.0.0", &files);
        let (package, version) = listing(
            "acme.chain",
            "1.0.0",
            "Chain.pwnode",
            1,
            &["network"],
            &bytes,
            files.len(),
        );
        let trust = store(&f);
        let constructed = crate::node_trait::CONSTRUCTED.with(|c| c.get());
        let (installed, outcome) = install_and_record(
            &trust,
            &Serve {
                bytes: bytes.clone(),
                cancel: None,
            },
            &f.library,
            &package,
            &version,
            &origin(),
            &app(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome.unwrap().grant, Grant::NotGranted);
        assert_eq!(
            crate::node_trait::CONSTRUCTED.with(|c| c.get()),
            constructed,
            "installing constructs (runs) nothing"
        );
        let policy = TrustPolicy::Store(trust.clone());
        let resolver = {
            let mut resolver =
                crate::library::resolve::PackageResolver::new(None, Some(f.library.clone()));
            resolver.set_trust(policy.clone());
            resolver
        };
        let entry = installed.package.entry_path.clone();
        let refused = authorize_entry(&policy, &entry, &resolver).err().unwrap();
        assert_eq!(
            refused.reason,
            BlockReason::NeedsPermission,
            "needs a decision"
        );

        let identity = identify(&installed.package.dir, true).unwrap().identity;
        trust.grant(&installed.package.dir, &identity).unwrap();
        // Its nested dependency needs its own grant: still blocked.
        let blocked = authorize_entry(&policy, &entry, &resolver).err().unwrap();
        let dependency = blocked.innermost().package.clone();
        assert_ne!(dependency.dir, installed.package.dir);
        let dependency_identity = dependency.identity.clone().unwrap();
        trust.grant(&dependency.dir, &dependency_identity).unwrap();
        if let Err(refused) = authorize_entry(&policy, &entry, &resolver) {
            panic!("granted but refused: {}", refused.message());
        }
        let StoreLoad::Loaded(loaded) = trust.load() else {
            panic!("store");
        };
        let location = origin().location;
        let mut catalogue = crate::library::marketplace::Catalogue {
            generated_at: None,
            publishers: Default::default(),
            packages: [(package.id.clone(), package.clone())]
                .into_iter()
                .collect(),
        };
        assert!(crate::library::trust::withdrawn_grants(&loaded, &catalogue, &location).is_empty());
        // Withdrawn by the catalogue: flagged (the grant is not changed).
        catalogue
            .packages
            .get_mut("acme.chain")
            .unwrap()
            .versions
            .values_mut()
            .for_each(|v| v.yanked = Some("unsafe".into()));
        let flagged = crate::library::trust::withdrawn_grants(&loaded, &catalogue, &location);
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].identity, identity);
        assert!(flagged[0].reason.contains("unsafe"));
        // No longer listed: flagged too. Another source: not this one's.
        catalogue.packages.clear();
        let flagged = crate::library::trust::withdrawn_grants(&loaded, &catalogue, &location);
        assert!(flagged[0].reason.contains("no longer listed"));
        let other = CatalogueLocation::new("someone", "else", "main").unwrap();
        assert!(crate::library::trust::withdrawn_grants(&loaded, &catalogue, &other).is_empty());

        // Revoking it stops it at once.
        trust.revoke_grant(&identity).unwrap();
        assert!(authorize_entry(&policy, &entry, &resolver).is_err());
    }
}
