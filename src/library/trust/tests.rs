//! P4.3.2C: content identity, trust store and migration tests.

use super::digest::{check_path, digest_of_files, read_tree};
use super::*;
use crate::library::bundle::tests::{child, composite, files_under, leaf, publish};
use crate::library::package::tests::{TempDir, request};
use crate::library::package::{MANIFEST_FILE, publish_package};

struct Fx {
    tmp: TempDir,
    library: PathBuf,
    store: TrustStore,
}

fn fx(tag: &str) -> Fx {
    let tmp = TempDir::new(tag);
    let library = tmp.0.join("library");
    std::fs::create_dir_all(&library).unwrap();
    let store = TrustStore::at(tmp.0.join("home").join(TRUST_DIR).join(TRUST_FILE));
    Fx {
        tmp,
        library,
        store,
    }
}

impl Fx {
    fn src(&self, name: &str) -> PathBuf {
        leaf(&self.tmp.0.join("src").join(name), name, 0.5)
    }

    /// Publish through the trusted workflow (records provenance).
    fn publish_recorded(&self, name: &str, version: &str) -> (VerifiedPackageAlias, TrustRecord) {
        let (package, outcome) = publish_and_record(
            &self.store,
            &self.library,
            &self.src(name),
            &request(name, version),
        )
        .unwrap();
        (package, outcome.unwrap())
    }

    /// Publish without recording (a package from before the trust store).
    fn publish_legacy(&self, name: &str, version: &str) -> VerifiedPackageAlias {
        publish_package(&self.library, &self.src(name), &request(name, version)).unwrap()
    }

    fn load(&self) -> StoreLoad {
        self.store.load()
    }

    fn store_bytes(&self) -> Option<Vec<u8>> {
        std::fs::read(&self.store.path).ok()
    }

    fn write_store(&self, text: &str) {
        std::fs::create_dir_all(self.store.path.parent().unwrap()).unwrap();
        std::fs::write(&self.store.path, text).unwrap();
    }
}

type VerifiedPackageAlias = crate::library::package::VerifiedPackage;

fn identity_of(dir: &Path) -> PackageIdentity {
    identify(dir, true).unwrap().identity
}

fn copy_dir(from: &Path, to: &Path) {
    for rel in files_under(from) {
        let target = rel.split('/').fold(to.to_path_buf(), |p, s| p.join(s));
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(
            rel.split('/').fold(from.to_path_buf(), |p, s| p.join(s)),
            &target,
        )
        .unwrap();
    }
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    files_under(dir)
        .into_iter()
        .map(|rel| {
            let bytes =
                std::fs::read(rel.split('/').fold(dir.to_path_buf(), |p, s| p.join(s))).unwrap();
            (rel, bytes)
        })
        .collect()
}

/// Rewrite one field of the only record in the store file.
fn edit_record(f: &Fx, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value = serde_json::from_slice(&f.store_bytes().unwrap()).unwrap();
    edit(&mut value["records"][0]);
    f.write_store(&serde_json::to_string_pretty(&value).unwrap());
}

// ── Content identity ─────────────────────────────────────────────────────

#[test]
fn tree_digests_are_stable_across_platforms() {
    // A fixed tree has a fixed digest: nothing depends on the system's
    // path separator, file order or file metadata.
    let files: [(&str, &[u8]); 3] = [
        ("package.json", b"{\"id\":\"acme.x\"}"),
        ("X.pwnode", b"{}"),
        ("deps/acme.y/1.0.0/Y.pwnode", b"[]"),
    ];
    let expected = digest_of_files(files).unwrap();
    assert_eq!(
        expected, GOLDEN_DIGEST,
        "the digest construction changed; that invalidates every trust record"
    );
    // Order of the input does not matter.
    let mut reversed = files;
    reversed.reverse();
    assert_eq!(digest_of_files(reversed).unwrap(), expected);
    // The same files on disk, created in another order, give the same digest.
    let tmp = TempDir::new("trust_digest_disk");
    for (path, bytes) in reversed {
        let target = path.split('/').fold(tmp.0.clone(), |p, s| p.join(s));
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, bytes).unwrap();
    }
    // Empty folders carry no content.
    std::fs::create_dir_all(tmp.0.join("empty")).unwrap();
    let tree = read_tree(&tmp.0).unwrap();
    assert_eq!(tree.digest, expected);
    assert_eq!(tree.files.len(), 3);
    assert_eq!(tree.files[0].path, "X.pwnode", "sorted by canonical bytes");
    assert_eq!(tree.graphs.len(), 2);
    assert!(tree.manifest.is_some());
}

/// The tree in `tree_digests_are_stable_across_platforms`, computed
/// independently (Python `hashlib`) from the construction in `digest.rs`.
const GOLDEN_DIGEST: &str = "c0114105c68acdb2ca53bb7fb9161c30489490a55a42172aa3d15583319bfc58";

#[test]
fn the_digest_input_is_unambiguous() {
    let d = |files: &[(&str, &[u8])]| digest_of_files(files.iter().copied()).unwrap();
    // Path/content boundaries cannot be shifted.
    assert_ne!(d(&[("ab", b"c")]), d(&[("a", b"bc")]));
    assert_ne!(d(&[("a", b""), ("b", b"")]), d(&[("a", b"b"), ("b", b"")]));
    // A file's folder is part of its identity.
    assert_ne!(d(&[("a/b", b"x")]), d(&[("b", b"x")]));
    assert_ne!(
        d(&[("x", b"1")]),
        d(&[("x", b"1"), ("y", b"")]),
        "an empty file counts"
    );
    assert!(
        digest_of_files(std::iter::empty()).is_err(),
        "nothing to identify"
    );
}

#[test]
fn any_change_to_a_package_changes_its_digest() {
    let f = fx("trust_change");
    let base = f.publish_legacy("Gain", "1.0.0");
    let original = read_tree(&base.dir).unwrap().digest;
    let variants: Vec<Box<dyn Fn(&Path)>> = vec![
        // One byte of the entry.
        Box::new(|dir| {
            let path = dir.join("Gain.pwnode");
            let mut bytes = std::fs::read(&path).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
            std::fs::write(path, bytes).unwrap();
        }),
        // The manifest (not covered by its own checksum field).
        Box::new(|dir| {
            let path = dir.join(MANIFEST_FILE);
            let text = std::fs::read_to_string(&path).unwrap();
            std::fs::write(path, text.replacen("\"name\"", " \"name\"", 1)).unwrap();
        }),
        // An added file.
        Box::new(|dir| std::fs::write(dir.join("notes.txt"), "").unwrap()),
        // A renamed file.
        Box::new(|dir| std::fs::rename(dir.join("Gain.pwnode"), dir.join("Gain2.pwnode")).unwrap()),
        // A removed file.
        Box::new(|dir| std::fs::remove_file(dir.join("Gain.pwnode")).unwrap()),
        // A file moved into a folder.
        Box::new(|dir| {
            std::fs::create_dir(dir.join("sub")).unwrap();
            std::fs::rename(dir.join("Gain.pwnode"), dir.join("sub").join("Gain.pwnode")).unwrap();
        }),
    ];
    for (index, change) in variants.iter().enumerate() {
        let copy = f.tmp.0.join(format!("copy{index}"));
        copy_dir(&base.dir, &copy);
        assert_eq!(
            read_tree(&copy).unwrap().digest,
            original,
            "a faithful copy"
        );
        change(&copy);
        assert_ne!(
            read_tree(&copy).unwrap().digest,
            original,
            "variant {index}"
        );
    }
}

#[test]
fn unsafe_and_ambiguous_paths_are_refused() {
    for bad in [
        "",
        "..",
        ".",
        "a/../b",
        "a//b",
        "/a",
        "a/",
        "a\\b",
        "C:",
        "c:x",
        "a:b",
        "con",
        "CON.pwnode",
        "nul.txt",
        "lpt1",
        "a ",
        " a",
        "a.",
        "caf\u{e9}.pwnode",
        "a\u{301}",
        "a\nb",
        "a\0b",
        "a*b",
        "a?b",
        "a|b",
        "a<b",
        "a\"b",
    ] {
        assert!(check_path(bad).is_err(), "{bad:?}");
    }
    for good in [
        "package.json",
        "Gain.pwnode",
        "deps/acme.x/1.0.0+b.1/X-1_a.pwnode",
    ] {
        assert!(check_path(good).is_ok(), "{good}");
    }
    // Case-only differences collide on Windows and macOS.
    assert!(digest_of_files([("A.pwnode", &b"1"[..]), ("a.pwnode", &b"2"[..])]).is_err());
    assert!(digest_of_files([("Deps/x", &b"1"[..]), ("deps/y", &b"2"[..])]).is_err());
    // The same path twice, or a path that is both a file and a folder.
    assert!(digest_of_files([("a", &b"1"[..]), ("a", &b"1"[..])]).is_err());
    assert!(digest_of_files([("a", &b"1"[..]), ("a/b", &b"2"[..])]).is_err());
    // On disk too.
    let tmp = TempDir::new("trust_paths");
    std::fs::write(tmp.0.join("ok.txt"), "x").unwrap();
    std::fs::write(tmp.0.join("bad."), "x").ok(); // Windows strips the dot
    let has_bad = std::fs::read_dir(&tmp.0)
        .unwrap()
        .any(|e| e.unwrap().file_name() == "bad.");
    assert_eq!(read_tree(&tmp.0).is_err(), has_bad);
}

#[cfg(unix)]
#[test]
fn links_are_never_followed() {
    let tmp = TempDir::new("trust_links");
    std::fs::write(tmp.0.join("real.txt"), "x").unwrap();
    std::os::unix::fs::symlink(tmp.0.join("real.txt"), tmp.0.join("link.txt")).unwrap();
    let error = read_tree(&tmp.0).unwrap_err();
    assert!(error.contains("link"), "{error}");
}

// ── Recording through trusted workflows ─────────────────────────────────

#[test]
fn local_publishing_records_the_exact_content() {
    let f = fx("trust_publish");
    assert_eq!(f.load(), StoreLoad::Missing);
    let (package, record) = f.publish_recorded("Gain", "1.0.0");
    assert_eq!(record.provenance, Provenance::LocalPublish);
    assert_eq!(record.identity, identity_of(&package.dir));
    assert_eq!(
        record.identity.tree_sha256,
        read_tree(&package.dir).unwrap().digest
    );
    assert_eq!(record.identity.id, "local.gain");
    assert_eq!(record.grant, Grant::NotGranted, "publishing grants nothing");
    assert_eq!(record.classification_version, CLASSIFICATION_VERSION);
    assert_eq!(record.declared, None);
    assert!(record.detected.is_empty(), "a slider uses nothing");
    let status = f.load().status(&record.identity);
    assert_eq!(status, TrustStatus::Recorded(record.clone()));
    assert!(status.has_verified_provenance());

    // A package with restricted nodes is still described honestly.
    let tmp = f.tmp.0.join("src").join("Term");
    std::fs::create_dir_all(&tmp).unwrap();
    let source = leaf(&tmp, "Term", 0.1);
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&source).unwrap()).unwrap();
    value["graph"]["nodes"]["77"] = serde_json::json!({
        "id": 77, "pos": [0.0, 0.0],
        "node_type": {"Dynamic": {"inner": {"type_tag": "terminal"}}}
    });
    value["graph"]["nodes"]["78"] = serde_json::json!({
        "id": 78, "pos": [0.0, 0.0],
        "node_type": {"Dynamic": {"inner": {"type_tag": "from_the_future"}}}
    });
    std::fs::write(&source, serde_json::to_string(&value).unwrap()).unwrap();
    let (_, outcome) =
        publish_and_record(&f.store, &f.library, &source, &request("Term", "1.0.0")).unwrap();
    let record = outcome.unwrap();
    assert!(record.restricted.contains("terminal"));
    assert!(record.unclassified.contains("from_the_future"));
}

#[test]
fn records_match_exact_id_version_and_content() {
    let f = fx("trust_exact");
    let (_, record) = f.publish_recorded("Gain", "1.0.0");
    let load = f.load();
    let mut other = record.identity.clone();
    other.version = semver::Version::new(1, 0, 1);
    assert_eq!(
        load.status(&other),
        TrustStatus::Unrecorded,
        "another version"
    );
    let mut other = record.identity.clone();
    other.id = "local.gain2".into();
    assert_eq!(load.status(&other), TrustStatus::Unrecorded, "another id");
    let mut other = record.identity.clone();
    other.id = "Local.Gain".into();
    assert_eq!(
        load.status(&other),
        TrustStatus::Unrecorded,
        "ids are exact"
    );
    let mut other = record.identity.clone();
    other.tree_sha256 = "0".repeat(64);
    assert_eq!(
        load.status(&other),
        TrustStatus::ContentChanged,
        "other content"
    );
    assert!(!load.status(&other).has_verified_provenance());
    let mut build = record.identity.clone();
    build.version = "1.0.0+other".parse().unwrap();
    assert_eq!(
        load.status(&build),
        TrustStatus::Unrecorded,
        "build metadata counts"
    );
}

#[test]
fn manifest_claims_and_local_ids_are_never_provenance() {
    let f = fx("trust_forged");
    // A package copied into the Library by hand, claiming to be both
    // locally published (`local.*`) and from the marketplace.
    let other = TempDir::new("trust_forged_src");
    let source = leaf(&other.0.join("s"), "Gain", 0.5);
    let original =
        publish_package(&other.0.join("lib"), &source, &request("Gain", "1.0.0")).unwrap();
    let target = f.library.join("packages").join("local.gain").join("1.0.0");
    copy_dir(&original.dir, &target);
    let manifest_path = target.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path).unwrap();
    std::fs::write(
        &manifest_path,
        text.replace("\"source\": \"local\"", "\"source\": \"marketplace\""),
    )
    .unwrap();
    assert!(identify(&target, true).is_ok(), "a valid package");

    let items = review_library(&f.library, &f.load());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "local.gain");
    assert_eq!(items[0].claimed_source, "marketplace", "shown as a claim");
    assert_eq!(items[0].status, TrustStatus::Unrecorded);
    assert!(f.store_bytes().is_none(), "reviewing writes nothing");

    // A record for the real local.gain does not cover a forged copy with
    // the same id and version.
    let f2 = fx("trust_forged2");
    let (package, record) = f2.publish_recorded("Gain", "1.0.0");
    let forged = f2.tmp.0.join("forged").join("local.gain").join("1.0.0");
    copy_dir(&package.dir, &forged);
    let manifest_path = forged.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path).unwrap();
    std::fs::write(
        &manifest_path,
        text.replace("\"description\": \"\"", "\"description\": \"x\""),
    )
    .unwrap();
    let (identity, status) = status_of_dir(&f2.load(), &forged).unwrap();
    assert_eq!(identity.id, record.identity.id);
    assert_eq!(status, TrustStatus::ContentChanged);
}

// ── Store robustness ─────────────────────────────────────────────────────

#[test]
fn damaged_stores_trust_nothing_and_are_never_overwritten() {
    let f = fx("trust_damaged");
    let (_, record) = f.publish_recorded("Gain", "1.0.0");
    let good = String::from_utf8(f.store_bytes().unwrap()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&good).unwrap();
    let with = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut v = value.clone();
        edit(&mut v);
        serde_json::to_string(&v).unwrap()
    };
    let damaged: Vec<(&str, String)> = vec![
        ("truncated", good[..good.len() / 2].to_string()),
        ("truncated by one byte", good[..good.len() - 2].to_string()),
        ("empty", String::new()),
        ("garbage", "\u{0}\u{1}not json".to_string()),
        ("wrong schema", with(&|v| v["schema"] = "other".into())),
        ("newer format", with(&|v| v["format"] = 2.into())),
        (
            "unknown field",
            with(&|v| v["trust_everything"] = true.into()),
        ),
        (
            "unknown record field",
            with(&|v| v["records"][0]["trusted"] = true.into()),
        ),
        (
            "bad digest",
            with(&|v| v["records"][0]["tree_sha256"] = "abc".into()),
        ),
        (
            "uppercase digest",
            with(&|v| {
                let d = v["records"][0]["tree_sha256"]
                    .as_str()
                    .unwrap()
                    .to_ascii_uppercase();
                v["records"][0]["tree_sha256"] = d.into();
            }),
        ),
        ("bad id", with(&|v| v["records"][0]["id"] = "../x".into())),
        (
            "non-canonical version",
            with(&|v| v["records"][0]["version"] = "01.0.0".into()),
        ),
        (
            "unknown capability",
            with(&|v| v["records"][0]["detected_capabilities"] = serde_json::json!(["everything"])),
        ),
        (
            "unknown grant",
            with(&|v| v["records"][0]["grant"] = "always".into()),
        ),
        (
            "unknown provenance",
            with(&|v| {
                v["records"][0]["provenance"] = serde_json::json!({"kind": "trusted_friend"})
            }),
        ),
        (
            "future classification",
            with(&|v| {
                v["records"][0]["classification_version"] = (CLASSIFICATION_VERSION + 1).into()
            }),
        ),
        (
            "declared on a local record",
            with(&|v| v["records"][0]["declared_capabilities"] = serde_json::json!([])),
        ),
        (
            "bad tree format",
            with(&|v| v["records"][0]["tree_format"] = 9.into()),
        ),
        (
            "missing grant",
            with(&|v| {
                v["records"][0].as_object_mut().unwrap().remove("grant");
            }),
        ),
        (
            "records not a list",
            with(&|v| v["records"] = serde_json::json!({})),
        ),
    ];
    for (index, (what, text)) in damaged.into_iter().enumerate() {
        f.write_store(&text);
        let load = f.load();
        assert!(matches!(load, StoreLoad::Damaged(_)), "{what}: {load:?}");
        assert_eq!(
            load.status(&record.identity),
            TrustStatus::Unrecorded,
            "{what}"
        );
        assert!(
            f.store.set_legacy_review(LegacyReview::Declined).is_err(),
            "{what}"
        );
        let (_, outcome) = publish_and_record(
            &f.store,
            &f.library,
            &f.src(&format!("Other{index}")),
            &request(&format!("Other{index}"), "1.0.0"),
        )
        .unwrap();
        assert!(outcome.is_err(), "{what}: nothing is recorded");
        assert_eq!(
            f.store_bytes().unwrap(),
            text.as_bytes(),
            "{what}: left as it was"
        );
    }
    // A folder where the file should be.
    std::fs::remove_file(&f.store.path).unwrap();
    std::fs::create_dir(&f.store.path).unwrap();
    assert!(matches!(f.load(), StoreLoad::Damaged(_)));
    std::fs::remove_dir(&f.store.path).unwrap();

    // The user can move a damaged store aside and start again.
    f.write_store("{ truncated");
    assert!(
        TrustStore::at(f.tmp.0.join("none.json"))
            .move_damaged_aside()
            .is_err()
    );
    let aside = f.store.move_damaged_aside().unwrap();
    assert_eq!(
        std::fs::read(&aside).unwrap(),
        b"{ truncated",
        "kept for inspection"
    );
    assert_eq!(f.load(), StoreLoad::Missing);
    let (_, record) = f.publish_recorded("Fresh", "1.0.0");
    assert!(matches!(
        f.load().status(&record.identity),
        TrustStatus::Recorded(_)
    ));
    assert!(
        f.store.move_damaged_aside().is_err(),
        "a readable store stays"
    );
}

#[test]
fn duplicate_records_are_refused_and_updates_keep_one_record() {
    let f = fx("trust_duplicates");
    let (_, record) = f.publish_recorded("Gain", "1.0.0");
    let mut value: serde_json::Value = serde_json::from_slice(&f.store_bytes().unwrap()).unwrap();
    let first = value["records"][0].clone();
    value["records"].as_array_mut().unwrap().push(first.clone());
    f.write_store(&serde_json::to_string(&value).unwrap());
    assert!(matches!(f.load(), StoreLoad::Damaged(why) if why.contains("twice")));
    // Same id@version, different content: still a duplicate.
    let mut other = first.clone();
    other["tree_sha256"] = "1".repeat(64).into();
    value["records"] = serde_json::json!([first, other]);
    f.write_store(&serde_json::to_string(&value).unwrap());
    assert!(matches!(f.load(), StoreLoad::Damaged(_)));

    // Recording the same version again replaces its record.
    value["records"] = serde_json::json!([value["records"][0].clone()]);
    f.write_store(&serde_json::to_string(&value).unwrap());
    let dir = f.library.join("packages").join("local.gain").join("1.0.0");
    apply_review(&f.store, &dir, &record.identity, ReviewDecision::Deny).unwrap();
    let StoreLoad::Loaded(store) = f.load() else {
        panic!("readable");
    };
    assert_eq!(store.records().count(), 1);
    assert_eq!(
        store
            .record_for("local.gain", &record.identity.version)
            .unwrap()
            .grant,
        Grant::Denied
    );
}

#[test]
fn a_failed_update_leaves_the_store_unchanged() {
    let f = fx("trust_atomic");
    f.publish_recorded("Gain", "1.0.0");
    let before = f.store_bytes().unwrap();
    FAIL_BEFORE_RENAME.with(|flag| flag.set(true));
    let (_, outcome) = publish_and_record(
        &f.store,
        &f.library,
        &f.src("Other"),
        &request("Other", "1.0.0"),
    )
    .unwrap();
    let failed_legacy = f.store.set_legacy_review(LegacyReview::Completed);
    FAIL_BEFORE_RENAME.with(|flag| flag.set(false));
    assert!(outcome.unwrap_err().contains("simulated"));
    assert!(failed_legacy.is_err());
    assert_eq!(
        f.store_bytes().unwrap(),
        before,
        "the previous store is intact"
    );
    let leftovers: Vec<_> = std::fs::read_dir(f.store.path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        vec![TRUST_FILE.to_string()],
        "no temporary files"
    );
    let StoreLoad::Loaded(store) = f.load() else {
        panic!("readable");
    };
    assert_eq!(store.records().count(), 1);
    assert_eq!(store.legacy_review, LegacyReview::Pending);
}

#[cfg(unix)]
#[test]
fn the_store_is_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx("trust_mode");
    f.publish_recorded("Gain", "1.0.0");
    let mode = std::fs::metadata(&f.store.path)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let dir = std::fs::metadata(f.store.path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(dir & 0o777, 0o700);
}

#[test]
fn classification_changes_require_a_new_review() {
    let f = fx("trust_reclassify");
    let (package, record) = f.publish_recorded("Gain", "1.0.0");
    edit_record(&f, |r| {
        r["classification_version"] = 1.into();
        r["grant"] = "granted".into();
        r["approved_capabilities"] = serde_json::json!([]);
    });
    let status = f.load().status(&record.identity);
    assert!(matches!(status, TrustStatus::Reclassified(_)), "{status:?}");
    assert!(
        !status.has_verified_provenance(),
        "not relied on until re-checked"
    );
    let items = review_library(&f.library, &f.load());
    assert_eq!(items.len(), 1);
    assert!(matches!(items[0].status, TrustStatus::Reclassified(_)));

    let updated = f.store.reclassify(&package.dir).unwrap();
    assert_eq!(updated.provenance, Provenance::LocalPublish, "origin kept");
    assert_eq!(updated.classification_version, CLASSIFICATION_VERSION);
    assert_eq!(updated.grant, Grant::NotGranted, "a grant is asked again");
    assert!(f.load().status(&record.identity).has_verified_provenance());
    assert!(review_library(&f.library, &f.load()).is_empty());

    // A denial survives re-classification.
    edit_record(&f, |r| {
        r["classification_version"] = 1.into();
        r["grant"] = "denied".into();
    });
    assert_eq!(
        f.store.reclassify(&package.dir).unwrap().grant,
        Grant::Denied
    );
    // Only for the recorded content.
    std::fs::write(package.dir.join("extra.txt"), "x").unwrap();
    assert!(f.store.reclassify(&package.dir).is_err());
}

#[test]
fn receipts_refuse_content_other_than_what_was_verified() {
    let f = fx("trust_receipt");
    let package = f.publish_legacy("Gain", "1.0.0");
    let wrong = "0".repeat(64);
    assert!(
        ProvenanceReceipt::for_content(
            &package.dir,
            Some(&wrong),
            Provenance::LocalPublish,
            None,
            Grant::NotGranted
        )
        .is_err()
    );
    // A marketplace receipt cannot cover more than was declared.
    let declared = BTreeSet::new();
    let source = leaf(&f.tmp.0.join("net"), "Net", 0.1);
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&source).unwrap()).unwrap();
    value["graph"]["nodes"]["50"] = serde_json::json!({
        "id": 50, "pos": [0.0, 0.0], "node_type": {"Dynamic": {"inner": {"type_tag": "monitor"}}}
    });
    std::fs::write(&source, serde_json::to_string(&value).unwrap()).unwrap();
    let net = publish_package(&f.library, &source, &request("Net", "1.0.0")).unwrap();
    let origin = MarketplaceOrigin {
        location: CatalogueLocation::new("patchwork-nodes", "catalogue", "main").unwrap(),
        generated_at: Some(5),
        catalogue_sha256: "a".repeat(64),
    };
    let provenance =
        marketplace_provenance(&origin, "https://example.com/x.pwpkg", &"b".repeat(64));
    assert!(
        ProvenanceReceipt::for_content(
            &net.dir,
            None,
            provenance,
            Some(declared),
            Grant::NotGranted
        )
        .is_err()
    );
}

// ── Migration ────────────────────────────────────────────────────────────

#[test]
fn existing_packages_stay_untrusted_until_the_user_confirms_them() {
    let f = fx("trust_migrate");
    let legacy = f.publish_legacy("Gain", "1.0.0");
    let leaf_pkg = f.publish_legacy("Leaf", "1.0.0");
    let chain_src = composite(
        &f.tmp.0.join("src").join("Chain"),
        "Chain",
        &[child(&leaf_pkg)],
    );
    let chain = publish(&f.library, &chain_src, "Chain", None, "1.0.0").unwrap();
    let bytes_before = snapshot(&f.library);

    // Listed for review; nothing recorded, nothing written.
    let items = review_library(&f.library, &f.load());
    let labels: BTreeSet<String> = items
        .iter()
        .map(|i| format!("{}@{}", i.id, i.version))
        .collect();
    assert_eq!(
        labels,
        ["local.chain@1.0.0", "local.gain@1.0.0", "local.leaf@1.0.0"]
            .map(String::from)
            .into()
    );
    assert!(items.iter().all(|i| i.status == TrustStatus::Unrecorded));
    assert!(items.iter().all(|i| i.identity.is_ok()));
    assert!(f.store_bytes().is_none());

    // Deferring or declining records nothing about packages.
    f.store.set_legacy_review(LegacyReview::Declined).unwrap();
    assert_eq!(review_library(&f.library, &f.load()).len(), 3);
    let identity = identity_of(&legacy.dir);
    assert_eq!(f.load().status(&identity), TrustStatus::Unrecorded);

    // Accepting records exactly the reviewed content, as imported: the
    // origin stays unverified and nothing is granted.
    let record = apply_review(&f.store, &legacy.dir, &identity, ReviewDecision::Accept).unwrap();
    assert_eq!(
        record.provenance,
        Provenance::Imported {
            origin: ImportOrigin::LegacyLibrary
        }
    );
    assert_eq!(record.grant, Grant::NotGranted);
    let status = f.load().status(&identity);
    assert!(matches!(status, TrustStatus::Recorded(_)));
    assert!(
        !status.has_verified_provenance(),
        "never upgraded to local_publish"
    );

    // Denying is recorded too.
    let chain_identity = identity_of(&chain.dir);
    let denied = apply_review(&f.store, &chain.dir, &chain_identity, ReviewDecision::Deny).unwrap();
    assert_eq!(denied.grant, Grant::Denied);
    assert_eq!(
        review_library(&f.library, &f.load()).len(),
        1,
        "only Leaf left"
    );

    // A package changed after it was reviewed is not recorded.
    let leaf_identity = identity_of(&leaf_pkg.dir);
    let manifest = leaf_pkg.dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace("\"description\": \"\"", "\"description\": \"x\""),
    )
    .unwrap();
    assert!(
        apply_review(
            &f.store,
            &leaf_pkg.dir,
            &leaf_identity,
            ReviewDecision::Accept
        )
        .is_err()
    );
    std::fs::write(&manifest, text).unwrap();

    // Package bytes are never changed by the review.
    assert_eq!(snapshot(&f.library), bytes_before);
    let StoreLoad::Loaded(store) = f.load() else {
        panic!("readable");
    };
    assert_eq!(store.legacy_review, LegacyReview::Declined);
    assert_eq!(store.records().count(), 2);
}

#[test]
fn project_copies_match_only_with_identical_content() {
    let f = fx("trust_project");
    let (package, record) = f.publish_recorded("Gain", "1.0.0");
    let project = f.tmp.0.join("project");
    let copy = project.join("packages").join("local.gain").join("1.0.0");
    copy_dir(&package.dir, &copy);
    let (identity, status) = status_of_dir(&f.load(), &copy).unwrap();
    assert_eq!(identity, record.identity, "same content, same identity");
    assert_eq!(status, TrustStatus::Recorded(record.clone()));

    // A copy that differs (here: an extra file and an edited manifest
    // that still verify) does not inherit the record.
    std::fs::write(copy.join("readme.txt"), "hello").unwrap();
    let (identity, status) = status_of_dir(&f.load(), &copy).unwrap();
    assert_ne!(identity.tree_sha256, record.identity.tree_sha256);
    assert_eq!(status, TrustStatus::ContentChanged);

    // A project package that was never recorded has no provenance, even
    // inside a project that holds recorded ones.
    let other = TempDir::new("trust_project_other");
    let foreign = publish_package(
        &other.0.join("lib"),
        &leaf(&other.0.join("s"), "Mix", 0.2),
        &request("Mix", "1.0.0"),
    )
    .unwrap();
    let foreign_copy = project.join("packages").join("local.mix").join("1.0.0");
    copy_dir(&foreign.dir, &foreign_copy);
    assert_eq!(
        status_of_dir(&f.load(), &foreign_copy).unwrap().1,
        TrustStatus::Unrecorded
    );
    // A damaged copy cannot be identified at all.
    std::fs::write(foreign_copy.join("Mix.pwnode"), "{}").unwrap();
    assert!(status_of_dir(&f.load(), &foreign_copy).is_err());
}

// ── P4.3.2F: managing recorded grants ────────────────────────────────────

#[test]
fn recorded_grants_can_be_revoked_narrowed_and_denied_but_never_widened() {
    let f = fx("trust_manage");
    let package = f.publish_legacy("Mixer", "1.0.0");
    let identity = identity_of(&package.dir);
    // Nothing recorded: nothing to change.
    assert!(f.store.revoke_grant(&identity).is_err());
    let granted = f.store.grant(&package.dir, &identity).unwrap();
    assert_eq!(granted.grant, Grant::Granted);
    let approved = granted.approved.clone().unwrap();

    // Narrowing within the approved set; never beyond it.
    let mut wider = approved.clone();
    wider.insert(Capability::Network);
    wider.insert(Capability::Process);
    assert!(f.store.narrow_grant(&identity, wider).is_err());
    let narrowed = f.store.narrow_grant(&identity, BTreeSet::new()).unwrap();
    assert_eq!(narrowed.approved, Some(BTreeSet::new()));
    assert_eq!(narrowed.grant, Grant::Granted);

    // Revoked: no longer allowed (and offered for review again).
    let revoked = f.store.revoke_grant(&identity).unwrap();
    assert_eq!((revoked.grant, revoked.approved), (Grant::NotGranted, None));
    assert!(
        f.store.revoke_grant(&identity).is_err(),
        "nothing to revoke"
    );

    // Denied without its folder, then the denial lifted: that grants
    // nothing; it is offered for review again.
    std::fs::remove_dir_all(&package.dir).unwrap();
    assert_eq!(
        f.store.deny_recorded(&identity).unwrap().grant,
        Grant::Denied
    );
    let lifted = f.store.withdraw_denial(&identity).unwrap();
    assert_eq!((lifted.grant, lifted.approved), (Grant::NotGranted, None));
    assert!(f.store.withdraw_denial(&identity).is_err());

    // Another content of the same id@version is never changed.
    let other = PackageIdentity {
        tree_sha256: "f".repeat(64),
        ..identity.clone()
    };
    assert!(f.store.deny_recorded(&other).is_err());

    // A damaged store is never changed (or trusted).
    f.write_store("{ damaged");
    assert!(f.store.deny_recorded(&identity).is_err());
    assert_eq!(f.store_bytes().unwrap(), b"{ damaged");
}
