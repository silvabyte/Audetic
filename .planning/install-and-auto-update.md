# install

## Goals
- Zero-dependency install: no git, Rust, or build tooling required on target machines.
- Provide deterministic pre-built binaries per OS/arch with mandatory cryptographic integrity checks (sha256 and optional signatures).
- Single cURL-able installer (`latest.sh`) that handles installing/updating the binary, user service, config file, and optional CLI helpers.
- Leverage existing `release/` directory + `godeploy` to publish static assets under `https://install.audetic.ai/`.

## Release assets layout
- `release/cli/version`: plain-text semantic version (e.g., `0.1.0`). Updated every release; consumed by installer + auto-updater.
- `release/cli/latest.sh`: installer script exposed at `/cli/latest.sh`.
- `release/cli/releases/<version>/manifest.json`: metadata describing assets (targets, sha256, size, signature path if present, URL path, changelog snippet, min-compatible config schema).
- `release/cli/releases/<version>/<target>/audetic-<version>-<target>.tar.gz`: tarball containing:
  - `audetic` binary (stripped) for the specific target (`linux-x86_64-gnu`, `linux-aarch64-gnu`, `macos-aarch64`, ...).
  - `audetic.service` template (user systemd) + `audetic-updater.service` if we split responsibilities later.
  - `README.txt` with manual install/update instructions.
  - `release/cli/releases/<version>/<target>/audetic-<version>-<target>.sha256` (also embedded in manifest for cross-checking; installer refuses to proceed without validating this hash).
- Optional: `release/cli/releases/<version>/<target>/audetic-<version>-<target>.sig` (minisign or cosign) for tamper detection.
- `release/cli/releases/<version>/notes.md`: short release notes consumed by CLI `audetic update --notes`.
- Add `release/cli/releases/*` tarballs + manifests to `.gitignore`; we only track metadata (manifest templates) or rely on CI to push artifacts directly to deploy bucket.

## Build + publish pipeline
1. `cargo dist` (or plain `cargo build --release` + `strip`) per target inside CI.
2. Package each binary into tarball along with service template + README.
3. Generate sha256 checksum (mandatory) + optional signature per artifact; publish alongside tarball + embed in manifest.
4. Emit/refresh `manifest.json` with artifact metadata.
5. Update top-level `version` file.
6. Run `godeploy deploy` to sync `release/cli` to `install.audetic.ai`.
7. Draft GitHub Release referencing the same version for visibility (optional but nice).

### `make deploy` convenience target
- Target lives in top-level `Makefile` and wraps the entire release workflow so we can run `make deploy VERSION=0.2.0` (or rely on auto-semver when VERSION unset).
- Steps performed:
  1. Validate clean git state (no unstaged changes) or confirm `ALLOW_DIRTY=1`.
  2. Ensure `VERSION` argument is semantic and greater than current `release/cli/version`.
  3. Run cross-build(s) via `cargo dist` (or matrix of `cross build`) producing binaries under `target/dist`.
  4. Package binaries + assets, produce checksums/signatures, assemble `release/cli/releases/<VERSION>/`.
  5. Update `release/cli/version` and manifest pointers.
  6. Regenerate `release/cli/latest.sh` if templated pieces depend on version metadata.
  7. Run tests/lints specified by `make test` (configurable via `SKIP_TESTS=1`).
  8. Execute `godeploy deploy` to publish to `install.audetic.ai`.
  9. Optionally tag repo (`git tag v$VERSION && git push origin v$VERSION`) unless `SKIP_TAG=1`.
- Provide dry-run mode (`make deploy DRY_RUN=1`) that walks through commands without publishing.
- Output human-friendly summary (artifact locations, checksums) at the end for sanity.

## Installer script responsibilities (`latest.sh`)
- Detect OS + architecture; map to target identifier. Abort with useful message if unsupported.
- Fetch `https://install.audetic.ai/cli/version` + latest manifest to know asset URLs.
- Download tarball + checksum; verify sha256 before extraction every time (fail closed) and verify signature if enabled.
- Extract to temp dir under `/tmp/audetic-install-XXXX`.
- Install binary into `/usr/local/bin/audetic` (or `$HOME/.local/bin` fallback when no sudo). Re-running the script should be idempotent: it compares installed version vs manifest, installs if different, and can perform `--clean` reinstall (remove old artifacts + config backups) deterministically.
- Drop `audetic.service` into `$HOME/.config/systemd/user/audetic.service` (default) or `/etc/systemd/system` if installing system-wide (flag `--system`).
- Ensure config directory exists (`~/.config/audetic/config.toml`). If not, copy `example_config.toml` and prompt user to edit. Preserve existing configs.
- Expose `audetic` CLI subcommands directly (no separate legacy shell wrappers); all lifecycle operations (`install`, `update`, `uninstall`, `status`) are funneled through `latest.sh` plus the Rust CLI.
- Run `systemctl --user daemon-reload` and `systemctl --user enable --now audetic`.
- Log every step with clear instructions; support flags: `--prefix`, `--channel`, `--no-start`, `--force-reinstall`, `--clean`, `--dry-run`.
- Provide uninstall flow via `latest.sh --uninstall` that removes services/binaries/config backups safely.

## Local filesystem layout after install
- `/usr/local/bin/audetic` → runtime binary (and CLI entrypoint).
- `/usr/local/bin/audeticctl` (optional) → thin wrapper around `audetic --cli` subcommands.
- `~/.config/audetic/` → config + auto-update metadata (current channel, last check, staged binary path).
- `~/.local/share/audetic/` → caches (models, logs, downloaded updates).
- `~/.config/systemd/user/audetic.service` → user unit referencing `/usr/local/bin/audetic`.
- `~/.config/systemd/user/audetic-updater.service` if we later separate updater.

## Testing/install validation
- Scripted smoke test per platform (GitHub Actions matrix) that runs `curl ... | bash -- --dry-run` to ensure integrity.
- Manual doc: `docs/installation.md` updated with new flow + troubleshooting.

# auto-update

## Goals
- Automatic, reliable delivery of new binaries without user intervention.
- Reuse the same release artifacts + `version` endpoint as installer.
- Support manual overrides via CLI (`audetic update`, `audetic update --channel beta`).
- Respect user uptime: configurable quiet hours, defers while recording, safe rollbacks on failure.

## Architecture
- Embed a Rust `UpdateManager` inside the main `audetic` binary:
  - Spawned alongside API server (same process) on a dedicated async task/thread.
  - Periodically polls `https://install.audetic.ai/cli/version` (default every 6h; jittered).
  - Reads local channel + current version from `~/.config/audetic/update_state.json`.
  - If remote > local, downloads manifest + appropriate tarball to staging dir (`~/.local/share/audetic/updates/<version>`).
  - Verifies checksum/signature, extracts binary to staging.
  - When idle (no active recording, CPU below threshold), swaps binaries atomically via:
    1. Move current `/usr/local/bin/audetic` → `/usr/local/bin/audetic.<oldversion>.bak`.
    2. Move staged binary into place, chmod +x.
    3. Touch marker file `~/.config/audetic/update_state.json` with `last_success_version`.
    4. Trigger service restart (DBus `systemd --user restart audetic.service`) or self-reexec (if running as CLI).
- Use OS-specific restart strategy:
  - If running under systemd user service: send `SIGTERM` to self after scheduling restart (systemd will restart).
  - If running interactively (CLI mode), delay install until next daemon restart and prompt user.

## Failure & rollback strategy
- Maintain `.bak` binary for last version; if new binary fails health check (process crashes N times on startup or explicit self-test fails), auto-rollback by restoring `.bak` and disabling updates until manual intervention.
- Health check: after update, run `audetic --self-test` (fast check verifying basic subsystems) before reporting success.
- On download failure/checksum mismatch: log error, retry with exponential backoff (max 24h) and surface status via `audetic status`.

## CLI / user controls
- `audetic update` → trigger immediate check + install (reuse UpdateManager).
- `audetic update --channel beta|stable` → switch release channel (persisted).
- `audetic update --disable/--enable` → toggle auto-updates (writes config, UpdateManager respects).
- `audetic version` → show running + latest available version info.
- `audetic rollback` → restore previous binary if `.bak` exists.

## Security considerations
- Require HTTPS + checksum verification before executing new binary.
- Optional signature verification using `minisign` public key embedded in updater.
- Drop privileges while downloading (use user account, never run as root except file move).
- Ensure atomic replace via `rename` to avoid partial binary writes.

## Interaction with installer
- Installer writes `~/.config/audetic/update_state.json` with:
  ```json
  {
    "current_version": "0.1.0",
    "channel": "stable",
    "last_check": null,
    "auto_update": true
  }
  ```
- Auto-updater reuses `version` endpoint + manifest; no extra infrastructure.
- `latest.sh` is the only manual touchpoint: users can rerun it anytime for clean reinstalls, channel switches, or repairs without touching git/Rust.

## Outstanding work items
- [ ] Finalize release artifact naming + manifest schema.
- [ ] Implement CI workflow (GitHub Actions) that builds, packages, updates manifest, runs `godeploy`.
- [ ] Write production-ready `latest.sh` with detection, flags, verification.
- [ ] Implement `UpdateManager` module in Rust, integrate into `main.rs`.
- [ ] Design CLI subcommands for manual control + telemetry.
- [ ] Add docs (`docs/installation.md`, `docs/review_tasks.md`) describing new flow.
- [ ] Add integration tests covering install + auto-update flows (mock server).
