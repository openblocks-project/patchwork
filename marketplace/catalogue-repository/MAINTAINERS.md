# Maintaining the catalogue

## How it works

| Workflow | Runs on | Token | Does |
|---|---|---|---|
| `submission-paths.yml` | every pull request (`pull_request_target`: the workflow always comes from the default branch) | read-only (`contents`, `pull-requests`), no secrets | builds the checker and runs `patchwork catalogue check-paths` on GitHub's changed-file list; checks out nothing from the pull request |
| `validate.yml` | every pull request (`pull_request`) | read-only, no secrets | builds the validator and runs `patchwork catalogue check-pr` |
| `publish.yml` | push to the default branch, or run by hand | read-only + deploy key | `validate`, `publish-artifacts` (commit), `publish-index` (commit), push |

**Both checks are required: "Submission paths" and "Validate submission".**
A pull request can rewrite `validate.yml` for its own run and make
"Validate submission" pass. It cannot change `submission-paths.yml`, because
GitHub takes `pull_request_target` workflows from the default branch. That
guard fails every contributor pull request that touches anything outside
`publishers/` and `packages/`, including `.github/`. So whenever both checks
pass, "Validate submission" was the reviewed workflow.

### Maintainer exception

- **Who counts as a maintainer:** the guard and the validator accept an
  account as a maintainer only by its numeric GitHub account id, taken from
  GitHub's own pull request record, and only if that id is listed in the
  repository variable `MAINTAINER_IDS`. Logins, display names and anything
  else in the pull request are never used.
- **Malformed list:** if `MAINTAINER_IDS` contains a malformed entry (a
  login, a sign, a leading zero), every pull request fails until it is
  fixed.
- **Empty list:** an empty `MAINTAINER_IDS` means there are no maintainers.
- **What maintainers may change:** a maintainer's pull request may change
  configuration: `.github/`, `moderation.json`, documentation, publisher
  claims, and version takedowns.
- **What nobody may change:** `index.json` and `artifacts/` are refused for
  everyone, because only the Publish workflow writes them.
- **Review still applies:** maintainer pull requests still need review
  (CODEOWNERS). A change to `submission-paths.yml` takes effect only after
  it is merged.
- **Adding or removing a maintainer** is a settings change: edit
  `MAINTAINER_IDS`. It never happens through a pull request.

The validator is the Patchwork binary itself in its headless `catalogue`
mode, built from a reviewed Patchwork commit. It uses exactly the desktop
app's code: artifact format and ordering, SHA-256, `parse_catalogue`,
`parse_artifact`, the capability and secret scans, `check_refresh`. It
never runs package content.

Publication makes two commits:
1. **Artifacts**: the artifacts of new versions. An artifact that already
   exists must rebuild to exactly its bytes, so published bytes can never be
   replaced.
2. **Index**: `index.json`, with every new version's URL pinned to the
   artifacts commit
   (`https://raw.githubusercontent.com/<owner>/<repo>/<commit>/artifacts/…`).

Published versions keep their original pins. `generated_at` is that
commit's time and never decreases. Patchwork rejects an index that rewrites
a published version or goes backwards.

## Repository setup

1. **Create the repository** (public), with this folder's contents at its
   root. Add a `LICENSE` for the catalogue data. Keep `.gitattributes`
   (`* -text`): published files are hashed byte for byte.
2. **Generate the first index:** run the Publish workflow by hand
   (Actions → Publish → Run workflow). It writes an empty `index.json`.
3. **Repository variables** (Settings → Secrets and variables → Actions →
   Variables):
   - `PATCHWORK_REPOSITORY`: the Patchwork source repository, `owner/repo`.
   - `PATCHWORK_COMMIT`: the full 40-character commit hash of the reviewed
     Patchwork version that builds the validator. Change it only in a
     reviewed maintainer change, after checking the release notes for
     catalogue-format changes.
   - `MAINTAINER_IDS`: comma-separated **numeric** GitHub account ids of the
     maintainers (for example `123,456`). Their pull requests may change
     configuration; never generated files or published bytes.
4. **Deploy key:** create an SSH key pair. Add the public key under
   Settings → Deploy keys with **write access**. Add the private key as the
   Actions secret `CATALOGUE_DEPLOY_KEY` in an environment named `publish`
   that is limited to the default branch (Settings → Environments →
   publish → Deployment branches: the default branch only).
5. **Branch rules** for the default branch (Settings → Rules → new branch
   ruleset):
   - require a pull request, with at least 1 approval, review from Code
     Owners, and dismissal of stale approvals on new commits;
   - require **both** status checks, **Submission paths** and **Validate
     submission**, with source GitHub Actions and branches up to date. A
     check appears in the list only after it has run once, so open a test
     pull request first;
   - block force pushes and deletions;
   - **bypass list:** the deploy key only (the Publish workflow's two
     commits). Do not give maintainers a standing bypass.
6. **CODEOWNERS:** fill in `.github/CODEOWNERS` with the maintainers team as
   default owner of everything (including `.github/`, `publishers/` and
   `moderation.json`).
7. **Actions settings:**
   - require approval for workflow runs from first-time contributors;
   - keep no repository-level secrets: `pull_request_target` workflows can
     read them. The deploy key belongs only in the `publish` environment;
   - keep no long-lived branches other than the default branch: the guard
     refuses pull requests that target any other branch;
   - keep "Send write tokens / secrets to workflows from fork pull requests"
     off;
   - consider pinning third-party actions (`dtolnay/rust-toolchain`,
     `Swatinem/rust-cache`) to full commit hashes.
8. **Private vulnerability reporting:** turn it on (Settings → Security).
9. **Point Patchwork at it:** users choose the repository in Patchwork's
   marketplace source settings. Patchwork has no built-in production
   catalogue.

## Reviewing a submission

Validation proves the files are well-formed, complete, use only allowed
node types and match the PR author's namespace. Review still decides
whether the package should be listed:
- Read the listing and the `.pwnode` (it is JSON) for misleading names,
  descriptions or behaviour, especially nodes that use the network, files,
  devices, capture or programs (shown as capabilities in the check log).
- **New namespace:** confirm the claim is for the PR author (the check
  compares numeric ids) and that the display name is not impersonating
  anyone. Then add the namespace to `CODEOWNERS` in a maintainer change.
- **Existing namespace:** the namespace owner's approval is required by
  CODEOWNERS.
- A contributor PR that changes anything outside `publishers/` and
  `packages/` fails **Submission paths**. Never merge it, whatever any other
  check says, and close it if it touches `.github/`. Look at every check on
  the PR: a red "Submission paths" next to a second, green check of the
  same name means someone added a look-alike workflow.

## Release procedures

- **Publish:** merge an approved pull request. The Publish workflow runs on
  the merge. If its push loses a race with another merge, it fails; the
  next run (or a manual run) publishes both, because both commands are
  idempotent.
- **Withdraw (yank):** a maintainer PR adds `"<id>@<version>": "<reason>"`
  to `moderation.json`. Installed copies keep working, and Patchwork stops
  offering the version.
- **Take down:** a maintainer PR deletes `packages/<ns>/<name>/<version>/`.
  The artifact stays, so the version can never return with different bytes.
- **Namespace rename** (login renamed): a maintainer PR updates
  `publishers/<ns>.json`. The numeric id must stay the same; Patchwork
  refuses a namespace whose owner id changes. A different account means a
  new namespace.
- **Validator update:** change `PATCHWORK_COMMIT` and re-run Validate and
  Submission paths on an open pull request, then run Publish by hand. It must report no changes to
  existing artifacts.
- **Never:** edit `index.json` or `artifacts/` by hand, force-push, or
  rewrite history. Patchwork rejects catalogues that change published
  versions or go back in time.

## Running the validator locally

```sh
cargo build --release --locked            # in the Patchwork checkout
patchwork catalogue validate --root <catalogue> --repository <owner>/<repo>
patchwork catalogue check-pr --base <base-checkout> --head <pr-checkout> \
  --repository <owner>/<repo> --author-login <login> --author-id <numeric-id>
```

Exit status: 0 valid, 1 problems (each printed), 2 usage error.
