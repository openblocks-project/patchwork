//! P4.3.2C: deterministic content digest of a complete package folder.
//!
//! The digest covers every file of a version folder: `package.json`, the
//! entry and every bundled dependency. It is the same on Windows, macOS and
//! Linux because it never depends on the file system's order, separators,
//! case handling or Unicode normalization:
//!
//! * paths are relative, `/`-separated and made of plain ASCII segments;
//!   anything that different systems could read differently is refused
//!   (`..`, `.`, empty segments, `\`, `:`, control characters, trailing dots
//!   or spaces, reserved Windows device names, non-ASCII names, and two
//!   paths that differ only in letter case);
//! * symbolic links and other special files are refused, never followed;
//! * files are sorted by the bytes of their canonical path;
//! * the hash input is length-delimited, so no two different trees can
//!   produce the same byte stream:
//!
//! ```text
//! SHA-256( "patchwork-package-tree\0"  u32be(TREE_FORMAT)  u64be(file count)
//!          for each file:  u64be(path length) path  u64be(size) SHA-256(file) )
//! ```
//!
//! Empty folders carry no content and are not part of the digest.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Version of the digest construction above (stored with each record).
pub const TREE_FORMAT: u32 = 1;
const DOMAIN: &[u8] = b"patchwork-package-tree\0";

/// Limits for one package folder.
pub const MAX_TREE_FILES: usize = 4096;
pub const MAX_TREE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_TREE_DEPTH: usize = 16;
pub const MAX_PATH_BYTES: usize = 1024;

/// One file of a package tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeFile {
    /// Canonical relative path (`deps/acme.x/1.0.0/X.pwnode`).
    pub path: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// A package folder read once: its digest, its files and the text of its
/// graphs and manifest (so scanning sees exactly the hashed bytes).
#[derive(Debug, Clone)]
pub struct PackageTree {
    /// Lowercase hex SHA-256 tree digest.
    pub digest: String,
    /// Every file, sorted by canonical path.
    #[allow(dead_code)]
    pub files: Vec<TreeFile>,
    /// `(path, text)` of every `.pwnode` file.
    pub graphs: Vec<(String, String)>,
    /// Text of the top-level `package.json`, if present.
    pub manifest: Option<String>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// One path segment that every supported system reads the same way.
pub fn check_segment(segment: &str) -> Result<(), String> {
    let shown = segment.escape_debug();
    if segment.is_empty() || segment == "." || segment == ".." {
        return Err(format!("the path segment '{shown}' is not allowed"));
    }
    if !segment.is_ascii() {
        return Err(format!(
            "'{shown}' contains non-ASCII characters, which are not stored the same way on every system"
        ));
    }
    if segment.chars().any(|c| {
        c.is_ascii_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return Err(format!(
            "'{shown}' contains a character that is not allowed in a package path"
        ));
    }
    if segment.ends_with(['.', ' ']) || segment.starts_with(' ') {
        return Err(format!(
            "'{shown}' starts or ends with a space or ends with a dot"
        ));
    }
    let stem = segment.split('.').next().unwrap_or("").to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        return Err(format!("'{shown}' is a reserved device name on Windows"));
    }
    Ok(())
}

/// A canonical relative path: checked segments joined by `/`.
pub fn check_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return Err("a package path is empty or too long".to_string());
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_TREE_DEPTH {
        return Err(format!("'{}' is nested too deeply", path.escape_debug()));
    }
    for segment in segments {
        check_segment(segment)?;
    }
    Ok(())
}

/// The digest of already-checked, hashed files.
fn digest(files: &mut Vec<TreeFile>) -> Result<String, String> {
    if files.is_empty() {
        return Err("the package folder is empty".to_string());
    }
    if files.len() > MAX_TREE_FILES {
        return Err(format!("the package has more than {MAX_TREE_FILES} files"));
    }
    files.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    // Paths that differ only in case are the same file on Windows and
    // (usually) macOS; the same goes for a file and a folder of that name.
    // Every path and every folder prefix, keyed by its lowercase form.
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for file in files.iter() {
        let mut end = 0;
        for segment in file.path.split('/') {
            end += segment.len();
            let prefix = &file.path[..end];
            let original = seen
                .entry(prefix.to_ascii_lowercase())
                .or_insert_with(|| prefix.to_string());
            if original != prefix {
                return Err(format!(
                    "'{original}' and '{prefix}' differ only in letter case"
                ));
            }
            end += 1; // the '/'
        }
    }
    // A file and a folder with the same path cannot both exist on disk, but
    // an in-memory list could claim it.
    let paths: std::collections::HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    if paths.len() != files.len() {
        return Err("a package path is listed twice".to_string());
    }
    for file in files.iter() {
        for (index, _) in file.path.match_indices('/') {
            if paths.contains(&file.path[..index]) {
                return Err(format!(
                    "'{}' is both a file and a folder",
                    &file.path[..index]
                ));
            }
        }
    }
    let total: u64 = files.iter().map(|f| f.size).sum();
    if total > MAX_TREE_BYTES {
        return Err("the package is too large".to_string());
    }
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(TREE_FORMAT.to_be_bytes());
    hasher.update((files.len() as u64).to_be_bytes());
    for file in files.iter() {
        hasher.update((file.path.len() as u64).to_be_bytes());
        hasher.update(file.path.as_bytes());
        hasher.update(file.size.to_be_bytes());
        hasher.update(file.sha256);
    }
    Ok(hex(&hasher.finalize()))
}

/// Digest of files held in memory (`(canonical path, bytes)`), e.g. a
/// verified marketplace artifact before it is written.
pub fn digest_of_files<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<String, String> {
    let mut tree = Vec::new();
    for (path, bytes) in files {
        check_path(path)?;
        tree.push(TreeFile {
            path: path.to_string(),
            size: bytes.len() as u64,
            sha256: Sha256::digest(bytes).into(),
        });
    }
    digest(&mut tree)
}

/// Read a package folder: every file is hashed, nothing is followed.
pub fn read_tree(dir: &Path) -> Result<PackageTree, String> {
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let mut files = Vec::new();
    let mut graphs = Vec::new();
    let mut manifest = None;
    let mut total = 0u64;
    let mut pending = vec![(dir.to_path_buf(), String::new(), 0usize)];
    while let Some((folder, prefix, depth)) = pending.pop() {
        if depth >= MAX_TREE_DEPTH {
            return Err(format!("'{prefix}' is nested too deeply"));
        }
        let entries = std::fs::read_dir(&folder)
            .map_err(|e| format!("Cannot read {}: {e}", folder.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("Cannot read {}: {e}", folder.display()))?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| format!("a file name in '{prefix}' is not valid Unicode"))?;
            check_segment(name)?;
            let rel = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            check_path(&rel)?;
            // Never follows links: the entry itself is inspected.
            let kind = entry
                .file_type()
                .map_err(|e| format!("Cannot read {rel}: {e}"))?;
            if kind.is_symlink() {
                return Err(format!("'{rel}' is a link, which packages may not contain"));
            }
            if kind.is_dir() {
                pending.push((entry.path(), rel, depth + 1));
                continue;
            }
            if !kind.is_file() {
                return Err(format!("'{rel}' is not a regular file"));
            }
            if files.len() >= MAX_TREE_FILES {
                return Err(format!("the package has more than {MAX_TREE_FILES} files"));
            }
            let bytes =
                std::fs::read(entry.path()).map_err(|e| format!("Cannot read {rel}: {e}"))?;
            total += bytes.len() as u64;
            if total > MAX_TREE_BYTES {
                return Err("the package is too large".to_string());
            }
            if rel.ends_with(".pwnode") || rel == super::super::package::MANIFEST_FILE {
                let text = String::from_utf8(bytes.clone())
                    .map_err(|_| format!("'{rel}' is not valid UTF-8 text"))?;
                if rel.ends_with(".pwnode") {
                    graphs.push((rel.clone(), text));
                } else {
                    manifest = Some(text);
                }
            }
            files.push(TreeFile {
                size: bytes.len() as u64,
                sha256: Sha256::digest(&bytes).into(),
                path: rel,
            });
        }
    }
    let digest = digest(&mut files)?;
    graphs.sort();
    Ok(PackageTree {
        digest,
        files,
        graphs,
        manifest,
    })
}
