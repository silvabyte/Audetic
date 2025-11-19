# Uninstall CLI Support

## Goals
- Provide a first-class way for users to remove Audetic (binary, services, config, caches) without re-running `latest.sh --uninstall`.
- Integrate uninstall behavior into the Rust CLI so `audetic uninstall` can be executed locally, respecting flags (`--system`, `--clean`) and user confirmation.
- Reuse as much logic as possible between the Bash installer and the new CLI subcommand to avoid drift.
- Ensure `make deploy` packages any new templates/scripts needed for uninstall flow.

## UX
- `audetic uninstall [--system] [--clean] [--yes]`
  - `--system`: remove systemd system service + `/etc/systemd/system/audetic.service` instead of user service.
  - `--clean`: remove config (`~/.config/audetic`) and data (`~/.local/share/audetic`) directories after uninstall, optionally backing them up.
  - `--yes`: skip interactive confirmation (default prompts with “This will remove Audetic from /usr/local/bin. Continue? [y/N]”).
  - Detect the install prefix/binary path automatically (default `/usr/local/bin/audetic`). Allow override via `--prefix` or env var if we want parity with installer.
- `audetic uninstall --help` explains what files/services will be touched and how to reinstall.
- When run by the installer script with `--uninstall`, we can shell out to `audetic uninstall --yes --clean` if the binary is still present; otherwise fall back to Bash removal logic.

## Implementation plan
1. **Path abstraction (reuse existing global utilities)**:
   - Expose helpers for `binary_path(prefix)`, `user_systemd_unit()`, `system_systemd_unit()`, config/data directories, etc., so uninstall logic (Rust + Bash) share the same definitions.
2. **Rust CLI additions**:
   - Extend `CliCommand` with `Uninstall(UninstallArgs)`.
   - Implement `handle_uninstall_command(UninstallArgs)` that:
     - Validates `--system` vs user mode.
     - Stops/disables the service via `systemctl` (user or system).
     - Removes the service file (`~/.config/systemd/user/audetic.service` or `/etc/systemd/system/audetic.service`).
     - Deletes the binary at the resolved prefix (requires sudo if prefix is root-owned; surface a clear error telling the user to rerun with sudo if needed).
     - If `--clean`, deletes config + data directories (with backup option? maybe `--backup-dir`).
     - Logs a summary of deleted items and next steps (e.g., rerun installer to reinstall).
   - Consider using the same interactive prompt helper as other CLI flows (maybe integrate with `dialoguer` or simple stdin prompt).
3. **Installer script alignment**:
   - `latest.sh --uninstall [--clean] [--system]` should:
     - Attempt to run `/usr/local/bin/audetic uninstall --yes ...` if the binary exists and is executable.
     - If the binary is missing, fall back to inline removal (basically what we have now).
     - Use the same logic for config/data cleanup so behavior matches the Rust CLI.
4. **Docs + README updates**:
   - Update `README.md` + `docs/installation.md` to mention `audetic uninstall` as the primary removal path.
   - Document flags like `--system` and `--clean`.
5. **Testing**:
   - Unit tests for the uninstall module (mock filesystem + systemctl calls with trait abstraction?).
   - Integration test (maybe under `tests/cli.rs`) that runs `audetic uninstall --dry-run` to verify plan generation.
   - Manual instructions: run `latest.sh`, then `audetic uninstall --yes --clean`, ensure everything is removed.

## Outstanding questions
- Should uninstall support a `--dry-run` flag that prints what would be removed?
- How do we gracefully handle environments without `systemctl` (e.g., macOS)? Probably skip service removal with a warning.
- Should we back up config before deleting when `--clean` is set? Maybe default to backing up to `~/.config/audetic/backups/uninstall_<timestamp>.tar.gz`.
- Do we need to remove any additional files (e.g., `audeticctl`, helper scripts) once we add them in the future? If so, centralize the list in one place (maybe a JSON manifest or Rust const).
