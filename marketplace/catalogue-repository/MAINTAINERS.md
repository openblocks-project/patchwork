# Maintaining the catalogue

The catalogue is the **`catalogue` branch** of the Patchwork repository: an
orphan branch, with no history in common with the application, holding only
this folder's contents. `main` stays the application's default branch and
holds the trusted Submission paths guard. Patchwork users set the
marketplace source to `<owner>/<repo>@catalogue`; with an empty branch,
Patchwork reads `main`.

## How it works

| Workflow | Lives on | Runs on | Token | Does |
|---|---|---|---|---|
| `submission-paths.yml` | `main` | pull requests targeting `catalogue` (`pull_request_target`; GitHub always takes it from the default branch) | `check`: read-only (`contents`, `pull-requests`); `report`: `statuses: write` only; no secrets | `check` builds the checker and runs `patchwork catalogue check-paths` on GitHub's changed-file list; `report` sets the **Submission paths** commit status on the pull request's head commit. Nothing from the pull request is checked out |
| `validate.yml` | `catalogue` | pull requests targeting `catalogue` (`pull_request`) | read-only, no secrets | builds the validator and runs `patchwork catalogue check-pr` |
| `publish.yml` | `catalogue` | pushes to `catalogue` | read-only + deploy key | `validate`, `publish-artifacts` (commit), `publish-index` (commit), push to `catalogue` |

The application's own workflows never run for the catalogue. Pull-request
workflows come from the merge result, and push workflows from the pushed
commit, and `catalogue` contains only the two workflows above.

**Both are required: the status "Submission paths" and the check
"Validate submission".** A pull request can rewrite `validate.yml` for its
own run and make "Validate submission" pass. It cannot change the guard,
because GitHub takes `pull_request_target` workflows from `main`. The guard
fails every contributor pull request that touches anything outside
`publishers/` and `packages/`, including `.github/`. So whenever both pass,
"Validate submission" was the reviewed workflow.

The guard reports a **commit status**, not just its own check run:
- **Where it lands:** the status is set on the exact head commit of the pull
  request. A check run created from `main` is not documented to attach
  there.
- **No look-alikes:** when a check run and a status share a name, GitHub
  requires both, so a look-alike job in a pull request cannot satisfy it.
- **Forks can't fake it:** fork pull requests only get read-only tokens and
  cannot set statuses.

See `docs/marketplace/catalogue-repository.md` in the Patchwork source for
the deployment gate. Deployment stays blocked until the live tests pass.

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
  configuration: `catalogue`'s `.github/`, `moderation.json`, documentation,
  publisher claims, and version takedowns.
- **What nobody may change:** `index.json` and `artifacts/` are refused for
  everyone, because only the Publish workflow writes them.
- **Review still applies:** maintainer pull requests still need review
  (CODEOWNERS).
- **The exception follows who opened the pull request**, not who pushed to
  it. Maintainers open catalogue pull requests only from branches nobody
  else can push to.
- **Adding or removing a maintainer** is a settings change: edit
  `MAINTAINER_IDS`. It never happens through a pull request.
- **Changing the guard** is a change to `main`, reviewed under `main`'s
  rules and its CODEOWNERS.

The validator is the Patchwork binary itself in its headless `catalogue`
mode, built from a reviewed Patchwork commit. It uses exactly the desktop
app's code: artifact format and ordering, SHA-256, `parse_catalogue`,
`parse_artifact`, the capability and secret scans, `check_refresh`. It
never runs package content.

Publication makes two commits on `catalogue`:
1. **Artifacts**: the artifacts of new versions. An artifact that already
   exists must rebuild to exactly its bytes, so published bytes can never be
   replaced.
2. **Index**: `index.json`, with every new version's URL pinned to the
   artifacts commit
   (`https://raw.githubusercontent.com/<owner>/<repo>/<commit>/artifacts/…`).

Published versions keep their original pins. `generated_at` is that
commit's time and never decreases. Patchwork rejects an index that rewrites
a published version or goes backwards.

## Setup (repository administrators)

The deploy key can write to the whole repository, including the
application. Steps 1–3 come **before** the key exists.

1. **Rulesets for the application:**
   - `main` and the development branches: require a pull request (on `main`
     with review from Code Owners), and block force pushes and deletions,
     with **no** deploy-key bypass;
   - tags: restrict creation, updates and deletion, with administrators as
     the only bypass, which protects `catalogue-validator-*`;
   - optionally: restrict creating branches other than `catalogue`, with
     administrators as the only bypass.
2. **Secrets and tokens:**
   - no repository-level secrets: `pull_request_target` workflows run with
     access to them. Keep secrets in environments limited to their
     branches;
   - Actions → General → Workflow permissions: **read repository contents**
     (the default token is read-only);
   - do not allow Actions to create or approve pull requests.
3. **Variables** (Settings → Secrets and variables → Actions → Variables):
   - `PATCHWORK_REPOSITORY`: this repository, `owner/repo`;
   - `PATCHWORK_COMMIT`: the full 40-character hash of the reviewed
     Patchwork commit that builds the validator. It must stay reachable, so
     protect its tag. Change it only after checking the release notes for
     catalogue-format changes;
   - `MAINTAINER_IDS`: comma-separated **numeric** GitHub account ids (for
     example `123,456`).
4. **The guard on `main`:** a pull request to `main` adding exactly
   `.github/workflows/submission-paths.yml` and the `.github/CODEOWNERS`
   lines, both from `marketplace/default-branch/` in the Patchwork source,
   with real owner names filled in. No application code is involved.
5. **Deploy key:**
   - create an SSH key pair and add the public key under Settings → Deploy
     keys with **write access**. It must be the repository's only deploy
     key;
   - create the environment `publish` with deployment branches limited to
     `catalogue`, and add the private key there as the secret
     `CATALOGUE_DEPLOY_KEY`. Never add it as a repository secret.
6. **Create `catalogue`:** an orphan branch with this folder's contents at
   its root, plus a `LICENSE` for the catalogue data. Keep `.gitattributes`
   (`* -text`): published files are hashed byte for byte. Fill in
   `.github/CODEOWNERS`. Pushing it runs Publish, which writes the empty
   `index.json` and pushes it.
7. **Pass the deployment gate** (live tests; see the Patchwork source docs).
   Deployment stays blocked until then.
8. **Ruleset for `catalogue`:**
   - require a pull request, with at least 1 approval, review from Code
     Owners, and dismissal of stale approvals on new commits;
   - required checks **Submission paths** (the commit status) and
     **Validate submission**, with source GitHub Actions and branches up to
     date. They appear in the list after they have run once;
   - block force pushes and deletions;
   - **bypass list:** the deploy key only (the Publish workflow's two
     commits). No standing bypass for maintainers.
9. **Actions settings:**
   - require approval for workflow runs from first-time contributors;
   - keep "Send write tokens / secrets to workflows from fork pull requests"
     off;
   - consider pinning third-party actions (`dtolnay/rust-toolchain`,
     `Swatinem/rust-cache`) to full commit hashes.
10. **Security:** turn on secret scanning, push protection and private
    vulnerability reporting.
11. **Point Patchwork at it:** users set the marketplace source to
    `<owner>/<repo>` with branch `catalogue`. Patchwork has no built-in
    production catalogue.

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
- **Wrong target:** a pull request must target `catalogue`. Pull requests
  into `main` with catalogue files are application changes and are closed.
- A contributor PR that changes anything outside `publishers/` and
  `packages/` fails **Submission paths**. Never merge it, whatever any other
  check says, and close it if it touches `.github/`. A red or missing
  Submission paths status next to a green check of the same name means
  someone added a look-alike workflow.

## Release procedures

- **Publish:** merge an approved pull request into `catalogue`. The Publish
  workflow runs on the merge. If its push loses a race with another merge,
  it fails; the next run publishes both, because both commands are
  idempotent. There is no manual Publish run: GitHub offers those only for
  workflows on `main`. To retry, use **Re-run all jobs** on the latest
  Publish run; it checks out the current `catalogue` tip.
- **Withdraw (yank):** a maintainer PR adds `"<id>@<version>": "<reason>"`
  to `moderation.json`. Installed copies keep working, and Patchwork stops
  offering the version.
- **Take down:** a maintainer PR deletes `packages/<ns>/<name>/<version>/`.
  The artifact stays, so the version can never return with different bytes.
- **Namespace rename** (login renamed): a maintainer PR updates
  `publishers/<ns>.json`. The numeric id must stay the same; Patchwork
  refuses a namespace whose owner id changes. A different account means a
  new namespace.
- **Validator update:**
  1. Tag and protect the new reviewed commit.
  2. Change `PATCHWORK_COMMIT`.
  3. Re-run Validate and Submission paths on an open pull request.
  4. Re-run the latest Publish run. It must report no changes to existing
     artifacts.
- **Never:** edit `index.json` or `artifacts/` by hand, force-push, merge
  `catalogue` and the application branches into each other, or rewrite
  history. Patchwork rejects catalogues that change published versions or go
  back in time.

## Running the validator locally

```sh
cargo build --release --locked            # in the Patchwork checkout
patchwork catalogue validate --root <catalogue-checkout> --repository <owner>/<repo> --branch catalogue
patchwork catalogue check-pr --base <base-checkout> --head <pr-checkout> \
  --repository <owner>/<repo> --branch catalogue \
  --author-login <login> --author-id <numeric-id>
```

Exit status: 0 valid, 1 problems (each printed), 2 usage error.
