//! P4.3.3: preparing a marketplace submission from a package published on
//! this computer.
//!
//! Patchwork never talks to GitHub here: it writes a submission folder
//! that the publisher adds to a fork of the catalogue repository and
//! submits as a pull request. GitHub authenticates the publisher, and the
//! repository's review and CI decide (see `docs/marketplace/architecture.md`
//! §3 and §5): the PR author's numeric account id must own the namespace.
//!
//! ```text
//! check     a verified Library version, recorded as published on this
//!           computer for exactly these bytes (local_publish), unchanged
//! identify  the public id <namespace>.<name> (a new, validated identity;
//!           the local package is never renamed or changed)
//! complete  every dependency bundled, and already public
//! scan      secrets refused; node types classified (restricted or unknown
//!           ones refused); capabilities declared exactly
//! build     deterministic .pwpkg (files sorted, compact JSON)
//! validate  through the installer's own pipeline: parse_catalogue on the
//!           listing, verify_download, parse_artifact, the capability scan
//!           and compatibility, plus conflicts with the cached catalogue
//! write     staging folder, re-read and compared, then renamed into place
//!           (never over an existing submission); removed on any failure
//! ```
//!
//! Nothing is evaluated or constructed: graphs are only read as JSON.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

use super::limits::MAX_PREVIEW_BYTES;
use super::scan::{check_declared, scan_files};
use super::source::CatalogueLocation;
use super::{
    ARTIFACT_EXTENSION, ARTIFACT_FORMAT, ArtifactEntry, Capability, Catalogue, CatalogueFile,
    MARKETPLACE_SOURCE, PREVIEW_FILES, PackageEntry, PublisherEntry, VersionEntry,
    artifact_repo_path, confusable_key, split_public_id, validate_publisher_claim,
};
use crate::library::package::{LOCAL_PREFIX, MANIFEST_FILE, PackageManifest};

/// What the publisher chose for the public listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRequest {
    /// The publisher's GitHub login, lowercase (P4.3.0 §5). Patchwork does
    /// not verify it: the catalogue's review and CI do.
    pub namespace: String,
    /// The package name within the namespace (`<namespace>.<name>`).
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub author: String,
    pub tags: Vec<String>,
    pub license: Option<String>,
    pub documentation: Option<String>,
    /// Oldest Patchwork that can use the version.
    pub min_patchwork: semver::Version,
    /// Optional preview image (`.png`, `.jpg`, `.jpeg` or `.webp`).
    pub preview: Option<PathBuf>,
}

/// A validated submission, ready to be written.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub public_id: String,
    pub namespace: String,
    pub name: String,
    pub version: semver::Version,
    pub entry: String,
    /// The local package it was made from.
    pub local_id: String,
    pub local_tree_sha256: String,
    pub capabilities: BTreeSet<Capability>,
    pub dependencies: Vec<(String, String)>,
    /// `(path, text)` in artifact order.
    pub files: Vec<(String, String)>,
    pub artifact: Vec<u8>,
    pub sha256: String,
    pub size: u64,
    pub file_count: u32,
    /// The catalogue listing, as CI will generate it (URL pinned later).
    pub listing: PackageEntry,
    pub preview: Option<(String, Vec<u8>)>,
    /// What the catalogue says about the namespace (claimed by whom), or
    /// what the publisher must still do.
    pub notes: Vec<String>,
}

/// Simulated interruptions (only triggered by tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailPoint {
    /// Before writing file number N.
    Writing(usize),
    BeforeCommit,
}

#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_AT: std::cell::Cell<Option<FailPoint>> = const { std::cell::Cell::new(None) };
}

fn fail_at(_point: FailPoint) -> Result<(), String> {
    #[cfg(test)]
    if FAIL_AT.with(|f| f.get()) == Some(_point) {
        return Err(format!("simulated failure at {_point:?}"));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every file of a version folder, as `/`-separated relative paths.
pub(crate) fn files_in(dir: &Path) -> Result<Vec<String>, String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), String> {
        for entry in
            std::fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err(format!(
                    "{} is a link; it is never shared.",
                    entry.path().display()
                ));
            }
            if kind.is_dir() {
                walk(root, &entry.path(), out)?;
            } else {
                let relative = entry.path();
                let relative = relative.strip_prefix(root).map_err(|e| e.to_string())?;
                let parts: Vec<String> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out)?;
    out.sort();
    Ok(out)
}

/// The `.pwpkg` envelope, written deterministically: fixed field order,
/// files sorted by path, compact JSON, UTF-8.
#[derive(Serialize)]
struct EnvelopeOut<'a> {
    pwpkg: u32,
    id: &'a str,
    version: String,
    files: Vec<EnvelopeFileOut<'a>>,
}

#[derive(Serialize)]
struct EnvelopeFileOut<'a> {
    path: &'a str,
    text: &'a str,
}

/// Artifact order: `package.json`, the entry, then bundled files by path.
pub(crate) fn artifact_order(files: &mut [(String, String)], entry: &str) {
    files.sort_by(|(a, _), (b, _)| {
        let rank = |p: &str| match p {
            p if p == MANIFEST_FILE => 0,
            p if p == entry => 1,
            _ => 2,
        };
        (rank(a), a.as_str()).cmp(&(rank(b), b.as_str()))
    });
}

pub fn build_artifact(id: &str, version: &semver::Version, files: &[(String, String)]) -> Vec<u8> {
    let envelope = EnvelopeOut {
        pwpkg: ARTIFACT_FORMAT,
        id,
        version: version.to_string(),
        files: files
            .iter()
            .map(|(path, text)| EnvelopeFileOut { path, text })
            .collect(),
    };
    serde_json::to_vec(&envelope).expect("the envelope serializes")
}

/// The file name of a preview image in the version folder.
fn preview_name(path: &Path) -> Result<String, String> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let name = format!("preview.{extension}");
    if PREVIEW_FILES.contains(&name.as_str()) {
        Ok(name)
    } else {
        Err("The preview must be a .png, .jpg, .jpeg or .webp image.".to_string())
    }
}

/// What may be shared: no secrets in any file, every node type classified
/// and allowed (restricted and unknown types fail closed), no native
/// code. Returns the capabilities the graphs use.
pub fn check_contents(files: &[(String, String)]) -> Result<BTreeSet<Capability>, String> {
    for (path, text) in files {
        let findings = crate::library::secrets::find_secrets_in_text(text);
        if !findings.is_empty() {
            return Err(crate::library::secrets::describe(path, &findings));
        }
    }
    let capabilities = scan_files(files.iter().map(|(p, t)| (p.as_str(), t.as_str())))?;
    if capabilities.contains(&Capability::NativeCode) {
        return Err("Native code is never distributed through the marketplace.".to_string());
    }
    Ok(capabilities)
}

/// The listing as a one-package catalogue, through the catalogue validator.
/// `commit` pins the artifact URL (CI uses the merge commit). A publisher
/// entry is needed for that; its account id is a placeholder: ownership is
/// verified by the repository, never by Patchwork.
pub fn listing_catalogue(
    prepared: &Prepared,
    location: &CatalogueLocation,
    commit: &str,
) -> Result<Catalogue, String> {
    let mut listing = prepared.listing.clone();
    for version in &mut listing.versions {
        version.artifact.url = version.artifact.url.replace(&"0".repeat(40), commit);
    }
    let file = CatalogueFile {
        kind: super::CATALOGUE_KIND.to_string(),
        catalogue_format: super::CATALOGUE_FORMAT,
        generated_at: None,
        publishers: vec![PublisherEntry {
            namespace: prepared.namespace.clone(),
            github: prepared.namespace.clone(),
            github_id: 1,
            display_name: prepared.listing.author.clone(),
            url: None,
        }],
        packages: vec![listing],
    };
    let bytes = serde_json::to_vec(&file).map_err(|e| e.to_string())?;
    super::parse_catalogue(&bytes, location.source())
        .map_err(|e| format!("The catalogue listing is not valid: {e}"))
}

/// Check everything and build the submission (writes nothing).
///
/// `source` is a Library version folder; `trust` the trust store (the
/// version must be recorded as published on this computer for exactly its
/// bytes); `catalogue` the configured catalogue's cached copy, if any (for
/// immutable versions, look-alikes and the namespace's owner).
pub fn prepare(
    source: &Path,
    request: &ShareRequest,
    trust: &crate::library::trust::TrustStore,
    location: &CatalogueLocation,
    catalogue: Option<&Catalogue>,
    app: &semver::Version,
) -> Result<Prepared, String> {
    // ── A published, unchanged version of this computer ──
    let verified = crate::library::package::verify_version_dir_static(source, true)
        .map_err(|e| format!("Only verified, published Library versions can be shared: {e}"))?;
    let local = &verified.manifest;
    if !local.id.starts_with(LOCAL_PREFIX) || local.source != "local" {
        return Err(format!(
            "{} was not published on this computer, so it cannot be shared from here.",
            local.id
        ));
    }
    let identified = crate::library::trust::identify(&verified.dir, true)?;
    match trust.load().status(&identified.identity) {
        crate::library::trust::TrustStatus::Recorded(record)
            if record.provenance == crate::library::trust::Provenance::LocalPublish => {}
        _ => {
            return Err(format!(
                "{}@{} is not recorded as published on this computer for exactly these \
                 files. Publish it again from its source, then share that version.",
                local.id, verified.version
            ));
        }
    }

    // ── The new public identity (never the local id, never inferred) ──
    let public_id = format!("{}.{}", request.namespace, request.name);
    split_public_id(&public_id)?;
    validate_publisher_claim(&request.namespace, &request.namespace, location.source())?;

    // ── Exactly the version folder's files ──
    let on_disk = files_in(&verified.dir)?;
    let mut expected: BTreeSet<String> = local.files.keys().cloned().collect();
    expected.insert(MANIFEST_FILE.to_string());
    expected.insert(local.entry.clone());
    for path in &on_disk {
        if !expected.contains(path) {
            return Err(format!(
                "{path} is not part of the published version; it is never shared."
            ));
        }
    }

    // ── Complete, public dependencies ──
    if !local.dependencies.is_empty() && local.bundle_format < 1 {
        return Err("Its dependencies are not bundled completely; publish it again.".to_string());
    }
    let mut dependencies = Vec::new();
    for dependency in &local.dependencies {
        if split_public_id(&dependency.id).is_err() {
            return Err(format!(
                "It uses {}@{}, which is not public. Share that package first, use its \
                 marketplace version in this node, publish a new version and share that.",
                dependency.id, dependency.version
            ));
        }
        dependencies.push((dependency.id.clone(), dependency.version.clone()));
    }

    // ── The public manifest (same files, new identity and listing) ──
    let mut manifest: PackageManifest = local.clone();
    manifest.id = public_id.clone();
    manifest.source = MARKETPLACE_SOURCE.to_string();
    manifest.name = request.display_name.clone();
    manifest.description = request.description.clone();
    manifest.author = request.author.clone();
    manifest.tags = request.tags.clone();
    if let Some(min) = &manifest.min_app_version {
        if crate::library::package::parse_stable_version(min)? > request.min_patchwork {
            return Err(format!(
                "The version needs Patchwork {min}; the minimum cannot be lower."
            ));
        }
    }
    // P4.3.4: the catalogue repository derives the listing's minimum
    // Patchwork from the version's own package.json.
    manifest.min_app_version = Some(request.min_patchwork.to_string());
    let manifest_text = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    let mut files = vec![(MANIFEST_FILE.to_string(), manifest_text)];
    for path in &on_disk {
        if path == MANIFEST_FILE {
            continue;
        }
        let full = path.split('/').fold(verified.dir.clone(), |p, s| p.join(s));
        let text =
            std::fs::read_to_string(&full).map_err(|e| format!("Cannot read {path}: {e}"))?;
        files.push((path.clone(), text));
    }
    artifact_order(&mut files, &manifest.entry);

    let capabilities = check_contents(&files)?;

    // ── The artifact and its listing ──
    let artifact = build_artifact(&public_id, &verified.version, &files);
    let sha256 = sha256_hex(&artifact);
    let file_count = u32::try_from(files.len()).map_err(|_| "Too many files.".to_string())?;
    let preview = match &request.preview {
        Some(path) => {
            let name = preview_name(path)?;
            let bytes = std::fs::read(path)
                .map_err(|e| format!("Cannot read the preview {}: {e}", path.display()))?;
            if bytes.is_empty() || bytes.len() as u64 > MAX_PREVIEW_BYTES {
                return Err(format!(
                    "The preview must be between 1 and {MAX_PREVIEW_BYTES} bytes."
                ));
            }
            Some((name, bytes))
        }
        None => None,
    };
    let listing = PackageEntry {
        id: public_id.clone(),
        name: request.display_name.clone(),
        description: request.description.clone(),
        author: request.author.clone(),
        tags: request.tags.clone(),
        license: request.license.clone(),
        documentation: request.documentation.clone(),
        versions: vec![VersionEntry {
            version: verified.version.to_string(),
            entry: manifest.entry.clone(),
            manifest_format: manifest.format,
            pwnode_version: manifest.pwnode_version,
            bundle_format: manifest.bundle_format,
            min_patchwork: request.min_patchwork.to_string(),
            capabilities: capabilities
                .iter()
                .map(|c| c.as_str().to_string())
                .collect(),
            artifact: ArtifactEntry {
                format: ARTIFACT_FORMAT,
                // Pinned to the merge commit by CI; a placeholder commit is
                // used only to validate the listing here.
                url: format!(
                    "https://{}/{}/{}/{}/{}",
                    super::RAW_HOST,
                    location.source().owner(),
                    location.source().repo(),
                    "0".repeat(40),
                    artifact_repo_path(&public_id, &verified.version)
                ),
                sha256: sha256.clone(),
                size: artifact.len() as u64,
                files: file_count,
            },
            preview: None,
            published_at: None,
            yanked: None,
        }],
    };

    let prepared = Prepared {
        public_id,
        namespace: request.namespace.clone(),
        name: request.name.clone(),
        version: verified.version.clone(),
        entry: manifest.entry.clone(),
        local_id: local.id.clone(),
        local_tree_sha256: identified.identity.tree_sha256.clone(),
        capabilities,
        dependencies,
        files,
        sha256,
        size: artifact.len() as u64,
        file_count,
        artifact,
        listing,
        preview,
        notes: Vec::new(),
    };
    let notes = validate(&prepared, location, catalogue, app)?;
    Ok(Prepared { notes, ..prepared })
}

/// Validate a prepared submission exactly as Patchwork validates what it
/// installs, and against the configured catalogue. Returns notes for the
/// publisher.
pub fn validate(
    prepared: &Prepared,
    location: &CatalogueLocation,
    catalogue: Option<&Catalogue>,
    app: &semver::Version,
) -> Result<Vec<String>, String> {
    // The listing, through the catalogue validator.
    let parsed = listing_catalogue(prepared, location, &"0".repeat(40))?;
    let package = parsed
        .packages
        .get(&prepared.public_id)
        .ok_or_else(|| "The listing lost its package.".to_string())?;
    let version = package
        .versions
        .get(&prepared.version)
        .ok_or_else(|| "The listing lost its version.".to_string())?;
    if !version.is_installable(app) {
        return Err(format!(
            "This Patchwork could not install it: {}.",
            super::browse::compatibility_label(version, app).0
        ));
    }
    // The artifact, through the installer's own checks.
    super::artifact::verify_download(&prepared.artifact, version)?;
    let artifact = super::artifact::parse_artifact(&prepared.artifact, package, version)?;
    let found = scan_files(artifact.texts())?;
    check_declared(&version.known_capabilities(), &found)?;
    if found != prepared.capabilities {
        return Err("The declared capabilities differ from the scanned ones.".to_string());
    }
    // It must be the very content prepared (nothing lost or reordered).
    let reparsed: Vec<(String, String)> = artifact
        .files
        .iter()
        .map(|f| (f.path.clone(), f.text.clone()))
        .collect();
    if reparsed != prepared.files
        || build_artifact(&prepared.public_id, &prepared.version, &reparsed) != prepared.artifact
    {
        return Err("The artifact does not rebuild identically.".to_string());
    }

    // The configured catalogue: immutable versions, look-alikes, owner.
    let mut notes = Vec::new();
    let Some(catalogue) = catalogue else {
        notes.push(
            "The marketplace catalogue was not available offline; the repository's checks \
             will compare it with the published versions."
                .to_string(),
        );
        return Ok(notes);
    };
    if let Some(existing) = catalogue.packages.get(&prepared.public_id) {
        if existing.versions.contains_key(&prepared.version) {
            return Err(format!(
                "{}@{} is already published. Published versions never change: publish a \
                 new version of the package and share that.",
                prepared.public_id, prepared.version
            ));
        }
        notes.push(format!(
            "This is a new version of {} (published: {}).",
            prepared.public_id,
            existing
                .versions
                .keys()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let key = confusable_key(&prepared.public_id);
    if let Some(other) = catalogue
        .packages
        .keys()
        .find(|id| **id != prepared.public_id && confusable_key(id) == key)
    {
        return Err(format!(
            "{} looks like the existing package {other}; choose a distinct name.",
            prepared.public_id
        ));
    }
    let namespace_key = confusable_key(&prepared.namespace);
    if let Some(other) = catalogue
        .publishers
        .keys()
        .find(|ns| **ns != prepared.namespace && confusable_key(ns) == namespace_key)
    {
        return Err(format!(
            "The namespace '{}' looks like the existing namespace '{other}'.",
            prepared.namespace
        ));
    }
    match catalogue.publishers.get(&prepared.namespace) {
        Some(publisher) => notes.push(format!(
            "The namespace '{}' belongs to the GitHub account @{} (id {}). Only a pull \
             request from that account can be accepted.",
            prepared.namespace, publisher.github, publisher.github_id
        )),
        None => notes.push(format!(
            "The namespace '{}' is not claimed yet. Your pull request must also add \
             publishers/{}.json naming your GitHub account; the repository checks that the \
             pull request comes from that account.",
            prepared.namespace, prepared.namespace
        )),
    }
    Ok(notes)
}

/// What the catalogue needs from the submission (`catalogue-entry.json`).
#[derive(Serialize)]
struct CatalogueEntryFile<'a> {
    id: &'a str,
    version: String,
    entry: &'a str,
    manifest_format: u32,
    pwnode_version: u32,
    bundle_format: u32,
    min_patchwork: &'a str,
    capabilities: &'a [String],
    dependencies: Vec<String>,
    artifact: ArtifactSummary,
    /// The Library version it was made from (for the publisher's records).
    made_from: MadeFrom<'a>,
}

#[derive(Serialize)]
struct ArtifactSummary {
    format: u32,
    /// Where CI generates it in the repository.
    path: String,
    sha256: String,
    size: u64,
    files: u32,
}

#[derive(Serialize)]
struct MadeFrom<'a> {
    local_id: &'a str,
    tree_sha256: &'a str,
    patchwork: String,
}

/// `package.meta.json`: the listing metadata (layout §3).
#[derive(Serialize)]
struct PackageMeta<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    description: &'a str,
    author: &'a str,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    tags: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    license: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    documentation: &'a Option<String>,
}

/// The submission folder's name.
pub fn submission_name(prepared: &Prepared) -> String {
    format!(
        "patchwork-submission-{}-{}",
        prepared.public_id, prepared.version
    )
}

/// The contribution guide of the catalogue repository (opened in the
/// browser; never fetched by Patchwork).
pub fn contribution_url(location: &CatalogueLocation) -> String {
    format!(
        "https://github.com/{}/{}/blob/{}/CONTRIBUTING.md",
        location.source().owner(),
        location.source().repo(),
        location.branch()
    )
}

fn readme(prepared: &Prepared, location: &CatalogueLocation) -> String {
    let owner = location.source().owner();
    let repo = location.source().repo();
    let (ns, name, version) = (&prepared.namespace, &prepared.name, &prepared.version);
    let capabilities = if prepared.capabilities.is_empty() {
        "none".to_string()
    } else {
        prepared
            .capabilities
            .iter()
            .map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut notes = String::new();
    for note in &prepared.notes {
        notes.push_str(&format!("- {note}\n"));
    }
    format!(
        "# Submission: {id}@{version}\n\
         \n\
         Prepared by Patchwork {app} from the Library package `{local}`\n\
         (content `{tree}`). Nothing in it was run.\n\
         \n\
         | | |\n\
         |---|---|\n\
         | Package | `{id}` version `{version}` |\n\
         | Entry | `{entry}` |\n\
         | Capabilities | {capabilities} |\n\
         | Artifact SHA-256 | `{sha}` |\n\
         | Artifact size | {size} bytes, {files} files |\n\
         \n\
         {notes}\n\
         ## What is in this folder\n\
         \n\
         - `repository/`: the files to add to the catalogue repository, already in\n\
           its layout (`packages/{ns}/{name}/…`).\n\
         - `artifact/{id}-{version}.{ext}`: the artifact CI will build from those\n\
           files. Do **not** add it to the repository: CI generates `artifacts/` and\n\
           `index.json`, and must arrive at exactly the SHA-256 above.\n\
         - `catalogue-entry.json`: the catalogue data for this version.\n\
         \n\
         ## Submit it through GitHub\n\
         \n\
         Patchwork never asks for your GitHub password, token or login. GitHub\n\
         identifies you when you open the pull request.\n\
         \n\
         The catalogue is the `{branch}` branch of `{owner}/{repo}`.\n\
         \n\
         1. Sign in to GitHub in your browser and fork\n\
            <https://github.com/{owner}/{repo}>. Untick the option to copy only the\n\
            default branch, so the fork includes the `{branch}` branch.\n\
         2. In your fork, switch to the `{branch}` branch and copy everything inside\n\
            `repository/` into it, keeping the folder structure. Never change existing\n\
            files under `packages/` or `artifacts/`: published versions are immutable.\n\
         3. If your namespace `{ns}` is not claimed yet, add `publishers/{ns}.json` with\n\
            your GitHub login and numeric account id, as described in the repository's\n\
            CONTRIBUTING.md. The namespace must be your GitHub login in lowercase.\n\
         4. Commit to `{branch}` in your fork, push, and open a pull request against\n\
            the `{branch}` branch of `{owner}/{repo}` (base: `{branch}`), from the\n\
            GitHub account that owns `{ns}`.\n\
         5. Automated validation runs the same checks Patchwork ran. It only reads\n\
            the files; it never runs them. Fix anything it reports and push again.\n\
         6. A maintainer (and the namespace owner) reviews the pull request. Once\n\
            merged, the package appears in the marketplace.\n",
        id = prepared.public_id,
        app = super::current_app_version(),
        local = prepared.local_id,
        tree = prepared.local_tree_sha256,
        entry = prepared.entry,
        sha = prepared.sha256,
        size = prepared.size,
        files = prepared.file_count,
        ext = ARTIFACT_EXTENSION,
        branch = location.branch(),
    )
}

/// Write the submission folder into `out_dir` and return it. It is built in
/// a staging folder, read back and compared, then renamed into place; it
/// never replaces an existing folder, and nothing is left on failure or
/// cancellation.
pub fn write_submission(
    prepared: &Prepared,
    location: &CatalogueLocation,
    out_dir: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let destination = out_dir.join(submission_name(prepared));
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(format!(
            "{} already exists; it was not replaced.",
            destination.display()
        ));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let staging = out_dir.join(format!(
        ".patchwork-submission-staging-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&staging)
        .map_err(|e| format!("Cannot write to {}: {e}", out_dir.display()))?;
    let result = write_into(prepared, location, &staging, cancel).and_then(|()| {
        fail_at(FailPoint::BeforeCommit)?;
        if cancel.load(Ordering::SeqCst) {
            return Err("The export was cancelled.".to_string());
        }
        std::fs::rename(&staging, &destination)
            .map_err(|e| format!("Cannot create {}: {e}", destination.display()))
    });
    match result {
        Ok(()) => Ok(destination),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

fn write_into(
    prepared: &Prepared,
    location: &CatalogueLocation,
    staging: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let package_dir = format!(
        "repository/packages/{}/{}",
        prepared.namespace, prepared.name
    );
    let version_dir = format!("{package_dir}/{}", prepared.version);
    let listing = &prepared.listing;
    let version = &listing.versions[0];
    let meta = PackageMeta {
        name: &listing.name,
        description: &listing.description,
        author: &listing.author,
        tags: &listing.tags,
        license: &listing.license,
        documentation: &listing.documentation,
    };
    let entry_file = CatalogueEntryFile {
        id: &prepared.public_id,
        version: prepared.version.to_string(),
        entry: &prepared.entry,
        manifest_format: version.manifest_format,
        pwnode_version: version.pwnode_version,
        bundle_format: version.bundle_format,
        min_patchwork: &version.min_patchwork,
        capabilities: &version.capabilities,
        dependencies: prepared
            .dependencies
            .iter()
            .map(|(id, v)| format!("{id}@{v}"))
            .collect(),
        artifact: ArtifactSummary {
            format: ARTIFACT_FORMAT,
            path: artifact_repo_path(&prepared.public_id, &prepared.version),
            sha256: prepared.sha256.clone(),
            size: prepared.size,
            files: prepared.file_count,
        },
        made_from: MadeFrom {
            local_id: &prepared.local_id,
            tree_sha256: &prepared.local_tree_sha256,
            patchwork: super::current_app_version().to_string(),
        },
    };
    let mut outputs: Vec<(String, Vec<u8>)> = Vec::new();
    for (path, text) in &prepared.files {
        outputs.push((format!("{version_dir}/{path}"), text.as_bytes().to_vec()));
    }
    if let Some((name, bytes)) = &prepared.preview {
        outputs.push((format!("{version_dir}/{name}"), bytes.clone()));
    }
    outputs.push((format!("{package_dir}/package.meta.json"), pretty(&meta)?));
    outputs.push(("catalogue-entry.json".to_string(), pretty(&entry_file)?));
    outputs.push((
        format!(
            "artifact/{}-{}.{ARTIFACT_EXTENSION}",
            prepared.public_id, prepared.version
        ),
        prepared.artifact.clone(),
    ));
    outputs.push((
        "SUBMISSION.md".to_string(),
        readme(prepared, location).into_bytes(),
    ));
    for (index, (path, bytes)) in outputs.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err("The export was cancelled.".to_string());
        }
        fail_at(FailPoint::Writing(index))?;
        let full = path
            .split('/')
            .fold(staging.to_path_buf(), |p, s| p.join(s));
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("Cannot write {path}: {e}"))?;
        }
        std::fs::write(&full, bytes).map_err(|e| format!("Cannot write {path}: {e}"))?;
    }
    // Read back: what was written is exactly what was validated.
    for (path, bytes) in &outputs {
        let full = path
            .split('/')
            .fold(staging.to_path_buf(), |p, s| p.join(s));
        if std::fs::read(&full).map_err(|e| e.to_string())? != *bytes {
            return Err(format!("{path} was not written correctly."));
        }
    }
    Ok(())
}

/// Pretty JSON for the small metadata files.
fn pretty<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// The listing metadata of a prepared submission, for display.
pub fn describe_dependencies(prepared: &Prepared) -> String {
    if prepared.dependencies.is_empty() {
        return "none".to_string();
    }
    prepared
        .dependencies
        .iter()
        .map(|(id, version)| format!("{id}@{version} (bundled)"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Defaults for the share form, from the local manifest.
pub fn request_from(manifest: &PackageManifest) -> ShareRequest {
    let slug = manifest
        .id
        .strip_prefix(LOCAL_PREFIX)
        .unwrap_or(&manifest.id)
        .to_string();
    ShareRequest {
        namespace: String::new(),
        name: slug,
        display_name: manifest.name.clone(),
        description: manifest.description.clone(),
        author: manifest.author.clone(),
        tags: manifest.tags.clone(),
        license: None,
        documentation: None,
        min_patchwork: manifest
            .min_app_version
            .as_deref()
            .and_then(|v| crate::library::package::parse_stable_version(v).ok())
            .unwrap_or_else(super::current_app_version),
        preview: None,
    }
}

#[cfg(test)]
mod tests;
