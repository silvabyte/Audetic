# Uninstall CLI Support

## Goals
- Provide a first-class way for users to remove Audetic (binary, services, config, caches) without re-running `latest.sh --uninstall`.
- Integrate uninstall behavior into the Rust CLI so `audetic uninstall` can be executed locally (and by the installer), respecting all of the knobs we already ship in `scripts/uninstall.sh`.
- Reuse as much logic as possible between the Bash installer/uninstaller and the new CLI subcommand to avoid drift (shared path helpers, consistent prompts, same list of artefacts).
- Ensure we remove updater-specific artefacts introduced by the new Rust auto-update engine (`update_state.json`, `updates/`, `.bak` binaries, locks) so the machine is left clean.
- Keep `make uninstall` and `release/cli/latest.sh --uninstall` working during the transition by shelling out to the CLI when available and gracefully falling back to the current Bash logic.

## Current uninstall surface area (Nov 2025)
- `release/cli/latest.sh` already exposes `--uninstall` which stops + disables the service, deletes the unit file, removes the binary under the detected prefix, and (with `--clean`) nukes `~/.config/audetic` and `~/.local/share/audetic`. It does **not** know about Whisper models, update wrappers, or updater state.
- `scripts/uninstall.sh` (invoked via `make uninstall`) accepts `--keep-config`, `--keep-whisper`, `--force`, enumerates exactly what will be removed, stops/ disables the user service, deletes `/usr/local/bin/audetic`, `/usr/local/bin/audetic-update`, removes config/data/whisper/source backups, clears `/tmp/audetic_*.wav`, and prints follow-up instructions.
- We currently install an `audetic-update` wrapper plus `~/.local/share/audetic/source` via the legacy shell tooling; users expect uninstall to clean those up (or at least tell them how to keep them).
- The Rust `update` module now writes `~/.config/audetic/update_state.json`, `~/.local/share/audetic/updates`, `~/.local/share/audetic/update.lock`, and creates `.bak` binaries in the install prefix. None of our shell uninstall paths are aware of these.
- `global::*` helpers already give us config/data directories and the update paths; `UpdateConfig::detect` resolves the live binary path + channel. The uninstall plan should consume those so CLI/Bash stay in sync.

## UX
- `audetic uninstall [FLAGS]`
  - `--system`: target `/etc/systemd/system/audetic.service` instead of the user unit. Mirrors `latest.sh --system`.
  - `--force` / `-y`: skip the interactive confirmation. Alias `--yes` if we want parity with `latest.sh`, but surface `--force` to match `scripts/uninstall.sh`.
  - `--dry-run`: enumerate what would be removed (binary, unit, config, Whisper models, update cache, temp files) without touching the filesystem. Doubles as automated test hook.
  - `--keep-config`: leave `~/.config/audetic` (same meaning as the Bash script).
  - `--keep-models` (alias `--keep-whisper`): preserve `~/.local/share/audetic/whisper`.
  - `--keep-source`: preserve `~/.local/share/audetic/source` if a developer wants to keep the checked-out repo.
  - `--keep-updates`: preserve `updates/`, `update_state.json`, and `.bak` binaries for debugging; default behavior is to remove them so auto-update gets a clean slate.
  - `--prefix <PATH>`: override detected binary/update script locations when the CLI is run from an unusual install root (parity with installer `--prefix`).
- The command shows a summary (same structure as `scripts/uninstall.sh`): headings via `print_step`/`print_success` analogues, list of targets, then prompts before executing unless `--force`.
- Errors calling `systemctl` should degrade (warn and continue) so uninstall still removes local files even on macOS or minimal WMs.
- The CLI prints explicit mentions of removed helper scripts (`audetic-update`) and shells out instructions similar to the current Bash footer (“remove Hyprland keybind”, “disable ydotool”) so users don’t lose that guidance.

## Implementation plan
1. **Shared uninstall inventory (path abstraction)**  
   - Extend `global` (or introduce an `install_layout` helper) that bundles: binary path (from `UpdateConfig::detect`), update wrapper (`audetic-update`), user/system unit path, config dir, data dir, whisper dir, sqlite history database (`~/.local/share/audetic/audetic.db`), update dirs/files, temp glob, and optional source backup.  
   - Add detection helpers that mirror `scripts/uninstall.sh`’s `ITEMS_TO_REMOVE` array so CLI, `latest.sh`, and any future Bash fallback stay synchronized. This helper should also expose the `systemctl` command/flags we plan to run.
2. **Rust CLI additions**  
   - Update `CliCommand` with `Uninstall(UninstallCliArgs)` plus struct definitions in `src/cli/mod.rs`. Wire `main.rs` to dispatch to `handle_uninstall_command`.  
   - Implement `handle_uninstall_command` in a new module (e.g., `src/cli/uninstall.rs`) that:
     - Calls the inventory helper to discover artefacts, prints the table, and respects `--dry-run`.  
     - Stops + disables the service (`systemctl --user/system ...`) with warnings if `systemctl` is missing or fails (parity with script).  
      - Removes the unit file and runs `systemctl daemon-reload` in the correct scope.  
      - Deletes the binary and `audetic-update`, reusing the sudo fallback logic we already wrote in `update::run_sudo_command` when permissions require elevation.  
      - Prompts the user (unless `--force`) about deleting the sqlite database before touching it so history can be preserved independently of `--keep-config`; `--dry-run` reports both outcomes for automated coverage.  
      - Cleans config/data directories unless `--keep-config`; within `data`, purge Whisper + updates selectively based on `--keep-models` / `--keep-updates`.  
     - Deletes `update_state.json`, `update.lock`, staged `updates/`, `.bak` binaries, and `/tmp/audetic_*.wav` (as `scripts/uninstall.sh` already attempts).  
     - Prints success/failure messages mirroring the Bash script’s UX.  
   - Provide a thin shim so `scripts/uninstall.sh` can eventually exec `audetic uninstall "$@"` once the Rust flow ships (long term: deprecate the shell script).
3. **Installer + automation alignment**  
   - Update `release/cli/latest.sh perform_uninstall` to prefer running the Rust CLI (`$BIN_DIR/audetic uninstall --force ...`) with flags derived from the installer arguments (`--system`, `--clean`). If the binary is missing, fall back to today’s inline Bash removal.  
   - Remove uninstall responsibilities from `scripts/install.sh`; instead of offering `--clean` destructive removal, have it instruct users to run `audetic uninstall` (optionally with `--clean`/`--keep-*`) before reinstalling.  
   - Keep `scripts/uninstall.sh` for a release or two but have it detect the Rust binary and delegate to `audetic uninstall` (so `make uninstall` exercises the same code path).  
   - Ensure `make deploy` bundles any new uninstall templates (if we add ones for systemd/system) and that `latest.sh` copies them so the CLI can rely on consistent unit paths.
4. **Docs + README updates**  
   - `README.md`, `docs/installation.md`, and `docs/text-injection-setup.md` should describe `audetic uninstall`, list the preservation flags, and mention how it relates to `latest.sh --uninstall` / `make uninstall`.  
   - Add a troubleshooting snippet covering permission errors (e.g., “rerun with sudo to remove `/usr/local/bin/audetic`”) to keep parity with the Bash guidance.
5. **Testing**  
   - Unit test the inventory/dry-run logic with tempdirs to guarantee that each flag matches what `scripts/uninstall.sh` does today (`keep-config`, `keep-whisper`, etc.).  
   - Integration test under `tests/cli.rs` that spawns the binary against a fixture directory tree and confirms files are removed or preserved based on flags.  
   - Add regression tests (or scripted checks) that ensure `latest.sh --uninstall` and `make uninstall` both route through the CLI when available.

## Outstanding questions
- Should the CLI attempt to remove `/usr/local/bin/audetic` automatically via sudo (matching `update::install_binary_with_sudo`), or should we require users to rerun with `sudo audetic uninstall`? Leaning toward the former for parity with update/install, but we need explicit error messaging.
- Do we keep `scripts/uninstall.sh` around as a wrapper forever (for environments without the Rust binary) or ship it only as a fallback inside the installer archive?
- Is there any scenario where we **don’t** want to delete `.bak` binaries that the updater left behind? (They can be useful for manual rollbacks.)
- Should `--dry-run` imply `--force` (no prompt) so it can run non-interactively in CI?
- How do we expose backup/archival flows (e.g., tar up config before deletion) without bloating the UX? The Bash script currently just warns; we may want an explicit `--backup <dir>` before we remove files.
