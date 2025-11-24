# Restart Command Plan

this should just be a live reload when config changes...

## Goals

- Provide a first-class `audetic restart` CLI subcommand so users can bounce the service after editing config, providers, or install settings—no manual `systemctl` invocation required.
- Reuse the restart logic already embedded in `handle_update_command`/`restart_user_service` so auto-update, installer, and manual workflows all share the same implementation.
- Support both user-mode (`systemctl --user`) and system-mode (`sudo systemctl`) installs, mirroring the knobs exposed by `release/cli/latest.sh --system`.
- Offer helpful diagnostics (current status, failure reason, follow-up guidance) to reduce guesswork when restart fails.
- Keep legacy entry points (`make restart`, scripts/install-arch instructions, docs) but have them call into the CLI where possible for consistency.

## Current behavior & gaps

- `src/cli/mod.rs` only exposes `update`, `version`, and `provider`. When `audetic update` installs a new version, it calls an internal `restart_user_service()` helper that shells out to `systemctl --user restart audetic.service`, but that helper is private and user-mode only.
- `Makefile`’s `restart` target and various shell scripts (`scripts/install-arch.sh`, `scripts/update-audetic.sh`) invoke `systemctl --user restart audetic.service` directly, assuming a user service and a working `systemctl`. There’s no parity for `latest.sh --system` installs.
- Users editing `~/.config/audetic/config.toml` or provider settings must remember the exact `systemctl` command; there’s no CLI guidance apart from ad-hoc messages (“please restart manually”).
- No centralized detection exists to decide whether the service is installed in user vs system scope; each script infers it independently.
- We don’t expose status or health checks after a restart, so it’s easy to miss failures (bad config, missing dependencies).

## Desired UX

- Command: `audetic restart [--system|--user] [--wait] [--timeout <seconds>] [--quiet]`.
  - Default mode auto-detects based on where the service file/binary was installed. If both exist, prefer user mode unless `--system` provided.
  - `--wait` (default on) blocks until `systemctl is-active` reports success or the timeout expires; `--no-wait` returns immediately after issuing restart.
  - `--quiet` suppresses extra log lines so scripts can consume output; otherwise we print clear `Starting restart…`, `Service active`, or failure messages.
  - Exit codes:
    - `0` → restart successful (service active).
    - `1` → restart command failed (bad config, missing service).
    - `2` → prerequisites missing (no `systemctl`, insufficient permissions, service not installed).
  - For non-systemd environments, warn and suggest restarting the binary manually (or re-running installer).
- `audetic restart --status` (optional flag) could report current service state without restarting; this can be part of the same subcommand (`--status-only`).
- When invoked after config changes, command should remind users where logs live (`journalctl --user -u audetic`) if restart fails.

## Implementation plan

1. **Service layout detection**
   - Add helpers (maybe under `src/global/service.rs`) that:
     - Determine user-mode unit path (`~/.config/systemd/user/audetic.service`).
     - Determine system-mode unit path (`/etc/systemd/system/audetic.service`).
     - Detect install mode by checking which unit file exists or by reading metadata from the config directory (installer could write a small marker, but fall back to heuristics).
   - Expose a struct `ServiceTarget { mode: SystemdMode, unit_name: String }`.

2. **CLI plumbing**
   - Extend `CliCommand` with `Restart(RestartArgs)`; add parsing for flags mentioned above.
   - In `main.rs`, route to `handle_restart_command(args)` before falling back to running the daemon.
   - Reuse/in generalize the existing `restart_user_service()` into a new module (e.g., `cli::service`) that can issue `systemctl` invocations for both modes, with optional `--wait` logic (call `systemctl is-active` in a loop up to timeout).
   - Provide friendly error messages when `systemctl` is missing, when the user lacks permission (suggest rerunning with `sudo audetic restart --system`), or when the unit doesn’t exist (suggest reinstall).

3. **Script + tooling alignment**
   - Update `Makefile restart` target to call `audetic restart --user` (or auto) so local workflows exercise the new code path.
   - Update `scripts/install-arch.sh`, docs, and any `print_success "service restarted"` sections to prefer `audetic restart` instead of raw `systemctl`.
   - In `release/cli/latest.sh`, after install/config changes, prefer running `$BIN_DIR/audetic restart --wait` (with `--system` if the installer was invoked with `--system`). Fall back to direct `systemctl` only if the CLI binary is missing.

4. **Docs & messaging**
   - Add guidance to `README.md` and `docs/installation.md` showing `audetic restart` as the canonical way to apply config changes.
   - Update troubleshooting docs to mention `audetic restart --status` / logs for debugging.
   - Mention the command in any CLI help text where we currently instruct users to run `systemctl --user restart audetic.service`.

5. **Testing**
   - Unit test the service detection helper by pointing it at temporary directories with fake unit files in user/system locations.
   - Add integration tests (maybe using `assert_cmd`) that stub `systemctl` via a temp script in `PATH`, verifying that `audetic restart` invokes the right arguments for user and system modes, respects `--wait`, and surfaces errors.
   - Add snapshot tests for CLI output to ensure helpful messaging.
   - Manual QA: run `latest.sh`, edit config, then `audetic restart`; repeat in `--system` mode (requires sudo) to ensure command escalates correctly or produces actionable errors.

## Outstanding questions

- Should we allow `audetic restart --binary` (restart the current process instead of systemd) for environments that run Audetic manually? Maybe via a `--standalone` flag.
- Do we want the command to attempt `systemctl daemon-reload` automatically if the unit file changed (e.g., after reinstall), or leave that to installer?
- Should we honor an environment variable (e.g., `AUDETIC_SYSTEM_MODE=1`) to override detection in headless/server deployments?
- How do we surface restart status when systemd is unavailable (e.g., macOS)? We could attempt to detect launchd/homebrew services, but that may warrant a separate plan.
- Do we log/restart the updater service separately if we eventually split it out, or is there a single `audetic.service` forever?
