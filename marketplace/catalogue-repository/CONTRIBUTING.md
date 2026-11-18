# Publishing a package

You publish through a normal GitHub pull request. Patchwork never asks for
your GitHub password, token or login: GitHub identifies you when you open
the pull request, and that account must own the namespace you publish in.

## 1. Prepare the submission in Patchwork

1. Publish your custom node to your Library (**File → Publish to Library**).
2. In the Library, right-click the package → **Share to Marketplace…** → the
   version.
3. Enter your **namespace**: your GitHub login in lowercase. The public id
   is `<namespace>.<name>`.
4. Fill in the listing, press **Check**, then **Create submission folder…**.

Patchwork refuses packages that contain credentials, restricted node types
(native code, terminals, MCP, settings, stored AI keys, …), unknown node
types, incomplete or unpublished dependencies, or versions already in the
catalogue. Nothing in the package is run.

The folder contains `SUBMISSION.md` (the artifact's SHA-256 and these steps),
`repository/` (the files to add), `artifact/` (for checking only) and
`catalogue-entry.json`.

## 2. Claim your namespace (first package only)

Add `publishers/<namespace>.json`:

```json
{
  "namespace": "<your-login-in-lowercase>",
  "github": "<YourLogin>",
  "github_id": 12345678,
  "display_name": "Your Name"
}
```

`github_id` is your **numeric** account id. Find it at
`https://api.github.com/users/<YourLogin>` (the `"id"` field). It must be
the id of the account that opens the pull request; the check compares it
with what GitHub reports for the pull request, not with what you type.

Not allowed: reserved namespaces (`local`, `builtin`, `core`, `std`,
`system`, `test(s)`, `example(s)`, `marketplace`, `official`, `patchwork`),
a namespace other than your own login, a second namespace for the same
account, and namespaces that differ from an existing one only by `-`, `_`
or `.`. An existing claim can only be changed by the maintainers (for
example after a login rename).

## 3. Open the pull request

1. Fork this repository and clone your fork (or use GitHub's web upload).
2. Copy everything inside the submission's `repository/` folder into the
   root of your fork, keeping the folders. Do **not** add `artifact/`,
   `SUBMISSION.md` or `catalogue-entry.json`.
3. Commit and push to your fork, then open a pull request against this
   repository **from the account that owns the namespace**.

A pull request may only:

- add your own `publishers/<namespace>.json`;
- add new version folders under `packages/<namespace>/…`;
- add or update `package.meta.json` and `README.md` of your packages.

It may never change an existing version folder, `artifacts/`, `index.json`,
`moderation.json`, another namespace, or the repository's configuration.

## 4. Automated validation

Two checks must pass.

**Submission paths** checks GitHub's list of changed files. Any change
outside `publishers/` and `packages/` fails it, including renames and
deletions. Workflow changes (`.github/`) fail it too.

**Validate submission** runs the same validator Patchwork uses. It checks:

- namespace ownership against your numeric GitHub account id;
- reserved and look-alike namespaces and package names;
- package ids and exact `MAJOR.MINOR.PATCH` versions;
- that published versions are unchanged;
- the version layout, file limits and safe paths;
- `package.json` (id, version, `source: "marketplace"`, `min_app_version`, checksums);
- complete bundled dependencies that are exact copies of published versions;
- node types and capabilities;
- secrets;
- preview images (type, size);
- the resulting catalogue and artifact, exactly as Patchwork installs them.

If it fails, the log lists every problem. Fix them in Patchwork (publish a
new local version, share again) and push to the same pull request.

## 5. Review and publication

A maintainer (and, for an existing namespace, its owner) reviews the pull
request. After merge, the **Publish** workflow generates the artifact, which
must have exactly the SHA-256 in your `SUBMISSION.md`. It then updates
`index.json`, and your version appears in Patchwork's marketplace.

To fix a published version, publish a new version. Versions are never
replaced.
