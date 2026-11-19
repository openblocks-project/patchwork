//! P4.3.3: marketplace submissions.

use super::*;
use crate::library::bundle::tests::{child, composite, leaf};
use crate::library::marketplace::install::install_artifact;
use crate::library::package::VerifiedPackage;
use crate::library::package::tests::{TempDir, request};
use crate::library::trust::TrustStore;

struct Fx {
    tmp: TempDir,
    library: PathBuf,
    store: TrustStore,
    location: CatalogueLocation,
}

fn fx(tag: &str) -> Fx {
    let tmp = TempDir::new(tag);
    let library = tmp.0.join("library");
    std::fs::create_dir_all(&library).unwrap();
    let store = TrustStore::at(tmp.0.join("home").join("trust").join("packages.json"));
    Fx {
        tmp,
        library,
        store,
        location: CatalogueLocation::new("patchwork-nodes", "catalogue", "main").unwrap(),
    }
}

fn app() -> semver::Version {
    super::super::current_app_version()
}

fn share(namespace: &str, name: &str) -> ShareRequest {
    ShareRequest {
        namespace: namespace.into(),
        name: name.into(),
        display_name: "Gain".into(),
        description: "A constant.\nTwo lines.".into(),
        author: "Acme Audio".into(),
        tags: vec!["audio".into(), "utility".into()],
        license: Some("MIT".into()),
        documentation: Some("https://example.com/gain".into()),
        min_patchwork: app(),
        preview: None,
    }
}

impl Fx {
    /// Published on this computer by the trusted workflow.
    fn published(&self, source: &Path, name: &str, version: &str) -> VerifiedPackage {
        let (package, outcome) = crate::library::trust::publish_and_record(
            &self.store,
            &self.library,
            source,
            &request(name, version),
        )
        .unwrap();
        outcome.unwrap();
        package
    }

    fn leaf(&self, name: &str, value: f32) -> VerifiedPackage {
        let source = leaf(&self.tmp.0.join("src"), name, value);
        self.published(&source, name, "1.0.0")
    }

    fn prepare(
        &self,
        package: &VerifiedPackage,
        request: &ShareRequest,
        catalogue: Option<&Catalogue>,
    ) -> Result<Prepared, String> {
        prepare(
            &package.dir,
            request,
            &self.store,
            &self.location,
            catalogue,
            &app(),
        )
    }

    /// Install a prepared artifact the way the marketplace does.
    fn install(&self, library: &Path, prepared: &Prepared) -> VerifiedPackage {
        let catalogue = listing_catalogue(prepared, &self.location, &"a".repeat(40)).unwrap();
        let package = &catalogue.packages[&prepared.public_id];
        let version = &package.versions[&prepared.version];
        install_artifact(
            library,
            package,
            version,
            &app(),
            &prepared.artifact,
            &AtomicBool::new(false),
        )
        .unwrap()
        .package
    }
}

/// Every file under `dir`, with its bytes.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    files_in(dir)
        .unwrap()
        .into_iter()
        .map(|path| {
            let full = path.split('/').fold(dir.to_path_buf(), |p, s| p.join(s));
            let bytes = std::fs::read(full).unwrap();
            (path, bytes)
        })
        .collect()
}

fn constructed() -> usize {
    crate::node_trait::CONSTRUCTED.with(|c| c.get())
}

#[test]
fn submissions_are_deterministic_verified_and_install_from_the_marketplace() {
    let f = fx("submit_round_trip");
    let local = f.leaf("Gain", 0.25);
    let before = snapshot(&local.dir);
    let built = constructed();

    let prepared = f.prepare(&local, &share("acme", "gain"), None).unwrap();
    let again = f.prepare(&local, &share("acme", "gain"), None).unwrap();
    assert_eq!(prepared.artifact, again.artifact, "byte-identical");
    assert_eq!(prepared.sha256, again.sha256);
    // Integrity: the listing describes exactly these bytes.
    assert_eq!(prepared.sha256, sha256_hex(&prepared.artifact));
    assert_eq!(prepared.size, prepared.artifact.len() as u64);
    assert_eq!(prepared.file_count as usize, prepared.files.len());
    let version = &prepared.listing.versions[0];
    assert_eq!(version.artifact.sha256, prepared.sha256);
    assert_eq!(version.artifact.size, prepared.size);
    assert_eq!(prepared.public_id, "acme.gain");
    assert_eq!(prepared.local_id, local.manifest.id);
    assert!(prepared.notes.iter().any(|n| n.contains("offline")));

    // Written: the reviewable folder in the repository layout, the
    // artifact CI must rebuild, and the catalogue data.
    let out = f.tmp.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let folder = write_submission(&prepared, &f.location, &out, &AtomicBool::new(false)).unwrap();
    assert_eq!(folder, out.join("patchwork-submission-acme.gain-1.0.0"));
    let version_dir = folder.join("repository/packages/acme/gain/1.0.0");
    for (path, text) in &prepared.files {
        let full = path.split('/').fold(version_dir.clone(), |p, s| p.join(s));
        assert_eq!(std::fs::read_to_string(full).unwrap(), *text, "{path}");
    }
    assert_eq!(
        std::fs::read(folder.join("artifact/acme.gain-1.0.0.pwpkg")).unwrap(),
        prepared.artifact
    );
    let meta: serde_json::Value = serde_json::from_slice(
        &std::fs::read(folder.join("repository/packages/acme/gain/package.meta.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["name"], "Gain");
    assert_eq!(meta["license"], "MIT");
    let entry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(folder.join("catalogue-entry.json")).unwrap())
            .unwrap();
    assert_eq!(entry["artifact"]["sha256"], prepared.sha256);
    assert_eq!(entry["artifact"]["size"], prepared.size);
    assert_eq!(
        entry["artifact"]["path"],
        "artifacts/acme.gain/1.0.0/acme.gain-1.0.0.pwpkg"
    );
    let readme = std::fs::read_to_string(folder.join("SUBMISSION.md")).unwrap();
    assert!(readme.contains(&prepared.sha256));
    assert!(readme.contains("never asks for your GitHub password"));
    // The pull request targets the configured catalogue branch, which a
    // fork must include.
    let branch = f.location.branch().to_string();
    assert!(readme.contains(&format!("(base: `{branch}`)")));
    assert!(readme.contains(&format!("so the fork includes the `{branch}` branch")));
    // The single-repository catalogue: a branch of the Patchwork repository.
    let single = CatalogueLocation::new("openblocks-project", "patchwork", "catalogue").unwrap();
    let steps = super::readme(&prepared, &single);
    assert!(
        steps
            .contains("The catalogue is the `catalogue` branch of `openblocks-project/patchwork`.")
    );
    assert!(steps.contains("switch to the `catalogue` branch"));
    assert!(steps.contains("(base: `catalogue`)"));
    let all = files_in(&folder).unwrap();
    assert!(
        !all.iter()
            .any(|p| p.starts_with("repository/artifacts") || p.ends_with("index.json")),
        "generated files are never submitted by hand"
    );
    // The written version folder is a valid package version.
    crate::library::package::verify_version_dir_static(&version_dir, false).unwrap();

    // Nothing ran, and the local package is unchanged.
    assert_eq!(constructed(), built, "no node was constructed");
    assert_eq!(snapshot(&local.dir), before);

    // Round trip: the marketplace installs it into another Library.
    let elsewhere = f.tmp.0.join("other-library");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let installed = f.install(&elsewhere, &prepared);
    assert_eq!(installed.manifest.id, "acme.gain");
    assert_eq!(installed.manifest.source, MARKETPLACE_SOURCE);
    assert_eq!(
        std::fs::read(&installed.entry_path).unwrap(),
        std::fs::read(&local.entry_path).unwrap(),
        "the graph is shared exactly"
    );
}

#[test]
fn nested_dependencies_are_bundled_and_must_already_be_public() {
    let f = fx("submit_nested");
    // A public dependency: shared, then installed from the marketplace.
    let gain = f
        .prepare(&f.leaf("Gain", 0.5), &share("acme", "gain"), None)
        .unwrap();
    let public_gain = f.install(&f.library, &gain);
    let source = composite(&f.tmp.0.join("src"), "Chain", &[child(&public_gain)]);
    let chain = f.published(&source, "Chain", "1.0.0");
    assert_eq!(chain.manifest.dependencies.len(), 1);

    let prepared = f.prepare(&chain, &share("acme", "chain"), None).unwrap();
    assert_eq!(
        prepared.dependencies,
        vec![("acme.gain".to_string(), "1.0.0".to_string())]
    );
    let paths: Vec<&str> = prepared.files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths[0], "package.json");
    assert_eq!(paths[1], "Chain.pwnode");
    assert!(paths.contains(&"deps/acme.gain/1.0.0/package.json"));
    assert!(paths.contains(&"deps/acme.gain/1.0.0/Gain.pwnode"));
    let elsewhere = f.tmp.0.join("other-library");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let installed = f.install(&elsewhere, &prepared);
    assert!(
        installed
            .dir
            .join("deps/acme.gain/1.0.0/Gain.pwnode")
            .is_file()
    );

    // A dependency published only on this computer is refused: its id
    // would have to be changed inside the graph.
    let own = f.leaf("Own", 0.1);
    let source = composite(&f.tmp.0.join("src2"), "Mix", &[child(&own)]);
    let mix = f.published(&source, "Mix", "1.0.0");
    let refused = f.prepare(&mix, &share("acme", "mix"), None).unwrap_err();
    assert!(refused.contains("not public"), "{refused}");
}

#[test]
fn secrets_restricted_nodes_and_native_code_are_never_shared() {
    let pwnode = |node_type: &str| {
        (
            "Node.pwnode".to_string(),
            format!(
                r#"{{"version":1,"name":"N","graph":{{"nodes":{{"1":{{"id":1,"pos":[0.0,0.0],"node_type":{node_type}}}}},"connections":[],"next_id":2}}}}"#
            ),
        )
    };
    let manifest = ("package.json".to_string(), "{}".to_string());
    assert!(check_contents(&[manifest.clone(), pwnode(r#""FileMenu""#)]).is_ok());
    // A credential anywhere (here a stored key in a label).
    let secret = pwnode(
        r#"{"Slider":{"value":0.5,"min":0.0,"max":1.0,"step":0.01,"slider_color":[0,0,0],"label":"AKIAABCDEFGHIJKLMNOP"}}"#,
    );
    let refused = check_contents(&[manifest.clone(), secret]).unwrap_err();
    assert!(
        !refused.contains("AKIAABCDEFGHIJKLMNOP"),
        "never echoes the value"
    );
    // Restricted and native node types.
    for node_type in [
        r#"{"RustPlugin":{}}"#,
        r#"{"ClapPlugin":{}}"#,
        r#""McpServer""#,
    ] {
        assert!(
            check_contents(&[manifest.clone(), pwnode(node_type)]).is_err(),
            "{node_type}"
        );
    }
    // Unknown node types fail closed.
    assert!(check_contents(&[manifest, pwnode(r#""NoSuchNode""#)]).is_err());
}

#[test]
fn publisher_namespaces_follow_the_ownership_rules() {
    let f = fx("submit_namespace");
    let local = f.leaf("Gain", 0.25);
    for (namespace, name) in [
        ("local", "gain"),
        ("patchwork", "gain"),
        ("test", "gain"),
        ("Acme", "gain"),
        ("a", "gain"),
        ("acme", "Gain"),
        ("acme", "my.gain"),
        ("-acme", "gain"),
    ] {
        assert!(
            f.prepare(&local, &share(namespace, name), None).is_err(),
            "{namespace}.{name}"
        );
    }
    // The maintainers' namespace only for the catalogue owner's account.
    let maintainers = CatalogueLocation::new("patchwork", "catalogue", "main").unwrap();
    assert!(
        prepare(
            &local.dir,
            &share("patchwork", "gain"),
            &f.store,
            &maintainers,
            None,
            &app()
        )
        .is_ok()
    );

    // A claimed namespace: ownership is stated, never assumed; the
    // repository checks the pull request's account.
    let published = f.prepare(&local, &share("acme", "gain"), None).unwrap();
    let mut catalogue = listing_catalogue(&published, &f.location, &"a".repeat(40)).unwrap();
    catalogue.packages.clear();
    let ok = f
        .prepare(&local, &share("acme", "gain"), Some(&catalogue))
        .unwrap();
    assert!(
        ok.notes
            .iter()
            .any(|n| n.contains("belongs to the GitHub account @acme"))
    );
    let unclaimed = f
        .prepare(&local, &share("bob", "gain"), Some(&catalogue))
        .unwrap();
    assert!(
        unclaimed
            .notes
            .iter()
            .any(|n| n.contains("not claimed yet"))
    );
    // A look-alike of a claimed namespace is refused.
    let mut lookalike = catalogue.clone();
    let mut publisher = lookalike.publishers["acme"].clone();
    publisher.namespace = "ac-me".into();
    lookalike.publishers.insert("ac-me".into(), publisher);
    lookalike.publishers.remove("acme");
    assert!(
        f.prepare(&local, &share("acme", "gain"), Some(&lookalike))
            .is_err()
    );
}

#[test]
fn published_versions_are_immutable_and_look_alikes_are_refused() {
    let f = fx("submit_conflicts");
    let local = f.leaf("Gain", 0.25);
    let first = f.prepare(&local, &share("acme", "gain"), None).unwrap();
    let catalogue = listing_catalogue(&first, &f.location, &"a".repeat(40)).unwrap();
    // The same id@version is already published (whatever its content).
    let refused = f
        .prepare(&local, &share("acme", "gain"), Some(&catalogue))
        .unwrap_err();
    assert!(refused.contains("already published"), "{refused}");
    // A new version of the same package is fine.
    let source = leaf(&f.tmp.0.join("src"), "Gain", 0.75);
    let mut next_version = request("Gain", "1.1.0");
    next_version.existing_id = Some(local.manifest.id.clone());
    let (next, outcome) =
        crate::library::trust::publish_and_record(&f.store, &f.library, &source, &next_version)
            .unwrap();
    outcome.unwrap();
    let ok = f
        .prepare(&next, &share("acme", "gain"), Some(&catalogue))
        .unwrap();
    assert!(ok.notes.iter().any(|n| n.contains("new version")));
    // A name that only differs by separators is refused.
    let refused = f
        .prepare(&local, &share("acme", "ga-in"), Some(&catalogue))
        .unwrap_err();
    assert!(refused.contains("looks like"), "{refused}");
}

#[test]
fn only_unchanged_versions_published_here_can_be_shared() {
    let f = fx("submit_source");
    // A loose custom-node file is not a published version.
    let loose = leaf(&f.tmp.0.join("loose"), "Loose", 0.5);
    assert!(
        prepare(
            loose.parent().unwrap(),
            &share("acme", "loose"),
            &f.store,
            &f.location,
            None,
            &app()
        )
        .is_err()
    );
    // Published without a record of this computer (copied in by hand).
    let source = leaf(&f.tmp.0.join("src"), "Copied", 0.5);
    let copied =
        crate::library::package::publish_package(&f.library, &source, &request("Copied", "1.0.0"))
            .unwrap();
    let refused = f
        .prepare(&copied, &share("acme", "copied"), None)
        .unwrap_err();
    assert!(refused.contains("not recorded"), "{refused}");
    // Changed after publishing: verification fails.
    let local = f.leaf("Gain", 0.25);
    let mut text = std::fs::read_to_string(&local.entry_path).unwrap();
    text = text.replace("0.25", "0.35");
    std::fs::write(&local.entry_path, text).unwrap();
    assert!(f.prepare(&local, &share("acme", "gain"), None).is_err());
    // An extra file in the version folder is never shared.
    let other = f.leaf("Other", 0.25);
    std::fs::write(other.dir.join("notes.txt"), "private").unwrap();
    assert!(f.prepare(&other, &share("acme", "other"), None).is_err());
}

#[test]
fn a_failed_or_cancelled_export_leaves_nothing_and_never_replaces() {
    let f = fx("submit_export_failure");
    let local = f.leaf("Gain", 0.25);
    let prepared = f.prepare(&local, &share("acme", "gain"), None).unwrap();
    let out = f.tmp.0.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let nothing_left = || std::fs::read_dir(&out).unwrap().next().is_none();

    for point in [
        FailPoint::Writing(0),
        FailPoint::Writing(3),
        FailPoint::BeforeCommit,
    ] {
        FAIL_AT.with(|fail| fail.set(Some(point)));
        let result = write_submission(&prepared, &f.location, &out, &AtomicBool::new(false));
        FAIL_AT.with(|fail| fail.set(None));
        assert!(result.is_err(), "{point:?}");
        assert!(nothing_left(), "no partial submission after {point:?}");
    }
    let cancelled = write_submission(&prepared, &f.location, &out, &AtomicBool::new(true));
    assert!(cancelled.unwrap_err().contains("cancelled"));
    assert!(nothing_left());

    // An existing submission is never replaced.
    let folder = write_submission(&prepared, &f.location, &out, &AtomicBool::new(false)).unwrap();
    std::fs::write(folder.join("SUBMISSION.md"), "edited by the user").unwrap();
    assert!(write_submission(&prepared, &f.location, &out, &AtomicBool::new(false)).is_err());
    assert_eq!(
        std::fs::read_to_string(folder.join("SUBMISSION.md")).unwrap(),
        "edited by the user"
    );
}

#[test]
fn a_submission_that_differs_from_its_listing_is_refused() {
    let f = fx("submit_tamper");
    let local = f.leaf("Gain", 0.25);
    let prepared = f.prepare(&local, &share("acme", "gain"), None).unwrap();
    // Artifact bytes changed after validation.
    let mut changed = prepared.clone();
    changed.artifact.push(b' ');
    assert!(validate(&changed, &f.location, None, &app()).is_err());
    // Files changed after the artifact was built.
    let mut fewer = prepared.clone();
    fewer.files[1].1.push(' ');
    assert!(validate(&fewer, &f.location, None, &app()).is_err());
    // A minimum Patchwork newer than this one: not installable here.
    let mut newer = share("acme", "gain");
    newer.min_patchwork = semver::Version::new(999, 0, 0);
    assert!(f.prepare(&local, &newer, None).is_err());
}
