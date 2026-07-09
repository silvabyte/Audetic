# Source-only distribution: kill hosted releases, install via make

Audetic previously shipped via a curl installer (`install.audetic.ai`) backed by hosted
tarballs, a bespoke deploy script (cross-compiled Linux builds, macOS notarization,
godeploy publishing), and an in-daemon auto-updater. The sole user is the maintainer,
and the release machinery cost more upkeep than it returned while the product is still
being hardened.

Decision: distribution is clone-and-build only. `git clone && make install` is the
single supported install path; `make install` dispatches on `uname -s` to
`install-macos` / `install-linux`, which build in release profile and hand off to
`audeticd install`. Install is idempotent and doubles as the upgrade path
(`git pull && make install`). Uninstall mirrors it: a new `audeticd uninstall`
subcommand owns per-OS teardown, wrapped by `make uninstall`.

Deleted, not feature-flagged: the auto-updater vertical slice (Rust `update` module,
`/update/*` API routes, `audetic update` CLI command, web-ui updates settings page),
`release/cli/` (installer, uninstaller, committed tarballs, version files),
`scripts/release/deploy.ts` and deploy Make targets, `Cross.toml`, `godeploy.config.json`,
and the notarization / Developer ID signing path (ad-hoc signing stays for local TCC).
Git history is the archive; a future updater would be `git pull` shaped, not
hosted-tarball shaped, so the old code was not worth carrying behind a flag.

No tombstone was published to `install.audetic.ai`; there are no external users.

## Consequences

- Versioning is whatever `Cargo.toml` says. No more release tags or `chore(release)` commits.
- Service lifecycle Make targets (`start`/`stop`/`restart`/`logs`/`status`) dispatch
  per-OS (systemctl/journalctl vs launchctl/log file) so both platforms share one vocabulary.
- Prebuilt binaries, notarization, and any package-manager story are explicitly deferred
  until the product stabilizes.
