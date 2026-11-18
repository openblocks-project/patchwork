//! P4.3.1B: installing one exact marketplace version into the Library.
//!
//! ```text
//! preflight      installable here (compatible, not withdrawn), not present
//! download       catalogue URL only, origin-restricted, ≤ declared size
//! verify         exact size + SHA-256 from the validated catalogue
//! parse          .pwpkg schema, paths, limits, manifest ↔ catalogue,
//!                bundled files and dependencies (all in memory)
//! scan           capabilities of every graph ⊆ catalogue declaration
//! stage          packages/<id>/.staging-*/ (ignored by Library scans)
//! verify staged  existing P4.1/P4.2 verify_version_dir_static
//! commit         rename to packages/<id>/<version>/ (never replaces)
//! ```
//!
//! Nothing is evaluated: graphs are only read (scanned as JSON, then
//! loaded by the existing verification). On any failure or cancellation
//! the staging folder is removed and the Library is unchanged.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::artifact::{ParsedArtifact, parse_artifact, verify_download};
use super::browse::compatibility_label;
use super::fetch::{FetchError, FetchResponse, Transport};
use super::scan::{check_declared, scan_files};
use super::{Capability, CataloguePackage, CatalogueVersion};
use crate::library::package::{
    STAGING_PREFIX, VerifiedPackage, package_dir, verify_version_dir_static, version_dir,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// Not offered on this Patchwork (incompatible or withdrawn).
    NotInstallable(String),
    /// The version folder already exists; it is never replaced.
    AlreadyInstalled(String),
    Download(FetchError),
    /// The artifact failed verification or the capability check.
    Rejected(String),
    Io(String),
    Cancelled,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallError::NotInstallable(why) => write!(f, "Not installable: {why}"),
            InstallError::AlreadyInstalled(what) => {
                write!(f, "{what} is already in the Library; it was not replaced.")
            }
            InstallError::Download(error) => write!(f, "Download failed: {error}"),
            InstallError::Rejected(why) => write!(f, "The package was refused: {why}"),
            InstallError::Io(why) => write!(f, "Could not write the package: {why}"),
            InstallError::Cancelled => write!(f, "Installation was cancelled."),
        }
    }
}

/// A successful installation.
#[derive(Debug, Clone)]
pub struct Installed {
    pub package: VerifiedPackage,
    /// Capabilities the scanner found (a subset of the declared ones).
    pub capabilities: BTreeSet<Capability>,
    /// P4.3.2B: [`super::scan::CLASSIFICATION_VERSION`] used for the check.
    pub classification_version: u32,
    /// P4.3.2C: tree digest of the committed folder (equal to the digest
    /// of the verified artifact files). Only this module sets these, so an
    /// `Installed` cannot be made up elsewhere.
    pub(in crate::library) tree_sha256: String,
    pub(in crate::library) artifact_url: String,
    pub(in crate::library) artifact_sha256: String,
    pub(in crate::library) declared: BTreeSet<Capability>,
}

impl Installed {
    /// A receipt for `marketplace` provenance from the catalogue the
    /// version was chosen from. The folder must still hold exactly the
    /// installed content.
    pub fn receipt(
        &self,
        origin: &crate::library::trust::MarketplaceOrigin,
    ) -> Result<crate::library::trust::ProvenanceReceipt, String> {
        crate::library::trust::ProvenanceReceipt::for_content(
            &self.package.dir,
            Some(&self.tree_sha256),
            crate::library::trust::marketplace_provenance(
                origin,
                &self.artifact_url,
                &self.artifact_sha256,
            ),
            Some(self.declared.clone()),
            crate::library::trust::Grant::NotGranted,
        )
    }
}

/// Simulated interruptions (only triggered by tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailPoint {
    /// Before writing file number N.
    Writing(usize),
    BeforeVerify,
    BeforeCommit,
    AfterCommit,
}

#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_AT: std::cell::Cell<Option<FailPoint>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn fail_at(point: FailPoint) -> Result<(), InstallError> {
    if FAIL_AT.with(|f| f.get()) == Some(point) {
        return Err(InstallError::Io(format!(
            "simulated interruption at {point:?}"
        )));
    }
    Ok(())
}

#[cfg(not(test))]
#[inline(always)]
fn fail_at(_point: FailPoint) -> Result<(), InstallError> {
    Ok(())
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), InstallError> {
    if cancel.load(Ordering::SeqCst) {
        Err(InstallError::Cancelled)
    } else {
        Ok(())
    }
}

/// Whether this version may be installed here at all.
pub fn preflight(
    library_root: &Path,
    package: &CataloguePackage,
    version: &CatalogueVersion,
    app: &semver::Version,
) -> Result<(), InstallError> {
    if !version.is_installable(app) {
        return Err(InstallError::NotInstallable(
            compatibility_label(version, app).0,
        ));
    }
    if version_dir(library_root, &package.id, &version.version).exists() {
        return Err(InstallError::AlreadyInstalled(format!(
            "{}@{}",
            package.id, version.version
        )));
    }
    Ok(())
}

/// Download the artifact from the URL in the validated catalogue (never
/// from anything inside a package), capped at its declared size.
pub fn download(
    transport: &dyn Transport,
    version: &CatalogueVersion,
) -> Result<Vec<u8>, InstallError> {
    match transport.fetch(&version.artifact.url, None, version.artifact.size as usize) {
        Ok(FetchResponse::Body { bytes, .. }) => Ok(bytes),
        Ok(FetchResponse::NotModified) => Err(InstallError::Download(FetchError::Status(304))),
        Err(error) => Err(InstallError::Download(error)),
    }
}

/// Download, verify and install.
pub fn install(
    transport: &dyn Transport,
    library_root: &Path,
    package: &CataloguePackage,
    version: &CatalogueVersion,
    app: &semver::Version,
    cancel: &AtomicBool,
) -> Result<Installed, InstallError> {
    preflight(library_root, package, version, app)?;
    check_cancel(cancel)?;
    let bytes = download(transport, version)?;
    check_cancel(cancel)?;
    install_artifact(library_root, package, version, app, &bytes, cancel)
}

/// Verify downloaded bytes and install them.
pub fn install_artifact(
    library_root: &Path,
    package: &CataloguePackage,
    version: &CatalogueVersion,
    app: &semver::Version,
    bytes: &[u8],
    cancel: &AtomicBool,
) -> Result<Installed, InstallError> {
    preflight(library_root, package, version, app)?;
    verify_download(bytes, version).map_err(InstallError::Rejected)?;
    let artifact = parse_artifact(bytes, package, version).map_err(InstallError::Rejected)?;
    // The catalogue declaration is not trusted: the graphs are scanned.
    let capabilities = scan_files(artifact.texts()).map_err(InstallError::Rejected)?;
    check_declared(&version.known_capabilities(), &capabilities).map_err(InstallError::Rejected)?;
    // P4.3.2C: the content identity of exactly these files.
    let tree_sha256 = crate::library::trust::digest::digest_of_files(
        artifact
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.text.as_bytes())),
    )
    .map_err(|e| InstallError::Rejected(format!("The package cannot be identified: {e}")))?;
    check_cancel(cancel)?;
    let package = stage_and_commit(
        library_root,
        package,
        version,
        &artifact,
        &tree_sha256,
        cancel,
    )?;
    Ok(Installed {
        package,
        capabilities,
        classification_version: super::scan::CLASSIFICATION_VERSION,
        tree_sha256,
        artifact_url: version.artifact.url.clone(),
        artifact_sha256: version.artifact.sha256.to_ascii_lowercase(),
        declared: version.known_capabilities(),
    })
}

fn stage_and_commit(
    library_root: &Path,
    package: &CataloguePackage,
    version: &CatalogueVersion,
    artifact: &ParsedArtifact,
    tree_sha256: &str,
    cancel: &AtomicBool,
) -> Result<VerifiedPackage, InstallError> {
    let io = |e: std::io::Error| InstallError::Io(e.to_string());
    let final_dir = version_dir(library_root, &package.id, &version.version);
    let package_root = package_dir(library_root, &package.id);
    let package_root_existed = package_root.is_dir();
    std::fs::create_dir_all(&package_root).map_err(io)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let staging = package_root.join(format!("{STAGING_PREFIX}{}-{nanos}", std::process::id()));
    std::fs::create_dir(&staging).map_err(io)?;

    let mut committed = false;
    let result = (|| -> Result<VerifiedPackage, InstallError> {
        for (index, file) in artifact.files.iter().enumerate() {
            fail_at(FailPoint::Writing(index))?;
            check_cancel(cancel)?;
            // Paths were validated: plain `/`-separated relative segments.
            let target: PathBuf = file
                .path
                .split('/')
                .fold(staging.clone(), |path, segment| path.join(segment));
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            crate::library::copy_exclusive_from(&mut file.text.as_bytes(), &target)
                .map_err(InstallError::Io)?;
        }
        fail_at(FailPoint::BeforeVerify)?;
        // The existing P4.1/P4.2 verification, on the files as written.
        let staged = verify_version_dir_static(&staging, false).map_err(InstallError::Rejected)?;
        if staged.manifest != artifact.manifest {
            return Err(InstallError::Rejected(
                "The staged manifest differs from the verified one.".to_string(),
            ));
        }
        check_cancel(cancel)?;
        fail_at(FailPoint::BeforeCommit)?;
        let label = format!("{}@{}", package.id, version.version);
        if final_dir.exists() {
            return Err(InstallError::AlreadyInstalled(label));
        }
        std::fs::rename(&staging, &final_dir).map_err(|e| {
            if final_dir.exists() {
                InstallError::AlreadyInstalled(label.clone())
            } else {
                InstallError::Io(e.to_string())
            }
        })?;
        committed = true;
        fail_at(FailPoint::AfterCommit)?;
        let verified =
            verify_version_dir_static(&final_dir, true).map_err(InstallError::Rejected)?;
        // P4.3.2C: the folder holds exactly the verified files, nothing else.
        let on_disk =
            crate::library::trust::digest::read_tree(&final_dir).map_err(InstallError::Rejected)?;
        if on_disk.digest != tree_sha256 {
            return Err(InstallError::Rejected(
                "The installed files differ from the verified package.".to_string(),
            ));
        }
        Ok(verified)
    })();

    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
        if committed {
            // Only the folder this call just created.
            let _ = std::fs::remove_dir_all(&final_dir);
        }
        if !package_root_existed {
            let _ = std::fs::remove_dir(&package_root);
        }
    }
    result
}

#[cfg(test)]
mod tests;
