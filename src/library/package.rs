//! P4.1: Versioned Library packages.
//!
//! ```text
//! <library root>/packages/<id>/<version>/
//!     package.json     PackageManifest (the authority for this version)
//!     <Entry>.pwnode   unchanged .pwnode format
//! ```
//!
//! A published version folder is never modified: a new version is a new
//! folder, so instances that point at an older version keep using it.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::PublishError;

/// Manifest file name inside every version folder.
pub const MANIFEST_FILE: &str = "package.json";
/// Current manifest format.
pub const MANIFEST_FORMAT: u32 = 1;
/// Folder under the Library root holding packages.
pub const PACKAGES_DIR: &str = "packages";
/// Prefix for packages published from this machine.
pub const LOCAL_PREFIX: &str = "local.";
const MAX_ID_LEN: usize = 64;
const MAX_NAME_LEN: usize = 100;
const STAGING_PREFIX: &str = ".staging-";

// ── Identifiers and versions ─────────────────────────────────────────────

/// Check a package id: lowercase `[a-z0-9][a-z0-9._-]*`, at most 64 chars,
/// no `..`, not ending in `.` or `-`.
pub fn validate_package_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("Package id is empty.".to_string());
    }
    if id.len() > MAX_ID_LEN {
        return Err(format!(
            "Package id is longer than {MAX_ID_LEN} characters."
        ));
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap_or_default();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(format!(
            "Package id '{id}' must start with a lowercase letter or digit."
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "Package id '{id}' may only contain lowercase letters, digits, '.', '_' and '-'."
        ));
    }
    if id.contains("..") || id.ends_with('.') || id.ends_with('-') {
        return Err(format!("Package id '{id}' is not valid."));
    }
    Ok(())
}

/// Turn a display name into an id fragment: "Audio Mixer!" → "audio-mixer".
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    let max = MAX_ID_LEN - LOCAL_PREFIX.len();
    if slug.len() > max {
        slug.truncate(max);
        while slug.ends_with('-') {
            slug.pop();
        }
    }
    slug
}

/// `local.<slug>` id for a new local package.
pub fn local_package_id(name: &str) -> Result<String, String> {
    let slug = slugify(name);
    if slug.is_empty() {
        return Err(format!(
            "Cannot derive a package id from '{name}'. Use letters or digits in the name."
        ));
    }
    let id = format!("{LOCAL_PREFIX}{slug}");
    validate_package_id(&id)?;
    Ok(id)
}

/// Parse a stable `MAJOR.MINOR.PATCH` version (no pre-release or build).
pub fn parse_stable_version(text: &str) -> Result<semver::Version, String> {
    let version = semver::Version::parse(text.trim())
        .map_err(|e| format!("'{text}' is not a valid version (expected e.g. 1.0.0): {e}"))?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(format!(
            "'{text}' is a pre-release or build version; only MAJOR.MINOR.PATCH is supported."
        ));
    }
    Ok(version)
}

/// Next patch version: 1.2.3 → 1.2.4.
pub fn bump_patch(version: &semver::Version) -> semver::Version {
    semver::Version::new(version.major, version.minor, version.patch + 1)
}

/// Check an entry file name: a plain `<name>.pwnode` with no path parts.
pub fn validate_entry_name(entry: &str) -> Result<(), String> {
    let path = Path::new(entry);
    let plain = path.components().count() == 1
        && path
            .file_name()
            .map(|n| n == path.as_os_str())
            .unwrap_or(false)
        && !entry.contains(['/', '\\', ':']);
    if !plain || entry.starts_with('.') {
        return Err(format!(
            "Package entry '{entry}' must be a plain file name."
        ));
    }
    if path.extension().and_then(|e| e.to_str()) != Some("pwnode") {
        return Err(format!("Package entry '{entry}' must be a .pwnode file."));
    }
    Ok(())
}

// ── Manifest ─────────────────────────────────────────────────────────────

fn default_pwnode_version() -> u32 {
    1
}

fn default_source() -> String {
    "local".to_string()
}

/// A dependency on another package version (filled by P4.2; empty now).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageDependency {
    pub id: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

/// `package.json` in a version folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageManifest {
    pub format: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// `.pwnode` file name inside the version folder.
    pub entry: String,
    /// Hex SHA-256 of the entry file.
    pub sha256: String,
    #[serde(default = "default_pwnode_version")]
    pub pwnode_version: u32,
    #[serde(default)]
    pub min_app_version: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<PackageDependency>,
    /// "local" for this machine; "marketplace" reserved for P4.3.
    #[serde(default = "default_source")]
    pub source: String,
    /// Unix seconds.
    #[serde(default)]
    pub published_at: u64,
    /// P4.2 Stage 3: SHA-256 of every bundled dependency file, keyed by its
    /// `/`-separated path inside the version folder (all under `deps/`).
    /// Absent for packages without bundled dependencies (P4.1 and
    /// standalone packages), which are verified by `sha256` alone.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub files: std::collections::BTreeMap<String, String>,
    /// P4.2 Stage 3 capability marker. `0` (absent): no complete bundle
    /// (P4.1 and standalone packages). `1`: a complete flat bundle; every
    /// declared dependency is bundled, everything bundled is declared, and
    /// nested instances never resolve outside the bundle. Higher values
    /// are not readable by this Patchwork.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bundle_format: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl PackageManifest {
    /// Structural checks (does not touch the filesystem).
    pub fn validate(&self) -> Result<semver::Version, String> {
        if self.format != MANIFEST_FORMAT {
            return Err(format!(
                "Unsupported package manifest format {} (this Patchwork reads format {MANIFEST_FORMAT}).",
                self.format
            ));
        }
        validate_package_id(&self.id)?;
        let version = parse_stable_version(&self.version)?;
        if self.name.trim().is_empty() {
            return Err("Package name is empty.".to_string());
        }
        validate_entry_name(&self.entry)?;
        if self.sha256.len() != 64 || !self.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("Package checksum is missing or malformed.".to_string());
        }
        if self.pwnode_version != 1 {
            return Err(format!(
                "Package uses custom node format {}, which this Patchwork cannot read.",
                self.pwnode_version
            ));
        }
        if self.bundle_format > super::bundle::BUNDLE_FORMAT {
            return Err(format!(
                "Package uses dependency bundle format {}, which this Patchwork cannot read.",
                self.bundle_format
            ));
        }
        Ok(version)
    }

    /// Portable reference to this version's entry (P4.2).
    #[allow(dead_code)]
    pub fn package_ref(&self) -> Result<PackageRef, String> {
        Ok(PackageRef {
            id: self.id.clone(),
            version: parse_stable_version(&self.version)?,
            entry: self.entry.clone(),
        })
    }
}

/// Location-independent reference to a package entry: `id@version/entry`.
///
/// Instances store it (as `resolve::InstancePackageRef`, plain strings)
/// next to their compatibility `file` path since P4.2 Stage 2.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageRef {
    pub id: String,
    pub version: semver::Version,
    pub entry: String,
}

impl PackageRef {
    /// Path of the referenced entry under a Library root (P4.2).
    #[allow(dead_code)]
    pub fn resolve(&self, library_root: &Path) -> PathBuf {
        version_dir(library_root, &self.id, &self.version).join(&self.entry)
    }

    /// Parse `id@version/entry`.
    #[allow(dead_code)]
    pub fn parse(text: &str) -> Result<Self, String> {
        let (id, rest) = text
            .split_once('@')
            .ok_or_else(|| format!("'{text}' is not a package reference (id@version/entry)."))?;
        let (version, entry) = rest
            .split_once('/')
            .ok_or_else(|| format!("'{text}' is not a package reference (id@version/entry)."))?;
        validate_package_id(id)?;
        validate_entry_name(entry)?;
        Ok(Self {
            id: id.to_string(),
            version: parse_stable_version(version)?,
            entry: entry.to_string(),
        })
    }
}

impl std::fmt::Display for PackageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}/{}", self.id, self.version, self.entry)
    }
}

// ── Layout ───────────────────────────────────────────────────────────────

pub fn packages_dir(library_root: &Path) -> PathBuf {
    library_root.join(PACKAGES_DIR)
}

pub fn package_dir(library_root: &Path, id: &str) -> PathBuf {
    packages_dir(library_root).join(id)
}

pub fn version_dir(library_root: &Path, id: &str, version: &semver::Version) -> PathBuf {
    package_dir(library_root, id).join(version.to_string())
}

#[cfg(test)]
thread_local! {
    /// Number of files hashed on this thread (tests check that verification
    /// results are cached instead of re-hashing every frame).
    pub(crate) static HASH_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    #[cfg(test)]
    HASH_COUNT.with(|count| count.set(count.get() + 1));
    let bytes =
        std::fs::read(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

pub(crate) fn read_manifest(dir: &Path) -> Result<PackageManifest, String> {
    let path = dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("Invalid {}: {e}", path.display()))
}

/// True when `dir` holds a readable package manifest.
fn is_package_dir(dir: &Path) -> bool {
    dir.join(MANIFEST_FILE).is_file() && read_manifest(dir).is_ok()
}

/// P4.2 Stage 3: the outermost published version folder containing `path`
/// (a package, or the package that bundles it). `None` outside packages.
pub fn containing_package_root(path: &Path) -> Option<PathBuf> {
    let mut found = None;
    for dir in path.ancestors().skip(1).take(64) {
        if dir.as_os_str().is_empty() {
            break;
        }
        if is_package_dir(dir) {
            found = Some(dir.to_path_buf());
        }
    }
    found
}

/// P4.2 Stage 3: true for any file inside a published package version,
/// including bundled dependencies. Such files are read-only in every
/// Patchwork editing workflow.
pub fn is_inside_published_package(path: &Path) -> bool {
    path.ancestors()
        .skip(1)
        .take(64)
        .take_while(|dir| !dir.as_os_str().is_empty())
        .any(is_package_dir)
}

/// True when `path` is the entry of a published package version, i.e. a
/// `package.json` next to it names it as the entry. Such files are
/// read-only in every Patchwork editing workflow.
/// (Editing workflows use [`is_inside_published_package`], which also
/// covers bundled dependencies.)
#[cfg(test)]
pub fn is_published_entry(path: &Path) -> bool {
    let (Some(dir), Some(file_name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    if !dir.join(MANIFEST_FILE).is_file() {
        return false;
    }
    read_manifest(dir)
        .map(|m| std::ffi::OsStr::new(&m.entry) == file_name)
        .unwrap_or(false)
}

// ── Verification ─────────────────────────────────────────────────────────

/// A version folder whose manifest and checksum were verified.
#[derive(Debug, Clone)]
pub struct VerifiedPackage {
    pub manifest: PackageManifest,
    pub version: semver::Version,
    pub dir: PathBuf,
    pub entry_path: PathBuf,
}

/// Verify a version folder: manifest is valid, folder names match the
/// manifest (when `expect_layout`), the entry exists, its SHA-256 matches,
/// and it loads with nested references contained in the folder.
pub fn verify_version_dir(dir: &Path, expect_layout: bool) -> Result<VerifiedPackage, String> {
    let manifest = read_manifest(dir)?;
    let version = manifest
        .validate()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    if expect_layout {
        let dir_version = dir.file_name().map(|n| n.to_string_lossy().into_owned());
        let dir_id = dir
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        if dir_version.as_deref() != Some(version.to_string().as_str())
            || dir_id.as_deref() != Some(manifest.id.as_str())
        {
            return Err(format!(
                "{}: folder does not match package {}@{}.",
                dir.display(),
                manifest.id,
                version
            ));
        }
    }
    let entry_path = dir.join(&manifest.entry);
    if !entry_path.is_file() {
        return Err(format!(
            "Package {}@{} is missing its file {}.",
            manifest.id, version, manifest.entry
        ));
    }
    let actual = sha256_file(&entry_path)?;
    if !actual.eq_ignore_ascii_case(&manifest.sha256) {
        return Err(format!(
            "Package {}@{} failed its checksum check: {} was changed after publishing. \
             It will not be used until the file is restored or republished.",
            manifest.id, version, manifest.entry
        ));
    }
    // P4.2 Stage 3: bundled dependency files and nested package references.
    super::bundle::verify_bundle(dir, &manifest).map_err(|problem| {
        format!(
            "Package {}@{} is damaged: {}",
            manifest.id, version, problem.message
        )
    })?;
    crate::document::CustomNodeFile::load_with_policy(
        &entry_path,
        &crate::document::DependencyPolicy::Contained {
            root: dir.to_path_buf(),
        },
    )
    .map_err(|e| format!("Package {}@{}: {e}", manifest.id, version))?;
    Ok(VerifiedPackage {
        manifest,
        version,
        dir: dir.to_path_buf(),
        entry_path,
    })
}

// ── Publishing ───────────────────────────────────────────────────────────

/// What to publish.
#[derive(Debug, Clone, Default)]
pub struct PublishRequest {
    /// `Some(id)` publishes a new version of an existing package (its id is
    /// kept); `None` creates a new `local.<slug>` package.
    pub existing_id: Option<String>,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub tags: Vec<String>,
}

/// Parse a comma-separated tag list.
pub fn parse_tags(text: &str) -> Vec<String> {
    let mut tags: Vec<String> = text
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    tags.dedup();
    tags
}

/// True when the package has at least one version the Library lists
/// (readable manifest matching its folder). A package folder holding only
/// damaged versions does not count, so its id can still be published as a
/// new package (L2); damaged version folders are never overwritten because
/// publishing refuses any existing version folder.
fn has_valid_version(library_root: &Path, id: &str) -> bool {
    let dir = package_dir(library_root, id);
    std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|e| super::index::is_listable_version(&e.path()))
        })
        .unwrap_or(false)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Publish `source` as a new, immutable package version under
/// `library_root/packages/<id>/<version>/`.
///
/// Never overwrites: an existing version is a [`PublishError::Conflict`].
/// The version is assembled in a staging folder, verified, then renamed
/// into place, so a failure never leaves a half-written version.
pub fn publish_package(
    library_root: &Path,
    source: &Path,
    request: &PublishRequest,
) -> Result<VerifiedPackage, PublishError> {
    let invalid = PublishError::Invalid;
    if !source.is_file() {
        return Err(invalid("Custom node file does not exist.".to_string()));
    }
    if source.extension().and_then(|e| e.to_str()) != Some("pwnode") {
        return Err(invalid("Only .pwnode files can be published.".to_string()));
    }
    let name = request.name.trim().to_string();
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(invalid(format!(
            "Package name must be 1–{MAX_NAME_LEN} characters."
        )));
    }
    let version = parse_stable_version(&request.version).map_err(invalid)?;

    let id = match &request.existing_id {
        Some(id) => {
            validate_package_id(id).map_err(invalid)?;
            if !has_valid_version(library_root, id) {
                return Err(invalid(format!(
                    "Package {id} does not exist in the Library."
                )));
            }
            id.clone()
        }
        None => {
            let id = local_package_id(&name).map_err(invalid)?;
            if has_valid_version(library_root, &id) {
                return Err(PublishError::Conflict(format!(
                    "package {id} (publish it as a new version of that package instead)"
                )));
            }
            id
        }
    };

    let custom = crate::document::CustomNodeFile::load(source)
        .map_err(|e| invalid(format!("Cannot read the custom node: {e}")))?;
    // P4.2 Stage 3: nested custom nodes must be exact, verified packages;
    // they are bundled into the new version (sources are only read).
    let plan = if custom.graph.nodes.values().any(|n| {
        matches!(
            n.node_type,
            crate::graph::NodeType::CustomNodeInstance { .. }
        )
    }) {
        Some(super::bundle::collect_dependencies(&custom, library_root).map_err(invalid)?)
    } else {
        None
    };

    let entry = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| validate_entry_name(n).is_ok())
        .unwrap_or_else(|| format!("{}.pwnode", slugify(&name)));
    validate_entry_name(&entry).map_err(invalid)?;

    let final_dir = version_dir(library_root, &id, &version);
    if final_dir.exists() {
        return Err(PublishError::Conflict(format!("{id} version {version}")));
    }
    let package_root = package_dir(library_root, &id);
    let package_root_existed = package_root.is_dir();
    super::ensure_dir(&package_root).map_err(PublishError::Io)?;

    let staging = package_root.join(format!(
        "{STAGING_PREFIX}{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir(&staging)
        .map_err(|e| PublishError::Io(format!("Cannot create staging folder: {e}")))?;

    let result = (|| -> Result<VerifiedPackage, PublishError> {
        let staged_entry = staging.join(&entry);
        let mut dependencies = Vec::new();
        let mut files = std::collections::BTreeMap::new();
        match &plan {
            // Standalone: the file is published byte for byte (P4.1).
            None => {
                let mut input = std::fs::File::open(source)
                    .map_err(|e| PublishError::Io(format!("Failed to read custom node: {e}")))?;
                super::copy_exclusive_from(&mut input, &staged_entry).map_err(PublishError::Io)?;
            }
            // With dependencies: nested instances name their exact package
            // and point at the bundled copy, which is copied into deps/.
            Some(plan) => {
                let json =
                    super::bundle::rewrite_root_entry(&custom, plan).map_err(PublishError::Io)?;
                super::copy_exclusive_from(&mut json.as_bytes(), &staged_entry)
                    .map_err(PublishError::Io)?;
                files =
                    super::bundle::bundle_dependencies(&staging, plan).map_err(PublishError::Io)?;
                dependencies = plan
                    .dependencies
                    .iter()
                    .map(|d| PackageDependency {
                        id: d.reference.id.clone(),
                        version: d.reference.version.to_string(),
                        entry: Some(d.reference.entry.clone()),
                    })
                    .collect();
            }
        }
        let manifest = PackageManifest {
            format: MANIFEST_FORMAT,
            id: id.clone(),
            name: name.clone(),
            version: version.to_string(),
            description: request.description.trim().to_string(),
            author: request.author.trim().to_string(),
            tags: request.tags.clone(),
            entry: entry.clone(),
            sha256: sha256_file(&staged_entry).map_err(PublishError::Io)?,
            pwnode_version: custom.version,
            min_app_version: None,
            dependencies,
            source: default_source(),
            published_at: unix_now(),
            // Packages with nested nodes are always complete bundles.
            bundle_format: if plan.is_some() {
                super::bundle::BUNDLE_FORMAT
            } else {
                0
            },
            files,
        };
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| PublishError::Io(format!("Cannot write manifest: {e}")))?;
        super::copy_exclusive_from(&mut json.as_bytes(), &staging.join(MANIFEST_FILE))
            .map_err(PublishError::Io)?;
        verify_version_dir(&staging, false).map_err(PublishError::Io)?;
        // Fails if another writer created the version meanwhile; never
        // replaces an existing version folder.
        if final_dir.exists() {
            return Err(PublishError::Conflict(format!("{id} version {version}")));
        }
        std::fs::rename(&staging, &final_dir).map_err(|e| {
            if final_dir.exists() {
                PublishError::Conflict(format!("{id} version {version}"))
            } else {
                PublishError::Io(format!("Cannot finish publishing: {e}"))
            }
        })?;
        verify_version_dir(&final_dir, true).map_err(PublishError::Io)
    })();

    if result.is_err() {
        // Only ever removes our own staging folder (and the package folder
        // if this call created it and it is now empty).
        let _ = std::fs::remove_dir_all(&staging);
        if !package_root_existed {
            let _ = std::fs::remove_dir(&package_root);
        }
    }
    result
}

/// True for leftover staging folders (ignored by scanning).
pub fn is_staging_dir(name: &str) -> bool {
    name.starts_with(STAGING_PREFIX)
}

// ── Duplicate for Editing ────────────────────────────────────────────────

/// Write an editable copy of `source` to `destination` (load + save, so
/// nested references are re-written relative to the new location).
/// Refuses to write over a published entry, or over any existing file
/// unless `overwrite` is set by an explicit user confirmation.
pub fn duplicate_for_editing(
    source: &Path,
    destination: &Path,
    overwrite: bool,
) -> Result<(), String> {
    if is_inside_published_package(destination) {
        return Err(
            "The destination is a published Library package, which is read-only.".to_string(),
        );
    }
    if destination.is_dir() {
        return Err("A folder with that name already exists.".to_string());
    }
    if destination.exists() && !overwrite {
        return Err(format!("{} already exists.", destination.display()));
    }
    if let (Ok(a), Ok(b)) = (
        std::fs::canonicalize(source),
        std::fs::canonicalize(destination),
    ) {
        if a == b {
            return Err("Choose a different file than the original.".to_string());
        }
    }
    let file = crate::document::CustomNodeFile::load(source)?;
    file.save(destination)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "patchwork_pkg_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn write_node(path: &Path, name: &str, nested: Option<&str>) {
        let mut file = crate::document::CustomNodeFile {
            version: 1,
            name: name.into(),
            graph: crate::graph::Graph::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
        };
        if let Some(reference) = nested {
            file.graph.add_node(
                crate::graph::NodeType::CustomNodeInstance {
                    file: reference.into(),
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    package: None,
                },
                [0.0, 0.0],
            );
        }
        std::fs::write(path, serde_json::to_string_pretty(&file).unwrap()).unwrap();
    }

    pub(crate) fn request(name: &str, version: &str) -> PublishRequest {
        PublishRequest {
            name: name.into(),
            version: version.into(),
            ..PublishRequest::default()
        }
    }

    // ── Stage 1: metadata ────────────────────────────────────────────

    #[test]
    fn package_ids_are_validated() {
        for ok in ["local.mixer", "a", "local.audio-mixer_2", "author.pkg.v2"] {
            assert!(validate_package_id(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "Local.mixer",
            ".mixer",
            "-mixer",
            "local..mixer",
            "local.mixer.",
            "local mixer",
            "local/mixer",
            "local\\mixer",
            &"a".repeat(65),
        ] {
            assert!(validate_package_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn local_ids_are_derived_from_names() {
        assert_eq!(
            local_package_id("Audio Mixer!").unwrap(),
            "local.audio-mixer"
        );
        assert_eq!(local_package_id("  LFO  2 ").unwrap(), "local.lfo-2");
        assert!(local_package_id("!!!").is_err());
        let long = local_package_id(&"x".repeat(200)).unwrap();
        assert!(long.len() <= 64);
    }

    #[test]
    fn only_stable_versions_are_accepted() {
        assert_eq!(
            parse_stable_version("1.2.3").unwrap(),
            semver::Version::new(1, 2, 3)
        );
        for bad in [
            "1.2",
            "v1.2.3",
            "01.2.3",
            "1.2.3-beta",
            "1.2.3+build",
            "",
            "x",
        ] {
            assert!(parse_stable_version(bad).is_err(), "{bad}");
        }
        assert!(semver::Version::new(1, 10, 0) > semver::Version::new(1, 9, 9));
        assert_eq!(
            bump_patch(&semver::Version::new(1, 2, 3)),
            semver::Version::new(1, 2, 4)
        );
    }

    #[test]
    fn entry_names_must_be_plain_pwnode_files() {
        assert!(validate_entry_name("Mixer.pwnode").is_ok());
        for bad in [
            "../Mixer.pwnode",
            "sub/Mixer.pwnode",
            "sub\\Mixer.pwnode",
            "C:Mixer.pwnode",
            "/abs.pwnode",
            "Mixer.json",
            ".hidden.pwnode",
        ] {
            assert!(validate_entry_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn manifest_round_trip_and_defaults() {
        let json = r#"{"format":1,"id":"local.mixer","name":"Mixer","version":"1.0.0",
            "entry":"Mixer.pwnode","sha256":"%","future_field":true}"#
            .replace('%', &"a".repeat(64));
        let manifest: PackageManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.source, "local");
        assert_eq!(manifest.pwnode_version, 1);
        assert!(manifest.dependencies.is_empty());
        assert_eq!(manifest.validate().unwrap(), semver::Version::new(1, 0, 0));
        let again: PackageManifest =
            serde_json::from_str(&serde_json::to_string(&manifest).unwrap()).unwrap();
        assert_eq!(again, manifest);

        let mut bad = manifest.clone();
        bad.format = 2;
        assert!(bad.validate().is_err());
        let mut bad = manifest.clone();
        bad.entry = "../x.pwnode".into();
        assert!(bad.validate().is_err());
        let mut bad = manifest;
        bad.sha256 = "nothex".into();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn package_refs_round_trip_and_resolve() {
        let r = PackageRef::parse("local.mixer@1.2.0/Mixer.pwnode").unwrap();
        assert_eq!(r.to_string(), "local.mixer@1.2.0/Mixer.pwnode");
        let root = PathBuf::from("lib");
        assert_eq!(
            r.resolve(&root),
            root.join("packages")
                .join("local.mixer")
                .join("1.2.0")
                .join("Mixer.pwnode")
        );
        for bad in [
            "local.mixer",
            "local.mixer@1.2/Mixer.pwnode",
            "x@1.0.0/../a.pwnode",
        ] {
            assert!(PackageRef::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn tags_are_normalised() {
        assert_eq!(parse_tags(" Audio, mix ,, FX"), vec!["audio", "mix", "fx"]);
    }

    // ── Stage 2: storage, publishing, validation ─────────────────────

    #[test]
    fn publish_new_package_creates_verified_version() {
        let tmp = TempDir::new("publish");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let root = tmp.0.join("library");
        let mut req = request("Mixer", "1.0.0");
        req.description = "Mixes".into();
        req.tags = vec!["audio".into()];
        let published = publish_package(&root, &source, &req).unwrap();
        assert_eq!(published.manifest.id, "local.mixer");
        assert_eq!(
            published.dir,
            root.join("packages").join("local.mixer").join("1.0.0")
        );
        assert_eq!(
            std::fs::read(&published.entry_path).unwrap(),
            std::fs::read(&source).unwrap()
        );
        assert_eq!(published.manifest.sha256, sha256_file(&source).unwrap());
        assert!(verify_version_dir(&published.dir, true).is_ok());
        assert!(is_published_entry(&published.entry_path));
        assert!(!is_published_entry(&source));
        // No staging leftovers.
        let leftovers: Vec<_> = std::fs::read_dir(root.join("packages").join("local.mixer"))
            .unwrap()
            .flatten()
            .filter(|e| is_staging_dir(&e.file_name().to_string_lossy()))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn new_version_keeps_id_and_leaves_old_version_untouched() {
        let tmp = TempDir::new("new_version");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let root = tmp.0.join("library");
        let v1 = publish_package(&root, &source, &request("Mixer", "1.0.0")).unwrap();
        let v1_bytes = std::fs::read(&v1.entry_path).unwrap();
        let v1_manifest = std::fs::read(v1.dir.join(MANIFEST_FILE)).unwrap();

        write_node(&source, "Mixer v2", None);
        let mut req = request("Renamed Mixer", "1.1.0");
        req.existing_id = Some("local.mixer".into());
        let v2 = publish_package(&root, &source, &req).unwrap();
        assert_eq!(v2.manifest.id, "local.mixer");
        assert_eq!(v2.manifest.name, "Renamed Mixer");
        assert_ne!(v2.dir, v1.dir);
        assert_eq!(std::fs::read(&v1.entry_path).unwrap(), v1_bytes);
        assert_eq!(
            std::fs::read(v1.dir.join(MANIFEST_FILE)).unwrap(),
            v1_manifest
        );
    }

    #[test]
    fn existing_version_and_id_are_never_overwritten() {
        let tmp = TempDir::new("conflict");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let root = tmp.0.join("library");
        let v1 = publish_package(&root, &source, &request("Mixer", "1.0.0")).unwrap();
        let before = std::fs::read(&v1.entry_path).unwrap();

        write_node(&source, "Changed", None);
        let mut req = request("Mixer", "1.0.0");
        req.existing_id = Some("local.mixer".into());
        assert!(matches!(
            publish_package(&root, &source, &req),
            Err(PublishError::Conflict(_))
        ));
        // A second *new* package with the same derived id is also refused.
        assert!(matches!(
            publish_package(&root, &source, &request("Mixer", "2.0.0")),
            Err(PublishError::Conflict(_))
        ));
        assert_eq!(std::fs::read(&v1.entry_path).unwrap(), before);
    }

    #[test]
    fn invalid_publish_requests_leave_nothing_behind() {
        let tmp = TempDir::new("invalid");
        let root = tmp.0.join("library");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let nested = tmp.0.join("Nested.pwnode");
        write_node(&nested, "Nested", Some("Mixer.pwnode"));
        let json = tmp.0.join("x.json");
        std::fs::write(&json, "{}").unwrap();

        assert!(
            publish_package(&root, &tmp.0.join("missing.pwnode"), &request("A", "1.0.0")).is_err()
        );
        assert!(publish_package(&root, &json, &request("A", "1.0.0")).is_err());
        assert!(publish_package(&root, &source, &request("A", "1.0")).is_err());
        assert!(publish_package(&root, &source, &request("A", "1.0.0-beta")).is_err());
        assert!(publish_package(&root, &source, &request("   ", "1.0.0")).is_err());
        assert!(publish_package(&root, &nested, &request("Nested", "1.0.0")).is_err());
        let mut req = request("A", "1.0.0");
        req.existing_id = Some("local.unknown".into());
        assert!(publish_package(&root, &source, &req).is_err());
        assert!(
            !root.join("packages").exists()
                || std::fs::read_dir(root.join("packages"))
                    .unwrap()
                    .next()
                    .is_none()
        );
    }

    #[test]
    fn failed_publish_removes_staging_and_new_package_folder() {
        let tmp = TempDir::new("failed");
        let root = tmp.0.join("library");
        // Not a valid custom node: load fails before staging.
        let broken = tmp.0.join("Broken.pwnode");
        std::fs::write(&broken, "not json").unwrap();
        assert!(publish_package(&root, &broken, &request("Broken", "1.0.0")).is_err());
        assert!(!package_dir(&root, "local.broken").exists());
    }

    #[test]
    fn checksum_mismatch_is_reported_clearly() {
        let tmp = TempDir::new("checksum");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let root = tmp.0.join("library");
        let published = publish_package(&root, &source, &request("Mixer", "1.0.0")).unwrap();
        write_node(&published.entry_path, "Tampered", None);
        let error = verify_version_dir(&published.dir, true).unwrap_err();
        assert!(error.contains("checksum"), "{error}");
        assert!(error.contains("local.mixer@1.0.0"), "{error}");
    }

    #[test]
    fn verification_rejects_bad_layout_and_missing_entry() {
        let tmp = TempDir::new("layout");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let root = tmp.0.join("library");
        let published = publish_package(&root, &source, &request("Mixer", "1.0.0")).unwrap();
        // Copy the version folder under a wrong version name.
        let wrong = package_dir(&root, "local.mixer").join("9.9.9");
        std::fs::create_dir_all(&wrong).unwrap();
        for f in [MANIFEST_FILE, "Mixer.pwnode"] {
            std::fs::copy(published.dir.join(f), wrong.join(f)).unwrap();
        }
        assert!(verify_version_dir(&wrong, true).is_err());
        assert!(verify_version_dir(&wrong, false).is_ok());
        std::fs::remove_file(wrong.join("Mixer.pwnode")).unwrap();
        assert!(
            verify_version_dir(&wrong, false)
                .unwrap_err()
                .contains("missing")
        );
    }

    #[test]
    fn unrelated_package_json_does_not_make_file_read_only() {
        let tmp = TempDir::new("npm");
        std::fs::write(tmp.0.join(MANIFEST_FILE), r#"{"name":"my-npm-app"}"#).unwrap();
        let node = tmp.0.join("Mine.pwnode");
        write_node(&node, "Mine", None);
        assert!(!is_published_entry(&node));
    }

    // ── Stage 5: read-only + duplicate ───────────────────────────────

    #[test]
    fn published_entries_cannot_be_saved_over() {
        let tmp = TempDir::new("readonly");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let root = tmp.0.join("library");
        let published = publish_package(&root, &source, &request("Mixer", "1.0.0")).unwrap();
        let before = std::fs::read(&published.entry_path).unwrap();
        let file = crate::document::CustomNodeFile::load(&published.entry_path).unwrap();
        let error = file.save(&published.entry_path).unwrap_err();
        assert!(error.contains("read-only"), "{error}");
        assert_eq!(std::fs::read(&published.entry_path).unwrap(), before);
        assert!(verify_version_dir(&published.dir, true).is_ok());
    }

    #[test]
    fn duplicate_for_editing_creates_editable_copy() {
        let tmp = TempDir::new("duplicate");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let root = tmp.0.join("library");
        let published = publish_package(&root, &source, &request("Mixer", "1.0.0")).unwrap();
        let copy = tmp.0.join("work").join("Mixer copy.pwnode");
        std::fs::create_dir_all(copy.parent().unwrap()).unwrap();

        duplicate_for_editing(&published.entry_path, &copy, false).unwrap();
        assert!(!is_published_entry(&copy));
        let loaded = crate::document::CustomNodeFile::load(&copy).unwrap();
        assert_eq!(loaded.name, "Mixer");
        loaded.save(&copy).unwrap(); // editable
        assert!(verify_version_dir(&published.dir, true).is_ok());

        // Never silently overwrites; never targets a published entry.
        assert!(duplicate_for_editing(&published.entry_path, &copy, false).is_err());
        assert!(duplicate_for_editing(&published.entry_path, &copy, true).is_ok());
        assert!(duplicate_for_editing(&copy, &published.entry_path, true).is_err());
        assert!(duplicate_for_editing(&copy, &copy, true).is_err());
    }

    #[test]
    fn package_tabs_open_read_only_and_local_tabs_do_not() {
        let tmp = TempDir::new("tabs");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let root = tmp.0.join("library");
        let published = publish_package(&root, &source, &request("Mixer", "1.0.0")).unwrap();

        let mut documents = crate::document::DocumentManager::new();
        let package_tab = documents.open_custom_node(published.entry_path).unwrap();
        let local_tab = documents.open_custom_node(source).unwrap();
        let read_only = |id: u64| {
            documents
                .documents
                .iter()
                .find(|d| d.id == id)
                .unwrap()
                .read_only
        };
        assert!(read_only(package_tab));
        assert!(!read_only(local_tab));
    }

    #[test]
    fn instances_stay_pinned_to_their_version() {
        // An instance stores the absolute path of one version's entry; the
        // runtime reloads only when that file changes. Publishing a newer
        // version must leave the old file byte- and mtime-identical.
        let tmp = TempDir::new("pinned");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let root = tmp.0.join("library");
        let v1 = publish_package(&root, &source, &request("Mixer", "1.0.0")).unwrap();
        let modified = std::fs::metadata(&v1.entry_path)
            .unwrap()
            .modified()
            .unwrap();
        let reference = v1.manifest.package_ref().unwrap();
        assert_eq!(reference.resolve(&root), v1.entry_path);

        write_node(&source, "Mixer v2", None);
        let mut req = request("Mixer", "2.0.0");
        req.existing_id = Some(v1.manifest.id.clone());
        publish_package(&root, &source, &req).unwrap();

        assert_eq!(
            std::fs::metadata(&v1.entry_path)
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        assert_eq!(reference.resolve(&root), v1.entry_path);
        let loaded = crate::document::CustomNodeFile::load(&v1.entry_path).unwrap();
        assert_eq!(loaded.name, "Mixer");
    }

    // ── Stage 0: L2 broken-package dead end ──────────────────────────

    #[test]
    fn package_with_only_damaged_versions_can_be_published_again() {
        let tmp = TempDir::new("l2");
        let root = tmp.0.join("library");
        let damaged = package_dir(&root, "local.mixer").join("1.0.0");
        std::fs::create_dir_all(&damaged).unwrap();
        std::fs::write(damaged.join(MANIFEST_FILE), "{ damaged").unwrap();
        std::fs::write(damaged.join("Mixer.pwnode"), "old bytes").unwrap();
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);

        // The damaged version folder is never overwritten...
        assert!(matches!(
            publish_package(&root, &source, &request("Mixer", "1.0.0")),
            Err(PublishError::Conflict(_))
        ));
        // ...but the id is no longer a dead end.
        let published = publish_package(&root, &source, &request("Mixer", "1.0.1")).unwrap();
        assert_eq!(published.manifest.id, "local.mixer");
        assert_eq!(
            std::fs::read_to_string(damaged.join(MANIFEST_FILE)).unwrap(),
            "{ damaged"
        );
        assert_eq!(
            std::fs::read_to_string(damaged.join("Mixer.pwnode")).unwrap(),
            "old bytes"
        );

        // Listed normally; the damaged folder is reported, not listed.
        let scan = crate::library::index::scan_packages(&root);
        assert_eq!(scan.packages.len(), 1);
        assert_eq!(scan.packages[0].versions.len(), 1);
        assert_eq!(scan.problems.len(), 1);

        // Now that a valid version exists, "new package" is a conflict
        // again and "new version" works.
        assert!(matches!(
            publish_package(&root, &source, &request("Mixer", "2.0.0")),
            Err(PublishError::Conflict(_))
        ));
        let mut req = request("Mixer", "2.0.0");
        req.existing_id = Some("local.mixer".into());
        assert!(publish_package(&root, &source, &req).is_ok());
    }

    #[test]
    fn checksum_failed_version_still_reserves_its_package_id() {
        // A tampered (but readable) version is listed with a warning, so
        // the id stays taken and users publish a new version instead.
        let tmp = TempDir::new("l2_tampered");
        let root = tmp.0.join("library");
        let source = tmp.0.join("Mixer.pwnode");
        write_node(&source, "Mixer", None);
        let v1 = publish_package(&root, &source, &request("Mixer", "1.0.0")).unwrap();
        std::fs::write(&v1.entry_path, "tampered").unwrap();
        assert!(matches!(
            publish_package(&root, &source, &request("Mixer", "1.0.1")),
            Err(PublishError::Conflict(_))
        ));
        let mut req = request("Mixer", "1.0.1");
        req.existing_id = Some("local.mixer".into());
        assert!(publish_package(&root, &source, &req).is_ok());
    }
}
