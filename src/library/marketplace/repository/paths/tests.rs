//! P4.3.4: the trusted "Submission paths" guard. Fixtures are the GitHub API
//! answers the workflow saves (pull request, changed-file pages), built
//! deterministically.

use super::*;
use crate::library::package::tests::TempDir;
use serde_json::{Value, json};

const HEAD: &str = "cccccccccccccccccccccccccccccccccccccccc";
const OTHER_HEAD: &str = "dddddddddddddddddddddddddddddddddddddddd";
const CONTRIBUTOR: u64 = 101;
const MAINTAINER: u64 = 900;
const MAINTAINERS: &str = "900, 901";

fn pull(changed: usize, author: u64, head: &str, base: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "number": 7,
        "state": "open",
        "changed_files": changed,
        "user": {"login": format!("user{author}"), "id": author, "type": "User"},
        "head": {"sha": head, "ref": "submission", "repo": {"full_name": "someone/fork"}},
        "base": {"sha": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee", "ref": base},
    }))
    .unwrap()
}

fn entry(filename: &str, status: &str) -> Value {
    json!({"sha": "0", "filename": filename, "status": status, "additions": 1, "deletions": 0, "changes": 1})
}

fn renamed(from: &str, to: &str) -> Value {
    json!({"filename": to, "previous_filename": from, "status": "renamed"})
}

/// Split entries into API pages of `PER_PAGE`, always ending with a short
/// (possibly empty) page, as the workflow fetches them.
fn pages(entries: &[Value]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = entries
        .chunks(PER_PAGE)
        .map(|c| serde_json::to_vec(c).unwrap())
        .collect();
    if entries.len() % PER_PAGE == 0 {
        out.push(b"[]".to_vec());
    }
    out
}

/// Run the guard as the workflow does for a pull request by `author`.
fn check(author: u64, entries: &[Value]) -> Result<String, Issues> {
    let pr = pull(entries.len(), author, HEAD, "main");
    check_submission_paths(&pr, &pr, &pages(entries), HEAD, "main", MAINTAINERS)
}

fn refused(result: Result<String, Issues>, needle: &str) {
    match result {
        Ok(summary) => panic!("accepted ({summary}), expected '{needle}'"),
        Err(issues) => assert!(
            issues.iter().any(|i| i.contains(needle)),
            "expected '{needle}' in {issues:?}"
        ),
    }
}

#[test]
fn a_normal_package_submission_and_a_namespace_claim_are_accepted() {
    let submission = [
        entry("packages/acme/gain/1.0.0/package.json", "added"),
        entry("packages/acme/gain/1.0.0/gain.pwnode", "added"),
        entry(
            "packages/acme/gain/1.0.0/deps/acme.util/1.0.0/util.pwnode",
            "added",
        ),
        entry("packages/acme/gain/1.0.0/preview.png", "added"),
        entry("packages/acme/gain/package.meta.json", "modified"),
        entry("packages/acme/gain/README.md", "modified"),
    ];
    let summary = check(CONTRIBUTOR, &submission).unwrap();
    assert!(summary.contains("6 changed file(s)") && !summary.contains("maintainer"));
    let claim = [
        entry("publishers/acme.json", "added"),
        entry("packages/acme/gain/1.0.0/package.json", "added"),
    ];
    assert!(check(CONTRIBUTOR, &claim).is_ok());
}

#[test]
fn workflow_edits_are_refused_including_replacing_the_guard_itself() {
    for path in [
        ".github/workflows/validate.yml",
        ".github/workflows/submission-paths.yml",
        ".github/workflows/publish.yml",
        ".github/workflows/new.yml",
        ".github/CODEOWNERS",
    ] {
        for status in ["modified", "added", "removed"] {
            let entries = [
                entry("packages/acme/gain/1.0.0/package.json", "added"),
                entry(path, status),
            ];
            refused(check(CONTRIBUTOR, &entries), path);
        }
    }
    // Case variants are not the contributor area either (and never reach
    // GitHub's workflow folder through a case-insensitive checkout).
    refused(
        check(
            CONTRIBUTOR,
            &[entry(".GitHub/workflows/validate.yml", "added")],
        ),
        "outside publishers/ and packages/",
    );
    refused(
        check(CONTRIBUTOR, &[entry("Packages/acme/x.json", "added")]),
        "outside publishers/ and packages/",
    );
}

#[test]
fn generated_files_and_configuration_are_refused_even_for_maintainers() {
    for path in [
        "index.json",
        "artifacts/acme.gain/1.0.0/acme.gain-1.0.0.pwpkg",
        "artifacts/new.pwpkg",
    ] {
        for status in ["modified", "added", "removed"] {
            refused(check(CONTRIBUTOR, &[entry(path, status)]), "generated");
            refused(check(MAINTAINER, &[entry(path, status)]), "generated");
        }
    }
    for path in [
        "moderation.json",
        ".gitattributes",
        ".gitignore",
        "README.md",
        "CONTRIBUTING.md",
        "LICENSE",
        "publishers",
        "packages",
        "other/file.txt",
    ] {
        refused(
            check(CONTRIBUTOR, &[entry(path, "modified")]),
            "only maintainers change this",
        );
    }
    // Malformed paths are refused for everyone.
    for path in [
        "packages/../.github/x",
        "packages//x",
        "packages\\x",
        "/packages/x",
        "",
    ] {
        refused(check(MAINTAINER, &[entry(path, "added")]), "not a valid");
    }
}

#[test]
fn renamed_or_deleted_protected_files_are_refused() {
    // Moving a protected file into the contributor area, or out of it onto a
    // protected path, is checked at both ends.
    refused(
        check(
            CONTRIBUTOR,
            &[renamed(
                ".github/workflows/validate.yml",
                "packages/acme/x.yml",
            )],
        ),
        ".github/workflows/validate.yml (renamed)",
    );
    refused(
        check(
            CONTRIBUTOR,
            &[renamed(
                "packages/acme/x.yml",
                ".github/workflows/submission-paths.yml",
            )],
        ),
        ".github/workflows/submission-paths.yml (renamed)",
    );
    refused(
        check(
            MAINTAINER,
            &[renamed("artifacts/a/1.0.0/a-1.0.0.pwpkg", "packages/a/b")],
        ),
        "generated",
    );
    // A rename without its previous path cannot be verified.
    refused(
        check(
            CONTRIBUTOR,
            &[json!({"filename": "packages/acme/x", "status": "renamed"})],
        ),
        "without its previous path",
    );
    for path in [
        ".gitattributes",
        "moderation.json",
        ".github/workflows/validate.yml",
    ] {
        refused(check(CONTRIBUTOR, &[entry(path, "removed")]), path);
    }
    // Renames inside the contributor area are left to `check-pr`.
    assert!(
        check(
            CONTRIBUTOR,
            &[renamed(
                "packages/acme/a/README.md",
                "packages/acme/b/README.md"
            )]
        )
        .is_ok()
    );
}

#[test]
fn maintainers_are_identified_by_numeric_id_only() {
    let config = [entry(".github/workflows/validate.yml", "modified")];
    assert!(check(MAINTAINER, &config).unwrap().contains("maintainer"));
    // The same login with another id is an ordinary contributor: only the
    // id from GitHub's pull request record counts.
    let impostor = pull(1, CONTRIBUTOR, HEAD, "main");
    let impostor = String::from_utf8(impostor)
        .unwrap()
        .replace("user101", "user900");
    refused(
        check_submission_paths(
            impostor.as_bytes(),
            impostor.as_bytes(),
            &pages(&config),
            HEAD,
            "main",
            MAINTAINERS,
        ),
        "only maintainers",
    );
    // Malformed maintainer lists (logins, signs, zero) refuse the whole
    // check rather than granting or dropping the exception.
    let pr = pull(1, MAINTAINER, HEAD, "main");
    for bad in [
        "octocat",
        "900,alice",
        "+900",
        "-900",
        "900.0",
        "0",
        "0900",
        "1e3",
    ] {
        refused(
            check_submission_paths(&pr, &pr, &pages(&config), HEAD, "main", bad),
            "numeric GitHub account ids",
        );
    }
    // An empty list means no maintainers.
    refused(
        check_submission_paths(&pr, &pr, &pages(&config), HEAD, "main", ""),
        "only maintainers",
    );
    assert_eq!(
        parse_maintainer_ids(" 900 ,901,,").unwrap(),
        BTreeSet::from([900, 901])
    );
}

#[test]
fn every_page_is_required_and_api_errors_fail_closed() {
    let many: Vec<Value> = (0..250)
        .map(|i| entry(&format!("packages/acme/gain/1.0.{i}/package.json"), "added"))
        .collect();
    assert!(check(CONTRIBUTOR, &many).unwrap().contains("250 changed"));
    // Exactly one full page: the empty page after it proves the end.
    let hundred = &many[..100];
    assert_eq!(pages(hundred).len(), 2);
    assert!(check(CONTRIBUTOR, hundred).is_ok());

    let pr = pull(250, CONTRIBUTOR, HEAD, "main");
    let run = |pages: &[Vec<u8>]| check_submission_paths(&pr, &pr, pages, HEAD, "main", "");
    let all = pages(&many);
    // A missing last page, a missing middle page, no pages.
    refused(run(&all[..2]), "later pages are missing");
    refused(run(&[all[0].clone(), all[2].clone()]), "incomplete");
    refused(run(&[]), "no page");
    // A short page before the last.
    let short = serde_json::to_vec(&many[..99]).unwrap();
    refused(
        run(&[short, all[1].clone(), all[2].clone()]),
        "before the last page",
    );
    // API errors in place of a page, or garbage.
    let error = br#"{"message":"API rate limit exceeded","documentation_url":"x"}"#.to_vec();
    refused(
        run(&[all[0].clone(), error, all[2].clone()]),
        "API rate limit exceeded",
    );
    refused(run(&[b"<html>".to_vec()]), "not what was expected");
    refused(
        run(&[br#"[{"status":"added"}]"#.to_vec()]),
        "not what was expected",
    );
    // A total that does not add up (the count says 250, 249 are listed).
    let mut fewer = many.clone();
    fewer.pop();
    refused(run(&pages(&fewer)), "incomplete");
    // The same file twice.
    let mut twice = many.clone();
    twice[249] = twice[0].clone();
    refused(run(&pages(&twice)), "listed twice");
    // More files than GitHub can list.
    let huge = pull(3001, CONTRIBUTOR, HEAD, "main");
    refused(
        check_submission_paths(&huge, &huge, &[], HEAD, "main", ""),
        "at most 3000",
    );
    // The pull request itself unreadable.
    refused(
        check_submission_paths(br#"{"message":"Not Found"}"#, &pr, &all, HEAD, "main", ""),
        "Not Found",
    );
}

#[test]
fn a_pull_request_that_changes_during_the_check_or_targets_another_branch_fails() {
    let entries = [entry("packages/acme/gain/1.0.0/package.json", "added")];
    let pr = pull(1, CONTRIBUTOR, HEAD, "main");
    let pushed = pull(1, CONTRIBUTOR, OTHER_HEAD, "main");
    let more = pull(2, CONTRIBUTOR, HEAD, "main");
    let p = pages(&entries);
    refused(
        check_submission_paths(&pr, &pushed, &p, HEAD, "main", ""),
        "changed while",
    );
    refused(
        check_submission_paths(&pr, &more, &p, HEAD, "main", ""),
        "changed while",
    );
    refused(
        check_submission_paths(&pr, &pr, &p, OTHER_HEAD, "main", ""),
        "changed while",
    );
    refused(
        check_submission_paths(&pr, &pr, &p, "abc", "main", ""),
        "full commit hash",
    );
    let elsewhere = pull(1, CONTRIBUTOR, HEAD, "staging");
    refused(
        check_submission_paths(&elsewhere, &elsewhere, &p, HEAD, "main", ""),
        "default branch",
    );
}

#[test]
fn the_check_paths_command_reads_the_saved_api_answers() {
    let tmp = TempDir::new("catalogue_paths_cli");
    let dir = tmp.0.clone();
    let entries: Vec<Value> = (0..150)
        .map(|i| entry(&format!("packages/acme/p{i}/1.0.0/package.json"), "added"))
        .collect();
    let pr = pull(entries.len(), CONTRIBUTOR, HEAD, "main");
    std::fs::write(dir.join("before.json"), &pr).unwrap();
    std::fs::write(dir.join("after.json"), &pr).unwrap();
    let page_dir = dir.join("pages");
    std::fs::create_dir(&page_dir).unwrap();
    for (i, page) in pages(&entries).iter().enumerate() {
        std::fs::write(page_dir.join(format!("{}.json", i + 1)), page).unwrap();
    }
    let args = |maintainers: &str| -> Vec<String> {
        [
            "check-paths",
            "--repository",
            "example-org/example-catalogue",
            "--pull-before",
            &dir.join("before.json").to_string_lossy(),
            "--pull-after",
            &dir.join("after.json").to_string_lossy(),
            "--pages",
            &page_dir.to_string_lossy(),
            "--head-sha",
            HEAD,
            "--default-branch",
            "main",
            "--maintainer-ids",
            maintainers,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    };
    let run = |args: Vec<String>| super::super::cli::run(args);
    assert_eq!(run(args("")), 0, "an empty maintainer list is allowed");
    assert_eq!(run(args("900")), 0);
    assert_eq!(run(args("alice")), 1, "a login is not a maintainer id");
    let mut missing = args("");
    missing.truncate(missing.len() - 2);
    assert_eq!(run(missing), 2, "--maintainer-ids must be given");
    // A gap in the saved pages, or a stray file, is refused.
    std::fs::rename(page_dir.join("2.json"), page_dir.join("3.json")).unwrap();
    assert_eq!(run(args("")), 2);
    std::fs::rename(page_dir.join("3.json"), page_dir.join("2.json")).unwrap();
    std::fs::write(page_dir.join("notes.txt"), "x").unwrap();
    assert_eq!(run(args("")), 2);
    std::fs::remove_file(page_dir.join("notes.txt")).unwrap();
    // A missing page file: the list no longer adds up.
    std::fs::remove_file(page_dir.join("2.json")).unwrap();
    assert_eq!(run(args("")), 1);
}
