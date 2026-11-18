//! P4.2 Stage 3 tests: nested package dependencies.

use super::*;
use crate::document::{CustomNodeFile, CustomNodePort};
use crate::graph::{Graph, PortKind, PortValue};
use crate::library::PublishError;
use crate::library::package::tests::{TempDir, request};
use crate::library::package::{HASH_COUNT, PACKAGES_DIR, VerifiedPackage, publish_package};
use crate::library::resolve::{PackageResolver, ResolveErrorKind};

fn port(name: &str) -> CustomNodePort {
    CustomNodePort {
        name: name.into(),
        kind: "Number".into(),
    }
}

/// A leaf custom node whose single output is a constant `value`.
pub(crate) fn leaf(dir: &Path, name: &str, value: f32) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let mut graph = Graph::new();
    let slider = graph.add_node(
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
    let out = graph.add_node(
        NodeType::CustomOutput {
            name: "out".into(),
            kind: PortKind::Number,
        },
        [100.0, 0.0],
    );
    graph.add_connection(slider, 0, out, 0);
    let path = dir.join(format!("{name}.pwnode"));
    CustomNodeFile {
        version: 1,
        name: name.into(),
        graph,
        inputs: Vec::new(),
        outputs: vec![port("out")],
    }
    .save(&path)
    .unwrap();
    path
}

/// A custom node forwarding the first output of each child instance to
/// its own outputs `out0`, `out1`, ...
pub(crate) fn composite(
    dir: &Path,
    name: &str,
    children: &[(String, Option<InstancePackageRef>)],
) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let mut graph = Graph::new();
    let mut outputs = Vec::new();
    for (index, (file, package)) in children.iter().enumerate() {
        // The instance carries the child's real interface, as insertion does.
        let child_outputs = if file.is_empty() {
            vec![port("out")]
        } else {
            CustomNodeFile::load(Path::new(file)).unwrap().outputs
        };
        let instance = graph.add_node(
            NodeType::CustomNodeInstance {
                file: file.clone(),
                inputs: Vec::new(),
                outputs: child_outputs,
                package: package.clone(),
            },
            [0.0, index as f32 * 100.0],
        );
        let out = graph.add_node(
            NodeType::CustomOutput {
                name: format!("out{index}"),
                kind: PortKind::Number,
            },
            [200.0, index as f32 * 100.0],
        );
        graph.add_connection(instance, 0, out, 0);
        outputs.push(port(&format!("out{index}")));
    }
    let path = dir.join(format!("{name}.pwnode"));
    CustomNodeFile {
        version: 1,
        name: name.into(),
        graph,
        inputs: Vec::new(),
        outputs,
    }
    .save(&path)
    .unwrap();
    path
}

pub(crate) fn publish(
    library: &Path,
    source: &Path,
    name: &str,
    existing: Option<&str>,
    version: &str,
) -> Result<VerifiedPackage, PublishError> {
    let mut req = request(name, version);
    req.existing_id = existing.map(str::to_string);
    publish_package(library, source, &req)
}

pub(crate) fn stored(v: &VerifiedPackage) -> InstancePackageRef {
    InstancePackageRef {
        id: v.manifest.id.clone(),
        version: v.version.to_string(),
        entry: v.manifest.entry.clone(),
    }
}

/// `(file, package)` for a child instance of a published package.
pub(crate) fn child(v: &VerifiedPackage) -> (String, Option<InstancePackageRef>) {
    (v.entry_path.to_string_lossy().into_owned(), Some(stored(v)))
}

/// Evaluate one instance and return its outputs (`None` = not evaluated).
fn outputs_of(
    file: &str,
    package: Option<InstancePackageRef>,
    count: usize,
    library: &Path,
) -> Vec<Option<f32>> {
    let mut graph = Graph::new();
    let id = graph.add_node(
        NodeType::CustomNodeInstance {
            file: file.to_string(),
            inputs: Vec::new(),
            outputs: (0..count).map(|i| port(&format!("out{i}"))).collect(),
            package,
        },
        [0.0, 0.0],
    );
    graph.set_package_environment(None, Some(library.to_path_buf()));
    let values = graph.evaluate(0.0);
    (0..count)
        .map(|i| match values.get(&(id, i)) {
            Some(PortValue::Float(v)) => Some(*v),
            _ => None,
        })
        .collect()
}

fn published_outputs(v: &VerifiedPackage, count: usize, library: &Path) -> Vec<Option<f32>> {
    // Only the portable reference: no usable file path.
    outputs_of("", Some(stored(v)), count, library)
}

pub(crate) fn files_under(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(
                    p.strip_prefix(dir)
                        .unwrap()
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/"),
                );
            }
        }
    }
    out.sort();
    out
}

fn remove_package(library: &Path, id: &str) {
    std::fs::remove_dir_all(library.join(PACKAGES_DIR).join(id)).unwrap();
}

/// Hand-written package (bypasses the publisher) with packaged nested
/// instances and the given declared dependencies, for cycle/depth tests.
fn craft(
    library: &Path,
    id: &str,
    version: &str,
    nested: &[InstancePackageRef],
) -> InstancePackageRef {
    let dir = library.join(PACKAGES_DIR).join(id).join(version);
    std::fs::create_dir_all(&dir).unwrap();
    let mut graph = Graph::new();
    for reference in nested {
        graph.add_node(
            NodeType::CustomNodeInstance {
                file: String::new(),
                inputs: Vec::new(),
                outputs: vec![port("out")],
                package: Some(reference.clone()),
            },
            [0.0, 0.0],
        );
    }
    let entry = dir.join("Node.pwnode");
    let file = CustomNodeFile {
        version: 1,
        name: id.into(),
        graph,
        inputs: Vec::new(),
        outputs: vec![port("out")],
    };
    std::fs::write(&entry, serde_json::to_string_pretty(&file).unwrap()).unwrap();
    let manifest = PackageManifest {
        format: package::MANIFEST_FORMAT,
        id: id.into(),
        name: id.into(),
        version: version.into(),
        description: String::new(),
        author: String::new(),
        tags: Vec::new(),
        entry: "Node.pwnode".into(),
        sha256: package::sha256_file(&entry).unwrap(),
        pwnode_version: 1,
        min_app_version: None,
        dependencies: nested
            .iter()
            .map(|r| package::PackageDependency {
                id: r.id.clone(),
                version: r.version.clone(),
                entry: Some(r.entry.clone()),
            })
            .collect(),
        source: "local".into(),
        published_at: 0,
        files: Default::default(),
        bundle_format: 0,
    };
    std::fs::write(
        dir.join(MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    InstancePackageRef {
        id: id.into(),
        version: version.into(),
        entry: "Node.pwnode".into(),
    }
}

fn crafted_child(reference: &InstancePackageRef) -> (String, Option<InstancePackageRef>) {
    (String::new(), Some(reference.clone()))
}

fn invalid_message(result: Result<VerifiedPackage, PublishError>) -> String {
    match result {
        Err(PublishError::Invalid(message)) => message,
        other => panic!("expected a publish refusal, got {other:?}"),
    }
}

// ── Dependency graphs ────────────────────────────────────────────────────

#[test]
fn a_depends_on_b() {
    let tmp = TempDir::new("s3_ab");
    let library = tmp.0.join("library");
    let b = publish(
        &library,
        &leaf(&tmp.0.join("src"), "B", 0.25),
        "B",
        None,
        "1.0.0",
    )
    .unwrap();
    let b_files: Vec<Vec<u8>> = [MANIFEST_FILE, "B.pwnode"]
        .iter()
        .map(|n| std::fs::read(b.dir.join(n)).unwrap())
        .collect();
    let a_src = composite(&tmp.0.join("src"), "A", &[child(&b)]);
    let a = publish(&library, &a_src, "A", None, "1.0.0").unwrap();

    // Manifest: the dependency and a checksum per bundled file.
    assert_eq!(a.manifest.dependencies.len(), 1);
    assert_eq!(a.manifest.dependencies[0].id, b.manifest.id);
    assert_eq!(a.manifest.dependencies[0].version, "1.0.0");
    let rel = format!("deps/{}/1.0.0", b.manifest.id);
    assert_eq!(
        a.manifest.files.keys().cloned().collect::<Vec<_>>(),
        vec![format!("{rel}/B.pwnode"), format!("{rel}/{MANIFEST_FILE}")]
    );
    // Bundled copies are byte-identical to the published dependency.
    for (name, bytes) in [MANIFEST_FILE, "B.pwnode"].iter().zip(&b_files) {
        assert_eq!(&std::fs::read(a.dir.join(&rel).join(name)).unwrap(), bytes);
    }
    // The published entry names the exact package and the bundled copy.
    let entry = CustomNodeFile::load(&a.entry_path).unwrap();
    let nested: Vec<_> = entry
        .graph
        .nodes
        .values()
        .filter_map(|n| match &n.node_type {
            NodeType::CustomNodeInstance { file, package, .. } => {
                Some((file.clone(), package.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(nested.len(), 1);
    assert_eq!(nested[0].1, Some(stored(&b)));
    assert_eq!(
        PathBuf::from(&nested[0].0),
        a.dir.join(&rel).join("B.pwnode")
    );

    assert_eq!(published_outputs(&a, 1, &library), vec![Some(0.25)]);
    // Self-contained: works with B gone from the Library.
    remove_package(&library, &b.manifest.id);
    assert_eq!(published_outputs(&a, 1, &library), vec![Some(0.25)]);
}

#[test]
fn a_depends_on_b_which_depends_on_c() {
    let tmp = TempDir::new("s3_abc");
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    let c = publish(&library, &leaf(&src, "C", 0.5), "C", None, "1.0.0").unwrap();
    let b = publish(
        &library,
        &composite(&src, "B", &[child(&c)]),
        "B",
        None,
        "1.0.0",
    )
    .unwrap();
    let a = publish(
        &library,
        &composite(&src, "A", &[child(&b)]),
        "A",
        None,
        "1.0.0",
    )
    .unwrap();

    // Flat: B's own deps/ is not copied; C is hoisted next to B.
    let mut ids: Vec<String> = a
        .manifest
        .dependencies
        .iter()
        .map(|d| d.id.clone())
        .collect();
    ids.sort();
    let mut expected = vec![b.manifest.id.clone(), c.manifest.id.clone()];
    expected.sort();
    assert_eq!(ids, expected);
    assert!(
        !a.dir
            .join("deps")
            .join(&b.manifest.id)
            .join("1.0.0")
            .join("deps")
            .exists()
    );
    assert!(
        a.dir
            .join("deps")
            .join(&c.manifest.id)
            .join("1.0.0")
            .is_dir()
    );

    assert_eq!(published_outputs(&a, 1, &library), vec![Some(0.5)]);
    remove_package(&library, &b.manifest.id);
    remove_package(&library, &c.manifest.id);
    assert_eq!(
        published_outputs(&a, 1, &library),
        vec![Some(0.5)],
        "self-contained"
    );
}

#[test]
fn two_routes_share_one_bundled_dependency() {
    let tmp = TempDir::new("s3_shared");
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    let c = publish(&library, &leaf(&src, "C", 0.5), "C", None, "1.0.0").unwrap();
    let b = publish(
        &library,
        &composite(&src, "B", &[child(&c)]),
        "B",
        None,
        "1.0.0",
    )
    .unwrap();
    let a = publish(
        &library,
        &composite(&src, "A", &[child(&b), child(&c)]),
        "A",
        None,
        "1.0.0",
    )
    .unwrap();
    assert_eq!(a.manifest.dependencies.len(), 2, "C bundled once");
    let c_files = a
        .manifest
        .files
        .keys()
        .filter(|k| k.contains(&c.manifest.id))
        .count();
    assert_eq!(c_files, 2);
    remove_package(&library, &b.manifest.id);
    remove_package(&library, &c.manifest.id);
    assert_eq!(
        published_outputs(&a, 2, &library),
        vec![Some(0.5), Some(0.5)]
    );
}

#[test]
fn different_versions_of_one_dependency_coexist() {
    let tmp = TempDir::new("s3_versions");
    let library = tmp.0.join("library");
    let c1 = publish(
        &library,
        &leaf(&tmp.0.join("v1"), "C", 0.25),
        "C",
        None,
        "1.0.0",
    )
    .unwrap();
    let c2 = publish(
        &library,
        &leaf(&tmp.0.join("v2"), "C", 0.75),
        "C",
        Some(&c1.manifest.id),
        "2.0.0",
    )
    .unwrap();
    let src = tmp.0.join("src");
    // B uses C@1; A uses B and C@2 directly.
    let b = publish(
        &library,
        &composite(&src, "B", &[child(&c1)]),
        "B",
        None,
        "1.0.0",
    )
    .unwrap();
    let a = publish(
        &library,
        &composite(&src, "A", &[child(&b), child(&c2)]),
        "A",
        None,
        "1.0.0",
    )
    .unwrap();
    let deps = a.dir.join("deps").join(&c1.manifest.id);
    assert!(deps.join("1.0.0").is_dir() && deps.join("2.0.0").is_dir());
    assert_eq!(a.manifest.dependencies.len(), 3);
    remove_package(&library, &c1.manifest.id);
    remove_package(&library, &b.manifest.id);
    assert_eq!(
        published_outputs(&a, 2, &library),
        vec![Some(0.25), Some(0.75)],
        "each route keeps its exact version"
    );
}

#[test]
fn evaluation_is_the_same_before_and_after_bundling() {
    let tmp = TempDir::new("s3_same_output");
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    let c1 = publish(
        &library,
        &leaf(&tmp.0.join("v1"), "C", 0.125),
        "C",
        None,
        "1.0.0",
    )
    .unwrap();
    let c2 = publish(
        &library,
        &leaf(&tmp.0.join("v2"), "C", 0.875),
        "C",
        Some(&c1.manifest.id),
        "2.0.0",
    )
    .unwrap();
    let b = publish(
        &library,
        &composite(&src, "B", &[child(&c1)]),
        "B",
        None,
        "1.0.0",
    )
    .unwrap();
    let a_src = composite(&src, "A", &[child(&b), child(&c2), child(&c1)]);

    let before = outputs_of(&a_src.to_string_lossy(), None, 3, &library);
    let a = publish(&library, &a_src, "A", None, "1.0.0").unwrap();
    let after = published_outputs(&a, 3, &library);
    assert_eq!(before, vec![Some(0.125), Some(0.875), Some(0.125)]);
    assert_eq!(after, before);
}

// ── Cycles and depth ─────────────────────────────────────────────────────

#[test]
fn direct_and_indirect_cycles_are_refused() {
    let tmp = TempDir::new("s3_cycles");
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    // Direct: x@1 contains x@1.
    let x = InstancePackageRef {
        id: "local.x".into(),
        version: "1.0.0".into(),
        entry: "Node.pwnode".into(),
    };
    craft(&library, "local.x", "1.0.0", std::slice::from_ref(&x));
    let message = invalid_message(publish(
        &library,
        &composite(&src, "A", &[crafted_child(&x)]),
        "A",
        None,
        "1.0.0",
    ));
    assert!(
        message.contains("cycle") && message.contains("local.x@1.0.0 → local.x@1.0.0"),
        "{message}"
    );

    // Indirect: y → z → y.
    let y = InstancePackageRef {
        id: "local.y".into(),
        version: "1.0.0".into(),
        entry: "Node.pwnode".into(),
    };
    let z = InstancePackageRef {
        id: "local.z".into(),
        version: "1.0.0".into(),
        entry: "Node.pwnode".into(),
    };
    craft(&library, "local.y", "1.0.0", std::slice::from_ref(&z));
    craft(&library, "local.z", "1.0.0", std::slice::from_ref(&y));
    let message = invalid_message(publish(
        &library,
        &composite(&src, "A2", &[crafted_child(&y)]),
        "A2",
        None,
        "1.0.0",
    ));
    assert!(
        message.contains("local.y@1.0.0 → local.z@1.0.0 → local.y@1.0.0"),
        "{message}"
    );

    // Evaluation of a cyclic package terminates and is refused.
    assert_eq!(outputs_of("", Some(y), 1, &library), vec![None]);
}

#[test]
fn nesting_depth_is_limited_to_eight() {
    let tmp = TempDir::new("s3_depth");
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    // chain[k] contains chain[k + 1]; the last one is a leaf.
    let make_chain = |prefix: &str, len: usize| -> InstancePackageRef {
        let mut next: Option<InstancePackageRef> = None;
        for k in (0..len).rev() {
            let nested: Vec<InstancePackageRef> = next.iter().cloned().collect();
            next = Some(craft(
                &library,
                &format!("local.{prefix}{k}"),
                "1.0.0",
                &nested,
            ));
        }
        next.unwrap()
    };
    let ok = make_chain("ok", 8);
    assert!(
        publish(
            &library,
            &composite(&src, "Ok", &[crafted_child(&ok)]),
            "Ok",
            None,
            "1.0.0"
        )
        .is_ok()
    );
    let deep = make_chain("deep", 9);
    let message = invalid_message(publish(
        &library,
        &composite(&src, "Deep", &[crafted_child(&deep)]),
        "Deep",
        None,
        "1.0.0",
    ));
    assert!(message.contains("more than 8 levels"), "{message}");
}

// ── Refusals ─────────────────────────────────────────────────────────────

#[test]
fn loose_files_and_missing_damaged_or_incompatible_dependencies_are_refused() {
    let tmp = TempDir::new("s3_refusals");
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    let loose = leaf(&src, "Loose", 0.1);
    let b = publish(&library, &leaf(&src, "B", 0.2), "B", None, "1.0.0").unwrap();
    let missing = InstancePackageRef {
        id: "local.missing".into(),
        version: "1.0.0".into(),
        entry: "Missing.pwnode".into(),
    };

    // Every problem is listed, with what to do about it.
    let message = invalid_message(publish(
        &library,
        &composite(
            &src,
            "A",
            &[
                (loose.to_string_lossy().into_owned(), None),
                (String::new(), Some(missing)),
                child(&b),
            ],
        ),
        "A",
        None,
        "1.0.0",
    ));
    assert!(
        message.contains("loose custom node file Loose.pwnode"),
        "{message}"
    );
    assert!(
        message.contains("Publish Loose.pwnode to the Library first"),
        "{message}"
    );
    assert!(
        message.contains("local.missing@1.0.0") && message.contains("not installed"),
        "{message}"
    );
    assert!(
        !message.contains("'B'"),
        "the valid node is not listed: {message}"
    );
    assert!(
        !library.join(PACKAGES_DIR).join("local.a").exists(),
        "nothing published"
    );

    // Damaged dependency.
    let mut text = std::fs::read_to_string(&b.entry_path).unwrap();
    text = text.replace("\"B\"", "\"Tampered\"");
    std::fs::write(&b.entry_path, text).unwrap();
    let message = invalid_message(publish(
        &library,
        &composite(&src, "A3", &[child(&b)]),
        "A3",
        None,
        "1.0.0",
    ));
    assert!(message.contains("damaged"), "{message}");

    // Incompatible dependency.
    let c = publish(&library, &leaf(&src, "C", 0.3), "C", None, "1.0.0").unwrap();
    let manifest_path = c.dir.join(MANIFEST_FILE);
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    value["format"] = serde_json::json!(99);
    std::fs::write(&manifest_path, value.to_string()).unwrap();
    let message = invalid_message(publish(
        &library,
        &composite(&src, "A4", &[child(&c)]),
        "A4",
        None,
        "1.0.0",
    ));
    assert!(message.contains("not compatible"), "{message}");
}

#[test]
fn verified_path_only_instances_are_identified_unambiguously() {
    let tmp = TempDir::new("s3_legacy");
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    let b = publish(&library, &leaf(&src, "B", 0.4), "B", None, "1.0.0").unwrap();
    // A P4.1-era instance: only the absolute path of the package entry.
    let a = publish(
        &library,
        &composite(
            &src,
            "A",
            &[(b.entry_path.to_string_lossy().into_owned(), None)],
        ),
        "A",
        None,
        "1.0.0",
    )
    .unwrap();
    let entry = CustomNodeFile::load(&a.entry_path).unwrap();
    let package = entry.graph.nodes.values().find_map(|n| match &n.node_type {
        NodeType::CustomNodeInstance { package, .. } => package.clone(),
        _ => None,
    });
    assert_eq!(package, Some(stored(&b)), "reference recorded");
    assert_eq!(published_outputs(&a, 1, &library), vec![Some(0.4)]);
}

// ── Integrity ────────────────────────────────────────────────────────────

fn tampered_kind(a: &VerifiedPackage, library: &Path) -> ResolveErrorKind {
    let reference = stored(a).to_ref().unwrap();
    PackageResolver::new(None, Some(library.to_path_buf()))
        .resolve(&reference)
        .expect_err("tampered package must not resolve")
        .kind
}

fn bundled_pair(tag: &str) -> (TempDir, PathBuf, VerifiedPackage, VerifiedPackage) {
    let tmp = TempDir::new(tag);
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    let b = publish(&library, &leaf(&src, "B", 0.6), "B", None, "1.0.0").unwrap();
    let a = publish(
        &library,
        &composite(&src, "A", &[child(&b)]),
        "A",
        None,
        "1.0.0",
    )
    .unwrap();
    (tmp, library, a, b)
}

#[test]
fn tampered_bundled_dependency_is_refused_without_substitution() {
    let (_tmp, library, a, b) = bundled_pair("s3_tamper");
    let bundled = a.dir.join("deps").join(&b.manifest.id).join("1.0.0");
    let bundled_entry = bundled.join("B.pwnode");
    let text = std::fs::read_to_string(&bundled_entry)
        .unwrap()
        .replace("0.6", "0.9");
    std::fs::write(&bundled_entry, &text).unwrap();

    assert_eq!(tampered_kind(&a, &library), ResolveErrorKind::Corrupted);
    assert!(package::verify_version_dir(&a.dir, true).is_err());
    // The intact Library copy of B is never used instead.
    assert_eq!(published_outputs(&a, 1, &library), vec![None]);
    let mut resolver = PackageResolver::new(None, Some(library.clone()));
    resolver.set_bundle(Some(a.dir.clone()));
    let error = resolver
        .resolve(&stored(&b).to_ref().unwrap())
        .expect_err("bundled copy decides");
    assert_eq!(error.kind, ResolveErrorKind::Corrupted);

    // Re-sealing the bundled manifest does not help: the root lists it.
    let manifest_path = bundled.join(MANIFEST_FILE);
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    value["sha256"] = serde_json::json!(package::sha256_file(&bundled_entry).unwrap());
    std::fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&value).unwrap(),
    )
    .unwrap();
    assert_eq!(tampered_kind(&a, &library), ResolveErrorKind::Corrupted);
    assert_eq!(published_outputs(&a, 1, &library), vec![None]);
}

#[test]
fn unexpected_missing_or_linked_bundle_files_are_refused() {
    // Extra file.
    let (_tmp, library, a, b) = bundled_pair("s3_extra");
    std::fs::write(a.dir.join("deps").join("extra.pwnode"), "{}").unwrap();
    assert_eq!(tampered_kind(&a, &library), ResolveErrorKind::Corrupted);

    // Removed dependency folder.
    let (_tmp2, library2, a2, b2) = bundled_pair("s3_removed");
    std::fs::remove_dir_all(a2.dir.join("deps").join(&b2.manifest.id)).unwrap();
    assert_eq!(tampered_kind(&a2, &library2), ResolveErrorKind::Corrupted);
    let _ = b;

    // A link inside deps/ (directory junction on Windows) is unsafe.
    let (tmp3, library3, a3, _) = bundled_pair("s3_link");
    let outside = tmp3.0.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let link = a3.dir.join("deps").join("linked");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
    #[cfg(windows)]
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if made {
        assert_eq!(tampered_kind(&a3, &library3), ResolveErrorKind::Unsafe);
    }
}

#[test]
fn unsafe_bundle_paths_and_undeclared_dependencies_are_refused() {
    let (_tmp, library, a, _) = bundled_pair("s3_unsafe_paths");
    let manifest_path = a.dir.join(MANIFEST_FILE);
    let original = std::fs::read_to_string(&manifest_path).unwrap();
    for bad in ["deps/../package.json", "../outside", "C:/abs", "deps\\x"] {
        let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
        value["files"][bad] = serde_json::json!("00");
        std::fs::write(&manifest_path, value.to_string()).unwrap();
        assert_eq!(
            tampered_kind(&a, &library),
            ResolveErrorKind::Unsafe,
            "{bad}"
        );
    }
    // An entry using a package that is not declared.
    let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
    value["dependencies"] = serde_json::json!([]);
    std::fs::write(&manifest_path, value.to_string()).unwrap();
    assert_eq!(tampered_kind(&a, &library), ResolveErrorKind::Corrupted);
}

#[test]
fn bundles_are_not_rehashed_every_frame() {
    let tmp = TempDir::new("s3_frames");
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    let c = publish(&library, &leaf(&src, "C", 0.5), "C", None, "1.0.0").unwrap();
    let b = publish(
        &library,
        &composite(&src, "B", &[child(&c)]),
        "B",
        None,
        "1.0.0",
    )
    .unwrap();
    let a = publish(
        &library,
        &composite(&src, "A", &[child(&b)]),
        "A",
        None,
        "1.0.0",
    )
    .unwrap();
    let mut graph = Graph::new();
    let id = graph.add_node(
        NodeType::CustomNodeInstance {
            file: String::new(),
            inputs: Vec::new(),
            outputs: vec![port("out0")],
            package: Some(stored(&a)),
        },
        [0.0, 0.0],
    );
    graph.set_package_environment(None, Some(library.clone()));
    let first = graph.evaluate(0.0);
    assert!(matches!(first.get(&(id, 0)), Some(PortValue::Float(v)) if *v == 0.5));
    let hashes = HASH_COUNT.with(|c| c.get());
    for frame in 1..30 {
        let values = graph.evaluate(frame as f64 / 60.0);
        assert!(matches!(values.get(&(id, 0)), Some(PortValue::Float(v)) if *v == 0.5));
    }
    assert_eq!(HASH_COUNT.with(|c| c.get()), hashes, "no hashing per frame");
}

// ── Portability, compatibility, immutability ─────────────────────────────

#[test]
fn bundled_packages_are_portable_across_directories() {
    let (tmp, library, a, b) = bundled_pair("s3_portable");
    // Copy only A's version folder into an unrelated Library elsewhere.
    let other = tmp.0.join("elsewhere").join("lib");
    let target = other.join(PACKAGES_DIR).join(&a.manifest.id).join("1.0.0");
    for rel in files_under(&a.dir) {
        let to = target.join(&rel);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(a.dir.join(&rel), &to).unwrap();
    }
    std::fs::remove_dir_all(&library).unwrap();
    assert_eq!(published_outputs(&a, 1, &other), vec![Some(0.6)]);
    // A stale absolute path from the old machine does not matter either.
    assert_eq!(
        outputs_of(&a.entry_path.to_string_lossy(), Some(stored(&a)), 1, &other),
        vec![Some(0.6)]
    );
    let _ = b;
}

#[test]
fn standalone_and_p41_packages_are_unchanged() {
    let tmp = TempDir::new("s3_p41");
    let library = tmp.0.join("library");
    let source = leaf(&tmp.0.join("src"), "Solo", 0.3);
    let solo = publish(&library, &source, "Solo", None, "1.0.0").unwrap();
    let json = std::fs::read_to_string(solo.dir.join(MANIFEST_FILE)).unwrap();
    assert!(
        !json.contains("\"files\""),
        "no new keys for standalone packages"
    );
    assert!(solo.manifest.dependencies.is_empty());
    assert_eq!(
        std::fs::read(&solo.entry_path).unwrap(),
        std::fs::read(&source).unwrap(),
        "standalone entry still copied byte for byte"
    );
    assert_eq!(
        files_under(&solo.dir),
        vec!["Solo.pwnode".to_string(), MANIFEST_FILE.to_string()]
    );

    // A P4.1 manifest (no files, no dependency entries) still verifies.
    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value.as_object_mut().unwrap().remove("files");
    value["dependencies"] = serde_json::json!([]);
    std::fs::write(solo.dir.join(MANIFEST_FILE), value.to_string()).unwrap();
    assert!(package::verify_version_dir(&solo.dir, true).is_ok());
    assert_eq!(published_outputs(&solo, 1, &library), vec![Some(0.3)]);
}

#[test]
fn published_and_bundled_files_cannot_be_written() {
    let (tmp, library, a, b) = bundled_pair("s3_readonly");
    let bundled_entry = a
        .dir
        .join("deps")
        .join(&b.manifest.id)
        .join("1.0.0")
        .join("B.pwnode");
    let custom = CustomNodeFile::load(&b.entry_path).unwrap();
    for target in [
        bundled_entry.clone(),
        a.entry_path.clone(),
        a.dir.join("deps").join("New.pwnode"),
        a.dir.join("Other.pwnode"),
    ] {
        assert!(
            package::is_inside_published_package(&target),
            "{}",
            target.display()
        );
        let error = custom.save(&target).unwrap_err();
        assert!(error.contains("read-only"), "{error}");
        assert!(package::duplicate_for_editing(&b.entry_path, &target, true).is_err());
    }
    assert!(!package::is_inside_published_package(
        &tmp.0.join("src").join("A.pwnode")
    ));

    // Publishing again never overwrites, and sources are untouched.
    let before: Vec<Vec<u8>> = files_under(&a.dir)
        .iter()
        .map(|rel| std::fs::read(a.dir.join(rel)).unwrap())
        .collect();
    let b_before = std::fs::read(&b.entry_path).unwrap();
    let again = publish(
        &library,
        &tmp.0.join("src").join("A.pwnode"),
        "A",
        Some(&a.manifest.id),
        "1.0.0",
    );
    assert!(matches!(again, Err(PublishError::Conflict(_))), "{again:?}");
    let after: Vec<Vec<u8>> = files_under(&a.dir)
        .iter()
        .map(|rel| std::fs::read(a.dir.join(rel)).unwrap())
        .collect();
    assert_eq!(before, after);
    assert_eq!(std::fs::read(&b.entry_path).unwrap(), b_before);
}

#[test]
fn bundling_is_deterministic() {
    let tmp = TempDir::new("s3_deterministic");
    let src = tmp.0.join("src");
    let one = tmp.0.join("one");
    let two = tmp.0.join("two");
    // Sources are written once (from the first Library's packages); the
    // same sources are then published into both Libraries.
    let c_src = leaf(&src, "C", 0.5);
    let c = publish(&one, &c_src, "C", None, "1.0.0").unwrap();
    let b_src = composite(&src, "B", &[child(&c)]);
    let b = publish(&one, &b_src, "B", None, "1.0.0").unwrap();
    let a_src = composite(&src, "A", &[child(&b), child(&c)]);
    let a_one = publish(&one, &a_src, "A", None, "1.0.0").unwrap();
    publish(&two, &c_src, "C", None, "1.0.0").unwrap();
    publish(&two, &b_src, "B", None, "1.0.0").unwrap();
    let a_two = publish(&two, &a_src, "A", None, "1.0.0").unwrap();

    // Manifests carry publish times, so compare layout, identities and
    // the bundled custom nodes byte for byte.
    let layout = |a: &VerifiedPackage| {
        let deps = a.dir.join("deps");
        let entries: Vec<Vec<u8>> = files_under(&deps)
            .iter()
            .filter(|rel| rel.ends_with(".pwnode"))
            .map(|rel| std::fs::read(deps.join(rel)).unwrap())
            .collect();
        (
            files_under(&deps),
            a.manifest.dependencies.clone(),
            a.manifest.files.keys().cloned().collect::<Vec<_>>(),
            entries,
            std::fs::read(&a.entry_path).unwrap(),
        )
    };
    assert_eq!(layout(&a_one), layout(&a_two));
}

// ── Stage 3 hardening ────────────────────────────────────────────────────

fn instance_graph(
    file: &str,
    package: Option<InstancePackageRef>,
    library: &Path,
) -> (Graph, NodeId) {
    let mut graph = Graph::new();
    let id = graph.add_node(
        NodeType::CustomNodeInstance {
            file: file.to_string(),
            inputs: Vec::new(),
            outputs: vec![port("out0")],
            package,
        },
        [0.0, 0.0],
    );
    graph.set_package_environment(None, Some(library.to_path_buf()));
    (graph, id)
}

fn value(graph: &mut Graph, id: NodeId) -> Option<f32> {
    match graph.evaluate(0.0).get(&(id, 0)) {
        Some(PortValue::Float(v)) => Some(*v),
        _ => None,
    }
}

fn edit_json(path: &Path, change: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    change(&mut value);
    std::fs::write(path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
}

fn chain_abc(tag: &str) -> (TempDir, PathBuf, VerifiedPackage, VerifiedPackage) {
    let tmp = TempDir::new(tag);
    let library = tmp.0.join("library");
    let src = tmp.0.join("src");
    let c = publish(&library, &leaf(&src, "C", 0.5), "C", None, "1.0.0").unwrap();
    let b = publish(
        &library,
        &composite(&src, "B", &[child(&c)]),
        "B",
        None,
        "1.0.0",
    )
    .unwrap();
    let a = publish(
        &library,
        &composite(&src, "A", &[child(&b)]),
        "A",
        None,
        "1.0.0",
    )
    .unwrap();
    (tmp, library, a, c)
}

#[test]
fn published_bundles_carry_the_explicit_complete_marker() {
    let (_tmp, library, a, b) = bundled_pair("s3h_marker");
    assert_eq!(a.manifest.bundle_format, BUNDLE_FORMAT);
    assert!(is_complete_bundle(&a.manifest));
    // Standalone packages keep the P4.1 manifest (no marker written).
    assert_eq!(b.manifest.bundle_format, 0);
    let json = std::fs::read_to_string(b.dir.join(MANIFEST_FILE)).unwrap();
    assert!(!json.contains("bundle_format"));
    // A newer bundle format is refused as incompatible.
    edit_json(&a.dir.join(MANIFEST_FILE), |v| {
        v["bundle_format"] = serde_json::json!(2)
    });
    assert_eq!(tampered_kind(&a, &library), ResolveErrorKind::Incompatible);
}

#[test]
fn declared_dependency_missing_from_a_complete_bundle_is_refused() {
    let (_tmp, library, a, b) = bundled_pair("s3h_declared_missing");
    // Remove B from the bundle and, consistently, from the file list: only
    // the declaration is left. B is still installed in the Library.
    std::fs::remove_dir_all(a.dir.join("deps").join(&b.manifest.id)).unwrap();
    edit_json(&a.dir.join(MANIFEST_FILE), |v| {
        v["files"] = serde_json::json!({});
    });
    let error = package::verify_version_dir(&a.dir, true).unwrap_err();
    assert!(error.contains("missing from the bundle"), "{error}");
    assert_eq!(tampered_kind(&a, &library), ResolveErrorKind::Corrupted);
    // Never falls back to the Library copy of B.
    let (mut graph, id) = instance_graph("", Some(stored(&a)), &library);
    assert_eq!(value(&mut graph, id), None);
    let mut resolver = PackageResolver::new(None, Some(library.clone()));
    resolver.set_bundle(Some(a.dir.clone()));
    assert!(resolver.resolve(&stored(&b).to_ref().unwrap()).is_err());
}

#[test]
fn undeclared_bundled_dependency_is_refused() {
    let (_tmp, library, a, b) = bundled_pair("s3h_undeclared");
    // A consistently checksummed extra package that is not declared.
    let extra = a.dir.join("deps").join("local.extra").join("1.0.0");
    std::fs::create_dir_all(&extra).unwrap();
    let mut listed = Vec::new();
    for name in [MANIFEST_FILE, "B.pwnode"] {
        std::fs::copy(b.dir.join(name), extra.join(name)).unwrap();
        listed.push((
            format!("deps/local.extra/1.0.0/{name}"),
            package::sha256_file(&extra.join(name)).unwrap(),
        ));
    }
    edit_json(&a.dir.join(MANIFEST_FILE), |v| {
        for (rel, sha) in listed {
            v["files"][rel] = serde_json::json!(sha);
        }
    });
    let error = package::verify_version_dir(&a.dir, true).unwrap_err();
    assert!(error.contains("not a declared dependency"), "{error}");
    assert_eq!(tampered_kind(&a, &library), ResolveErrorKind::Corrupted);
}

#[test]
fn legacy_package_without_bundled_dependencies_still_works() {
    let tmp = TempDir::new("s3h_legacy_pkg");
    let library = tmp.0.join("library");
    let b = publish(
        &library,
        &leaf(&tmp.0.join("src"), "B", 0.2),
        "B",
        None,
        "1.0.0",
    )
    .unwrap();
    // No marker: a declared dependency that is not bundled resolves from
    // the Library as before.
    let legacy = craft(&library, "local.legacy", "1.0.0", &[stored(&b)]);
    let mut resolver = PackageResolver::new(None, Some(library.clone()));
    assert!(resolver.resolve(&legacy.to_ref().unwrap()).is_ok());
    let (mut graph, id) = instance_graph("", Some(legacy), &library);
    graph.evaluate(0.0);
    assert!(
        graph.has_custom_runtime(id),
        "{:?}",
        graph.custom_failure(id)
    );
    // A standalone package used through a P4.1 path-only reference too.
    let (mut graph, id) = instance_graph(&b.entry_path.to_string_lossy(), None, &library);
    assert_eq!(value(&mut graph, id), Some(0.2));
}

#[test]
fn nested_manifest_corruption_stops_a_loaded_node() {
    let (_tmp, library, a, b) = bundled_pair("s3h_manifest_loaded");
    let (mut graph, id) = instance_graph("", Some(stored(&a)), &library);
    assert_eq!(value(&mut graph, id), Some(0.6));
    // Metadata-only change of the bundled B manifest (entry untouched).
    let bundled = a.dir.join("deps").join(&b.manifest.id).join("1.0.0");
    edit_json(&bundled.join(MANIFEST_FILE), |v| {
        v["description"] = serde_json::json!("edited after publishing");
    });
    // Detected at the next bounded recheck, then the runtime is dropped.
    graph.expire_package_rechecks();
    assert_eq!(value(&mut graph, id), None);
    assert!(!graph.has_custom_runtime(id), "stale runtime dropped");
    let error = graph.custom_failure(id).unwrap();
    assert!(error.contains("changed after publishing"), "{error}");
}

#[test]
fn deep_dependency_corruption_after_evaluation_is_never_evaluated_and_recovers() {
    let (_tmp, library, a, c) = chain_abc("s3h_deep");
    let (mut graph, id) = instance_graph("", Some(stored(&a)), &library);
    assert_eq!(value(&mut graph, id), Some(0.5));

    let deep_entry = a
        .dir
        .join("deps")
        .join(&c.manifest.id)
        .join("1.0.0")
        .join("C.pwnode");
    let original = std::fs::read(&deep_entry).unwrap();
    let tampered = String::from_utf8(original.clone())
        .unwrap()
        .replace("0.5", "0.7");
    assert_ne!(tampered.as_bytes(), &original[..]);
    std::fs::write(&deep_entry, &tampered).unwrap();

    // The tampered value is never produced, with or without a recheck.
    assert_ne!(value(&mut graph, id), Some(0.7));
    graph.expire_package_rechecks();
    assert_eq!(value(&mut graph, id), None);
    assert!(!graph.has_custom_runtime(id));
    for _ in 0..3 {
        graph.expire_package_rechecks();
        assert_eq!(value(&mut graph, id), None, "stays refused");
    }

    // Restore the file and refresh the Library: it verifies and runs again.
    std::fs::write(&deep_entry, &original).unwrap();
    graph.clear_custom_runtime_failures();
    assert_eq!(value(&mut graph, id), Some(0.5));
}

#[test]
fn unchanged_bundles_are_rechecked_without_hashing_and_refresh_reverifies() {
    let (_tmp, library, a, _c) = chain_abc("s3h_nohash");
    let (mut graph, id) = instance_graph("", Some(stored(&a)), &library);
    assert_eq!(value(&mut graph, id), Some(0.5));
    let hashes = HASH_COUNT.with(|c| c.get());
    for _ in 0..30 {
        // Every frame past the recheck interval: fingerprints only.
        graph.expire_package_rechecks();
        assert_eq!(value(&mut graph, id), Some(0.5));
    }
    assert_eq!(HASH_COUNT.with(|c| c.get()), hashes, "no checksum hashing");
    // An explicit Library refresh forces full re-verification.
    graph.clear_custom_runtime_failures();
    assert_eq!(value(&mut graph, id), Some(0.5));
    assert!(HASH_COUNT.with(|c| c.get()) > hashes, "refresh re-verified");
}

#[test]
fn path_only_references_into_packages_are_verified() {
    let (_tmp, library, a, b) = bundled_pair("s3h_path_only");
    let bundled_entry = a
        .dir
        .join("deps")
        .join(&b.manifest.id)
        .join("1.0.0")
        .join("B.pwnode");
    let (mut graph, id) = instance_graph(&bundled_entry.to_string_lossy(), None, &library);
    assert_eq!(value(&mut graph, id), Some(0.6));
    let text = std::fs::read_to_string(&bundled_entry)
        .unwrap()
        .replace("0.6", "0.9");
    std::fs::write(&bundled_entry, text).unwrap();
    assert_eq!(
        value(&mut graph, id),
        None,
        "tampered bundled file not evaluated"
    );
    let (mut fresh, fresh_id) = instance_graph(&bundled_entry.to_string_lossy(), None, &library);
    assert_eq!(
        value(&mut fresh, fresh_id),
        None,
        "refused before first evaluation"
    );
}
