//! P4.3.4: the trusted "Submission paths" guard.
//!
//! `validate.yml` runs on `pull_request`, so a pull request can replace it
//! for its own run and report a successful "Validate submission" check. This
//! guard closes that gap. It runs from `submission-paths.yml` on
//! `pull_request_target`, which GitHub always takes from the default branch,
//! never from the pull request. It checks out nothing from the pull request
//! and reads only GitHub's own API answers:
//! - the pull request before and after listing its files (author's numeric
//!   id, head commit, base branch, number of changed files);
//! - every page of the changed-file list.
//!
//! Contributors may change only `publishers/` and `packages/`. As long as
//! this check passes, the pull request has not touched `.github/`, so the
//! "Validate submission" run is the reviewed workflow. Everything fails
//! closed: an incomplete, inconsistent or unreadable answer is a failure.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::{ARTIFACTS_DIR, INDEX_FILE, Issues, PACKAGES_DIR, PUBLISHERS_DIR};

/// Files per page requested from the API; `submission-paths.yml` asks for
/// exactly this many.
pub const PER_PAGE: usize = 100;
/// GitHub lists at most 3000 changed files for a pull request. A larger pull
/// request cannot be fully verified, so it is refused.
pub const MAX_LISTED_FILES: u64 = 3000;

#[derive(Deserialize)]
struct UserJson {
    id: u64,
    login: String,
}

#[derive(Deserialize)]
struct RefJson {
    sha: String,
    #[serde(rename = "ref")]
    name: String,
}

#[derive(Deserialize)]
struct PullJson {
    changed_files: u64,
    user: UserJson,
    head: RefJson,
    base: RefJson,
}

/// One entry of GitHub's changed-file list.
#[derive(Debug, Clone, Deserialize)]
pub struct ChangedFile {
    pub filename: String,
    pub status: String,
    #[serde(default)]
    pub previous_filename: Option<String>,
}

/// What GitHub reported about the pull request at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub author_id: u64,
    pub author_login: String,
    pub head_sha: String,
    pub base_ref: String,
    pub changed_files: u64,
}

fn api_error(bytes: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Error {
        message: String,
    }
    match serde_json::from_slice::<Error>(bytes) {
        Ok(error) => format!(
            "the API answered with an error: {}",
            error.message.chars().take(200).collect::<String>()
        ),
        Err(_) => "the API answer is not what was expected".to_string(),
    }
}

/// Read a pull request (`GET /repos/{owner}/{repo}/pulls/{number}`).
pub fn read_pull(bytes: &[u8]) -> Result<Snapshot, String> {
    let pull: PullJson =
        serde_json::from_slice(bytes).map_err(|_| format!("pull request: {}", api_error(bytes)))?;
    Ok(Snapshot {
        author_id: pull.user.id,
        author_login: pull.user.login,
        head_sha: pull.head.sha,
        base_ref: pull.base.name,
        changed_files: pull.changed_files,
    })
}

/// Maintainers' numeric GitHub account ids, comma-separated (the repository
/// variable `MAINTAINER_IDS`). A malformed entry, such as a login, refuses
/// the whole list: a mistake must never grant or silently drop the
/// exception.
pub fn parse_maintainer_ids(text: &str) -> Result<BTreeSet<u64>, String> {
    let mut ids = BTreeSet::new();
    for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let valid = entry.bytes().all(|b| b.is_ascii_digit()) && !entry.starts_with('0');
        match entry.parse::<u64>() {
            Ok(id) if valid => {
                ids.insert(id);
            }
            _ => {
                return Err(format!(
                    "MAINTAINER_IDS must list numeric GitHub account ids; '{}' is not one",
                    entry.chars().take(40).collect::<String>()
                ));
            }
        }
    }
    Ok(ids)
}

/// Join every page of the changed-file list, refusing anything that is not
/// provably complete: each page but the last is full, the last is not, and
/// the total is exactly the pull request's `changed_files`.
pub fn collect_pages(pages: &[Vec<u8>], expected: u64) -> Result<Vec<ChangedFile>, Issues> {
    if expected > MAX_LISTED_FILES {
        return Err(vec![format!(
            "the pull request changes {expected} files; at most {MAX_LISTED_FILES} can be \
             verified, so split it"
        )]);
    }
    if pages.is_empty() {
        return Err(vec![
            "no page of the changed-file list was retrieved".into(),
        ]);
    }
    let mut files = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let number = index + 1;
        let entries: Vec<ChangedFile> = serde_json::from_slice(page)
            .map_err(|_| vec![format!("changed files, page {number}: {}", api_error(page))])?;
        let last = number == pages.len();
        if !last && entries.len() != PER_PAGE {
            return Err(vec![format!(
                "changed files, page {number}: {} entries before the last page (expected \
                 {PER_PAGE}); the list is incomplete",
                entries.len()
            )]);
        }
        if last && entries.len() >= PER_PAGE {
            return Err(vec![format!(
                "changed files: page {number} is full, so later pages are missing"
            )]);
        }
        files.extend(entries);
    }
    if files.len() as u64 != expected {
        return Err(vec![format!(
            "changed files: {} listed, but the pull request changes {expected}; the list is \
             incomplete",
            files.len()
        )]);
    }
    let mut seen = BTreeSet::new();
    for file in &files {
        if !seen.insert(file.filename.as_str()) {
            return Err(vec![format!(
                "changed files: '{}' is listed twice",
                file.filename
            )]);
        }
    }
    Ok(files)
}

/// A repository path as GitHub reports it: relative, `/`-separated, no
/// empty, `.` or `..` segments, no backslashes or control characters.
fn well_formed(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// Generated by the Publish workflow; nobody changes them in a pull request.
fn generated(path: &str) -> bool {
    path == INDEX_FILE || path == ARTIFACTS_DIR || path.starts_with(&format!("{ARTIFACTS_DIR}/"))
}

/// The only places an ordinary contributor may change. The comparison is
/// exact (case-sensitive): `Packages/` or `.GitHub/` are refused.
fn contributor_area(path: &str) -> bool {
    [PUBLISHERS_DIR, PACKAGES_DIR].iter().any(|dir| {
        path.strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Path rules for one changed file. A rename is checked at both its old and
/// its new path, so moving a protected file away (or onto one) is refused.
pub fn check_file(file: &ChangedFile, maintainer: bool) -> Vec<String> {
    let mut paths = vec![file.filename.as_str()];
    match (file.status.as_str(), &file.previous_filename) {
        ("renamed" | "copied", Some(previous)) => paths.push(previous),
        ("renamed" | "copied", None) => {
            return vec![format!(
                "{}: {} without its previous path",
                file.filename, file.status
            )];
        }
        _ => {}
    }
    let mut issues = Vec::new();
    for path in paths {
        let problem = if !well_formed(path) {
            Some("not a valid repository path")
        } else if generated(path) {
            Some("generated by the Publish workflow; never changed in a pull request")
        } else if !maintainer && !contributor_area(path) {
            Some("outside publishers/ and packages/; only maintainers change this")
        } else {
            None
        };
        if let Some(problem) = problem {
            issues.push(format!("{path} ({}): {problem}", file.status));
        }
    }
    issues
}

/// The whole guard. `before` and `after` are the pull request as the API
/// returned it before and after listing its files; `head_sha` and
/// `default_branch` come from GitHub's event. Returns a summary or every
/// problem found.
pub fn check_submission_paths(
    before: &[u8],
    after: &[u8],
    pages: &[Vec<u8>],
    head_sha: &str,
    default_branch: &str,
    maintainer_ids: &str,
) -> Result<String, Issues> {
    let maintainers = parse_maintainer_ids(maintainer_ids).map_err(|e| vec![e])?;
    let before = read_pull(before).map_err(|e| vec![e])?;
    let after = read_pull(after).map_err(|e| vec![e])?;
    let sha_ok = |sha: &str| {
        sha.len() == 40
            && sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if !sha_ok(head_sha) {
        return Err(vec!["--head-sha must be a full commit hash".into()]);
    }
    if before.head_sha != head_sha || after != before {
        return Err(vec![
            "the pull request changed while it was being checked; the newer push is checked \
             by its own run"
                .into(),
        ]);
    }
    if default_branch.is_empty() || before.base_ref != default_branch {
        return Err(vec![format!(
            "the pull request targets '{}'; submissions target the default branch '{}'",
            before.base_ref, default_branch
        )]);
    }
    let files = collect_pages(pages, before.changed_files)?;
    let maintainer = maintainers.contains(&before.author_id);
    let issues: Issues = files
        .iter()
        .flat_map(|file| check_file(file, maintainer))
        .collect();
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok(format!(
        "submission paths accepted: {} changed file(s) by {} (id {}{})",
        files.len(),
        before.author_login,
        before.author_id,
        if maintainer { ", maintainer" } else { "" }
    ))
}

#[cfg(test)]
mod tests;
