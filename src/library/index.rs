//! P4.1: Package index.
//!
//! The manifests in `packages/<id>/<version>/package.json` are the
//! authority. `index.json` is a derived summary written after each scan
//! (for tools and the future marketplace); it is never trusted on its own
//! and is simply rewritten if missing, stale or corrupt.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::package::{self, PackageManifest};

pub const INDEX_FILE: &str = "index.json";
pub const INDEX_FORMAT: u32 = 1;

/// One installed version of a package.
#[derive(Debug, Clone)]
pub struct InstalledVersion {
    pub manifest: PackageManifest,
    pub version: semver::Version,
    pub dir: PathBuf,
    pub entry_path: PathBuf,
    /// `Err` when the checksum or content check failed; such versions are
    /// listed (so the user sees the problem) but cannot be inserted.
    pub integrity: Result<(), String>,
}

/// All installed versions of one package, newest first.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub id: String,
    pub versions: Vec<InstalledVersion>,
}

impl InstalledPackage {
    /// Newest version (used for display and default insertion).
    pub fn latest(&self) -> &InstalledVersion {
        &self.versions[0]
    }

    /// Newest version that passed verification (P4.2 update checks).
    #[allow(dead_code)]
    pub fn latest_valid(&self) -> Option<&InstalledVersion> {
        self.versions.iter().find(|v| v.integrity.is_ok())
    }

    /// Case-insensitive search over name, id, description and tags.
    pub fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let m = &self.latest().manifest;
        let query = query.to_lowercase();
        m.name.to_lowercase().contains(&query)
            || m.id.contains(&query)
            || m.description.to_lowercase().contains(&query)
            || m.tags.iter().any(|t| t.contains(&query))
    }
}

/// Result of scanning `packages/`.
#[derive(Debug, Default)]
pub struct PackageScan {
    pub packages: Vec<InstalledPackage>,
    /// Folders that could not be read as packages: (folder, reason).
    pub problems: Vec<(PathBuf, String)>,
}

/// Scan `library_root/packages/*/*/`. Unreadable manifests are reported
/// in `problems`; checksum failures stay listed with `integrity: Err`.
pub fn scan_packages(library_root: &Path) -> PackageScan {
    let mut scan = PackageScan::default();
    let root = package::packages_dir(library_root);
    let Ok(package_dirs) = std::fs::read_dir(&root) else {
        return scan;
    };
    for package_entry in package_dirs.flatten() {
        let package_path = package_entry.path();
        if !package_path.is_dir() {
            continue;
        }
        let Ok(version_dirs) = std::fs::read_dir(&package_path) else {
            scan.problems
                .push((package_path, "Cannot read package folder.".to_string()));
            continue;
        };
        let mut versions = Vec::new();
        for version_entry in version_dirs.flatten() {
            let dir = version_entry.path();
            let name = version_entry.file_name().to_string_lossy().into_owned();
            if !dir.is_dir() || package::is_staging_dir(&name) {
                continue;
            }
            match package::verify_version_dir(&dir, true) {
                Ok(verified) => versions.push(InstalledVersion {
                    manifest: verified.manifest,
                    version: verified.version,
                    dir: verified.dir,
                    entry_path: verified.entry_path,
                    integrity: Ok(()),
                }),
                Err(error) => match read_listable(&dir) {
                    // Manifest is fine but contents failed (e.g. checksum):
                    // keep it visible with the error.
                    Some((manifest, version)) => versions.push(InstalledVersion {
                        entry_path: dir.join(&manifest.entry),
                        manifest,
                        version,
                        dir,
                        integrity: Err(error),
                    }),
                    None => scan.problems.push((dir, error)),
                },
            }
        }
        if versions.is_empty() {
            continue;
        }
        versions.sort_by(|a, b| b.version.cmp(&a.version));
        scan.packages.push(InstalledPackage {
            id: versions[0].manifest.id.clone(),
            versions,
        });
    }
    scan.packages.sort_by(|a, b| {
        a.latest()
            .manifest
            .name
            .to_lowercase()
            .cmp(&b.latest().manifest.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    scan
}

/// True when `dir` is a version folder the Library would list.
pub fn is_listable_version(dir: &Path) -> bool {
    dir.is_dir() && read_listable(dir).is_some()
}

/// A manifest that is structurally valid and matches its folder.
fn read_listable(dir: &Path) -> Option<(PackageManifest, semver::Version)> {
    let text = std::fs::read_to_string(dir.join(package::MANIFEST_FILE)).ok()?;
    let manifest: PackageManifest = serde_json::from_str(&text).ok()?;
    let version = manifest.validate().ok()?;
    let id_ok = dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy() == manifest.id)
        .unwrap_or(false);
    let version_ok = dir
        .file_name()
        .map(|n| n.to_string_lossy() == version.to_string())
        .unwrap_or(false);
    (id_ok && version_ok).then_some((manifest, version))
}

/// `index.json` contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexFile {
    pub format: u32,
    pub generated_at: u64,
    pub packages: Vec<PackageManifest>,
}

/// Build the index for a scan (verified versions only).
pub fn build_index(scan: &PackageScan) -> IndexFile {
    IndexFile {
        format: INDEX_FORMAT,
        generated_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        packages: scan
            .packages
            .iter()
            .flat_map(|p| p.versions.iter())
            .filter(|v| v.integrity.is_ok())
            .map(|v| v.manifest.clone())
            .collect(),
    }
}

/// Write `index.json` atomically (temp file + rename). Only writes when
/// the Library root already exists, so listing never creates folders.
pub fn write_index(library_root: &Path, index: &IndexFile) -> Result<(), String> {
    if !library_root.is_dir() {
        return Ok(());
    }
    let json =
        serde_json::to_string_pretty(index).map_err(|e| format!("Cannot write index: {e}"))?;
    let target = library_root.join(INDEX_FILE);
    let temp = library_root.join(format!(".{INDEX_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&temp, json).map_err(|e| format!("Cannot write index: {e}"))?;
    std::fs::rename(&temp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("Cannot write index: {e}")
    })
}

/// Read `index.json`; `None` when missing, corrupt or of another format
/// (callers then rescan). Reserved for P4.3 tooling.
#[allow(dead_code)]
pub fn read_index(library_root: &Path) -> Option<IndexFile> {
    let text = std::fs::read_to_string(library_root.join(INDEX_FILE)).ok()?;
    let index: IndexFile = serde_json::from_str(&text).ok()?;
    (index.format == INDEX_FORMAT).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::package::tests::{TempDir, request, write_node};
    use crate::library::package::{package_dir, publish_package};

    fn publish(root: &Path, tmp: &Path, name: &str, version: &str, existing: bool) {
        let source = tmp.join(format!("{name}.pwnode"));
        write_node(&source, name, None);
        let mut req = request(name, version);
        if existing {
            req.existing_id = Some(crate::library::package::local_package_id(name).unwrap());
        }
        publish_package(root, &source, &req).unwrap();
    }

    #[test]
    fn scan_groups_versions_newest_first() {
        let tmp = TempDir::new("scan");
        let root = tmp.0.join("library");
        publish(&root, &tmp.0, "Mixer", "1.0.0", false);
        publish(&root, &tmp.0, "Mixer", "1.10.0", true);
        publish(&root, &tmp.0, "Mixer", "1.2.0", true);
        publish(&root, &tmp.0, "Filter", "0.1.0", false);
        let scan = scan_packages(&root);
        assert!(scan.problems.is_empty(), "{:?}", scan.problems);
        let ids: Vec<_> = scan.packages.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["local.filter", "local.mixer"]);
        let versions: Vec<_> = scan.packages[1]
            .versions
            .iter()
            .map(|v| v.version.to_string())
            .collect();
        assert_eq!(versions, vec!["1.10.0", "1.2.0", "1.0.0"]);
        assert_eq!(scan.packages[1].latest().version.to_string(), "1.10.0");
    }

    #[test]
    fn scan_of_missing_library_is_empty() {
        let tmp = TempDir::new("scan_missing");
        let scan = scan_packages(&tmp.0.join("nope"));
        assert!(scan.packages.is_empty() && scan.problems.is_empty());
    }

    #[test]
    fn bad_manifests_are_reported_not_listed() {
        let tmp = TempDir::new("scan_bad");
        let root = tmp.0.join("library");
        publish(&root, &tmp.0, "Good", "1.0.0", false);
        let bad = package_dir(&root, "local.bad").join("1.0.0");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("package.json"), "{ not json").unwrap();
        // Leftover staging folders are ignored entirely.
        std::fs::create_dir_all(package_dir(&root, "local.good").join(".staging-1-2")).unwrap();
        let scan = scan_packages(&root);
        assert_eq!(scan.packages.len(), 1);
        assert_eq!(scan.problems.len(), 1);
        assert!(scan.problems[0].0.ends_with("1.0.0"));
    }

    #[test]
    fn checksum_failures_stay_listed_but_flagged() {
        let tmp = TempDir::new("scan_checksum");
        let root = tmp.0.join("library");
        publish(&root, &tmp.0, "Mixer", "1.0.0", false);
        publish(&root, &tmp.0, "Mixer", "1.1.0", true);
        let v11 = package_dir(&root, "local.mixer")
            .join("1.1.0")
            .join("Mixer.pwnode");
        write_node(&v11, "Tampered", None);
        let scan = scan_packages(&root);
        let pkg = &scan.packages[0];
        assert!(pkg.latest().integrity.is_err());
        assert_eq!(pkg.latest_valid().unwrap().version.to_string(), "1.0.0");
        assert!(
            !build_index(&scan)
                .packages
                .iter()
                .any(|m| m.version == "1.1.0")
        );
    }

    #[test]
    fn deleted_version_disappears_on_rescan() {
        let tmp = TempDir::new("scan_deleted");
        let root = tmp.0.join("library");
        publish(&root, &tmp.0, "Mixer", "1.0.0", false);
        publish(&root, &tmp.0, "Mixer", "1.1.0", true);
        std::fs::remove_dir_all(package_dir(&root, "local.mixer").join("1.1.0")).unwrap();
        let scan = scan_packages(&root);
        assert_eq!(scan.packages[0].versions.len(), 1);
    }

    #[test]
    fn index_round_trip_and_corruption() {
        let tmp = TempDir::new("index");
        let root = tmp.0.join("library");
        publish(&root, &tmp.0, "Mixer", "1.0.0", false);
        let index = build_index(&scan_packages(&root));
        write_index(&root, &index).unwrap();
        assert_eq!(read_index(&root).unwrap(), index);
        std::fs::write(root.join(INDEX_FILE), "garbage").unwrap();
        assert!(read_index(&root).is_none());
        // Rewritten from manifests.
        write_index(&root, &build_index(&scan_packages(&root))).unwrap();
        assert_eq!(read_index(&root).unwrap().packages.len(), 1);
        // Never creates a missing Library root.
        let missing = tmp.0.join("missing");
        write_index(&missing, &index).unwrap();
        assert!(!missing.exists());
    }

    #[test]
    fn search_matches_name_id_description_and_tags() {
        let tmp = TempDir::new("search");
        let root = tmp.0.join("library");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let mut req = request("Audio Mixer", "1.0.0");
        req.description = "Blends channels".into();
        req.tags = vec!["dsp".into()];
        publish_package(&root, &source, &req).unwrap();
        let pkg = &scan_packages(&root).packages[0];
        for q in ["audio", "local.audio", "BLENDS", "dsp", ""] {
            assert!(pkg.matches(q), "{q}");
        }
        assert!(!pkg.matches("video"));
    }
}
