# winget manifests

Templates for the three files winget wants, submitted to
[`microsoft/winget-pkgs`](https://github.com/microsoft/winget-pkgs) under
`manifests/v/VincentArelBundock/Pulpit/<version>/`.

winget requires a **checksum, not a signature** (SPEC-package.md §6.1), which
is why publishing here is not gated on a certificate. Fill
`InstallerSha256` from the `.sha256` file the release publishes, and bump
`PackageVersion` in all three files together — winget rejects a version set
whose files disagree.

The installer is per-user (`InstallerScope: user`), so an install through
winget raises no UAC prompt.

Generating these with `wingetcreate update` at release time is the intended
eventual automation; until a certificate and a release cadence exist, one
hand edit per release is honest and cheap.
