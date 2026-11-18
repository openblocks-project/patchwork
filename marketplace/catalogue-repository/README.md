# Patchwork Marketplace Catalogue

This repository is the public catalogue of custom nodes for Patchwork.
Patchwork reads `index.json` from here and
installs exact, reviewed package versions into the user's Library.

There is no server: everything is a file in this repository, reviewed in
pull requests and published by automation.

| Path | What it is | Who changes it |
|---|---|---|
| `publishers/<namespace>.json` | a namespace claim: GitHub login and numeric account id | the account itself, once (then maintainers) |
| `packages/<namespace>/<name>/package.meta.json` | listing: name, description, author, tags, license, documentation link | the namespace owner |
| `packages/<namespace>/<name>/<version>/` | one immutable version: `package.json`, the `.pwnode` entry, bundled `deps/`, optional `preview.png/jpg/jpeg/webp` | the namespace owner, once |
| `artifacts/<id>/<version>/<id>-<version>.pwpkg` | the downloadable artifact | **generated** by the publish workflow |
| `index.json` | the catalogue Patchwork reads | **generated** by the publish workflow |
| `moderation.json` | withdrawn (yanked) versions | maintainers |

**Published versions never change.** A version folder, once merged, is never
modified; its artifact is generated once and pinned by commit hash in
`index.json`. Fixes are new versions.

**Nothing here is run.** Validation and publication only read the files.
Patchwork itself verifies every download (size, SHA-256, layout, checksums,
capabilities) before installing, and asks the user before a package runs.

- Publishing a package: [CONTRIBUTING.md](CONTRIBUTING.md)
- Reporting a problem package: [SECURITY.md](SECURITY.md)
- Running this repository: [MAINTAINERS.md](MAINTAINERS.md)
