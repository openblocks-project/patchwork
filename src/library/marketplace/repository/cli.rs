//! P4.3.4: `patchwork catalogue …`, the headless tool the catalogue
//! repository's CI runs. It opens no window, reads no user settings, uses
//! no network and never runs package content.
//!
//! ```text
//! patchwork catalogue validate          --root DIR --repository OWNER/REPO [--branch B]
//! patchwork catalogue check-pr          --base DIR --head DIR --repository OWNER/REPO [--branch B]
//!                                       --author-login LOGIN --author-id ID [--maintainer]
//! patchwork catalogue publish-artifacts --root DIR --repository OWNER/REPO [--branch B]
//! patchwork catalogue publish-index     --root DIR --repository OWNER/REPO [--branch B]
//!                                       --commit SHA --generated-at UNIX_SECONDS
//! patchwork catalogue check-paths       --repository OWNER/REPO --pull-before FILE
//!                                       --pull-after FILE --pages DIR --head-sha SHA
//!                                       --default-branch B --maintainer-ids "ID,ID"
//! ```
//!
//! The repository identity is always given by the caller (CI passes
//! `GITHUB_REPOSITORY`); nothing about the production repository is built
//! in. Exit status: 0 success, 1 problems found, 2 usage error.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::super::source::CatalogueLocation;

const USAGE: &str = "usage: patchwork catalogue <validate|check-pr|publish-artifacts|publish-index|check-paths> \
    --repository OWNER/REPO [--branch BRANCH] …  (see docs/marketplace/catalogue-repository.md)";

/// Parse `--name value` pairs (and bare `--flag`s).
fn options(args: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let name = arg
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument '{arg}'"))?;
        let value = match iter.peek() {
            Some(next) if !next.starts_with("--") => iter.next().cloned().unwrap_or_default(),
            _ => String::new(),
        };
        if out.insert(name.to_string(), value).is_some() {
            return Err(format!("--{name} given twice"));
        }
    }
    Ok(out)
}

fn required<'a>(options: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str, String> {
    options
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("--{name} is required"))
}

fn location(options: &BTreeMap<String, String>) -> Result<CatalogueLocation, String> {
    let repository = required(options, "repository")?;
    let (owner, repo) = repository
        .split_once('/')
        .ok_or_else(|| "--repository must be OWNER/REPO".to_string())?;
    CatalogueLocation::new(
        owner,
        repo,
        options.get("branch").map(String::as_str).unwrap_or(""),
    )
}

fn report(issues: &[String]) -> i32 {
    for issue in issues {
        println!("::error::{issue}");
        eprintln!("error: {issue}");
    }
    eprintln!("{} problem(s) found.", issues.len());
    1
}

/// The changed-file pages `1.json`, `2.json`, ... in order. Any other file,
/// or a gap in the numbering, is refused: the list must be provably whole.
fn read_pages(dir: &std::path::Path) -> Result<Vec<Vec<u8>>, String> {
    let mut numbers = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("--pages {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("--pages: {e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let number = name
            .strip_suffix(".json")
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n > 0 && name == format!("{n}.json"))
            .ok_or_else(|| format!("--pages: unexpected file '{name}'"))?;
        numbers.push(number);
    }
    numbers.sort_unstable();
    if numbers.iter().enumerate().any(|(i, n)| *n != i + 1) {
        return Err("--pages: pages must be numbered 1.json, 2.json, ... without gaps".into());
    }
    numbers
        .iter()
        .map(|n| std::fs::read(dir.join(format!("{n}.json"))).map_err(|e| format!("--pages: {e}")))
        .collect()
}

/// Run a command; returns the process exit status.
pub fn run(args: Vec<String>) -> i32 {
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let parsed = options(rest).and_then(|options| location(&options).map(|l| (options, l)));
    let (options, location) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("error: {error}\n{USAGE}");
            return 2;
        }
    };
    let path = |name: &str| required(&options, name).map(PathBuf::from);
    let result: Result<Result<String, Vec<String>>, String> = (|| {
        Ok(match command.as_str() {
            "validate" => {
                let root = path("root")?;
                super::read_repository(&root).and_then(|repository| {
                    let previous = super::previous_index(&root, &location)?;
                    let catalogue = super::validate(&repository, &location, previous.as_ref())?;
                    let versions: usize =
                        catalogue.packages.values().map(|p| p.versions.len()).sum();
                    Ok(format!(
                        "valid: {} publisher(s), {} package(s), {versions} version(s)",
                        catalogue.publishers.len(),
                        catalogue.packages.len()
                    ))
                })
            }
            "check-pr" => {
                let author = super::Author {
                    login: required(&options, "author-login")?.to_string(),
                    id: required(&options, "author-id")?
                        .parse()
                        .map_err(|_| "--author-id must be the numeric account id".to_string())?,
                    maintainer: options.contains_key("maintainer"),
                };
                super::check_pull_request(&path("base")?, &path("head")?, &location, &author).map(
                    |report| {
                        format!(
                            "pull request accepted by validation: new namespaces [{}], new \
                             versions [{}]",
                            report.new_publishers.join(", "),
                            report.new_versions.join(", ")
                        )
                    },
                )
            }
            "publish-artifacts" => {
                super::publish_artifacts(&path("root")?, &location).map(|written| {
                    format!(
                        "{} artifact(s) generated: {}",
                        written.len(),
                        written.join(" ")
                    )
                })
            }
            "publish-index" => {
                let root = path("root")?;
                let commit = required(&options, "commit")?;
                let generated_at = required(&options, "generated-at")?
                    .parse()
                    .map_err(|_| "--generated-at must be Unix seconds".to_string())?;
                super::publish_index(&root, &location, commit, generated_at).and_then(|bytes| {
                    std::fs::write(root.join(super::INDEX_FILE), &bytes)
                        .map_err(|e| vec![format!("{}: {e}", super::INDEX_FILE)])?;
                    Ok(format!(
                        "{} written ({} bytes)",
                        super::INDEX_FILE,
                        bytes.len()
                    ))
                })
            }
            "check-paths" => {
                let read = |name: &str| -> Result<Vec<u8>, String> {
                    let file = path(name)?;
                    std::fs::read(&file).map_err(|e| format!("--{name} {}: {e}", file.display()))
                };
                let before = read("pull-before")?;
                let after = read("pull-after")?;
                let pages = read_pages(&path("pages")?)?;
                // Present even when empty: a missing variable must be noticed.
                let maintainer_ids = options
                    .get("maintainer-ids")
                    .ok_or_else(|| "--maintainer-ids is required (may be empty)".to_string())?;
                super::paths::check_submission_paths(
                    &before,
                    &after,
                    &pages,
                    required(&options, "head-sha")?,
                    required(&options, "default-branch")?,
                    maintainer_ids,
                )
            }
            other => return Err(format!("unknown command '{other}'")),
        })
    })();
    match result {
        Ok(Ok(message)) => {
            println!("{message}");
            0
        }
        Ok(Err(issues)) => report(&issues),
        Err(error) => {
            eprintln!("error: {error}\n{USAGE}");
            2
        }
    }
}
