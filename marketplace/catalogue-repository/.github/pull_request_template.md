<!-- Patchwork marketplace submission into the `catalogue` branch (base:
catalogue, never main). See CONTRIBUTING.md on the catalogue branch.
GitHub only offers templates from the default branch, so copy this
checklist into your pull request description. -->

**Package:** `<namespace>.<name>@<version>`

- [ ] The files come from Patchwork's *Share to Marketplace* (`repository/` folder), unchanged.
- [ ] This pull request is opened from the GitHub account that owns the namespace.
- [ ] If the namespace is new, it adds `publishers/<namespace>.json` for this account.
- [ ] It does not change existing versions, `artifacts/` or `index.json`.
- [ ] Its base branch is `catalogue`.

**Artifact SHA-256 reported by Patchwork:** `<sha256 from SUBMISSION.md>`

**What the node does:**
