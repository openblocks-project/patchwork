//! P4.3.1B: `.pwpkg` artifacts.
//!
//! An artifact is one JSON document (not an archive):
//!
//! ```json
//! { "pwpkg": 1, "id": "acme.gain", "version": "1.2.0",
//!   "files": [ { "path": "package.json", "text": "…" },
//!              { "path": "Gain.pwnode",  "text": "…" } ] }
//! ```
//!
//! Files are a list (so duplicate paths are visible, unlike JSON object
//! keys) of UTF-8 text. There are no permissions, links, timestamps or
//! hooks to carry. Everything is validated in memory before a single byte
//! is written.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::limits::{MAX_ARTIFACT_BYTES, MAX_ARTIFACT_FILE_BYTES, MAX_ARTIFACT_FILES};
use super::{
    ARTIFACT_FORMAT, ArtifactPath, CataloguePackage, CatalogueVersion, check_artifact_listing,
    check_manifest_matches, split_public_id, validate_artifact_path,
};
use crate::library::package::{MANIFEST_FILE, PackageManifest};

/// Deepest JSON nesting accepted in the envelope and in every file.
pub const MAX_JSON_DEPTH: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    pwpkg: u32,
    id: String,
    version: String,
    files: Vec<EnvelopeFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeFile {
    path: String,
    text: String,
}

/// One validated file.
#[derive(Debug, Clone)]
pub struct ArtifactFile {
    pub path: String,
    pub kind: ArtifactPath,
    pub text: String,
}

/// A fully validated artifact, ready to be written to staging.
#[derive(Debug, Clone)]
pub struct ParsedArtifact {
    pub files: Vec<ArtifactFile>,
    pub manifest: PackageManifest,
}

impl ParsedArtifact {
    pub fn texts(&self) -> impl Iterator<Item = (&str, &str)> {
        self.files
            .iter()
            .map(|f| (f.path.as_str(), f.text.as_str()))
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Deepest `{`/`[` nesting outside strings (stops counting past `limit`).
pub fn json_depth_exceeds(text: &str, limit: usize) -> bool {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for byte in text.bytes() {
        if in_string {
            match (escaped, byte) {
                (true, _) => escaped = false,
                (false, b'\\') => escaped = true,
                (false, b'"') => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > limit {
                    return true;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

/// The downloaded bytes are exactly the catalogue's artifact.
pub fn verify_download(bytes: &[u8], version: &CatalogueVersion) -> Result<(), String> {
    if bytes.len() as u64 != version.artifact.size {
        return Err(format!(
            "The download is {} bytes; the catalogue says {}.",
            bytes.len(),
            version.artifact.size
        ));
    }
    let actual = hex_sha256(bytes);
    if actual != version.artifact.sha256 {
        return Err(format!(
            "The download does not match the catalogue checksum (got {actual})."
        ));
    }
    Ok(())
}

/// Parse and fully validate an artifact against its catalogue entry.
pub fn parse_artifact(
    bytes: &[u8],
    package: &CataloguePackage,
    version: &CatalogueVersion,
) -> Result<ParsedArtifact, String> {
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "The artifact is larger than {MAX_ARTIFACT_BYTES} bytes."
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "The artifact is not UTF-8.".to_string())?;
    if json_depth_exceeds(text, MAX_JSON_DEPTH) {
        return Err("The artifact is nested too deeply.".to_string());
    }
    let envelope: Envelope =
        serde_json::from_str(text).map_err(|e| format!("The artifact is not valid: {e}"))?;
    if envelope.pwpkg != ARTIFACT_FORMAT {
        return Err(format!(
            "Unsupported artifact format {} (this Patchwork reads {ARTIFACT_FORMAT}).",
            envelope.pwpkg
        ));
    }
    if envelope.id != package.id || envelope.version != version.version.to_string() {
        return Err(format!(
            "The artifact is {}@{}, not {}@{}.",
            envelope.id, envelope.version, package.id, version.version
        ));
    }
    if envelope.files.len() > MAX_ARTIFACT_FILES as usize
        || envelope.files.len() != version.artifact.files as usize
    {
        return Err(format!(
            "The artifact has {} files; the catalogue says {}.",
            envelope.files.len(),
            version.artifact.files
        ));
    }
    check_artifact_listing(
        envelope.files.iter().map(|f| f.path.as_str()),
        &version.entry,
    )?;

    let mut total = 0u64;
    let mut files = Vec::with_capacity(envelope.files.len());
    for file in envelope.files {
        let size = file.text.len() as u64;
        if size > MAX_ARTIFACT_FILE_BYTES {
            return Err(format!(
                "{} is larger than {MAX_ARTIFACT_FILE_BYTES} bytes.",
                file.path
            ));
        }
        total += size;
        if total > MAX_ARTIFACT_BYTES {
            return Err(format!(
                "The artifact's files add up to more than {MAX_ARTIFACT_BYTES} bytes."
            ));
        }
        if json_depth_exceeds(&file.text, MAX_JSON_DEPTH) {
            return Err(format!("{} is nested too deeply.", file.path));
        }
        let kind = validate_artifact_path(&file.path, &version.entry)?;
        files.push(ArtifactFile {
            path: file.path,
            kind,
            text: file.text,
        });
    }
    let text_of = |path: &str| {
        files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.text.as_str())
    };

    // P4.3.2B: a package carrying credentials is refused (they would be
    // the publisher's, shared with everyone, or bait for exfiltration).
    for file in &files {
        let findings = crate::library::secrets::find_secrets_in_text(&file.text);
        if !findings.is_empty() {
            return Err(crate::library::secrets::describe(&file.path, &findings));
        }
    }

    // Root manifest: identity, formats, dependencies (P4.3.0 contract).
    let manifest: PackageManifest = serde_json::from_str(text_of(MANIFEST_FILE).unwrap_or(""))
        .map_err(|e| format!("The artifact's {MANIFEST_FILE} is not valid: {e}"))?;
    check_manifest_matches(package, version, &manifest)?;
    let entry_text = text_of(&version.entry).unwrap_or("");
    if hex_sha256(entry_text.as_bytes()) != manifest.sha256.to_ascii_lowercase() {
        return Err(format!(
            "{} does not match the checksum in {MANIFEST_FILE}.",
            version.entry
        ));
    }

    // Bundled files: exactly the declared set, with matching checksums.
    let bundled: BTreeSet<&str> = files
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                ArtifactPath::DependencyManifest { .. } | ArtifactPath::DependencyFile { .. }
            )
        })
        .map(|f| f.path.as_str())
        .collect();
    let declared: BTreeSet<&str> = manifest.files.keys().map(String::as_str).collect();
    if let Some(extra) = bundled.difference(&declared).next() {
        return Err(format!(
            "{extra} is in the artifact but not in {MANIFEST_FILE}."
        ));
    }
    if let Some(missing) = declared.difference(&bundled).next() {
        return Err(format!(
            "{missing} is declared in {MANIFEST_FILE} but missing."
        ));
    }
    for (path, expected) in &manifest.files {
        let text = text_of(path).unwrap_or("");
        if hex_sha256(text.as_bytes()) != expected.to_ascii_lowercase() {
            return Err(format!("{path} does not match its checksum."));
        }
    }

    // Each bundled dependency: a valid manifest for exactly its folder,
    // a public id, and its entry present.
    let mut dependency_dirs: BTreeMap<(String, semver::Version), &str> = BTreeMap::new();
    for file in &files {
        if let ArtifactPath::DependencyManifest { id, version } = &file.kind {
            dependency_dirs.insert((id.clone(), version.clone()), file.text.as_str());
        }
    }
    for ((id, dep_version), text) in &dependency_dirs {
        let dep: PackageManifest = serde_json::from_str(text)
            .map_err(|e| format!("The manifest of bundled {id}@{dep_version} is not valid: {e}"))?;
        let parsed = dep
            .validate()
            .map_err(|e| format!("Bundled {id}@{dep_version}: {e}"))?;
        if dep.id != *id || parsed != *dep_version {
            return Err(format!(
                "The bundled folder {id}@{dep_version} holds {}@{parsed}.",
                dep.id
            ));
        }
        split_public_id(id).map_err(|e| format!("Bundled dependency {id}: {e}"))?;
        let entry_path = format!("deps/{id}/{dep_version}/{}", dep.entry);
        if text_of(&entry_path).is_none() {
            return Err(format!("Bundled {id}@{dep_version} is missing its entry."));
        }
    }
    // No silent substitution: every declared dependency is bundled as
    // exactly that version.
    for dependency in &manifest.dependencies {
        let version = crate::library::package::parse_stable_version(&dependency.version)?;
        if !dependency_dirs.contains_key(&(dependency.id.clone(), version.clone())) {
            return Err(format!(
                "Dependency {}@{version} is declared but not bundled.",
                dependency.id
            ));
        }
    }
    Ok(ParsedArtifact { files, manifest })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_depth_is_measured_outside_strings() {
        assert!(!json_depth_exceeds("{\"a\":[1,{\"b\":2}]}", 3));
        assert!(json_depth_exceeds("{\"a\":[1,{\"b\":2}]}", 2));
        assert!(!json_depth_exceeds("{\"a\":\"[[[[[[[[\\\"[[[[\"}", 1));
        assert!(json_depth_exceeds(&"[".repeat(100), MAX_JSON_DEPTH));
    }
}
