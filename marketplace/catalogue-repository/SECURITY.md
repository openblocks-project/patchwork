# Security and takedowns

## Reporting

Report a malicious, broken or infringing package privately, through the
Patchwork repository's **Security → Report a vulnerability** (private
vulnerability reporting), not in a public issue. Include the package id and
version, and say that it concerns the marketplace catalogue.

Report problems in Patchwork itself (the validator, the installer) to the
Patchwork project.

## What happens

- **Withdrawn (yanked):** a maintainer adds the version to
  `moderation.json` with a reason. Patchwork stops offering it for new
  installs and shows the reason. Installed copies keep working, and
  Patchwork flags granted copies so users can revoke them.
- **Taken down:** a maintainer removes the version folder in a pull request
  into `catalogue`. The version disappears from `index.json`. Its artifact
  file stays on the branch, so the same id and version can never be
  republished with different bytes.

Published bytes are never edited. Packages run only after the user allows
them in Patchwork, with the capabilities they were approved for.
