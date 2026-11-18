//! P4.3.2C: package provenance and the trust store.
//!
//! `~/.patchwork/trust/packages.json` (outside the Library, so nothing
//! inside a package or project can write it) records, for each exact
//! package version, where Patchwork itself saw it come from:
//!
//! * **identity**: exact id, exact version and the SHA-256 tree digest of
//!   the complete version folder ([`digest`]). A record applies only to
//!   content with that exact digest, wherever the folder is (Library or a
//!   project's `packages/`); a copy that differs in any byte has no
//!   record.
//! * **provenance**: `local_publish` (published on this computer by the
//!   Publish workflow), `marketplace` (installed from a verified catalogue,
//!   with its source, revision and artifact digest) or `imported` (a
//!   package the user reviewed in the Library migration). It is never read
//!   from the package: a `local.*` id, a manifest `source` field or a
//!   project folder prove nothing.
//! * the capability classification version, the declared capabilities
//!   (marketplace only) and the capabilities detected by the scanner;
//! * an explicit execution grant: `not_granted`, `granted` or `denied`.
//!   P4.3.2C only records it; nothing enforces it yet.
//!
//! Only trusted application workflows can add records: [`TrustStore::record`]
//! takes a [`ProvenanceReceipt`], which can only be created inside
//! `crate::library` by publishing, installing or the user's migration
//! review, and only for content whose digest was verified.
//!
//! The store fails closed. A missing store means "nothing recorded". A
//! damaged, truncated, oversized, duplicated or newer-format store is
//! reported, treated as empty (nothing is trusted), and never overwritten
//! until the user explicitly moves it aside.

pub mod authorize;
pub mod content;
pub mod digest;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::marketplace::Capability;
use super::marketplace::scan::{CLASSIFICATION_VERSION, Inventory, inventory_files};
use super::marketplace::source::CatalogueLocation;
use super::package::{
    PackageManifest, parse_stable_version, validate_package_id, verify_version_dir_static,
};
use digest::{PackageTree, TREE_FORMAT, read_tree};

pub const TRUST_SCHEMA: &str = "patchwork.trust.packages";
pub const TRUST_FORMAT: u32 = 1;
pub const TRUST_DIR: &str = "trust";
pub const TRUST_FILE: &str = "packages.json";
const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RECORDS: usize = 50_000;
const MAX_LIST: usize = 256;
const MAX_TEXT: usize = 2048;

/// Serializes read-modify-write cycles of this process.
static STORE_LOCK: Mutex<()> = Mutex::new(());

/// P4.3.2D: bumped after every change this process makes to a trust store
/// (grants, denials, new records, reset). Runtimes and blocked states
/// remember the generation they were decided under and are decided again
/// when it changes, including runtimes carried in cloned graphs (undo
/// snapshots, copies).
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

// ── Identity ─────────────────────────────────────────────────────────────

/// Exact content identity of one package version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageIdentity {
    pub id: String,
    pub version: semver::Version,
    /// Lowercase hex tree digest ([`digest`], format [`TREE_FORMAT`]).
    pub tree_sha256: String,
}

impl PackageIdentity {
    pub fn label(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

/// A verified version folder, read once: identity, manifest and graphs.
#[derive(Debug, Clone)]
pub struct IdentifiedPackage {
    pub identity: PackageIdentity,
    /// The manifest parsed from the hashed bytes.
    pub manifest: PackageManifest,
    pub tree: PackageTree,
}

/// Verify a version folder (statically, nothing runs) and compute its
/// identity. The manifest used is the one inside the hashed bytes, so the
/// id and version cannot change between verification and hashing.
pub fn identify(dir: &Path, expect_layout: bool) -> Result<IdentifiedPackage, String> {
    let verified = verify_version_dir_static(dir, expect_layout)?;
    let tree = read_tree(dir).map_err(|e| {
        format!(
            "{}@{} cannot be identified: {e}",
            verified.manifest.id, verified.version
        )
    })?;
    let manifest: PackageManifest = tree
        .manifest
        .as_deref()
        .ok_or_else(|| "The package has no package.json.".to_string())
        .and_then(|text| serde_json::from_str(text).map_err(|e| e.to_string()))?;
    if manifest != verified.manifest {
        return Err(format!(
            "{}@{} changed while it was being read.",
            verified.manifest.id, verified.version
        ));
    }
    Ok(IdentifiedPackage {
        identity: PackageIdentity {
            id: manifest.id.clone(),
            version: verified.version,
            tree_sha256: tree.digest.clone(),
        },
        manifest,
        tree,
    })
}

// ── Records ──────────────────────────────────────────────────────────────

/// Execution grant (recorded only; enforced from a later stage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    NotGranted,
    Granted,
    Denied,
}

/// How an imported package reached the Library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportOrigin {
    /// Present before the trust store existed; reviewed by the user.
    LegacyLibrary,
    /// P4.3.2D: first recorded when the user allowed (or denied) it to run,
    /// e.g. a package bundled in a project received from someone else.
    ExecutionReview,
}

/// The verified catalogue a marketplace package was installed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketplaceOrigin {
    pub location: CatalogueLocation,
    /// The catalogue's `generated_at` revision, when it has one.
    pub generated_at: Option<u64>,
    /// SHA-256 of the exact catalogue text that was validated.
    pub catalogue_sha256: String,
}

/// Where Patchwork saw a package version come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provenance {
    /// Created by the Publish to Library workflow on this computer.
    LocalPublish,
    /// Installed from a verified marketplace catalogue.
    Marketplace {
        owner: String,
        repo: String,
        branch: String,
        catalogue_generated_at: Option<u64>,
        catalogue_sha256: String,
        artifact_url: String,
        artifact_sha256: String,
    },
    /// Reviewed by the user; its origin is not verified.
    Imported { origin: ImportOrigin },
}

impl Provenance {
    pub fn label(&self) -> String {
        match self {
            Provenance::LocalPublish => "published on this computer".to_string(),
            Provenance::Marketplace {
                owner,
                repo,
                branch,
                ..
            } => format!("marketplace {owner}/{repo}@{branch}"),
            Provenance::Imported {
                origin: ImportOrigin::LegacyLibrary,
            } => "reviewed Library package (origin not verified)".to_string(),
            Provenance::Imported {
                origin: ImportOrigin::ExecutionReview,
            } => "origin not verified".to_string(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if let Provenance::Marketplace {
            owner,
            repo,
            branch,
            catalogue_sha256,
            artifact_url,
            artifact_sha256,
            ..
        } = self
        {
            let location = CatalogueLocation::new(owner, repo, branch)?;
            if location.source().owner() != owner
                || location.source().repo() != repo
                || location.branch() != branch
            {
                return Err("the catalogue source is not in canonical form".to_string());
            }
            if !is_hex64(catalogue_sha256) || !is_hex64(artifact_sha256) {
                return Err("a marketplace digest is malformed".to_string());
            }
            if artifact_url.is_empty()
                || artifact_url.len() > MAX_TEXT
                || !artifact_url.starts_with("https://")
                || artifact_url.chars().any(char::is_control)
            {
                return Err("the artifact URL is malformed".to_string());
            }
        }
        Ok(())
    }
}

/// One record, as validated in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustRecord {
    pub identity: PackageIdentity,
    pub provenance: Provenance,
    pub classification_version: u32,
    /// What the catalogue declared (marketplace only).
    pub declared: Option<BTreeSet<Capability>>,
    /// What the scanner found in the package's graphs.
    pub detected: BTreeSet<Capability>,
    /// Node types the marketplace never allows (local packages only).
    pub restricted: BTreeSet<String>,
    /// Node types this classification does not know.
    pub unclassified: BTreeSet<String>,
    pub grant: Grant,
    /// P4.3.2D: the capabilities the user approved with a `granted`
    /// grant (`None` otherwise). A grant only covers content whose rescanned
    /// capabilities are within this set.
    pub approved: Option<BTreeSet<Capability>>,
    pub recorded_at: u64,
}

/// A record as stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordFile {
    id: String,
    version: String,
    tree_format: u32,
    tree_sha256: String,
    provenance: Provenance,
    classification_version: u32,
    declared_capabilities: Option<Vec<String>>,
    detected_capabilities: Vec<String>,
    restricted_node_types: Vec<String>,
    unclassified_node_types: Vec<String>,
    grant: Grant,
    /// P4.3.2D (optional, so P4.3.2C stores stay readable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approved_capabilities: Option<Vec<String>>,
    recorded_at: u64,
}

/// Whether the one-time review of pre-existing Library packages is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyReview {
    /// Offer the review (the user may defer it for this session).
    #[default]
    Pending,
    /// The user chose not to migrate; never offered automatically again.
    Declined,
    /// The user finished the review.
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreFile {
    schema: String,
    format: u32,
    legacy_review: LegacyReview,
    records: Vec<RecordFile>,
}

fn capability_names(set: &BTreeSet<Capability>) -> Vec<String> {
    set.iter().map(|c| c.as_str().to_string()).collect()
}

fn parse_capabilities(names: &[String]) -> Result<BTreeSet<Capability>, String> {
    if names.len() > MAX_LIST {
        return Err("too many capabilities".to_string());
    }
    let mut set = BTreeSet::new();
    for name in names {
        let capability =
            Capability::parse(name).ok_or_else(|| format!("unknown capability '{name}'"))?;
        if !set.insert(capability) {
            return Err(format!("capability '{name}' is listed twice"));
        }
    }
    Ok(set)
}

fn parse_names(names: &[String]) -> Result<BTreeSet<String>, String> {
    if names.len() > MAX_LIST {
        return Err("too many node types".to_string());
    }
    let mut set = BTreeSet::new();
    for name in names {
        if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
            return Err("a node type name is malformed".to_string());
        }
        if !set.insert(name.clone()) {
            return Err(format!("node type '{name}' is listed twice"));
        }
    }
    Ok(set)
}

impl RecordFile {
    fn from_record(record: &TrustRecord) -> Self {
        Self {
            id: record.identity.id.clone(),
            version: record.identity.version.to_string(),
            tree_format: TREE_FORMAT,
            tree_sha256: record.identity.tree_sha256.clone(),
            provenance: record.provenance.clone(),
            classification_version: record.classification_version,
            declared_capabilities: record.declared.as_ref().map(capability_names),
            detected_capabilities: capability_names(&record.detected),
            restricted_node_types: record.restricted.iter().cloned().collect(),
            unclassified_node_types: record.unclassified.iter().cloned().collect(),
            grant: record.grant,
            approved_capabilities: record.approved.as_ref().map(capability_names),
            recorded_at: record.recorded_at,
        }
    }

    fn validate(self) -> Result<TrustRecord, String> {
        validate_package_id(&self.id)?;
        let version = parse_stable_version(&self.version)?;
        if version.to_string() != self.version {
            return Err(format!("version '{}' is not canonical", self.version));
        }
        let label = format!("{}@{}", self.id, version);
        let fail = |why: String| format!("{label}: {why}");
        if self.tree_format != TREE_FORMAT {
            return Err(fail(format!(
                "unsupported digest format {}",
                self.tree_format
            )));
        }
        if !is_hex64(&self.tree_sha256) {
            return Err(fail("the content digest is malformed".to_string()));
        }
        self.provenance.validate().map_err(fail)?;
        if self.classification_version == 0 || self.classification_version > CLASSIFICATION_VERSION
        {
            return Err(fail(format!(
                "unknown classification version {}",
                self.classification_version
            )));
        }
        let declared = self
            .declared_capabilities
            .as_deref()
            .map(parse_capabilities)
            .transpose()
            .map_err(fail)?;
        let is_marketplace = matches!(self.provenance, Provenance::Marketplace { .. });
        if declared.is_some() != is_marketplace {
            return Err(fail(
                "declared capabilities belong to marketplace records only".to_string(),
            ));
        }
        let detected = parse_capabilities(&self.detected_capabilities).map_err(fail)?;
        if let Some(declared) = &declared {
            if !detected.is_subset(declared) {
                return Err(fail(
                    "detected capabilities exceed the declared ones".to_string(),
                ));
            }
        }
        let restricted = parse_names(&self.restricted_node_types).map_err(fail)?;
        let unclassified = parse_names(&self.unclassified_node_types).map_err(fail)?;
        if is_marketplace && !(restricted.is_empty() && unclassified.is_empty()) {
            return Err(fail(
                "a marketplace package cannot contain restricted node types".to_string(),
            ));
        }
        let approved = self
            .approved_capabilities
            .as_deref()
            .map(parse_capabilities)
            .transpose()
            .map_err(fail)?;
        match (&self.grant, &approved) {
            (Grant::Granted, Some(approved)) if detected.is_subset(approved) => {}
            (Grant::Granted, _) => {
                return Err(fail(
                    "a grant must approve every detected capability".to_string(),
                ));
            }
            (_, Some(_)) => {
                return Err(fail(
                    "approved capabilities belong to granted records only".to_string(),
                ));
            }
            (_, None) => {}
        }
        Ok(TrustRecord {
            identity: PackageIdentity {
                id: self.id,
                version,
                tree_sha256: self.tree_sha256,
            },
            provenance: self.provenance,
            classification_version: self.classification_version,
            declared,
            detected,
            restricted,
            unclassified,
            grant: self.grant,
            approved,
            recorded_at: self.recorded_at,
        })
    }
}

/// The validated contents of the trust store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Store {
    /// At most one record per exact `id@version`.
    records: BTreeMap<(String, semver::Version), TrustRecord>,
    pub legacy_review: LegacyReview,
}

impl Store {
    pub fn records(&self) -> impl Iterator<Item = &TrustRecord> {
        self.records.values()
    }

    /// The record for exactly `id@version`, whatever its content digest.
    pub fn record_for(&self, id: &str, version: &semver::Version) -> Option<&TrustRecord> {
        self.records.get(&(id.to_string(), version.clone()))
    }

    /// What the store says about this exact content.
    pub fn status(&self, identity: &PackageIdentity) -> TrustStatus {
        match self.record_for(&identity.id, &identity.version) {
            None => TrustStatus::Unrecorded,
            Some(record) if record.identity.tree_sha256 != identity.tree_sha256 => {
                TrustStatus::ContentChanged
            }
            Some(record) if record.classification_version != CLASSIFICATION_VERSION => {
                TrustStatus::Reclassified(record.clone())
            }
            Some(record) => TrustStatus::Recorded(record.clone()),
        }
    }

    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let file: StoreFile =
            serde_json::from_slice(bytes).map_err(|e| format!("it is not valid: {e}"))?;
        if file.schema != TRUST_SCHEMA {
            return Err("it is not a Patchwork trust store".to_string());
        }
        if file.format != TRUST_FORMAT {
            return Err(format!(
                "it uses format {}, which this Patchwork does not read",
                file.format
            ));
        }
        if file.records.len() > MAX_RECORDS {
            return Err("it has too many records".to_string());
        }
        let mut store = Store {
            records: BTreeMap::new(),
            legacy_review: file.legacy_review,
        };
        for record in file.records {
            let record = record.validate()?;
            let key = (record.identity.id.clone(), record.identity.version.clone());
            if store.records.insert(key, record).is_some() {
                return Err("it lists a package version twice".to_string());
            }
        }
        Ok(store)
    }

    fn to_json(&self) -> Result<Vec<u8>, String> {
        let file = StoreFile {
            schema: TRUST_SCHEMA.to_string(),
            format: TRUST_FORMAT,
            legacy_review: self.legacy_review,
            records: self.records.values().map(RecordFile::from_record).collect(),
        };
        serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())
    }
}

/// What the store says about one exact package content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustStatus {
    /// No record for this `id@version`.
    Unrecorded,
    /// A record exists for this `id@version`, but for different content:
    /// it does not apply.
    ContentChanged,
    /// Recorded under an older capability classification; the capabilities
    /// must be reviewed again before the record is relied on.
    Reclassified(TrustRecord),
    /// Recorded for exactly this content under the current classification.
    Recorded(TrustRecord),
}

impl TrustStatus {
    /// Verified provenance for exactly this content (grants are separate).
    /// Used by the runtime gates of the next stage; tested here.
    #[allow(dead_code)]
    pub fn has_verified_provenance(&self) -> bool {
        matches!(
            self,
            TrustStatus::Recorded(TrustRecord {
                provenance: Provenance::LocalPublish | Provenance::Marketplace { .. },
                ..
            })
        )
    }

    pub fn label(&self) -> String {
        match self {
            TrustStatus::Unrecorded => "No provenance recorded".to_string(),
            TrustStatus::ContentChanged => "Content differs from the recorded version".to_string(),
            TrustStatus::Reclassified(_) => {
                "Recorded under an older capability classification".to_string()
            }
            TrustStatus::Recorded(record) => {
                format!("Recorded: {}", record.provenance.label())
            }
        }
    }
}

// ── Receipts ─────────────────────────────────────────────────────────────

/// Proof that a trusted workflow verified exactly this content. Only code
/// in `crate::library` can create one.
#[derive(Debug, Clone)]
pub struct ProvenanceReceipt {
    record: TrustRecord,
}

impl ProvenanceReceipt {
    /// Identify `dir` (checking that its digest is `expected`, when given),
    /// scan its graphs and prepare a record.
    pub(in crate::library) fn for_content(
        dir: &Path,
        expected_digest: Option<&str>,
        provenance: Provenance,
        declared: Option<BTreeSet<Capability>>,
        grant: Grant,
    ) -> Result<Self, String> {
        let package = identify(dir, true)?;
        if let Some(expected) = expected_digest {
            if package.identity.tree_sha256 != expected {
                return Err(format!(
                    "{} does not match the content that was verified.",
                    package.identity.label()
                ));
            }
        }
        let inventory = inventory_files(
            package
                .tree
                .graphs
                .iter()
                .map(|(p, t)| (p.as_str(), t.as_str())),
        )?;
        if let Some(declared) = &declared {
            if !inventory.capabilities.is_subset(declared)
                || !inventory.restricted.is_empty()
                || !inventory.unclassified.is_empty()
            {
                return Err(format!(
                    "{} uses more than its catalogue entry declares.",
                    package.identity.label()
                ));
            }
        }
        provenance.validate()?;
        Ok(Self {
            record: TrustRecord {
                identity: package.identity,
                provenance,
                classification_version: CLASSIFICATION_VERSION,
                declared,
                detected: inventory.capabilities,
                restricted: inventory.restricted,
                unclassified: inventory.unclassified,
                grant,
                approved: None,
                recorded_at: now(),
            },
        })
    }

    pub fn identity(&self) -> &PackageIdentity {
        &self.record.identity
    }

    #[cfg(test)]
    pub fn record(&self) -> &TrustRecord {
        &self.record
    }
}

/// Marketplace provenance for a verified install.
pub(in crate::library) fn marketplace_provenance(
    origin: &MarketplaceOrigin,
    artifact_url: &str,
    artifact_sha256: &str,
) -> Provenance {
    Provenance::Marketplace {
        owner: origin.location.source().owner().to_string(),
        repo: origin.location.source().repo().to_string(),
        branch: origin.location.branch().to_string(),
        catalogue_generated_at: origin.generated_at,
        catalogue_sha256: origin.catalogue_sha256.to_ascii_lowercase(),
        artifact_url: artifact_url.to_string(),
        artifact_sha256: artifact_sha256.to_ascii_lowercase(),
    }
}

// ── The store file ───────────────────────────────────────────────────────

/// Result of reading the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreLoad {
    /// No store yet: nothing is recorded.
    Missing,
    Loaded(Store),
    /// Unusable: nothing is trusted, and it is not written over.
    Damaged(String),
}

impl StoreLoad {
    /// Damaged or missing stores record nothing.
    pub fn status(&self, identity: &PackageIdentity) -> TrustStatus {
        match self {
            StoreLoad::Loaded(store) => store.status(identity),
            StoreLoad::Missing | StoreLoad::Damaged(_) => TrustStatus::Unrecorded,
        }
    }
}

// Simulated failures (only triggered by tests).
#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_BEFORE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The trust store file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustStore {
    path: PathBuf,
}

impl TrustStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// `~/.patchwork/trust/packages.json`.
    pub fn default_location() -> Result<Self, String> {
        dirs::home_dir()
            .filter(|home| !home.as_os_str().is_empty())
            .map(|home| Self::at(home.join(".patchwork").join(TRUST_DIR).join(TRUST_FILE)))
            .ok_or_else(|| "Cannot locate your home directory for the trust store.".to_string())
    }

    pub fn load(&self) -> StoreLoad {
        let meta = match std::fs::symlink_metadata(&self.path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return StoreLoad::Missing,
            Err(e) => return StoreLoad::Damaged(format!("The trust store cannot be read: {e}")),
        };
        if !meta.is_file() {
            return StoreLoad::Damaged("The trust store is not a regular file.".to_string());
        }
        if meta.len() > MAX_STORE_BYTES {
            return StoreLoad::Damaged("The trust store is too large.".to_string());
        }
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) => return StoreLoad::Damaged(format!("The trust store cannot be read: {e}")),
        };
        match Store::parse(&bytes) {
            Ok(store) => StoreLoad::Loaded(store),
            Err(why) => StoreLoad::Damaged(format!("The trust store is damaged: {why}.")),
        }
    }

    /// Read, change and atomically rewrite the store. Refuses to touch a
    /// damaged store.
    fn update<T>(&self, change: impl FnOnce(&mut Store) -> Result<T, String>) -> Result<T, String> {
        let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut store = match self.load() {
            StoreLoad::Missing => Store::default(),
            StoreLoad::Loaded(store) => store,
            StoreLoad::Damaged(why) => {
                return Err(format!(
                    "{why} Nothing was recorded. Review the trust store from Library → \
                     Package Trust Review."
                ));
            }
        };
        let value = change(&mut store)?;
        self.write(&store)?;
        Ok(value)
    }

    /// Temporary file in the same folder, flushed, then renamed over the
    /// store. Owner-only permissions where the system supports them.
    fn write(&self, store: &Store) -> Result<(), String> {
        write_atomically(&self.path, &store.to_json()?)
    }

    /// P4.3.2E1: the content ledger kept next to this store.
    pub fn content_ledger(&self) -> content::ContentLedger {
        content::ContentLedger::at(self.path.with_file_name(content::CONTENT_FILE))
    }

    /// Add (or replace, for the same `id@version`) the record a trusted
    /// workflow produced.
    pub fn record(&self, receipt: ProvenanceReceipt) -> Result<TrustRecord, String> {
        let record = receipt.record;
        self.update(|store| {
            let key = (record.identity.id.clone(), record.identity.version.clone());
            if store.records.len() >= MAX_RECORDS && !store.records.contains_key(&key) {
                return Err("The trust store is full.".to_string());
            }
            store.records.insert(key, record.clone());
            Ok(record)
        })
    }

    pub fn set_legacy_review(&self, state: LegacyReview) -> Result<(), String> {
        self.update(|store| {
            store.legacy_review = state;
            Ok(())
        })
    }

    /// Re-scan a recorded package under the current classification. Its
    /// provenance is kept (the content digest must still match); a grant
    /// returns to `not_granted`, a denial stays.
    pub fn reclassify(&self, dir: &Path) -> Result<TrustRecord, String> {
        let package = identify(dir, true)?;
        let inventory = inventory_files(
            package
                .tree
                .graphs
                .iter()
                .map(|(p, t)| (p.as_str(), t.as_str())),
        )?;
        self.update(|store| {
            let key = (
                package.identity.id.clone(),
                package.identity.version.clone(),
            );
            let record = store
                .records
                .get_mut(&key)
                .filter(|r| r.identity.tree_sha256 == package.identity.tree_sha256)
                .ok_or_else(|| {
                    format!(
                        "{} has no record for this content.",
                        package.identity.label()
                    )
                })?;
            if let Some(declared) = &record.declared {
                if !inventory.capabilities.is_subset(declared)
                    || !inventory.restricted.is_empty()
                    || !inventory.unclassified.is_empty()
                {
                    return Err(format!(
                        "{} now uses more than its catalogue entry declared; it needs a new \
                         review.",
                        package.identity.label()
                    ));
                }
            }
            record.classification_version = CLASSIFICATION_VERSION;
            record.detected = inventory.capabilities.clone();
            record.restricted = inventory.restricted.clone();
            record.unclassified = inventory.unclassified.clone();
            if record.grant == Grant::Granted {
                record.grant = Grant::NotGranted;
                record.approved = None;
            }
            record.recorded_at = now();
            Ok(record.clone())
        })
    }

    /// P4.3.2F: change the grant of exactly this recorded content, without
    /// its folder (a package can be revoked even when it is gone).
    fn change_grant(
        &self,
        identity: &PackageIdentity,
        change: impl FnOnce(&mut TrustRecord) -> Result<(), String>,
    ) -> Result<TrustRecord, String> {
        self.update(|store| {
            let record = store
                .records
                .get_mut(&(identity.id.clone(), identity.version.clone()))
                .filter(|r| r.identity.tree_sha256 == identity.tree_sha256)
                .ok_or_else(|| format!("{} has no record for this content.", identity.label()))?;
            change(record)?;
            record.recorded_at = now();
            Ok(record.clone())
        })
    }

    /// P4.3.2F: withdraw a grant. The package no longer runs (in any open
    /// document, without a restart) and is offered for review again.
    pub fn revoke_grant(&self, identity: &PackageIdentity) -> Result<TrustRecord, String> {
        self.change_grant(identity, |record| {
            if record.grant != Grant::Granted {
                return Err(format!("{} is not allowed to run.", identity.label()));
            }
            record.grant = Grant::NotGranted;
            record.approved = None;
            Ok(())
        })
    }

    /// P4.3.2F: refuse a recorded package (no review needed to refuse).
    pub fn deny_recorded(&self, identity: &PackageIdentity) -> Result<TrustRecord, String> {
        self.change_grant(identity, |record| {
            record.grant = Grant::Denied;
            record.approved = None;
            Ok(())
        })
    }

    /// P4.3.2F: lift a denial. This grants nothing: the package is offered
    /// for review again, where allowing it needs the usual explicit
    /// acknowledgement (and rescans its content).
    pub fn withdraw_denial(&self, identity: &PackageIdentity) -> Result<TrustRecord, String> {
        self.change_grant(identity, |record| {
            if record.grant != Grant::Denied {
                return Err(format!("{} is not denied.", identity.label()));
            }
            record.grant = Grant::NotGranted;
            record.approved = None;
            Ok(())
        })
    }

    /// P4.3.2F: approve fewer capabilities for a granted package. Never
    /// widens (widening goes through review). A package that uses a
    /// capability outside the new set no longer runs until reviewed again.
    pub fn narrow_grant(
        &self,
        identity: &PackageIdentity,
        approved: BTreeSet<Capability>,
    ) -> Result<TrustRecord, String> {
        self.change_grant(identity, |record| {
            let current = match (&record.grant, &record.approved) {
                (Grant::Granted, Some(current)) => current,
                _ => return Err(format!("{} is not allowed to run.", identity.label())),
            };
            if !approved.is_subset(current) {
                return Err(
                    "Approving more than before needs a new review of the package.".to_string(),
                );
            }
            record.approved = Some(approved);
            Ok(())
        })
    }

    /// Move a damaged store aside (kept for inspection) so a new one can be
    /// started. Refuses when the store is readable.
    pub fn move_damaged_aside(&self) -> Result<PathBuf, String> {
        let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(self.load(), StoreLoad::Damaged(_)) {
            return Err("The trust store is not damaged.".to_string());
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let aside = self
            .path
            .with_file_name(format!("packages.damaged-{nanos}.json"));
        std::fs::rename(&self.path, &aside)
            .map_err(|e| format!("Cannot move the damaged trust store: {e}"))?;
        bump_generation();
        Ok(aside)
    }

    /// P4.3.2D: the user allowed exactly the reviewed content to run. The
    /// folder is identified again and must still be `reviewed` (the same
    /// bytes); its capabilities are rescanned under the current
    /// classification and become the approved set. A record for another
    /// content of the same `id@version` is never replaced: that conflict is
    /// refused. A package without a record is recorded as imported
    /// (origin not verified). Reverses a denial: only call it for an
    /// explicit user action.
    pub fn grant(&self, dir: &Path, reviewed: &PackageIdentity) -> RecordOutcome {
        self.decide(dir, reviewed, Grant::Granted)
    }

    /// P4.3.2D: the user refused to run exactly the reviewed content. The
    /// denial persists until the user explicitly allows it.
    pub fn deny(&self, dir: &Path, reviewed: &PackageIdentity) -> RecordOutcome {
        self.decide(dir, reviewed, Grant::Denied)
    }

    fn decide(&self, dir: &Path, reviewed: &PackageIdentity, grant: Grant) -> RecordOutcome {
        let package = identify(dir, true)?;
        if &package.identity != reviewed {
            return Err(format!(
                "{} changed after it was reviewed; review it again.",
                reviewed.label()
            ));
        }
        let inventory = inventory_files(
            package
                .tree
                .graphs
                .iter()
                .map(|(p, t)| (p.as_str(), t.as_str())),
        )?;
        self.update(|store| {
            let key = (reviewed.id.clone(), reviewed.version.clone());
            let record = match store.records.get(&key) {
                Some(existing) if existing.identity.tree_sha256 != reviewed.tree_sha256 => {
                    return Err(format!(
                        "A different content of {} is already recorded. Two packages with \
                         the same id and version but different files cannot both be allowed; \
                         remove one of them.",
                        reviewed.label()
                    ));
                }
                Some(existing) => {
                    if let Some(declared) = &existing.declared {
                        if !inventory.capabilities.is_subset(declared) {
                            return Err(format!(
                                "{} uses more than its catalogue entry declared.",
                                reviewed.label()
                            ));
                        }
                    }
                    TrustRecord {
                        classification_version: CLASSIFICATION_VERSION,
                        detected: inventory.capabilities.clone(),
                        restricted: inventory.restricted.clone(),
                        unclassified: inventory.unclassified.clone(),
                        grant,
                        approved: (grant == Grant::Granted).then(|| inventory.capabilities.clone()),
                        recorded_at: now(),
                        ..existing.clone()
                    }
                }
                None => TrustRecord {
                    identity: reviewed.clone(),
                    provenance: Provenance::Imported {
                        origin: ImportOrigin::ExecutionReview,
                    },
                    classification_version: CLASSIFICATION_VERSION,
                    declared: None,
                    detected: inventory.capabilities.clone(),
                    restricted: inventory.restricted.clone(),
                    unclassified: inventory.unclassified.clone(),
                    grant,
                    approved: (grant == Grant::Granted).then(|| inventory.capabilities.clone()),
                    recorded_at: now(),
                },
            };
            store.records.insert(key, record.clone());
            Ok(record)
        })
    }
}

/// P4.3.2F: a granted marketplace package whose catalogue version was
/// withdrawn (yanked) or is no longer listed. Its grant is not changed by
/// itself; the Permissions window offers to revoke it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithdrawnGrant {
    pub identity: PackageIdentity,
    pub reason: String,
}

/// P4.3.2F: granted marketplace packages from `catalogue`'s source that it
/// no longer offers.
pub fn withdrawn_grants(
    store: &Store,
    catalogue: &crate::library::marketplace::Catalogue,
    location: &CatalogueLocation,
) -> Vec<WithdrawnGrant> {
    store
        .records
        .values()
        .filter(|record| record.grant == Grant::Granted)
        .filter_map(|record| {
            let Provenance::Marketplace {
                owner,
                repo,
                branch,
                ..
            } = &record.provenance
            else {
                return None;
            };
            let same_source = location.source().owner() == owner
                && location.source().repo() == repo
                && location.branch() == branch;
            if !same_source {
                return None;
            }
            let identity = &record.identity;
            let reason = match catalogue
                .packages
                .get(&identity.id)
                .and_then(|package| package.versions.get(&identity.version))
            {
                None => "no longer listed in the catalogue".to_string(),
                Some(version) => format!("withdrawn: {}", version.yanked.as_ref()?),
            };
            Some(WithdrawnGrant {
                identity: identity.clone(),
                reason,
            })
        })
        .collect()
}

/// Temporary file in the same folder, flushed, then renamed over `path`.
/// Owner-only permissions where the system supports them. A successful
/// write starts a new trust generation.
fn write_atomically(path: &Path, json: &[u8]) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "The trust file has no folder.".to_string())?;
    create_private_dir(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = dir.join(format!(".{name}.{}-{nanos}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut out = options.open(&temp)?;
        out.write_all(json)?;
        out.sync_all()?;
        drop(out);
        #[cfg(test)]
        if FAIL_BEFORE_RENAME.with(|f| f.get()) {
            return Err(std::io::Error::other("simulated failure before rename"));
        }
        std::fs::rename(&temp, path)
    };
    let result = write().map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("Cannot save {name}: {e}")
    });
    if result.is_ok() {
        bump_generation();
    }
    result
}

fn create_private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

// ── Workflows ────────────────────────────────────────────────────────────

/// Outcome of recording provenance after a successful operation. The
/// operation itself is never undone because of it: an unrecorded package
/// simply has no provenance and is listed for review.
pub type RecordOutcome = Result<TrustRecord, String>;

/// Publish to the Library, then record `local_publish` provenance for the
/// exact content that was published.
pub fn publish_and_record(
    store: &TrustStore,
    library_root: &Path,
    source: &Path,
    request: &super::package::PublishRequest,
) -> Result<(super::package::VerifiedPackage, RecordOutcome), super::PublishError> {
    let (package, receipt) =
        super::package::publish_package_with_receipt(library_root, source, request)?;
    let outcome = receipt.and_then(|receipt| store.record(receipt));
    Ok((package, outcome))
}

/// Install from the marketplace, then record `marketplace` provenance
/// bound to the installed content, the catalogue and the artifact digest.
pub fn install_and_record(
    store: &TrustStore,
    transport: &dyn super::marketplace::fetch::Transport,
    library_root: &Path,
    package: &super::marketplace::CataloguePackage,
    version: &super::marketplace::CatalogueVersion,
    origin: &MarketplaceOrigin,
    app: &semver::Version,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<
    (super::marketplace::install::Installed, RecordOutcome),
    super::marketplace::install::InstallError,
> {
    let installed = super::marketplace::install::install(
        transport,
        library_root,
        package,
        version,
        app,
        cancel,
    )?;
    let outcome = installed
        .receipt(origin)
        .and_then(|receipt| store.record(receipt));
    Ok((installed, outcome))
}

// ── Library migration ────────────────────────────────────────────────────

/// A Library package version without an applicable record.
#[derive(Debug, Clone)]
pub struct ReviewItem {
    pub id: String,
    pub version: semver::Version,
    pub name: String,
    pub dir: PathBuf,
    /// What the manifest claims (`source`); shown, never trusted.
    pub claimed_source: String,
    /// `Ok` when the content could be identified and scanned.
    pub identity: Result<(PackageIdentity, Inventory), String>,
    pub status: TrustStatus,
}

/// Every Library package version whose exact content has no current
/// record (never recorded, changed since, or classified by an older
/// version). Reading only: nothing is written, nothing runs.
pub fn review_library(library_root: &Path, load: &StoreLoad) -> Vec<ReviewItem> {
    let scan = super::index::scan_packages(library_root);
    let mut items = Vec::new();
    for package in &scan.packages {
        for version in &package.versions {
            let identified = identify(&version.dir, true).and_then(|identified| {
                let inventory = inventory_files(
                    identified
                        .tree
                        .graphs
                        .iter()
                        .map(|(p, t)| (p.as_str(), t.as_str())),
                )?;
                Ok((identified.identity, inventory))
            });
            let status = match &identified {
                Ok((identity, _)) => load.status(identity),
                Err(_) => TrustStatus::Unrecorded,
            };
            if matches!(status, TrustStatus::Recorded(_)) {
                continue;
            }
            items.push(ReviewItem {
                id: version.manifest.id.clone(),
                version: version.version.clone(),
                name: version.manifest.name.clone(),
                dir: version.dir.clone(),
                claimed_source: version.manifest.source.chars().take(64).collect(),
                identity: identified,
                status,
            });
        }
    }
    items
}

/// The user's decision for one reviewed package.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    /// Record it as an imported package (origin not verified, not granted).
    Accept,
    /// Record it as imported and denied.
    Deny,
}

/// Apply the user's decision to the package that was reviewed. The folder
/// is identified again, and must still have the reviewed digest; package
/// bytes are never changed. Records for other content of the same
/// `id@version` are replaced: they no longer describe the Library copy.
pub fn apply_review(
    store: &TrustStore,
    dir: &Path,
    reviewed: &PackageIdentity,
    decision: ReviewDecision,
) -> RecordOutcome {
    let grant = match decision {
        ReviewDecision::Accept => Grant::NotGranted,
        ReviewDecision::Deny => Grant::Denied,
    };
    let receipt = ProvenanceReceipt::for_content(
        dir,
        Some(&reviewed.tree_sha256),
        Provenance::Imported {
            origin: ImportOrigin::LegacyLibrary,
        },
        None,
        grant,
    )?;
    if receipt.identity() != reviewed {
        return Err(format!(
            "{} changed after it was reviewed.",
            reviewed.label()
        ));
    }
    store.record(receipt)
}

/// The status of a package folder anywhere (Library or a project's
/// `packages/<id>/<version>/`). A project copy has a record only when its
/// content is byte-for-byte the recorded content. Used by the runtime
/// gates of the next stage; tested here.
#[allow(dead_code)]
pub fn status_of_dir(
    load: &StoreLoad,
    dir: &Path,
) -> Result<(PackageIdentity, TrustStatus), String> {
    let package = identify(dir, true)?;
    let status = load.status(&package.identity);
    Ok((package.identity, status))
}

#[cfg(test)]
mod tests;
