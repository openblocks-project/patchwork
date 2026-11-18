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
//! ```
//!
//! The repository identity is always given by the caller (CI passes
//! `GITHUB_REPOSITORY`); nothing about the production repository is built
//! in. Exit status: 0 success, 1 problems found, 2 usage error.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::super::source::CatalogueLocation;

const USAGE: &str = "usage: patchwork catalogue <validate|check-pr|publish-artifacts|publish-index> \
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
