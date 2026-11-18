//! P4.3.0: catalogue contract tests.

use super::*;
use crate::library::package::{PackageDependency, PackageManifest};
use serde_json::{Value, json};

const EXAMPLE: &str = include_str!("../../../docs/marketplace/index.example.json");
const COMMIT: &str = "3f2a9c1e7b4d5a6f8e0c2b1a9d7e6f5c4b3a2918";

fn source() -> CatalogueSource {
    CatalogueSource::new("patchwork-nodes", "catalogue").unwrap()
}

fn digest(n: u8) -> String {
    format!("{:02x}", n.max(1)).repeat(32)
}

fn artifact_url(id: &str, version: &str) -> String {
    format!(
        "https://raw.githubusercontent.com/patchwork-nodes/catalogue/{COMMIT}/artifacts/{id}/{version}/{id}-{version}.pwpkg"
    )
}

fn version(id: &str, version_text: &str, n: u8) -> Value {
    json!({
        "version": version_text,
        "entry": "Gain.pwnode",
        "manifest_format": 1,
        "pwnode_version": 1,
        "min_patchwork": "0.0.1",
        "artifact": {
            "format": 1,
            "url": artifact_url(id, version_text),
            "sha256": digest(n),
            "size": 1024,
            "files": 2
        }
    })
}

/// One publisher (`acme`) with one package (`acme.gain` 1.0.0).
fn minimal() -> Value {
    json!({
        "kind": "patchwork-catalogue",
        "catalogue_format": 1,
        "publishers": [
            {"namespace": "acme", "github": "acme", "github_id": 7, "display_name": "Acme"}
        ],
        "packages": [
            {
                "id": "acme.gain",
                "name": "Gain",
                "author": "Acme",
                "versions": [version("acme.gain", "1.0.0", 1)]
            }
        ]
    })
}

fn parse(value: &Value) -> Result<Catalogue, CatalogueError> {
    parse_catalogue(&serde_json::to_vec(value).unwrap(), &source())
}

/// Parsing fails and some issue mentions `needle`.
fn rejects(value: &Value, needle: &str) -> CatalogueError {
    let error = parse(value).expect_err("catalogue should be rejected");
    assert!(
        error.issues.iter().any(|i| i.contains(needle)),
        "expected an issue containing {needle:?}, got {:?}",
        error.issues
    );
    error
}

fn v1(value: &mut Value) -> &mut Value {
    &mut value["packages"][0]["versions"][0]
}

// ── Valid catalogues ────────────────────────────────────────────────────

#[test]
fn the_documented_example_is_valid() {
    let catalogue = parse_catalogue(EXAMPLE.as_bytes(), &source()).unwrap();
    assert_eq!(catalogue.publishers.len(), 2);
    assert_eq!(catalogue.packages.len(), 3);
    let gain = catalogue.package("acme-audio.gain").unwrap();
    assert_eq!(gain.namespace, "acme-audio");
    assert_eq!(gain.local_name, "gain");
    assert_eq!(gain.versions.len(), 3);

    let app = semver::Version::new(0, 0, 10);
    let yanked = &gain.versions[&semver::Version::new(1, 1, 0)];
    assert!(yanked.yanked.is_some());
    assert!(
        !yanked.is_installable(&app),
        "yanked versions are not offered"
    );
    let newer = &gain.versions[&semver::Version::new(2, 0, 0)];
    assert_eq!(
        newer.compatibility(&app),
        Compatibility::NeedsNewerPatchwork(semver::Version::new(0, 1, 0))
    );
    assert_eq!(
        newer.known_capabilities().into_iter().collect::<Vec<_>>(),
        vec![Capability::Network]
    );
    let installable: Vec<String> = gain
        .installable_versions(&app)
        .iter()
        .map(|v| v.version.to_string())
        .collect();
    assert_eq!(
        installable,
        vec!["1.2.0"],
        "exact versions; nothing implied"
    );

    let delay = catalogue.package("acme-audio.stereo-delay").unwrap();
    let delay_v1 = &delay.versions[&semver::Version::new(1, 0, 0)];
    assert_eq!(delay_v1.bundle_format, 1);
    assert_eq!(delay_v1.entry, "Stereo Delay.pwnode");
    assert!(delay_v1.preview.is_some());
    assert_eq!(delay_v1.artifact.commit, COMMIT);
    assert!(
        catalogue
            .package("patchwork.lfo")
            .unwrap()
            .versions
            .values()
            .all(|v| v.is_installable(&app))
    );
}

#[test]
fn a_minimal_catalogue_parses() {
    let catalogue = parse(&minimal()).unwrap();
    let package = catalogue.package("acme.gain").unwrap();
    assert_eq!(package.name, "Gain");
    let v = catalogue
        .version("acme.gain", &semver::Version::new(1, 0, 0))
        .unwrap();
    assert_eq!(v.artifact.sha256, digest(1));
    assert_eq!(
        v.compatibility(&current_app_version()),
        Compatibility::Compatible
    );
}

#[test]
fn display_names_are_not_identities() {
    let mut value = minimal();
    let mut other = value["packages"][0].clone();
    other["id"] = json!("acme.gain-pro");
    other["versions"] = json!([version("acme.gain-pro", "1.0.0", 2)]);
    value["packages"].as_array_mut().unwrap().push(other);
    let catalogue = parse(&value).unwrap();
    assert_eq!(
        catalogue.packages.len(),
        2,
        "same display name, two packages"
    );
}

#[test]
fn unknown_optional_fields_are_ignored() {
    let mut value = minimal();
    value["mirror_hint"] = json!({"nested": [1, 2, 3]});
    value["publishers"][0]["avatar"] = json!("https://example.com/a.png");
    value["packages"][0]["rating"] = json!(5);
    v1(&mut value)["changelog"] = json!("Faster.");
    v1(&mut value)["artifact"]["compression"] = json!("none");
    let catalogue = parse(&value).unwrap();
    assert_eq!(
        catalogue,
        parse(&minimal()).unwrap(),
        "no effect on the model"
    );
}

// ── Schema ──────────────────────────────────────────────────────────────

#[test]
fn invalid_schema_versions_are_rejected() {
    let mut value = minimal();
    value["catalogue_format"] = json!(2);
    let error = parse(&value).unwrap_err();
    assert_eq!(error.kind, CatalogueErrorKind::UnsupportedFormat(2));
    assert!(error.issues[0].contains("Update Patchwork"));

    for bad in [json!(0), json!("1"), json!(1.5), json!(-1), json!(null)] {
        let mut value = minimal();
        value["catalogue_format"] = bad.clone();
        let error = parse(&value).unwrap_err();
        assert_eq!(error.kind, CatalogueErrorKind::Malformed, "{bad}");
    }
    let mut value = minimal();
    value.as_object_mut().unwrap().remove("catalogue_format");
    assert_eq!(
        parse(&value).unwrap_err().kind,
        CatalogueErrorKind::Malformed
    );

    let mut value = minimal();
    value["kind"] = json!("npm-registry");
    rejects(&value, "not a Patchwork catalogue");
}

#[test]
fn missing_required_fields_are_rejected() {
    let cases: [(&str, fn(&mut Value)); 7] = [
        ("id", |v| {
            v["packages"][0].as_object_mut().unwrap().remove("id");
        }),
        ("versions", |v| {
            v["packages"][0].as_object_mut().unwrap().remove("versions");
        }),
        ("author", |v| {
            v["packages"][0].as_object_mut().unwrap().remove("author");
        }),
        ("entry", |v| {
            v1(v).as_object_mut().unwrap().remove("entry");
        }),
        ("min_patchwork", |v| {
            v1(v).as_object_mut().unwrap().remove("min_patchwork");
        }),
        ("sha256", |v| {
            v1(v)["artifact"].as_object_mut().unwrap().remove("sha256");
        }),
        ("github_id", |v| {
            v["publishers"][0]
                .as_object_mut()
                .unwrap()
                .remove("github_id");
        }),
    ];
    for (field, remove) in cases {
        let mut value = minimal();
        remove(&mut value);
        let error = rejects(&value, field);
        assert_eq!(error.kind, CatalogueErrorKind::Malformed, "{field}");
    }

    let mut value = minimal();
    value["packages"][0]["versions"] = json!([]);
    rejects(&value, "versions: is empty");
    let mut value = minimal();
    value["packages"][0]["name"] = json!("   ");
    rejects(&value, "name: is empty");
}

// ── Identity ────────────────────────────────────────────────────────────

#[test]
fn namespaces_and_ids_are_validated() {
    assert_eq!(split_public_id("acme.gain").unwrap(), ("acme", "gain"));
    assert_eq!(
        split_public_id("acme-audio.stereo_delay").unwrap(),
        ("acme-audio", "stereo_delay")
    );
    for bad in [
        "gain",
        "local.gain",
        "acme.gain.extra",
        "Acme.gain",
        "a.gain",
        "-acme.gain",
        "acme--x.gain",
        "acme.",
        "acme.gain-",
        "acme._gain",
        "acme.ga in",
    ] {
        assert!(split_public_id(bad).is_err(), "{bad} should be rejected");
    }

    let src = source();
    assert!(validate_publisher_claim("acme-audio", "Acme-Audio", &src).is_ok());
    assert!(validate_publisher_claim("acme", "someone-else", &src).is_err());
    assert!(validate_publisher_claim("local", "local", &src).is_err());
    assert!(validate_publisher_claim("test", "test", &src).is_err());
    assert!(validate_publisher_claim("patchwork", "patchwork-nodes", &src).is_ok());
    assert!(validate_publisher_claim("patchwork", "patchwork", &src).is_err());
}

#[test]
fn catalogue_ownership_rules_are_enforced() {
    // Package in an unclaimed namespace.
    let mut value = minimal();
    value["packages"][0]["id"] = json!("other.gain");
    v1(&mut value)["artifact"]["url"] = json!(artifact_url("other.gain", "1.0.0"));
    rejects(&value, "not claimed by any publisher");

    // Namespace that is not the owner's login.
    let mut value = minimal();
    value["publishers"][0]["github"] = json!("mallory");
    rejects(&value, "must be the owner's GitHub login");

    // Local ids are never public.
    let mut value = minimal();
    value["packages"][0]["id"] = json!("local.gain");
    rejects(&value, "reserved for packages published on this machine");

    // One account, one namespace; look-alike namespaces and names.
    let mut value = minimal();
    value["publishers"].as_array_mut().unwrap().push(
        json!({"namespace": "acme2", "github": "acme2", "github_id": 7, "display_name": "Acme 2"}),
    );
    rejects(&value, "already owns namespace");
    let mut value = minimal();
    value["publishers"][0] =
        json!({"namespace": "acme-io", "github": "acme-io", "github_id": 7, "display_name": "A"});
    value["publishers"].as_array_mut().unwrap().push(
        json!({"namespace": "acmeio", "github": "acmeio", "github_id": 8, "display_name": "B"}),
    );
    value["packages"] = json!([]);
    rejects(&value, "looks like 'acme-io'");
    let mut value = minimal();
    let mut twin = value["packages"][0].clone();
    twin["id"] = json!("acme.ga-in");
    twin["versions"] = json!([version("acme.ga-in", "1.0.0", 2)]);
    value["packages"].as_array_mut().unwrap().push(twin);
    rejects(&value, "looks like 'acme.gain'");
}

// ── Duplicates ──────────────────────────────────────────────────────────

#[test]
fn identical_duplicates_are_merged() {
    let mut value = minimal();
    let package = value["packages"][0].clone();
    value["packages"].as_array_mut().unwrap().push(package);
    let publisher = value["publishers"][0].clone();
    value["publishers"].as_array_mut().unwrap().push(publisher);
    let again = v1(&mut value).clone();
    value["packages"][0]["versions"]
        .as_array_mut()
        .unwrap()
        .push(again);
    let catalogue = parse(&value).unwrap();
    assert_eq!(catalogue, parse(&minimal()).unwrap());

    // Same package listed twice with different versions: merged.
    let mut value = minimal();
    let mut more = value["packages"][0].clone();
    more["versions"] = json!([version("acme.gain", "1.1.0", 2)]);
    value["packages"].as_array_mut().unwrap().push(more);
    assert_eq!(
        parse(&value).unwrap().packages["acme.gain"].versions.len(),
        2
    );
}

#[test]
fn conflicting_duplicates_are_rejected() {
    let mut value = minimal();
    let mut changed = v1(&mut value).clone();
    changed["artifact"]["sha256"] = json!(digest(9));
    value["packages"][0]["versions"]
        .as_array_mut()
        .unwrap()
        .push(changed);
    rejects(
        &value,
        "acme.gain@1.0.0 is listed twice with different metadata",
    );

    let mut value = minimal();
    let mut changed = value["packages"][0].clone();
    changed["description"] = json!("Something else");
    changed["versions"] = json!([version("acme.gain", "1.1.0", 2)]);
    value["packages"].as_array_mut().unwrap().push(changed);
    rejects(&value, "listed twice with different metadata");

    let mut value = minimal();
    let mut changed = value["publishers"][0].clone();
    changed["display_name"] = json!("Not Acme");
    value["publishers"].as_array_mut().unwrap().push(changed);
    rejects(&value, "claimed twice");

    // Two versions sharing one artifact digest.
    let mut value = minimal();
    value["packages"][0]["versions"]
        .as_array_mut()
        .unwrap()
        .push(version("acme.gain", "1.1.0", 1));
    rejects(&value, "same artifact digest");
}

// ── Versions, checksums, URLs and paths ─────────────────────────────────

#[test]
fn invalid_semantic_versions_are_rejected() {
    for bad in [
        "1.0",
        "1.0.0-beta.1",
        "1.0.0+build",
        "01.0.0",
        " 1.0.0",
        "v1.0.0",
        "1",
    ] {
        let mut value = minimal();
        v1(&mut value)["version"] = json!(bad);
        assert!(parse(&value).is_err(), "version {bad:?}");
    }
    for bad in ["latest", ">=0.0.1", "0.0", ""] {
        let mut value = minimal();
        v1(&mut value)["min_patchwork"] = json!(bad);
        rejects(&value, "min_patchwork");
    }
}

#[test]
fn artifact_checksums_must_be_sha256() {
    let upper = digest(0xab).to_uppercase();
    for bad in [
        upper.as_str(),
        &digest(1)[..63],
        "z".repeat(64).as_str(),
        &"0".repeat(64),
        "",
    ] {
        let mut value = minimal();
        v1(&mut value)["artifact"]["sha256"] = json!(bad);
        rejects(&value, "sha256");
    }
    // Limits are consistent with each other.
    assert!(limits::MAX_ARTIFACT_FILE_BYTES <= limits::MAX_ARTIFACT_BYTES);
    assert!(limits::MAX_ARTIFACT_BYTES as usize > limits::MAX_CATALOGUE_BYTES);

    let mut value = minimal();
    v1(&mut value)["artifact"]["size"] = json!(0);
    rejects(&value, "size");
    let mut value = minimal();
    v1(&mut value)["artifact"]["size"] = json!(limits::MAX_ARTIFACT_BYTES + 1);
    rejects(&value, "size");
    let mut value = minimal();
    v1(&mut value)["artifact"]["files"] = json!(limits::MAX_ARTIFACT_FILES + 1);
    rejects(&value, "files");
}

#[test]
fn unsafe_urls_are_rejected() {
    let good = artifact_url("acme.gain", "1.0.0");
    let base = format!(
        "raw.githubusercontent.com/patchwork-nodes/catalogue/{COMMIT}/artifacts/acme.gain/1.0.0"
    );
    let cases = [
        (good.replace("https://", "http://"), "https://"),
        (good.replace("https://", "https://user:pw@"), "credentials"),
        (good.replace(".com/", ".com:8443/"), "port"),
        (
            good.replace("raw.githubusercontent.com", "evil.example.com"),
            "hosted on",
        ),
        (
            good.replace("patchwork-nodes/catalogue", "mallory/catalogue"),
            "catalogue repository",
        ),
        (good.replace(COMMIT, "main"), "commit"),
        (good.replace(COMMIT, &COMMIT.to_uppercase()), "commit"),
        (
            format!("https://{base}/../../x/acme.gain-1.0.0.pwpkg"),
            "unsafe path",
        ),
        (
            format!("https://{base}/%2e%2e/acme.gain-1.0.0.pwpkg"),
            "percent",
        ),
        (format!("{good}?raw=1"), "query"),
        (format!("{good}#x"), "query"),
        (good.replace('/', "\\"), "backslash"),
        (
            format!("https://{base}/acme.gain-1.0.1.pwpkg"),
            "exactly this version",
        ),
        (
            good.replace(
                "acme.gain/1.0.0/acme.gain-1.0.0",
                "acme.gain/1.0.0/other.x-1.0.0",
            ),
            "exactly this version",
        ),
        (
            format!("https://{base}/acme.gain-1.0.0.pwpkg\u{202e}"),
            "printable ASCII",
        ),
        ("javascript:alert(1)".to_string(), "https://"),
        (
            format!("https://{}", "a".repeat(limits::MAX_URL_LEN)),
            "longer",
        ),
    ];
    for (url, needle) in cases {
        let mut value = minimal();
        v1(&mut value)["artifact"]["url"] = json!(url);
        rejects(&value, needle);
    }

    for bad in [
        "http://docs.example.com",
        "https://localhost/x",
        "https://1.2.3.4/x",
        "https://Docs.example.com",
    ] {
        let mut value = minimal();
        value["packages"][0]["documentation"] = json!(bad);
        rejects(&value, "documentation");
    }
    let mut value = minimal();
    value["packages"][0]["documentation"] = json!("https://docs.example.com/gain?tab=usage#ports");
    assert!(
        parse(&value).is_ok(),
        "documentation links may be any https page"
    );

    // Previews are downloaded, so they are pinned like artifacts.
    let preview = |path: &str| {
        json!({
            "url": format!("https://raw.githubusercontent.com/patchwork-nodes/catalogue/{COMMIT}/{path}"),
            "sha256": digest(5),
            "size": 2048
        })
    };
    let mut value = minimal();
    v1(&mut value)["preview"] = preview("packages/acme/gain/1.0.0/preview.png");
    assert!(parse(&value).is_ok());
    for bad in [
        "packages/acme/gain/1.0.0/preview.svg",
        "packages/acme/other/1.0.0/preview.png",
        "packages/acme/gain/2.0.0/preview.png",
        "artifacts/acme.gain/1.0.0/preview.png",
    ] {
        let mut value = minimal();
        v1(&mut value)["preview"] = preview(bad);
        rejects(&value, "preview.url");
    }
}

#[test]
fn unsafe_entry_names_are_rejected() {
    for bad in [
        "../Gain.pwnode",
        "sub/Gain.pwnode",
        "C:Gain.pwnode",
        ".Gain.pwnode",
        "Gain.exe",
        "CON.pwnode",
        "lpt1.pwnode",
        "Gäin.pwnode",
        " Gain.pwnode",
    ] {
        let mut value = minimal();
        v1(&mut value)["entry"] = json!(bad);
        rejects(&value, "entry");
    }
    assert!(validate_marketplace_entry("Stereo Delay.pwnode").is_ok());
}

#[test]
fn artifact_paths_follow_the_package_layout() {
    let entry = "Gain.pwnode";
    assert_eq!(
        validate_artifact_path("package.json", entry).unwrap(),
        ArtifactPath::Manifest
    );
    assert_eq!(
        validate_artifact_path(entry, entry).unwrap(),
        ArtifactPath::Entry
    );
    assert!(matches!(
        validate_artifact_path("deps/acme.filter/1.0.0/package.json", entry).unwrap(),
        ArtifactPath::DependencyManifest { .. }
    ));
    assert!(matches!(
        validate_artifact_path("deps/acme.filter/1.0.0/Filter.pwnode", entry).unwrap(),
        ArtifactPath::DependencyFile { .. }
    ));
    for bad in [
        "",
        "../package.json",
        "/package.json",
        "C:/package.json",
        "deps\\a\\1.0.0\\package.json",
        "./package.json",
        "deps/acme.filter/1.0.0/../../package.json",
        "deps/acme.filter/1.0/Filter.pwnode",
        "deps/ACME/1.0.0/Filter.pwnode",
        "deps/acme.filter/1.0.0/nested/Filter.pwnode",
        "deps/acme.filter/1.0.0/install.sh",
        "deps/acme.filter/1.0.0/NUL.pwnode",
        "Other.pwnode",
        "install.sh",
        "README.md",
    ] {
        assert!(validate_artifact_path(bad, entry).is_err(), "{bad:?}");
    }
    assert!(validate_artifact_path(&"a/".repeat(200), entry).is_err());

    assert!(check_artifact_listing(["package.json", entry], entry).is_ok());
    assert!(
        check_artifact_listing(
            [
                "package.json",
                entry,
                "deps/acme.filter/1.0.0/package.json",
                "deps/acme.filter/1.0.0/Filter.pwnode"
            ],
            entry
        )
        .is_ok()
    );
    assert!(
        check_artifact_listing(["package.json"], entry).is_err(),
        "no entry"
    );
    assert!(
        check_artifact_listing(["package.json", entry, "PACKAGE.json"], entry).is_err(),
        "case-insensitive duplicate"
    );
    assert!(
        check_artifact_listing(
            [
                "package.json",
                entry,
                "deps/acme.filter/1.0.0/Filter.pwnode"
            ],
            entry
        )
        .is_err(),
        "dependency without manifest"
    );
    let many: Vec<String> = (0..limits::MAX_ARTIFACT_FILES)
        .map(|i| format!("deps/acme.d{i}/1.0.0/package.json"))
        .collect();
    let listing = ["package.json", entry]
        .into_iter()
        .chain(many.iter().map(String::as_str));
    assert!(
        check_artifact_listing(listing, entry)
            .unwrap_err()
            .contains("more than")
    );
}

// ── Compatibility and capabilities ──────────────────────────────────────

#[test]
fn compatibility_requirements_are_reported_per_version() {
    let app = semver::Version::new(0, 0, 10);
    let check = |edit: fn(&mut Value)| {
        let mut value = minimal();
        edit(&mut value);
        let catalogue = parse(&value).expect("forward-compatible entries are accepted");
        catalogue.packages["acme.gain"].versions[&semver::Version::new(1, 0, 0)]
            .compatibility(&semver::Version::new(0, 0, 10))
    };
    assert_eq!(check(|_| {}), Compatibility::Compatible);
    assert_eq!(
        check(|v| v1(v)["min_patchwork"] = json!("0.2.0")),
        Compatibility::NeedsNewerPatchwork(semver::Version::new(0, 2, 0))
    );
    assert!(matches!(
        check(|v| v1(v)["bundle_format"] = json!(BUNDLE_FORMAT + 1)),
        Compatibility::UnsupportedFormat(_)
    ));
    assert!(matches!(
        check(|v| v1(v)["manifest_format"] = json!(2)),
        Compatibility::UnsupportedFormat(_)
    ));
    assert!(matches!(
        check(|v| v1(v)["pwnode_version"] = json!(2)),
        Compatibility::UnsupportedFormat(_)
    ));
    assert!(matches!(
        check(|v| v1(v)["artifact"]["format"] = json!(2)),
        Compatibility::UnsupportedFormat(_)
    ));
    assert_eq!(
        check(|v| v1(v)["capabilities"] = json!(["network", "teleport"])),
        Compatibility::UnknownCapabilities(vec!["teleport".to_string()])
    );
    let mut value = minimal();
    v1(&mut value)["min_patchwork"] = json!("0.2.0");
    let catalogue = parse(&value).unwrap();
    assert!(
        catalogue.packages["acme.gain"]
            .installable_versions(&app)
            .is_empty()
    );
}

#[test]
fn capabilities_are_validated() {
    let mut value = minimal();
    v1(&mut value)["capabilities"] = json!([
        "filesystem",
        "network",
        "devices",
        "media_capture",
        "scripting"
    ]);
    let catalogue = parse(&value).unwrap();
    let v = &catalogue.packages["acme.gain"].versions[&semver::Version::new(1, 0, 0)];
    assert_eq!(v.known_capabilities().len(), 5);

    for (bad, needle) in [
        (json!(["native_code"]), "native code"),
        (json!(["network", "network"]), "repeated"),
        (json!(["Network"]), "invalid"),
        (json!([""]), "invalid"),
    ] {
        let mut value = minimal();
        v1(&mut value)["capabilities"] = bad;
        rejects(&value, needle);
    }
    for capability in Capability::ALL {
        assert_eq!(Capability::parse(capability.as_str()), Some(capability));
    }
}

// ── Malicious and oversized input ───────────────────────────────────────

#[test]
fn malicious_and_oversized_catalogues_are_rejected() {
    let huge = vec![b' '; limits::MAX_CATALOGUE_BYTES + 1];
    assert_eq!(
        parse_catalogue(&huge, &source()).unwrap_err().kind,
        CatalogueErrorKind::TooLarge
    );
    assert_eq!(
        parse_catalogue(b"{\"kind\": \"\xff\"}", &source())
            .unwrap_err()
            .kind,
        CatalogueErrorKind::Malformed
    );
    assert_eq!(
        parse_catalogue(b"not json", &source()).unwrap_err().kind,
        CatalogueErrorKind::Malformed
    );

    // Deep nesting never overflows the stack: inside an ignored field it is
    // skipped (and has no effect); inside a field that is read it hits
    // serde's recursion limit.
    let nest = |inner: &str| format!("{}{inner}{}", "[".repeat(100_000), "]".repeat(100_000));
    let ignored = format!(
        "{{\"kind\":\"patchwork-catalogue\",\"catalogue_format\":1,\"x\":{}}}",
        nest("")
    );
    let catalogue = parse_catalogue(ignored.as_bytes(), &source()).unwrap();
    assert!(catalogue.packages.is_empty() && catalogue.publishers.is_empty());
    let read = format!("{{\"kind\":{},\"catalogue_format\":1}}", nest(""));
    assert_eq!(
        parse_catalogue(read.as_bytes(), &source())
            .unwrap_err()
            .kind,
        CatalogueErrorKind::Malformed
    );

    // Duplicate keys cannot smuggle a second value past review.
    let text = serde_json::to_string(&minimal()).unwrap().replacen(
        "\"id\":\"acme.gain\"",
        "\"id\":\"acme.gain\",\"id\":\"acme.evil\"",
        1,
    );
    assert_eq!(
        parse_catalogue(text.as_bytes(), &source())
            .unwrap_err()
            .kind,
        CatalogueErrorKind::Malformed
    );

    // Out-of-range numbers.
    for bad in [json!(-1), json!(1e30), json!(u64::MAX as f64)] {
        let mut value = minimal();
        v1(&mut value)["artifact"]["size"] = bad;
        assert!(parse(&value).is_err());
    }

    // Count and length limits.
    let mut value = minimal();
    value["packages"][0]["versions"] = Value::Array(
        (0..=limits::MAX_VERSIONS_PER_PACKAGE)
            .map(|i| version("acme.gain", &format!("1.0.{i}"), 1))
            .collect(),
    );
    rejects(&value, "more than");
    let mut value = minimal();
    value["packages"][0]["tags"] = Value::Array(
        (0..=limits::MAX_TAGS)
            .map(|i| json!(format!("t{i}")))
            .collect(),
    );
    rejects(&value, "tags");
    let mut value = minimal();
    value["packages"][0]["tags"] = json!(["Audio"]);
    rejects(&value, "tag 'Audio'");
    let mut value = minimal();
    value["packages"][0]["description"] = json!("x".repeat(limits::MAX_DESCRIPTION_LEN + 1));
    rejects(&value, "description");

    // Spoofing characters in display text.
    for bad in ["Gain\u{202e}", "Ga\u{200b}in", "Gain\nPro", "Gain\u{7}"] {
        let mut value = minimal();
        value["packages"][0]["name"] = json!(bad);
        rejects(&value, "name");
    }
    let mut value = minimal();
    value["packages"][0]["description"] = json!("Line one.\nLine two.");
    assert!(parse(&value).is_ok(), "descriptions may have newlines");

    // Reporting is bounded.
    let mut value = minimal();
    value["packages"][0]["versions"] = Value::Array(
        (0..limits::MAX_VERSIONS_PER_PACKAGE)
            .map(|i| {
                let mut v = version("acme.gain", &format!("1.0.{i}"), 1);
                v["artifact"]["sha256"] = json!("bad");
                v
            })
            .collect(),
    );
    assert_eq!(parse(&value).unwrap_err().issues.len(), limits::MAX_ISSUES);
}

#[test]
fn catalogue_source_is_validated() {
    assert!(CatalogueSource::new("patchwork-nodes", "catalogue").is_ok());
    assert!(CatalogueSource::new("-bad", "catalogue").is_err());
    assert!(CatalogueSource::new("ok", "..").is_err());
    assert!(CatalogueSource::new("ok", "a/b").is_err());
}

// ── Manifest consistency and refresh ────────────────────────────────────

fn manifest_for(v: &CatalogueVersion) -> PackageManifest {
    PackageManifest {
        format: v.manifest_format,
        id: "acme.gain".into(),
        name: "Gain".into(),
        version: v.version.to_string(),
        description: String::new(),
        author: "Acme".into(),
        tags: Vec::new(),
        entry: v.entry.clone(),
        sha256: digest(3),
        pwnode_version: v.pwnode_version,
        min_app_version: None,
        dependencies: Vec::new(),
        source: MARKETPLACE_SOURCE.into(),
        published_at: 0,
        files: Default::default(),
        bundle_format: v.bundle_format,
    }
}

#[test]
fn installed_manifest_must_match_the_catalogue() {
    let catalogue = parse(&minimal()).unwrap();
    let package = &catalogue.packages["acme.gain"];
    let v = &package.versions[&semver::Version::new(1, 0, 0)];
    assert!(check_manifest_matches(package, v, &manifest_for(v)).is_ok());

    let cases: [(&str, fn(&mut PackageManifest)); 6] = [
        ("id", |m| m.id = "acme.other".into()),
        ("version", |m| m.version = "1.0.1".into()),
        ("entry", |m| m.entry = "Other.pwnode".into()),
        ("source", |m| m.source = "local".into()),
        ("needs Patchwork", |m| {
            m.min_app_version = Some("9.0.0".into())
        }),
        ("not a complete bundle", |m| {
            m.dependencies = vec![PackageDependency {
                id: "acme.filter".into(),
                version: "1.0.0".into(),
                entry: None,
            }]
        }),
    ];
    for (needle, edit) in cases {
        let mut manifest = manifest_for(v);
        edit(&mut manifest);
        let error = check_manifest_matches(package, v, &manifest).unwrap_err();
        assert!(error.contains(needle), "{needle}: {error}");
    }

    // A complete bundle may depend on public packages only.
    let mut value = minimal();
    v1(&mut value)["bundle_format"] = json!(1);
    v1(&mut value)["artifact"]["files"] = json!(4);
    let catalogue = parse(&value).unwrap();
    let package = &catalogue.packages["acme.gain"];
    let v = &package.versions[&semver::Version::new(1, 0, 0)];
    let mut manifest = manifest_for(v);
    manifest.dependencies = vec![PackageDependency {
        id: "acme.filter".into(),
        version: "1.0.0".into(),
        entry: None,
    }];
    assert!(check_manifest_matches(package, v, &manifest).is_ok());
    manifest.dependencies[0].id = "local.filter".into();
    let error = check_manifest_matches(package, v, &manifest).unwrap_err();
    assert!(error.contains("non-public"), "{error}");
}

#[test]
fn refreshes_cannot_rewrite_published_versions() {
    let mut old_value = minimal();
    old_value["generated_at"] = json!(100);
    let old = parse(&old_value).unwrap();

    // Newer catalogue adding a version, re-pinned to another commit: fine.
    let mut next = old_value.clone();
    next["generated_at"] = json!(200);
    let other_commit = "1".repeat(40);
    v1(&mut next)["artifact"]["url"] =
        json!(artifact_url("acme.gain", "1.0.0").replace(COMMIT, &other_commit));
    next["packages"][0]["versions"]
        .as_array_mut()
        .unwrap()
        .push(version("acme.gain", "1.1.0", 2));
    assert!(check_refresh(&old, &parse(&next).unwrap()).is_ok());

    // Takedown: allowed.
    let mut removed = old_value.clone();
    removed["packages"] = json!([]);
    assert!(check_refresh(&old, &parse(&removed).unwrap()).is_ok());

    // Changed bytes for a published version.
    let mut rewritten = old_value.clone();
    v1(&mut rewritten)["artifact"]["sha256"] = json!(digest(9));
    let problems = check_refresh(&old, &parse(&rewritten).unwrap()).unwrap_err();
    assert!(problems[0].contains("immutable"));

    // Rollback to an older catalogue.
    let mut older = old_value.clone();
    older["generated_at"] = json!(50);
    assert!(check_refresh(&old, &parse(&older).unwrap()).is_err());

    // Namespace taken over by another account.
    let mut taken = old_value.clone();
    taken["publishers"][0]["github_id"] = json!(8);
    assert!(check_refresh(&old, &parse(&taken).unwrap()).is_err());
}
