//! P4.3.4: catalogue repository validation and publication.
//!
//! Fixtures are built deterministically in temporary folders: publisher
//! claims and version folders written exactly as the desktop's Share to
//! Marketplace writes them (one test goes through the real desktop
//! submission), then pull requests, publication and installation.

use super::*;
use crate::library::package::tests::TempDir;
use crate::library::package::{PackageDependency, PackageManifest};
use std::sync::atomic::AtomicBool;

const ACME_ID: u64 = 101;
const BOB_ID: u64 = 202;
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// The smallest valid PNG signature-led file (the signature is checked).
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

fn location() -> CatalogueLocation {
    // A test repository (never a real production name).
    CatalogueLocation::new("example-org", "example-catalogue", "main").unwrap()
}

fn app() -> semver::Version {
    super::super::current_app_version()
}

fn acme() -> Author {
    Author {
        login: "Acme".into(),
        id: ACME_ID,
        maintainer: false,
    }
}

fn write(root: &Path, path: &str, bytes: impl AsRef<[u8]>) {
    let full = path.split('/').fold(root.to_path_buf(), |p, s| p.join(s));
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, bytes).unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn claim(root: &Path, namespace: &str, github: &str, id: u64) {
    write(
        root,
        &format!("publishers/{namespace}.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "namespace": namespace, "github": github, "github_id": id,
            "display_name": github
        }))
        .unwrap(),
    );
}

fn pwnode(node_type: serde_json::Value) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "version": 1, "name": "N",
        "graph": {"nodes": {"1": {"id": 1, "pos": [0.0, 0.0], "node_type": node_type}},
                  "connections": [], "next_id": 2}
    }))
    .unwrap()
}

fn slider(value: f32) -> serde_json::Value {
    serde_json::json!({"Slider": {"value": value, "min": 0.0, "max": 1.0, "step": 0.01,
                                  "slider_color": [0, 0, 0], "label": ""}})
}

fn sha(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

fn manifest(id: &str, version: &str, entry: &str, entry_text: &str) -> PackageManifest {
    PackageManifest {
        format: 1,
        id: id.into(),
        name: "Fixture".into(),
        version: version.into(),
        description: String::new(),
        author: "Fixture".into(),
        tags: Vec::new(),
        entry: entry.into(),
        sha256: sha(entry_text),
        pwnode_version: 1,
        min_app_version: Some("0.0.1".into()),
        dependencies: Vec::new(),
        source: "marketplace".into(),
        published_at: 0,
        files: Default::default(),
        bundle_format: 0,
    }
}

/// A version folder (and the package's listing metadata).
fn version(root: &Path, ns: &str, name: &str, v: &str, entry_text: &str) -> PackageManifest {
    let id = format!("{ns}.{name}");
    let manifest = manifest(&id, v, "Node.pwnode", entry_text);
    put(root, ns, name, v, &manifest, &[("Node.pwnode", entry_text)]);
    manifest
}

fn put(
    root: &Path,
    ns: &str,
    name: &str,
    v: &str,
    manifest: &PackageManifest,
    files: &[(&str, &str)],
) {
    let dir = format!("packages/{ns}/{name}");
    write(
        root,
        &format!("{dir}/package.meta.json"),
        r#"{"name": "Fixture", "author": "Fixture", "tags": ["test"]}"#,
    );
    write(
        root,
        &format!("{dir}/{v}/package.json"),
        serde_json::to_string_pretty(manifest).unwrap(),
    );
    for (path, text) in files {
        write(root, &format!("{dir}/{v}/{path}"), text);
    }
}

/// A repository with Acme's claim and `acme.gain@1.0.0`.
fn base(tmp: &TempDir, name: &str) -> PathBuf {
    let root = tmp.0.join(name);
    std::fs::create_dir_all(&root).unwrap();
    write(&root, "README.md", "catalogue");
    claim(&root, "acme", "Acme", ACME_ID);
    root
}

fn copy_of(tmp: &TempDir, root: &Path, name: &str) -> PathBuf {
    let copy = tmp.0.join(name);
    std::fs::create_dir_all(&copy).unwrap();
    copy_tree(root, &copy);
    copy
}

/// Merge and publish as the publish workflow does.
fn publish(root: &Path, commit: &str, at: u64) -> Vec<String> {
    let written = publish_artifacts(root, &location()).unwrap();
    let index = publish_index(root, &location(), commit, at).unwrap();
    std::fs::write(root.join(INDEX_FILE), index).unwrap();
    written
}

fn check(base: &Path, head: &Path, author: &Author) -> Result<PullRequestReport, Issues> {
    check_pull_request(base, head, &location(), author)
}

fn assert_refused(result: Result<PullRequestReport, Issues>, needle: &str) {
    let issues = result.expect_err("refused");
    assert!(
        issues.iter().any(|i| i.contains(needle)),
        "expected '{needle}' in {issues:#?}"
    );
}

#[test]
fn a_desktop_submission_is_validated_published_and_installed() {
    let tmp = TempDir::new("catalogue_round_trip");
    // The desktop: publish locally, then Share to Marketplace.
    let library = tmp.0.join("library");
    std::fs::create_dir_all(&library).unwrap();
    let store = crate::library::trust::TrustStore::at(
        tmp.0.join("home").join("trust").join("packages.json"),
    );
    let source = crate::library::bundle::tests::leaf(&tmp.0.join("src"), "Gain", 0.25);
    let (local, outcome) = crate::library::trust::publish_and_record(
        &store,
        &library,
        &source,
        &crate::library::package::tests::request("Gain", "1.0.0"),
    )
    .unwrap();
    outcome.unwrap();
    let request = crate::library::marketplace::submit::ShareRequest {
        namespace: "acme".into(),
        name: "gain".into(),
        display_name: "Gain".into(),
        description: "A constant.".into(),
        author: "Acme Audio".into(),
        tags: vec!["audio".into()],
        license: Some("MIT".into()),
        documentation: None,
        min_patchwork: app(),
        preview: None,
    };
    let prepared = crate::library::marketplace::submit::prepare(
        &local.dir,
        &request,
        &store,
        &location(),
        None,
        &app(),
    )
    .unwrap();
    let out = tmp.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let folder = crate::library::marketplace::submit::write_submission(
        &prepared,
        &location(),
        &out,
        &AtomicBool::new(false),
    )
    .unwrap();

    // The pull request: the submission's repository/ folder plus a claim.
    let base_root = base(&tmp, "base");
    std::fs::remove_file(base_root.join("publishers/acme.json")).unwrap();
    let head = copy_of(&tmp, &base_root, "head");
    copy_tree(&folder.join("repository"), &head);
    claim(&head, "acme", "Acme", ACME_ID);
    let report = check(&base_root, &head, &acme()).unwrap();
    assert_eq!(report.new_publishers, vec!["acme"]);
    assert_eq!(report.new_versions, vec!["acme.gain@1.0.0"]);

    // After merge: CI generates exactly the desktop's artifact bytes.
    let built = crate::node_trait::CONSTRUCTED.with(|c| c.get());
    assert_eq!(
        publish(&head, COMMIT_A, 1_700_000_000),
        vec!["artifacts/acme.gain/1.0.0/acme.gain-1.0.0.pwpkg".to_string()]
    );
    assert_eq!(
        crate::node_trait::CONSTRUCTED.with(|c| c.get()),
        built,
        "nothing ran"
    );
    let artifact =
        std::fs::read(head.join("artifacts/acme.gain/1.0.0/acme.gain-1.0.0.pwpkg")).unwrap();
    assert_eq!(
        artifact, prepared.artifact,
        "byte-identical to the desktop's"
    );
    let index_bytes = std::fs::read(head.join(INDEX_FILE)).unwrap();
    let catalogue = super::super::parse_catalogue(&index_bytes, location().source()).unwrap();
    let package = &catalogue.packages["acme.gain"];
    let listed = &package.versions[&semver::Version::new(1, 0, 0)];
    assert_eq!(listed.artifact.sha256, prepared.sha256);
    assert_eq!(
        listed.artifact.commit, COMMIT_A,
        "pinned to the artifacts commit"
    );
    assert_eq!(listed.min_patchwork, app());
    assert_eq!(catalogue.generated_at, Some(1_700_000_000));

    // Patchwork installs it from the generated catalogue.
    let elsewhere = tmp.0.join("other-library");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let installed = crate::library::marketplace::install::install_artifact(
        &elsewhere,
        package,
        listed,
        &app(),
        &artifact,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&installed.package.entry_path).unwrap(),
        std::fs::read(&local.entry_path).unwrap()
    );
}

#[test]
fn publication_is_deterministic_pinned_and_append_only() {
    let tmp = TempDir::new("catalogue_publish");
    let root = base(&tmp, "repo");
    version(&root, "acme", "gain", "1.0.0", &pwnode(slider(0.5)));
    let twin = copy_of(&tmp, &root, "twin");
    publish(&root, COMMIT_A, 100);
    publish(&twin, COMMIT_A, 100);
    let index = std::fs::read(root.join(INDEX_FILE)).unwrap();
    assert_eq!(
        index,
        std::fs::read(twin.join(INDEX_FILE)).unwrap(),
        "deterministic"
    );

    // A new version later: the old one keeps its pin, the new one is pinned
    // to the new commit; nothing is rewritten.
    let old_artifact =
        std::fs::read(root.join("artifacts/acme.gain/1.0.0/acme.gain-1.0.0.pwpkg")).unwrap();
    version(&root, "acme", "gain", "1.1.0", &pwnode(slider(0.6)));
    assert_eq!(
        publish(&root, COMMIT_B, 50).len(),
        1,
        "only the new artifact"
    );
    let catalogue = previous_index(&root, &location()).unwrap().unwrap();
    let package = &catalogue.packages["acme.gain"];
    assert_eq!(
        package.versions[&semver::Version::new(1, 0, 0)]
            .artifact
            .commit,
        COMMIT_A
    );
    assert_eq!(
        package.versions[&semver::Version::new(1, 1, 0)]
            .artifact
            .commit,
        COMMIT_B
    );
    assert_eq!(catalogue.generated_at, Some(100), "never goes backwards");
    assert_eq!(
        std::fs::read(root.join("artifacts/acme.gain/1.0.0/acme.gain-1.0.0.pwpkg")).unwrap(),
        old_artifact
    );

    // A published artifact that no longer matches its folder is refused.
    write(
        &root,
        "artifacts/acme.gain/1.0.0/acme.gain-1.0.0.pwpkg",
        b"replaced",
    );
    let issues = publish_artifacts(&root, &location()).unwrap_err();
    assert!(
        issues.iter().any(|i| i.contains("never replaced")),
        "{issues:?}"
    );
    std::fs::write(
        root.join("artifacts/acme.gain/1.0.0/acme.gain-1.0.0.pwpkg"),
        &old_artifact,
    )
    .unwrap();

    // Changing a published version folder is refused at publication too.
    write(
        &root,
        "packages/acme/gain/1.0.0/Node.pwnode",
        pwnode(slider(0.9)),
    );
    assert!(publish_artifacts(&root, &location()).is_err());

    // The index needs a real commit, and the new artifacts committed first.
    let fresh = base(&tmp, "fresh");
    version(&fresh, "acme", "gain", "1.0.0", &pwnode(slider(0.5)));
    assert!(
        publish_index(&fresh, &location(), COMMIT_A, 1).is_err(),
        "no artifact yet"
    );
    assert!(publish_index(&fresh, &location(), "HEAD", 1).is_err());
}

#[test]
fn moderation_withdraws_and_takedowns_never_free_published_bytes() {
    let tmp = TempDir::new("catalogue_moderation");
    let root = base(&tmp, "repo");
    version(&root, "acme", "gain", "1.0.0", &pwnode(slider(0.5)));
    publish(&root, COMMIT_A, 100);
    write(
        &root,
        MODERATION_FILE,
        r#"{"yanked": {"acme.gain@1.0.0": "unsafe output levels"}}"#,
    );
    publish(&root, COMMIT_B, 200);
    let catalogue = previous_index(&root, &location()).unwrap().unwrap();
    let listed = &catalogue.packages["acme.gain"].versions[&semver::Version::new(1, 0, 0)];
    assert_eq!(listed.yanked.as_deref(), Some("unsafe output levels"));
    assert_eq!(listed.artifact.commit, COMMIT_A);
    // An unknown version in moderation.json is an error.
    write(
        &root,
        MODERATION_FILE,
        r#"{"yanked": {"acme.gone@1.0.0": "x"}}"#,
    );
    assert!(read_repository(&root).is_err());
    std::fs::remove_file(root.join(MODERATION_FILE)).unwrap();

    // Taken down by the maintainers (folder removed): the artifact stays,
    // and the version can only come back with exactly the same bytes.
    let head = copy_of(&tmp, &root, "takedown");
    std::fs::remove_dir_all(head.join("packages/acme/gain")).unwrap();
    let maintainer = Author {
        login: "maint".into(),
        id: 1,
        maintainer: true,
    };
    check(&root, &head, &maintainer).unwrap();
    publish(&head, COMMIT_B, 300);
    let after = previous_index(&head, &location()).unwrap().unwrap();
    assert!(!after.packages.contains_key("acme.gain"));
    let readd = copy_of(&tmp, &head, "readd");
    version(&readd, "acme", "gain", "1.0.0", &pwnode(slider(0.7)));
    assert_refused(check(&head, &readd, &acme()), "never replaced");
}

#[test]
fn malicious_and_invalid_pull_requests_are_refused() {
    let tmp = TempDir::new("catalogue_malicious");
    let published = base(&tmp, "published");
    version(&published, "acme", "gain", "1.0.0", &pwnode(slider(0.5)));
    publish(&published, COMMIT_A, 100);
    let mut n = 0;
    let mut head = |edit: &dyn Fn(&Path)| {
        n += 1;
        let head = copy_of(&tmp, &published, &format!("head{n}"));
        edit(&head);
        head
    };
    let bob = Author {
        login: "bob".into(),
        id: BOB_ID,
        maintainer: false,
    };

    // Control: a new version from the namespace's owner.
    let ok = head(&|h| {
        version(h, "acme", "gain", "1.1.0", &pwnode(slider(0.6)));
    });
    assert_eq!(
        check(&published, &ok, &acme()).unwrap().new_versions,
        vec!["acme.gain@1.1.0"]
    );

    // Ownership: another account (or the same login with another numeric
    // id: a renamed or recycled login) cannot publish in the namespace.
    assert_refused(check(&published, &ok, &bob), "does not belong to @bob");
    let recycled = Author { id: 999, ..acme() };
    assert_refused(check(&published, &ok, &recycled), "does not belong");
    // Claims only for the PR's own account; existing claims never change.
    let foreign_claim = head(&|h| claim(h, "carol", "carol", 303));
    assert_refused(
        check(&published, &foreign_claim, &bob),
        "own GitHub account",
    );
    let own_claim = head(&|h| claim(h, "bob", "bob", BOB_ID));
    check(&published, &own_claim, &bob).unwrap();
    let mismatched_id = head(&|h| claim(h, "bob", "bob", 1));
    assert_refused(
        check(&published, &mismatched_id, &bob),
        "own GitHub account",
    );
    let takeover = head(&|h| claim(h, "acme", "bob", BOB_ID));
    assert_refused(
        check(&published, &takeover, &bob),
        "only be changed by the maintainers",
    );
    // Reserved and look-alike namespaces, and login-namespace mismatch.
    let tester = Author {
        login: "test".into(),
        id: 404,
        maintainer: false,
    };
    let reserved = head(&|h| claim(h, "test", "test", 404));
    assert!(check(&published, &reserved, &tester).is_err());
    let acme_io = Author {
        login: "ac-me".into(),
        id: 405,
        maintainer: false,
    };
    let lookalike = head(&|h| claim(h, "ac-me", "ac-me", 405));
    assert!(check(&published, &lookalike, &acme_io).is_err());

    // Published bytes and generated files never change in a pull request.
    let changed = head(&|h| {
        write(
            h,
            "packages/acme/gain/1.0.0/Node.pwnode",
            pwnode(slider(0.9)),
        )
    });
    assert_refused(
        check(&published, &changed, &acme()),
        "published versions never change",
    );
    let added = head(&|h| {
        write(
            h,
            "packages/acme/gain/1.0.0/extra.pwnode",
            pwnode(slider(0.1)),
        )
    });
    assert_refused(
        check(&published, &added, &acme()),
        "published versions never change",
    );
    let index = head(&|h| write(h, INDEX_FILE, "{}"));
    assert_refused(
        check(&published, &index, &acme()),
        "generated by the publish workflow",
    );
    let artifact = head(&|h| write(h, "artifacts/acme.gain/9.0.0/x.pwpkg", "x"));
    assert_refused(
        check(&published, &artifact, &acme()),
        "generated by the publish workflow",
    );
    let workflow = head(&|h| write(h, ".github/workflows/validate.yml", "on: push"));
    assert_refused(
        check(&published, &workflow, &acme()),
        "only the maintainers",
    );
    let maintainer = Author {
        login: "maint".into(),
        id: 1,
        maintainer: true,
    };
    check(&published, &workflow, &maintainer).unwrap();
    assert_refused(
        check(&published, &index, &maintainer),
        "generated by the publish workflow",
    );

    // Content: secrets, restricted node types, unknown node types.
    for (needle, node) in [
        ("AKIA", slider(0.5)),
        (
            "cannot be installed from the marketplace",
            serde_json::json!({"RustPlugin": {}}),
        ),
        (
            "cannot be installed from the marketplace",
            serde_json::json!("McpServer"),
        ),
        ("", serde_json::json!("NoSuchNodeType")),
    ] {
        let text = if needle == "AKIA" {
            pwnode(node).replace("\"label\": \"\"", "\"label\": \"AKIAABCDEFGHIJKLMNOP\"")
        } else {
            pwnode(node)
        };
        let bad = head(&|h| {
            version(h, "acme", "bad", "1.0.0", &text);
        });
        let issues = check(&published, &bad, &acme()).unwrap_err();
        assert!(
            !issues.iter().any(|i| i.contains("AKIAABCDEFGHIJKLMNOP")),
            "values never echoed"
        );
        if needle != "AKIA" && !needle.is_empty() {
            assert!(issues.iter().any(|i| i.contains(needle)), "{issues:?}");
        }
    }

    // Detected capabilities are what is listed (network here).
    let network = head(&|h| {
        version(
            h,
            "acme",
            "fetch",
            "1.0.0",
            &pwnode(serde_json::json!({"HttpRequest": {}})),
        );
    });
    check(&published, &network, &acme()).unwrap();
    let catalogue = validate(&read_repository(&network).unwrap(), &location(), None).unwrap();
    let listed = &catalogue.packages["acme.fetch"].versions[&semver::Version::new(1, 0, 0)];
    assert_eq!(
        listed.known_capabilities(),
        [Capability::Network].into_iter().collect()
    );

    // Paths and layout: extra files, bad version folders, id mismatch,
    // missing minimum Patchwork, links.
    let extra = head(&|h| {
        version(h, "acme", "extra", "1.0.0", &pwnode(slider(0.5)));
        write(h, "packages/acme/extra/1.0.0/notes.txt", "x");
    });
    assert!(check(&published, &extra, &acme()).is_err());
    let nested_dir = head(&|h| {
        version(h, "acme", "nest", "1.0.0", &pwnode(slider(0.5)));
        write(
            h,
            "packages/acme/nest/1.0.0/sub/Node.pwnode",
            pwnode(slider(0.5)),
        );
    });
    assert!(check(&published, &nested_dir, &acme()).is_err());
    let noncanonical = head(&|h| {
        let text = pwnode(slider(0.5));
        put(
            h,
            "acme",
            "odd",
            "01.0.0",
            &manifest("acme.odd", "1.0.0", "Node.pwnode", &text),
            &[("Node.pwnode", &text)],
        );
    });
    assert_refused(
        check(&published, &noncanonical, &acme()),
        "MAJOR.MINOR.PATCH",
    );
    let mismatch = head(&|h| {
        let text = pwnode(slider(0.5));
        put(
            h,
            "acme",
            "one",
            "1.0.0",
            &manifest("acme.other", "1.0.0", "Node.pwnode", &text),
            &[("Node.pwnode", &text)],
        );
    });
    assert_refused(check(&published, &mismatch, &acme()), "not the folder's");
    let no_min = head(&|h| {
        let text = pwnode(slider(0.5));
        let mut m = manifest("acme.nomin", "1.0.0", "Node.pwnode", &text);
        m.min_app_version = None;
        put(h, "acme", "nomin", "1.0.0", &m, &[("Node.pwnode", &text)]);
    });
    assert_refused(check(&published, &no_min, &acme()), "min_app_version");
    let wrong_checksum = head(&|h| {
        let text = pwnode(slider(0.5));
        let m = manifest("acme.sum", "1.0.0", "Node.pwnode", "something else");
        put(h, "acme", "sum", "1.0.0", &m, &[("Node.pwnode", &text)]);
    });
    assert!(check(&published, &wrong_checksum, &acme()).is_err());
    let local_source = head(&|h| {
        let text = pwnode(slider(0.5));
        let mut m = manifest("acme.loc", "1.0.0", "Node.pwnode", &text);
        m.source = "local".into();
        put(h, "acme", "loc", "1.0.0", &m, &[("Node.pwnode", &text)]);
    });
    assert!(check(&published, &local_source, &acme()).is_err());

    // Previews: allowed names, real images of that type, size.
    let good_preview = head(&|h| {
        version(h, "acme", "pic", "1.0.0", &pwnode(slider(0.5)));
        write(h, "packages/acme/pic/1.0.0/preview.png", PNG);
    });
    check(&published, &good_preview, &acme()).unwrap();
    let fake_preview = head(&|h| {
        version(h, "acme", "pic", "1.0.0", &pwnode(slider(0.5)));
        write(h, "packages/acme/pic/1.0.0/preview.png", b"<script>");
    });
    assert_refused(check(&published, &fake_preview, &acme()), "not a png image");
    let huge_preview = head(&|h| {
        version(h, "acme", "pic", "1.0.0", &pwnode(slider(0.5)));
        let mut bytes = PNG.to_vec();
        bytes.resize(MAX_PREVIEW_BYTES as usize + 1, 0);
        write(h, "packages/acme/pic/1.0.0/preview.png", bytes);
    });
    assert!(check(&published, &huge_preview, &acme()).is_err());
}

#[test]
fn dependencies_must_be_complete_published_and_unmodified() {
    let tmp = TempDir::new("catalogue_dependencies");
    let published = base(&tmp, "published");
    let gain_text = pwnode(slider(0.5));
    let gain = version(&published, "acme", "gain", "1.0.0", &gain_text);
    publish(&published, COMMIT_A, 100);
    let gain_manifest =
        std::fs::read_to_string(published.join("packages/acme/gain/1.0.0/package.json")).unwrap();

    let chain = |h: &Path, bundled_entry: &str, declared: bool, bundled: bool| {
        let text = pwnode(serde_json::json!({"CustomNodeInstance": {
            "file": "deps/acme.gain/1.0.0/Node.pwnode", "inputs": [], "outputs": []}}));
        let mut m = manifest("acme.chain", "1.0.0", "Chain.pwnode", &text);
        let mut files = vec![("Chain.pwnode", text.as_str())];
        if declared {
            m.dependencies = vec![PackageDependency {
                id: gain.id.clone(),
                version: "1.0.0".into(),
                entry: None,
            }];
        }
        m.bundle_format = 1;
        if bundled {
            m.files.insert(
                "deps/acme.gain/1.0.0/package.json".into(),
                sha(&gain_manifest),
            );
            m.files.insert(
                "deps/acme.gain/1.0.0/Node.pwnode".into(),
                sha(bundled_entry),
            );
            files.push(("deps/acme.gain/1.0.0/package.json", gain_manifest.as_str()));
            files.push(("deps/acme.gain/1.0.0/Node.pwnode", bundled_entry));
        }
        put(h, "acme", "chain", "1.0.0", &m, &files);
    };
    let complete = copy_of(&tmp, &published, "complete");
    chain(&complete, &gain_text, true, true);
    check(&published, &complete, &acme()).unwrap();
    // Declared but not bundled.
    let missing = copy_of(&tmp, &published, "missing");
    chain(&missing, &gain_text, true, false);
    assert!(check(&published, &missing, &acme()).is_err());
    // A modified copy of the published dependency.
    let forged = copy_of(&tmp, &published, "forged");
    chain(
        &forged,
        &pwnode(serde_json::json!({"HttpRequest": {}})),
        true,
        true,
    );
    assert_refused(
        check(&published, &forged, &acme()),
        "differs from the published version",
    );
    // A dependency the catalogue does not have.
    let unknown = copy_of(&tmp, &published, "unknown");
    std::fs::remove_dir_all(unknown.join("packages/acme/gain")).unwrap();
    chain(&unknown, &gain_text, true, true);
    let maintainer = Author {
        login: "maint".into(),
        id: 1,
        maintainer: true,
    };
    assert_refused(
        check(&published, &unknown, &maintainer),
        "not in the catalogue",
    );
}

#[test]
fn the_command_line_reports_problems_with_its_exit_status() {
    let tmp = TempDir::new("catalogue_cli");
    let root = base(&tmp, "repo");
    version(&root, "acme", "gain", "1.0.0", &pwnode(slider(0.5)));
    let args = |command: &str, extra: &[&str]| {
        let mut args = vec![
            command.to_string(),
            "--root".into(),
            root.to_string_lossy().into_owned(),
            "--repository".into(),
            "example-org/example-catalogue".into(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        args
    };
    assert_eq!(cli::run(args("validate", &[])), 0);
    assert_eq!(cli::run(args("publish-artifacts", &[])), 0);
    assert_eq!(
        cli::run(args(
            "publish-index",
            &["--commit", COMMIT_A, "--generated-at", "5"]
        )),
        0
    );
    assert!(root.join(INDEX_FILE).is_file());
    write(&root, "packages/acme/gain/1.0.0/notes.txt", "x");
    assert_eq!(cli::run(args("validate", &[])), 1);
    assert_eq!(cli::run(vec!["validate".into()]), 2, "no repository given");
    assert_eq!(cli::run(args("frobnicate", &[])), 2);
    assert_eq!(cli::run(Vec::new()), 2);
}

/// The repository template shipped with Patchwork
/// (`marketplace/catalogue-repository/`) is a valid, empty catalogue, and
/// its workflows only call commands this tool has.
#[test]
fn the_repository_template_is_a_valid_empty_catalogue() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("marketplace/catalogue-repository");
    let repository = read_repository(&template).unwrap();
    assert!(repository.publishers.is_empty() && repository.packages.is_empty());
    assert!(
        !template.join(INDEX_FILE).exists(),
        "generated, never committed by hand"
    );
    let tmp = TempDir::new("catalogue_template");
    let root = copy_of(&tmp, &template, "repo");
    assert!(publish_artifacts(&root, &location()).unwrap().is_empty());
    let index = publish_index(&root, &location(), COMMIT_A, 1).unwrap();
    let catalogue = super::super::parse_catalogue(&index, location().source()).unwrap();
    assert!(catalogue.packages.is_empty());
    assert_eq!(
        std::fs::read_to_string(template.join(".gitattributes"))
            .unwrap()
            .lines()
            .last(),
        Some("* -text"),
        "exact bytes"
    );
    let workflows = template.join(".github/workflows");
    let mut names: Vec<String> = std::fs::read_dir(&workflows)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["publish.yml", "submission-paths.yml", "validate.yml"],
        "every workflow is reviewed here"
    );
    for workflow in ["validate.yml", "publish.yml", "submission-paths.yml"] {
        let text = std::fs::read_to_string(workflows.join(workflow)).unwrap();
        let code: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect();
        let has = |needle: &str| code.iter().any(|l| l.contains(needle));
        if workflow == "submission-paths.yml" {
            // The trusted guard: taken from the default branch, read-only,
            // never touching pull-request code.
            assert!(has("  pull_request_target:"), "{workflow}");
            assert!(!has("  pull_request:"), "{workflow}");
            let permissions = text
                .split("\npermissions:\n")
                .nth(1)
                .and_then(|rest| rest.split("\n\n").next())
                .unwrap();
            assert_eq!(
                permissions, "  contents: read\n  pull-requests: read",
                "{workflow}: minimal read-only permissions"
            );
            assert!(
                has("    name: Submission paths"),
                "{workflow}: stable check name"
            );
            assert_eq!(
                code.iter()
                    .filter(|l| l.contains("actions/checkout"))
                    .count(),
                1,
                "{workflow}: only the checker is checked out"
            );
            assert!(has("repository: ${{ vars.PATCHWORK_REPOSITORY }}"));
            for forbidden in [
                "github.event.pull_request.head.ref",
                "github.event.pull_request.head.repo",
                "github.head_ref",
                "refs/pull/",
                "secrets.",
                "${{ github.event.pull_request.title",
                "${{ github.event.pull_request.body",
            ] {
                assert!(!has(forbidden), "{workflow}: {forbidden}");
            }
            // Every value from the event reaches scripts through `env:`.
            for line in code.iter().filter(|l| l.contains("${{ github.event")) {
                let key = line.trim_start().split(':').next().unwrap();
                assert!(
                    ["HEAD_SHA", "DEFAULT_BRANCH", "PR_NUMBER", "group"].contains(&key),
                    "{workflow}: {line}"
                );
            }
            assert!(has("files?per_page=100&page="));
            assert_eq!(paths::PER_PAGE, 100);
        } else {
            assert!(!has("pull_request_target"), "{workflow}");
        }
        if workflow == "validate.yml" {
            assert!(
                has("    name: Validate submission"),
                "{workflow}: stable check name"
            );
        }
        let mut calls = 0;
        for line in code {
            let Some((_, after)) = line
                .split_once(".exe catalogue ")
                .or_else(|| line.split_once("\" catalogue "))
            else {
                continue;
            };
            calls += 1;
            let command = after.split_whitespace().next().unwrap();
            assert!(
                [
                    "validate",
                    "check-pr",
                    "publish-artifacts",
                    "publish-index",
                    "check-paths"
                ]
                .contains(&command),
                "{workflow}: unknown command {command}"
            );
        }
        assert!(calls > 0, "{workflow} runs the validator");
    }
}
