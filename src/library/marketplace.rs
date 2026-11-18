//! P4.3.0: Public node marketplace — catalogue contract and validation.
//!
//! Pure data model for the public catalogue (`index.json`) hosted in a
//! GitHub repository. No networking, UI or installation lives here; see
//! `docs/marketplace/architecture.md` for the architecture and the P4.3.1
//! plan.
//!
//! Everything parsed here is untrusted input:
//!
//! * Parsing is all-or-nothing. A catalogue with any structural or semantic
//!   error is rejected as a whole and callers keep their last good copy.
//! * Only forward-compatible differences are accepted, and reported per
//!   version (see [`Compatibility`]): a newer minimum Patchwork, newer
//!   package formats, or capabilities this Patchwork does not know.
//! * The catalogue never makes a package trusted. It only says where an
//!   immutable artifact is and what its SHA-256 must be; installing still
//!   verifies the artifact like any other package version, and its declared
//!   capabilities are checked against its actual contents.
//!
//! Catalogue metadata ([`CatalogueVersion`]) is separate from the installed
//! [`PackageManifest`]; [`check_manifest_matches`] ties the two together.

// P4.3.0 defines the contract; its first callers arrive in P4.3.1.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::bundle::{BUNDLE_FORMAT, DEPS_DIR};
use super::package::{
    LOCAL_PREFIX, MANIFEST_FILE, MANIFEST_FORMAT, PackageManifest, parse_stable_version,
    validate_entry_name, validate_package_id,
};

/// `kind` of every catalogue file.
pub const CATALOGUE_KIND: &str = "patchwork-catalogue";
/// Catalogue format this Patchwork reads.
pub const CATALOGUE_FORMAT: u32 = 1;
/// `.pwpkg` artifact envelope format this Patchwork reads.
pub const ARTIFACT_FORMAT: u32 = 1;
/// Artifact file extension.
pub const ARTIFACT_EXTENSION: &str = "pwpkg";
/// Custom node (`.pwnode`) format this Patchwork reads.
pub const PWNODE_FORMAT: u32 = 1;
/// `PackageManifest::source` of an installed marketplace package.
pub const MARKETPLACE_SOURCE: &str = "marketplace";
/// The only host Patchwork downloads catalogue content from.
pub const RAW_HOST: &str = "raw.githubusercontent.com";
/// Namespace reserved for the catalogue maintainers (see
/// [`validate_publisher_claim`]).
pub const MAINTAINER_NAMESPACE: &str = "patchwork";
/// Namespaces no publisher can claim. `local` is used by packages
/// published on this machine, so marketplace ids never collide with them.
pub const RESERVED_NAMESPACES: &[&str] = &[
    "local",
    "builtin",
    "core",
    "std",
    "system",
    "test",
    "tests",
    "example",
    "examples",
    "marketplace",
    "official",
    MAINTAINER_NAMESPACE,
];
/// File names allowed for a version's preview image.
pub const PREVIEW_FILES: &[&str] = &["preview.png", "preview.jpg", "preview.jpeg", "preview.webp"];

/// Hard limits for untrusted marketplace input.
pub mod limits {
    /// Largest `index.json` accepted (checked before parsing).
    pub const MAX_CATALOGUE_BYTES: usize = 8 * 1024 * 1024;
    pub const MAX_PUBLISHERS: usize = 5_000;
    pub const MAX_PACKAGES: usize = 10_000;
    pub const MAX_VERSIONS_PER_PACKAGE: usize = 200;
    pub const MAX_TAGS: usize = 12;
    pub const MAX_TAG_LEN: usize = 32;
    pub const MAX_NAME_LEN: usize = 100;
    pub const MAX_AUTHOR_LEN: usize = 100;
    pub const MAX_DESCRIPTION_LEN: usize = 2_000;
    pub const MAX_LICENSE_LEN: usize = 64;
    pub const MAX_REASON_LEN: usize = 500;
    pub const MAX_URL_LEN: usize = 2_048;
    pub const MAX_CAPABILITIES: usize = 16;
    /// Largest `.pwpkg` artifact downloaded (the download is aborted past
    /// the catalogue's declared size, and never exceeds this).
    pub const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
    /// Most files in one artifact (root manifest + entry + bundled deps).
    pub const MAX_ARTIFACT_FILES: u32 = 256;
    /// Largest single file inside an artifact.
    pub const MAX_ARTIFACT_FILE_BYTES: u64 = 8 * 1024 * 1024;
    pub const MAX_ARTIFACT_PATH_LEN: usize = 200;
    pub const MAX_ENTRY_NAME_LEN: usize = 100;
    pub const MAX_PREVIEW_BYTES: u64 = 2 * 1024 * 1024;
    /// Most problems reported for one catalogue.
    pub const MAX_ISSUES: usize = 100;
}

use limits::*;

// ── Wire format (index.json) ─────────────────────────────────────────────
//
// Unknown fields are ignored everywhere (forward compatibility); they can
// never change what a known field means. Duplicate keys in one object are
// rejected by serde.

/// `index.json` as published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueFile {
    pub kind: String,
    pub catalogue_format: u32,
    /// Unix seconds; a refresh never goes backwards.
    #[serde(default)]
    pub generated_at: Option<u64>,
    #[serde(default)]
    pub publishers: Vec<PublisherEntry>,
    #[serde(default)]
    pub packages: Vec<PackageEntry>,
}

/// A claimed namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublisherEntry {
    pub namespace: String,
    /// GitHub login that owns the namespace.
    pub github: String,
    /// Numeric GitHub account id (survives renames, detects recycled logins).
    pub github_id: u64,
    pub display_name: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// A package: listing metadata plus its published versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageEntry {
    /// `<namespace>.<name>`: the identity. The display name never is.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub license: Option<String>,
    /// Opened in the user's browser only; never fetched by Patchwork.
    #[serde(default)]
    pub documentation: Option<String>,
    pub versions: Vec<VersionEntry>,
}

/// One immutable published version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionEntry {
    /// Exact `MAJOR.MINOR.PATCH`.
    pub version: String,
    /// Entry `.pwnode` file name inside the version folder.
    pub entry: String,
    /// `PackageManifest::format` of the artifact's `package.json`.
    pub manifest_format: u32,
    pub pwnode_version: u32,
    #[serde(default)]
    pub bundle_format: u32,
    /// Oldest Patchwork that can use this version.
    pub min_patchwork: String,
    /// Privileged capabilities the version uses (see [`Capability`]).
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub artifact: ArtifactEntry,
    #[serde(default)]
    pub preview: Option<PreviewEntry>,
    #[serde(default)]
    pub published_at: Option<u64>,
    /// Reason, when the maintainers withdrew this version. Installed copies
    /// keep working; it is not offered for new installs.
    #[serde(default)]
    pub yanked: Option<String>,
}

/// The downloadable `.pwpkg` artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactEntry {
    pub format: u32,
    /// `https://raw.githubusercontent.com/<owner>/<repo>/<commit>/artifacts/<id>/<version>/<id>-<version>.pwpkg`
    pub url: String,
    /// Lowercase hex SHA-256 of the complete artifact file.
    pub sha256: String,
    /// Exact artifact size in bytes.
    pub size: u64,
    /// Number of files inside the artifact.
    pub files: u32,
}

/// Optional preview image (fetched by Patchwork, so pinned and hashed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewEntry {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

// ── Validated model ──────────────────────────────────────────────────────

/// Where the catalogue lives. Artifact and preview URLs must point into
/// this same repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueSource {
    owner: String,
    repo: String,
}

impl CatalogueSource {
    pub fn new(owner: &str, repo: &str) -> Result<Self, String> {
        validate_github_login(owner)?;
        let repo_ok = !repo.is_empty()
            && repo.len() <= 100
            && repo != "."
            && repo != ".."
            && repo
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !repo_ok {
            return Err(format!("'{repo}' is not a valid GitHub repository name."));
        }
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }
}

/// A validated catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalogue {
    pub generated_at: Option<u64>,
    /// By namespace.
    pub publishers: BTreeMap<String, Publisher>,
    /// By package id.
    pub packages: BTreeMap<String, CataloguePackage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publisher {
    pub namespace: String,
    pub github: String,
    pub github_id: u64,
    pub display_name: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CataloguePackage {
    pub id: String,
    pub namespace: String,
    /// The part of the id after the namespace.
    pub local_name: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub tags: Vec<String>,
    pub license: Option<String>,
    pub documentation: Option<String>,
    pub versions: BTreeMap<semver::Version, CatalogueVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueVersion {
    pub version: semver::Version,
    pub entry: String,
    pub manifest_format: u32,
    pub pwnode_version: u32,
    pub bundle_format: u32,
    pub min_patchwork: semver::Version,
    pub capabilities: Vec<DeclaredCapability>,
    pub artifact: Artifact,
    pub preview: Option<Preview>,
    pub published_at: Option<u64>,
    pub yanked: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub format: u32,
    pub url: String,
    /// The pinned commit in the URL.
    pub commit: String,
    pub sha256: String,
    pub size: u64,
    pub files: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl Catalogue {
    pub fn package(&self, id: &str) -> Option<&CataloguePackage> {
        self.packages.get(id)
    }

    pub fn version(&self, id: &str, version: &semver::Version) -> Option<&CatalogueVersion> {
        self.packages.get(id)?.versions.get(version)
    }
}

impl CataloguePackage {
    /// Versions a user may choose to install on `app` (exact versions only;
    /// nothing is ever upgraded automatically).
    pub fn installable_versions(&self, app: &semver::Version) -> Vec<&CatalogueVersion> {
        self.versions
            .values()
            .filter(|v| v.is_installable(app))
            .collect()
    }
}

// ── Capabilities ─────────────────────────────────────────────────────────

/// Privileged capabilities a node graph can use. Declared per version in
/// the catalogue, and (from P4.3.1) computed from the artifact's graphs:
/// an artifact using anything it does not declare is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Reads or writes files outside the package (file/folder browsers,
    /// media and model loaders).
    Filesystem,
    /// Sends or receives over a network (OSC, Art-Net, HTTP, AI requests,
    /// network send/receive, MCP).
    Network,
    /// Talks to hardware (MIDI, serial, DMX, HID, controller hubs).
    Devices,
    /// Captures camera or microphone input.
    MediaCapture,
    /// Runs user-supplied script or shader code.
    Scripting,
    /// P4.3.2B: starts other programs (ffmpeg, yt-dlp, espeak-ng, lp,
    /// system tools) with arguments derived from the graph.
    Process,
    /// P4.3.2B: reads information about this computer (hardware, load,
    /// running processes).
    SystemInfo,
    /// P4.3.2B: uses the AI service keys and endpoints stored in this
    /// Patchwork (spending the user's quota under the user's identity).
    AiCredentials,
    /// Loads native code (CLAP or Rust plugins). Never allowed in the
    /// public catalogue.
    NativeCode,
}

impl Capability {
    pub const ALL: [Capability; 9] = [
        Capability::Filesystem,
        Capability::Network,
        Capability::Devices,
        Capability::MediaCapture,
        Capability::Scripting,
        Capability::Process,
        Capability::SystemInfo,
        Capability::AiCredentials,
        Capability::NativeCode,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Filesystem => "filesystem",
            Capability::Network => "network",
            Capability::Devices => "devices",
            Capability::MediaCapture => "media_capture",
            Capability::Scripting => "scripting",
            Capability::Process => "process",
            Capability::SystemInfo => "system_info",
            Capability::AiCredentials => "ai_credentials",
            Capability::NativeCode => "native_code",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == text)
    }
}

/// A declared capability; unknown ones come from newer catalogues and make
/// the version incompatible with this Patchwork (never silently ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredCapability {
    Known(Capability),
    Unknown(String),
}

// ── Compatibility ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    Compatible,
    NeedsNewerPatchwork(semver::Version),
    UnsupportedFormat(String),
    UnknownCapabilities(Vec<String>),
}

/// This Patchwork's version.
pub fn current_app_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

impl CatalogueVersion {
    pub fn compatibility(&self, app: &semver::Version) -> Compatibility {
        if self.manifest_format != MANIFEST_FORMAT {
            return Compatibility::UnsupportedFormat(format!(
                "package manifest format {}",
                self.manifest_format
            ));
        }
        if self.pwnode_version != PWNODE_FORMAT {
            return Compatibility::UnsupportedFormat(format!(
                "custom node format {}",
                self.pwnode_version
            ));
        }
        if self.bundle_format > BUNDLE_FORMAT {
            return Compatibility::UnsupportedFormat(format!(
                "dependency bundle format {}",
                self.bundle_format
            ));
        }
        if self.artifact.format != ARTIFACT_FORMAT {
            return Compatibility::UnsupportedFormat(format!(
                "artifact format {}",
                self.artifact.format
            ));
        }
        let unknown: Vec<String> = self
            .capabilities
            .iter()
            .filter_map(|c| match c {
                DeclaredCapability::Unknown(name) => Some(name.clone()),
                DeclaredCapability::Known(_) => None,
            })
            .collect();
        if !unknown.is_empty() {
            return Compatibility::UnknownCapabilities(unknown);
        }
        if *app < self.min_patchwork {
            return Compatibility::NeedsNewerPatchwork(self.min_patchwork.clone());
        }
        Compatibility::Compatible
    }

    /// Offered for a new install: compatible and not yanked.
    pub fn is_installable(&self, app: &semver::Version) -> bool {
        self.yanked.is_none() && self.compatibility(app) == Compatibility::Compatible
    }

    pub fn known_capabilities(&self) -> BTreeSet<Capability> {
        self.capabilities
            .iter()
            .filter_map(|c| match c {
                DeclaredCapability::Known(c) => Some(*c),
                DeclaredCapability::Unknown(_) => None,
            })
            .collect()
    }
}

// ── Identity and namespaces ──────────────────────────────────────────────

/// A GitHub login: `[A-Za-z0-9-]`, 1–39 chars, no leading, trailing or
/// doubled `-`.
pub fn validate_github_login(login: &str) -> Result<(), String> {
    let ok = !login.is_empty()
        && login.len() <= 39
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !login.starts_with('-')
        && !login.ends_with('-')
        && !login.contains("--");
    if ok {
        Ok(())
    } else {
        Err(format!("'{login}' is not a valid GitHub login."))
    }
}

/// A namespace: a lowercase GitHub login, at least 2 characters.
pub fn validate_namespace(namespace: &str) -> Result<(), String> {
    let ok = namespace.len() >= 2
        && validate_github_login(namespace).is_ok()
        && namespace
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "'{namespace}' is not a valid namespace (lowercase letters, digits and single '-', \
             2–39 characters)."
        ))
    }
}

pub fn is_reserved_namespace(namespace: &str) -> bool {
    RESERVED_NAMESPACES.contains(&namespace)
}

/// Split a public package id `<namespace>.<name>`.
///
/// Public ids are valid package ids with exactly one `.`; `local.*` is
/// reserved for packages published on this machine.
pub fn split_public_id(id: &str) -> Result<(&str, &str), String> {
    validate_package_id(id)?;
    if id.starts_with(LOCAL_PREFIX) {
        return Err(format!(
            "'{id}' uses the '{LOCAL_PREFIX}' prefix, which is reserved for packages published \
             on this machine."
        ));
    }
    let (namespace, name) = id
        .split_once('.')
        .ok_or_else(|| format!("'{id}' must be '<namespace>.<name>'."))?;
    validate_namespace(namespace)?;
    let name_ok = !name.is_empty()
        && !name.contains('.')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
        && !name.ends_with(['-', '_']);
    if !name_ok {
        return Err(format!(
            "'{id}' must be '<namespace>.<name>' with a name of lowercase letters, digits, '-' \
             and '_'."
        ));
    }
    Ok((namespace, name))
}

/// Key used to detect look-alike identifiers (`acme-audio` / `acmeaudio`,
/// `my-gain` / `my_gain`): separators removed.
pub fn confusable_key(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '-' | '_' | '.'))
        .collect()
}

/// Ownership rule: a namespace equals its owner's lowercase GitHub login
/// (GitHub logins are unique, so unrelated publishers cannot share one).
/// The maintainer namespace belongs to the catalogue repository's owner.
/// Other reserved namespaces cannot be claimed.
pub fn validate_publisher_claim(
    namespace: &str,
    github: &str,
    source: &CatalogueSource,
) -> Result<(), String> {
    validate_namespace(namespace)?;
    validate_github_login(github)?;
    if namespace == MAINTAINER_NAMESPACE {
        if github.eq_ignore_ascii_case(source.owner()) {
            return Ok(());
        }
        return Err(format!(
            "Namespace '{namespace}' is reserved for the catalogue maintainers ({}).",
            source.owner()
        ));
    }
    if is_reserved_namespace(namespace) {
        return Err(format!("Namespace '{namespace}' is reserved."));
    }
    if namespace != github.to_ascii_lowercase() {
        return Err(format!(
            "Namespace '{namespace}' must be the owner's GitHub login in lowercase ('{}').",
            github.to_ascii_lowercase()
        ));
    }
    Ok(())
}

// ── Text, digests, URLs and paths ────────────────────────────────────────

fn is_invisible_or_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

/// Display text: bounded, no control characters (newlines only when
/// `multiline`), no invisible or bidirectional-override characters.
fn check_text(
    value: &str,
    max_chars: usize,
    multiline: bool,
    required: bool,
) -> Result<(), String> {
    if required && value.trim().is_empty() {
        return Err("is empty".to_string());
    }
    if value.chars().count() > max_chars {
        return Err(format!("is longer than {max_chars} characters"));
    }
    if value
        .chars()
        .any(|c| (c.is_control() && !(multiline && c == '\n')) || is_invisible_or_bidi(c))
    {
        return Err("contains control, invisible or bidirectional characters".to_string());
    }
    Ok(())
}

/// Lowercase 64-digit hex SHA-256 (never the all-zero placeholder).
pub fn validate_sha256(value: &str) -> Result<(), String> {
    let ok = value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        && value.chars().any(|c| c != '0');
    if ok {
        Ok(())
    } else {
        Err("must be a lowercase hex SHA-256 digest (64 characters)".to_string())
    }
}

fn is_commit_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Parsed `https://` URL.
struct HttpsUrl<'a> {
    host: &'a str,
    /// Path without its leading `/`, before any query or fragment.
    path: &'a str,
    has_query_or_fragment: bool,
}

fn parse_https_url(url: &str) -> Result<HttpsUrl<'_>, String> {
    if url.len() > MAX_URL_LEN {
        return Err(format!("is longer than {MAX_URL_LEN} characters"));
    }
    if !url.chars().all(|c| c.is_ascii_graphic()) || url.contains('\\') {
        return Err("must be printable ASCII without spaces or backslashes".to_string());
    }
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| "must be an https:// URL".to_string())?;
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(split);
    if authority.contains('@') {
        return Err("must not contain credentials".to_string());
    }
    if authority.contains(':') {
        return Err("must not specify a port".to_string());
    }
    let labels: Vec<&str> = authority.split('.').collect();
    let host_ok = authority.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        && labels
            .last()
            .is_some_and(|tld| tld.chars().any(|c| c.is_ascii_lowercase()));
    if !host_ok {
        return Err(format!("has an invalid host '{authority}'"));
    }
    let query = tail.find(['?', '#']).unwrap_or(tail.len());
    let (path, extra) = tail.split_at(query);
    Ok(HttpsUrl {
        host: authority,
        path: path.strip_prefix('/').unwrap_or(path),
        has_query_or_fragment: !extra.is_empty(),
    })
}

/// A link opened in the user's browser (documentation, publisher page).
fn check_link(url: &str) -> Result<(), String> {
    parse_https_url(url).map(|_| ())
}

/// A URL Patchwork downloads from: `raw.githubusercontent.com`, inside the
/// catalogue repository, pinned to a full commit, with plain path segments.
/// Returns the commit and the path segments after it.
fn parse_pinned_raw<'a>(
    url: &'a str,
    source: &CatalogueSource,
) -> Result<(&'a str, Vec<&'a str>), String> {
    let parsed = parse_https_url(url)?;
    if parsed.host != RAW_HOST {
        return Err(format!("must be hosted on {RAW_HOST}"));
    }
    if parsed.has_query_or_fragment || url.contains('%') {
        return Err("must not contain a query, fragment or percent-encoding".to_string());
    }
    let segments: Vec<&str> = parsed.path.split('/').collect();
    if segments.iter().any(|s| {
        s.is_empty()
            || *s == "."
            || *s == ".."
            || !s
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    }) {
        return Err("contains an unsafe path segment".to_string());
    }
    if segments.len() < 4 {
        return Err("must name an owner, repository, commit and file".to_string());
    }
    if !segments[0].eq_ignore_ascii_case(source.owner())
        || !segments[1].eq_ignore_ascii_case(source.repo())
    {
        return Err(format!(
            "must point into the catalogue repository {}/{}",
            source.owner(),
            source.repo()
        ));
    }
    if !is_commit_sha(segments[2]) {
        return Err("must be pinned to a full 40-character commit hash".to_string());
    }
    Ok((segments[2], segments[3..].to_vec()))
}

/// Exact path of a version's artifact inside the catalogue repository.
pub fn artifact_repo_path(id: &str, version: &semver::Version) -> String {
    format!("artifacts/{id}/{version}/{id}-{version}.{ARTIFACT_EXTENSION}")
}

/// File names Windows cannot create (`CON`, `NUL`, `COM1`, …) or silently
/// changes (trailing `.` or space).
fn is_windows_unsafe(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    device || name.ends_with('.') || name.ends_with(' ')
}

/// A marketplace entry name: a valid package entry, plain ASCII, safe on
/// every platform.
pub fn validate_marketplace_entry(entry: &str) -> Result<(), String> {
    validate_entry_name(entry)?;
    let ok = entry.len() <= MAX_ENTRY_NAME_LEN
        && entry
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '))
        && !entry.starts_with(' ')
        && !is_windows_unsafe(entry);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "Entry '{entry}' must be a short ASCII file name that is valid on every platform."
        ))
    }
}

fn canonical_version(text: &str) -> Result<semver::Version, String> {
    let version = parse_stable_version(text)?;
    if version.to_string() != text {
        return Err(format!(
            "'{text}' is not written canonically ('{version}')."
        ));
    }
    Ok(version)
}

// ── Artifact contents (used by P4.3.1 extraction) ────────────────────────

/// A file inside a `.pwpkg` artifact. Only the layout of a verified
/// package version is allowed: nothing executable, no other files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactPath {
    /// `package.json`
    Manifest,
    /// The root entry `.pwnode`.
    Entry,
    /// `deps/<id>/<version>/package.json`
    DependencyManifest {
        id: String,
        version: semver::Version,
    },
    /// `deps/<id>/<version>/<file>.pwnode`
    DependencyFile {
        id: String,
        version: semver::Version,
        file: String,
    },
}

/// Validate one artifact file path (`/`-separated, relative) against the
/// root `entry`. Rejects traversal, absolute and drive paths, backslashes,
/// unknown files and names unsafe on Windows.
pub fn validate_artifact_path(path: &str, entry: &str) -> Result<ArtifactPath, String> {
    let bad = |why: &str| Err(format!("Artifact path '{path}' {why}."));
    if path.is_empty() || path.len() > MAX_ARTIFACT_PATH_LEN {
        return bad("is empty or too long");
    }
    if path.contains(['\\', ':']) || path.starts_with('/') {
        return bad("is not a plain relative path");
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return bad("contains an empty, '.' or '..' segment");
    }
    match segments.as_slice() {
        [file] if *file == MANIFEST_FILE => Ok(ArtifactPath::Manifest),
        [file] if *file == entry => {
            validate_marketplace_entry(file)?;
            Ok(ArtifactPath::Entry)
        }
        [deps, id, version, file] if *deps == DEPS_DIR => {
            validate_package_id(id)?;
            let version = canonical_version(version)?;
            if *file == MANIFEST_FILE {
                return Ok(ArtifactPath::DependencyManifest {
                    id: id.to_string(),
                    version,
                });
            }
            validate_marketplace_entry(file)?;
            Ok(ArtifactPath::DependencyFile {
                id: id.to_string(),
                version,
                file: file.to_string(),
            })
        }
        _ => bad("is not part of a package version layout"),
    }
}

/// Validate an artifact's complete file listing: limits, no duplicates
/// (case-insensitively, as on Windows and macOS), root manifest and entry
/// present, and a manifest for every bundled dependency.
pub fn check_artifact_listing<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    entry: &str,
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    let mut manifest = false;
    let mut root_entry = false;
    let mut dep_dirs = BTreeSet::new();
    let mut dep_manifests = BTreeSet::new();
    for path in paths {
        if seen.len() >= MAX_ARTIFACT_FILES as usize {
            return Err(format!(
                "The artifact has more than {MAX_ARTIFACT_FILES} files."
            ));
        }
        if !seen.insert(path.to_lowercase()) {
            return Err(format!("The artifact lists '{path}' more than once."));
        }
        match validate_artifact_path(path, entry)? {
            ArtifactPath::Manifest => manifest = true,
            ArtifactPath::Entry => root_entry = true,
            ArtifactPath::DependencyManifest { id, version } => {
                dep_manifests.insert((id, version));
            }
            ArtifactPath::DependencyFile { id, version, .. } => {
                dep_dirs.insert((id, version));
            }
        }
    }
    if !manifest || !root_entry {
        return Err(format!(
            "The artifact must contain {MANIFEST_FILE} and its entry {entry}."
        ));
    }
    if let Some((id, version)) = dep_dirs.difference(&dep_manifests).next() {
        return Err(format!(
            "The bundled dependency {id}@{version} has no {MANIFEST_FILE}."
        ));
    }
    Ok(())
}

// ── Parsing ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogueErrorKind {
    /// Larger than [`limits::MAX_CATALOGUE_BYTES`].
    TooLarge,
    /// Not UTF-8 JSON of the catalogue shape.
    Malformed,
    /// A catalogue format this Patchwork cannot read (update Patchwork).
    UnsupportedFormat(u64),
    /// Well-formed but breaks the contract.
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueError {
    pub kind: CatalogueErrorKind,
    pub issues: Vec<String>,
}

impl CatalogueError {
    fn one(kind: CatalogueErrorKind, issue: impl Into<String>) -> Self {
        Self {
            kind,
            issues: vec![issue.into()],
        }
    }
}

impl std::fmt::Display for CatalogueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.issues.join("\n"))
    }
}

/// Parse and validate an untrusted `index.json`.
pub fn parse_catalogue(
    bytes: &[u8],
    source: &CatalogueSource,
) -> Result<Catalogue, CatalogueError> {
    if bytes.len() > MAX_CATALOGUE_BYTES {
        return Err(CatalogueError::one(
            CatalogueErrorKind::TooLarge,
            format!("The catalogue is larger than {MAX_CATALOGUE_BYTES} bytes."),
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        CatalogueError::one(CatalogueErrorKind::Malformed, "The catalogue is not UTF-8.")
    })?;

    // Format gate first, so a future format is reported as such rather than
    // as a confusing shape error.
    #[derive(Deserialize)]
    struct Header {
        #[serde(default)]
        kind: Option<serde_json::Value>,
        #[serde(default)]
        catalogue_format: Option<serde_json::Value>,
    }
    let malformed = |e: serde_json::Error| {
        CatalogueError::one(
            CatalogueErrorKind::Malformed,
            format!("Invalid catalogue JSON: {e}"),
        )
    };
    let header: Header = serde_json::from_str(text).map_err(malformed)?;
    if header.kind.as_ref().and_then(|k| k.as_str()) != Some(CATALOGUE_KIND) {
        return Err(CatalogueError::one(
            CatalogueErrorKind::Malformed,
            format!("This is not a Patchwork catalogue (kind must be '{CATALOGUE_KIND}')."),
        ));
    }
    let format = match header.catalogue_format.as_ref().map(|f| f.as_u64()) {
        None => {
            return Err(CatalogueError::one(
                CatalogueErrorKind::Malformed,
                "The catalogue has no catalogue_format.",
            ));
        }
        Some(None) => {
            return Err(CatalogueError::one(
                CatalogueErrorKind::Malformed,
                "catalogue_format must be a whole number.",
            ));
        }
        Some(Some(format)) => format,
    };
    if format == 0 {
        return Err(CatalogueError::one(
            CatalogueErrorKind::Malformed,
            "catalogue_format 0 is not valid.",
        ));
    }
    if format != u64::from(CATALOGUE_FORMAT) {
        return Err(CatalogueError::one(
            CatalogueErrorKind::UnsupportedFormat(format),
            format!(
                "The catalogue uses format {format}; this Patchwork reads format \
                 {CATALOGUE_FORMAT}. Update Patchwork to browse it."
            ),
        ));
    }
    let file: CatalogueFile = serde_json::from_str(text).map_err(malformed)?;
    validate_catalogue(&file, source)
}

#[derive(Default)]
struct Checker {
    issues: Vec<String>,
}

impl Checker {
    fn fail(&mut self, at: &str, message: impl std::fmt::Display) {
        if self.issues.len() < MAX_ISSUES {
            self.issues.push(format!("{at}: {message}"));
        }
    }

    fn check(&mut self, at: &str, result: Result<(), String>) -> bool {
        match result {
            Ok(()) => true,
            Err(e) => {
                self.fail(at, e);
                false
            }
        }
    }
}

/// Validate a parsed catalogue file.
pub fn validate_catalogue(
    file: &CatalogueFile,
    source: &CatalogueSource,
) -> Result<Catalogue, CatalogueError> {
    let invalid = |issues| CatalogueError {
        kind: CatalogueErrorKind::Invalid,
        issues,
    };
    if file.kind != CATALOGUE_KIND || file.catalogue_format != CATALOGUE_FORMAT {
        return Err(invalid(vec![
            "Unsupported catalogue kind or format.".to_string(),
        ]));
    }
    if file.publishers.len() > MAX_PUBLISHERS || file.packages.len() > MAX_PACKAGES {
        return Err(invalid(vec![format!(
            "The catalogue lists more than {MAX_PUBLISHERS} publishers or {MAX_PACKAGES} packages."
        )]));
    }
    let mut c = Checker::default();

    // Publishers.
    let mut publishers: BTreeMap<String, Publisher> = BTreeMap::new();
    let mut by_account: BTreeMap<u64, String> = BTreeMap::new();
    let mut by_key: BTreeMap<String, String> = BTreeMap::new();
    for (i, entry) in file.publishers.iter().enumerate() {
        let at = format!("publishers[{i}]");
        let Some(publisher) = validate_publisher(&mut c, &at, entry, source) else {
            continue;
        };
        if let Some(existing) = publishers.get(&publisher.namespace) {
            if *existing != publisher {
                c.fail(
                    &at,
                    format!("namespace '{}' is claimed twice", publisher.namespace),
                );
            }
            continue;
        }
        if let Some(other) = by_account.insert(publisher.github_id, publisher.namespace.clone()) {
            c.fail(
                &at,
                format!(
                    "GitHub account {} already owns namespace '{other}'",
                    publisher.github_id
                ),
            );
            continue;
        }
        if let Some(other) = by_key.insert(
            confusable_key(&publisher.namespace),
            publisher.namespace.clone(),
        ) {
            c.fail(
                &at,
                format!("namespace '{}' looks like '{other}'", publisher.namespace),
            );
            continue;
        }
        publishers.insert(publisher.namespace.clone(), publisher);
    }

    // Packages.
    let mut packages: BTreeMap<String, CataloguePackage> = BTreeMap::new();
    let mut package_meta: BTreeMap<String, PackageEntry> = BTreeMap::new();
    let mut version_wire: BTreeMap<(String, semver::Version), VersionEntry> = BTreeMap::new();
    let mut name_keys: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    for (i, entry) in file.packages.iter().enumerate() {
        let at = format!("packages[{i}]");
        let Some(mut package) = validate_package(&mut c, &at, entry, &publishers) else {
            continue;
        };
        let meta = PackageEntry {
            versions: Vec::new(),
            ..entry.clone()
        };
        match package_meta.get(&package.id) {
            Some(existing) if *existing != meta => {
                c.fail(
                    &at,
                    format!(
                        "package '{}' is listed twice with different metadata",
                        package.id
                    ),
                );
                continue;
            }
            Some(_) => {}
            None => {
                let key = (
                    package.namespace.clone(),
                    confusable_key(&package.local_name),
                );
                if let Some(other) = name_keys.get(&key) {
                    c.fail(
                        &at,
                        format!("package '{}' looks like '{other}'", package.id),
                    );
                    continue;
                }
                name_keys.insert(key, package.id.clone());
                package_meta.insert(package.id.clone(), meta);
            }
        }
        for (j, wire) in entry.versions.iter().enumerate() {
            let vat = format!("{at}.versions[{j}]");
            let Some(version) = validate_version(&mut c, &vat, &package, wire, source) else {
                continue;
            };
            let key = (package.id.clone(), version.version.clone());
            if let Some(existing) = version_wire.get(&key) {
                if existing != wire {
                    c.fail(
                        &vat,
                        format!(
                            "{}@{} is listed twice with different metadata",
                            package.id, version.version
                        ),
                    );
                }
                continue;
            }
            let label = format!("{}@{}", package.id, version.version);
            if let Some(other) = digests.insert(version.artifact.sha256.clone(), label.clone()) {
                c.fail(
                    &vat,
                    format!("{label} has the same artifact digest as {other}"),
                );
                continue;
            }
            version_wire.insert(key, wire.clone());
            package.versions.insert(version.version.clone(), version);
        }
        let merged = packages
            .entry(package.id.clone())
            .or_insert_with(|| CataloguePackage {
                versions: BTreeMap::new(),
                ..package.clone()
            });
        merged.versions.append(&mut package.versions);
        if merged.versions.len() > MAX_VERSIONS_PER_PACKAGE {
            c.fail(
                &at,
                format!("more than {MAX_VERSIONS_PER_PACKAGE} versions"),
            );
        }
    }
    for (id, package) in &packages {
        if package.versions.is_empty() && package_meta.contains_key(id) {
            c.fail(&format!("package '{id}'"), "has no valid versions");
        }
    }

    if !c.issues.is_empty() {
        return Err(invalid(c.issues));
    }
    Ok(Catalogue {
        generated_at: file.generated_at,
        publishers,
        packages,
    })
}

fn validate_publisher(
    c: &mut Checker,
    at: &str,
    entry: &PublisherEntry,
    source: &CatalogueSource,
) -> Option<Publisher> {
    let mut ok = c.check(
        at,
        validate_publisher_claim(&entry.namespace, &entry.github, source),
    );
    if entry.github_id == 0 {
        c.fail(at, "github_id is missing");
        ok = false;
    }
    ok &= c.check(
        &format!("{at}.display_name"),
        check_text(&entry.display_name, MAX_NAME_LEN, false, true),
    );
    if let Some(url) = &entry.url {
        ok &= c.check(&format!("{at}.url"), check_link(url));
    }
    ok.then(|| Publisher {
        namespace: entry.namespace.clone(),
        github: entry.github.clone(),
        github_id: entry.github_id,
        display_name: entry.display_name.clone(),
        url: entry.url.clone(),
    })
}

fn validate_package(
    c: &mut Checker,
    at: &str,
    entry: &PackageEntry,
    publishers: &BTreeMap<String, Publisher>,
) -> Option<CataloguePackage> {
    let (namespace, local_name) = match split_public_id(&entry.id) {
        Ok(parts) => parts,
        Err(e) => {
            c.fail(&format!("{at}.id"), e);
            return None;
        }
    };
    let mut ok = true;
    if !publishers.contains_key(namespace) {
        c.fail(
            &format!("{at}.id"),
            format!("namespace '{namespace}' is not claimed by any publisher"),
        );
        ok = false;
    }
    ok &= c.check(
        &format!("{at}.name"),
        check_text(&entry.name, MAX_NAME_LEN, false, true),
    );
    ok &= c.check(
        &format!("{at}.description"),
        check_text(&entry.description, MAX_DESCRIPTION_LEN, true, false),
    );
    ok &= c.check(
        &format!("{at}.author"),
        check_text(&entry.author, MAX_AUTHOR_LEN, false, true),
    );
    if entry.tags.len() > MAX_TAGS {
        c.fail(&format!("{at}.tags"), format!("more than {MAX_TAGS} tags"));
        ok = false;
    }
    let mut tags = BTreeSet::new();
    for tag in &entry.tags {
        let valid = !tag.is_empty()
            && tag.len() <= MAX_TAG_LEN
            && tag
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
        if !valid || !tags.insert(tag.as_str()) {
            c.fail(
                &format!("{at}.tags"),
                format!("tag '{tag}' is invalid or repeated"),
            );
            ok = false;
        }
    }
    if let Some(license) = &entry.license {
        let valid = !license.is_empty()
            && license.len() <= MAX_LICENSE_LEN
            && license.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+' | ' ' | '(' | ')')
            });
        if !valid {
            c.fail(&format!("{at}.license"), "must be an SPDX expression");
            ok = false;
        }
    }
    if let Some(url) = &entry.documentation {
        ok &= c.check(&format!("{at}.documentation"), check_link(url));
    }
    if entry.versions.is_empty() {
        c.fail(&format!("{at}.versions"), "is empty");
        ok = false;
    }
    if entry.versions.len() > MAX_VERSIONS_PER_PACKAGE {
        c.fail(
            &format!("{at}.versions"),
            format!("more than {MAX_VERSIONS_PER_PACKAGE} versions"),
        );
        ok = false;
    }
    ok.then(|| CataloguePackage {
        id: entry.id.clone(),
        namespace: namespace.to_string(),
        local_name: local_name.to_string(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        author: entry.author.clone(),
        tags: entry.tags.clone(),
        license: entry.license.clone(),
        documentation: entry.documentation.clone(),
        versions: BTreeMap::new(),
    })
}

fn validate_version(
    c: &mut Checker,
    at: &str,
    package: &CataloguePackage,
    entry: &VersionEntry,
    source: &CatalogueSource,
) -> Option<CatalogueVersion> {
    let version = match canonical_version(&entry.version) {
        Ok(v) => v,
        Err(e) => {
            c.fail(&format!("{at}.version"), e);
            return None;
        }
    };
    let mut ok = c.check(
        &format!("{at}.entry"),
        validate_marketplace_entry(&entry.entry),
    );
    if entry.manifest_format == 0 || entry.pwnode_version == 0 {
        c.fail(at, "manifest_format and pwnode_version must be at least 1");
        ok = false;
    }
    let min_patchwork = match canonical_version(&entry.min_patchwork) {
        Ok(v) => Some(v),
        Err(e) => {
            c.fail(&format!("{at}.min_patchwork"), e);
            None
        }
    };

    // Capabilities.
    let mut capabilities = Vec::new();
    let mut seen = BTreeSet::new();
    if entry.capabilities.len() > MAX_CAPABILITIES {
        c.fail(
            &format!("{at}.capabilities"),
            format!("more than {MAX_CAPABILITIES} capabilities"),
        );
        ok = false;
    }
    for name in &entry.capabilities {
        let syntax = !name.is_empty()
            && name.len() <= 32
            && name.starts_with(|ch: char| ch.is_ascii_lowercase())
            && name
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
        if !syntax || !seen.insert(name.as_str()) {
            c.fail(
                &format!("{at}.capabilities"),
                format!("capability '{name}' is invalid or repeated"),
            );
            ok = false;
            continue;
        }
        match Capability::parse(name) {
            Some(Capability::NativeCode) => {
                c.fail(
                    &format!("{at}.capabilities"),
                    "native code is never allowed in the public catalogue",
                );
                ok = false;
            }
            Some(known) => capabilities.push(DeclaredCapability::Known(known)),
            None => capabilities.push(DeclaredCapability::Unknown(name.clone())),
        }
    }

    // Artifact.
    let art = &entry.artifact;
    let aat = format!("{at}.artifact");
    if art.format == 0 {
        c.fail(&aat, "format must be at least 1");
        ok = false;
    }
    ok &= c.check(&format!("{aat}.sha256"), validate_sha256(&art.sha256));
    if art.size == 0 || art.size > MAX_ARTIFACT_BYTES {
        c.fail(
            &format!("{aat}.size"),
            format!("must be between 1 and {MAX_ARTIFACT_BYTES} bytes"),
        );
        ok = false;
    }
    if art.files < 2 || art.files > MAX_ARTIFACT_FILES {
        c.fail(
            &format!("{aat}.files"),
            format!("must be between 2 and {MAX_ARTIFACT_FILES}"),
        );
        ok = false;
    }
    let commit = match parse_pinned_raw(&art.url, source) {
        Ok((commit, path)) => {
            if path.join("/") != artifact_repo_path(&package.id, &version) {
                c.fail(
                    &format!("{aat}.url"),
                    format!(
                        "must end in {} (the artifact of exactly this version)",
                        artifact_repo_path(&package.id, &version)
                    ),
                );
                ok = false;
            }
            commit.to_string()
        }
        Err(e) => {
            c.fail(&format!("{aat}.url"), e);
            ok = false;
            String::new()
        }
    };

    // Preview.
    let preview = match &entry.preview {
        None => None,
        Some(p) => {
            let pat = format!("{at}.preview");
            let mut good = c.check(&format!("{pat}.sha256"), validate_sha256(&p.sha256));
            if p.size == 0 || p.size > MAX_PREVIEW_BYTES {
                c.fail(
                    &format!("{pat}.size"),
                    format!("must be between 1 and {MAX_PREVIEW_BYTES} bytes"),
                );
                good = false;
            }
            match parse_pinned_raw(&p.url, source) {
                Ok((_, path)) => {
                    let version_text = version.to_string();
                    let expected = matches!(
                        path.as_slice(),
                        ["packages", ns, name, v, file]
                            if *ns == package.namespace
                                && *name == package.local_name
                                && *v == version_text
                                && PREVIEW_FILES.contains(file)
                    );
                    if !expected {
                        c.fail(
                            &format!("{pat}.url"),
                            "must be packages/<namespace>/<name>/<version>/preview.<png|jpg|jpeg|webp>",
                        );
                        good = false;
                    }
                }
                Err(e) => {
                    c.fail(&format!("{pat}.url"), e);
                    good = false;
                }
            }
            ok &= good;
            Some(Preview {
                url: p.url.clone(),
                sha256: p.sha256.clone(),
                size: p.size,
            })
        }
    };
    if let Some(reason) = &entry.yanked {
        ok &= c.check(
            &format!("{at}.yanked"),
            check_text(reason, MAX_REASON_LEN, false, true),
        );
    }

    let min_patchwork = min_patchwork?;
    ok.then(|| CatalogueVersion {
        version,
        entry: entry.entry.clone(),
        manifest_format: entry.manifest_format,
        pwnode_version: entry.pwnode_version,
        bundle_format: entry.bundle_format,
        min_patchwork,
        capabilities,
        artifact: Artifact {
            format: art.format,
            url: art.url.clone(),
            commit,
            sha256: art.sha256.clone(),
            size: art.size,
            files: art.files,
        },
        preview,
        published_at: entry.published_at,
        yanked: entry.yanked.clone(),
    })
}

// ── Consistency checks used by installation and refresh (P4.3.1) ─────────

/// The artifact's `package.json` must describe exactly the catalogue
/// version: same id, version, entry and formats, `source: "marketplace"`,
/// no looser minimum Patchwork, and dependencies only as a complete
/// bundle of public packages (so nothing resolves against the user's own
/// Library).
pub fn check_manifest_matches(
    package: &CataloguePackage,
    version: &CatalogueVersion,
    manifest: &PackageManifest,
) -> Result<(), String> {
    let parsed = manifest.validate()?;
    let label = format!("{}@{}", package.id, version.version);
    let mismatch = |what: &str| {
        Err(format!(
            "The package.json of {label} has a different {what}."
        ))
    };
    if manifest.id != package.id {
        return mismatch("id");
    }
    if parsed != version.version {
        return mismatch("version");
    }
    if manifest.entry != version.entry {
        return mismatch("entry");
    }
    if manifest.format != version.manifest_format
        || manifest.pwnode_version != version.pwnode_version
        || manifest.bundle_format != version.bundle_format
    {
        return mismatch("format");
    }
    if manifest.source != MARKETPLACE_SOURCE {
        return Err(format!(
            "The package.json of {label} must declare source \"{MARKETPLACE_SOURCE}\"."
        ));
    }
    if let Some(min) = &manifest.min_app_version {
        let min = parse_stable_version(min)?;
        if min > version.min_patchwork {
            return Err(format!(
                "{label} needs Patchwork {min}, but the catalogue says {}.",
                version.min_patchwork
            ));
        }
    }
    if !manifest.dependencies.is_empty() && manifest.bundle_format < 1 {
        return Err(format!(
            "{label} has dependencies but is not a complete bundle."
        ));
    }
    for dependency in &manifest.dependencies {
        split_public_id(&dependency.id)
            .map_err(|e| format!("{label} depends on a non-public package: {e}"))?;
    }
    Ok(())
}

/// Accept a refreshed catalogue only if it does not rewrite history:
/// a published version keeps its artifact (digest, size, entry),
/// a namespace keeps its owner account, and `generated_at` never goes
/// backwards (rollback). Versions and packages may disappear (takedown);
/// installed copies are unaffected.
pub fn check_refresh(previous: &Catalogue, next: &Catalogue) -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    match (previous.generated_at, next.generated_at) {
        (Some(old), Some(new)) if new < old => problems.push(format!(
            "The refreshed catalogue is older ({new}) than the cached one ({old})."
        )),
        // Dropping the revision would disable rollback protection.
        (Some(_), None) => problems.push(
            "The refreshed catalogue has no generated_at revision, but the cached one does."
                .to_string(),
        ),
        _ => {}
    }
    for (namespace, old) in &previous.publishers {
        if let Some(new) = next.publishers.get(namespace) {
            if new.github_id != old.github_id {
                problems.push(format!(
                    "Namespace '{namespace}' changed owner account ({} → {}).",
                    old.github_id, new.github_id
                ));
            }
        }
    }
    for (id, old_package) in &previous.packages {
        let Some(new_package) = next.packages.get(id) else {
            continue;
        };
        for (version, old) in &old_package.versions {
            let Some(new) = new_package.versions.get(version) else {
                continue;
            };
            // The URL may be re-pinned to a later commit; the bytes may not
            // change.
            if new.artifact.sha256 != old.artifact.sha256
                || new.artifact.size != old.artifact.size
                || new.entry != old.entry
            {
                problems.push(format!(
                    "{id}@{version} changed after publication; published versions are immutable."
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

// P4.3.1A: read-only catalogue browsing.
pub mod browse;
pub mod cache;
pub mod fetch;
pub mod source;
// P4.3.1B: verified installation.
pub mod artifact;
pub mod install;
pub mod scan;
pub mod submit;
// P4.3.4: the catalogue repository's CI tooling.
pub mod repository;

#[cfg(test)]
mod tests;
