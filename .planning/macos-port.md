# macOS Port — Touch Point Audit

Inventory of every place in the repo that assumes Linux. No fluff, no marketing — just where the bodies are buried. File refs are `path:line` where useful.

State as of branch `feat/mac-runtime` off `9563e0c` (v0.1.25).

---

## TL;DR risk ranking

| Risk | Subsystem | Why |
|---|---|---|
| Blocker | System audio capture (`pw-cat`/PipeWire) | No drop-in macOS equivalent. Either ship mic-only or take an Objective‑C / loopback dependency. |
| Blocker | Keybind module (Hyprland-only) | Whole module is Hyprland config-file editing. macOS has no analog; needs a different model entirely. |
| Blocker | Text injection (`wtype`/`ydotool`/`xdotool`) | Need `osascript` or `CGEventPost`. Probably also needs Accessibility TCC grant. |
| Medium | Service mgmt (`systemd`/`journalctl`) | Need launchd `.plist` + file-based logs. Mechanical but touches install/uninstall/logs. |
| Medium | Installer scripts (`latest.sh` / `uninstall.sh`) | Hard-rejects non-Linux, uses `sha256sum` (not on macOS), `xdg-open`, `systemctl`. |
| Medium | Notifications (`hyprctl notify`) | Swap to `osascript` or `terminal-notifier`. |
| Small | Audio feedback (`aplay`/`paplay`/`speaker-test`) | Swap to `afplay`. |
| Small | CI matrix | Add `macos-latest`, gate apt installs. |
| Trivial | Paths | `dirs` crate already does the right thing; just verify. |
| Trivial | cpal mic capture | Works on CoreAudio via cpal. Need to handle TCC mic prompt. |
| Trivial | FFmpeg | `ffmpeg-sidecar` already downloads evermeet macOS builds. |
| Trivial | `arboard` clipboard | Has macOS backend; just drop the Wayland feature flag on macOS. |
| Trivial | Release `deploy.ts` | `macos-aarch64`/`macos-x86_64` targets already declared. |

---

## 1. Audio capture

### 1a. Microphone — `cpal` (fine on macOS)

- `crates/audetic/src/audio/mic_source.rs:29-83` — `default_host()` → `default_input_device()` → 16 kHz mono `StreamConfig`.
- `crates/audetic/src/audio/audio_stream_manager.rs:29-91` — same pattern, drives the VTT pipeline.

cpal uses CoreAudio on macOS automatically. Only thing to handle: **first call triggers the macOS microphone TCC prompt**. If denied, `default_input_device()` returns `None` — error path already exists, just verify the message reads sensibly on macOS.

### 1b. System audio — PipeWire `pw-cat` (blocker)

- `crates/audetic/src/audio/system_source.rs:36-53` — shells out to `pactl get-default-sink`, appends `.monitor`.
- `crates/audetic/src/audio/system_source.rs:70` — `which("pw-cat")` gate.
- `crates/audetic/src/audio/system_source.rs:99-126` — spawns `pw-cat --record --raw --format f32`.
- `crates/audetic/src/audio/system_source.rs:200-256` — reads raw f32 LE from stdout.

None of `pactl`, `pw-cat`, or PipeWire exist on macOS. Options:

1. **Mic-only on macOS (cheapest).** The `which("pw-cat")` gate at line 70 already degrades to mic-only when the tool is missing — on macOS that path triggers automatically. Ship like this for v1, document the limitation.
2. **CoreAudio aggregate device via `ScreenCaptureKit` audio tap** (macOS 13+). Native, requires Screen Recording TCC. Objective‑C interop or a wrapper crate.
3. **Document BlackHole / Loopback.** User installs a virtual loopback device; we treat it as just another input via cpal. Zero code, bad UX.

Recommend (1) for the first macOS build, (3) as a documented workaround, (2) as a follow-up.

### 1c. Audio feedback / beeps

- `crates/audetic/src/ui/mod.rs:101-200` — tries `aplay`, then `speaker-test`, then `beep`, then `python3 → paplay`.

None of these exist on macOS. Add an `afplay /System/Library/Sounds/Glass.aiff` (or similar) branch behind `cfg(target_os = "macos")`.

---

## 2. Daemon / service management

### 2a. Service install

- `crates/audetic/src/install/mod.rs:61-78` — paths via `dirs::data_local_dir()` + `dirs::config_local_dir()`. These already resolve correctly on macOS (`~/Library/Application Support`).
- `crates/audetic/src/install/mod.rs:114-128` — renders `audetic.service.tmpl`.
- `crates/audetic/src/install/mod.rs:130-140` — `systemctl --user daemon-reload`.
- `crates/audetic/src/install/mod.rs:142-152` — `systemctl --user enable --now`.
- `crates/audetic/src/install/mod.rs:180-187` — `xdg-open` to launch browser.
- `crates/audetic/src/install/audetic.service.tmpl` — systemd unit with `ProtectSystem=strict`, `ReadWritePaths`, `StandardOutput=journal`.

macOS analog: **LaunchAgent** at `~/Library/LaunchAgents/com.audetic.service.plist`, loaded with `launchctl bootstrap gui/$UID …` (modern) or `launchctl load -w …` (older but still works). `KeepAlive=true` for restart. No equivalent to systemd sandboxing — `ProtectSystem`/`ReadWritePaths` just gets dropped.

Browser open: replace `xdg-open` with `open` on macOS.

Plan: introduce a `ServiceManager` trait with `LinuxSystemd` + `MacosLaunchd` impls, behind `cfg(target_os = ...)`. Templates live next to each impl.

### 2b. Service logs

- `crates/audetic/src/logs/mod.rs:49-57` — `journalctl --user -u audetic.service`.

No journald on macOS. Two clean options:

- Have the daemon write to `~/Library/Logs/Audetic/audetic.log` (rotated) and read the file on macOS. Simplest and works regardless of how the daemon was launched.
- `log show --predicate 'process == "audetic"' --info` — uses the unified log, but requires the daemon to use `os_log` (it doesn't today). Skip.

Go with file-based on macOS. Probably worth adding file-based logging on Linux too as a fallback when the unit isn't installed.

### 2c. launchd plist sketch

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "...">
<plist version="1.0"><dict>
  <key>Label</key><string>com.audetic.service</string>
  <key>ProgramArguments</key><array><string>__EXEC_START__</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>__LOG_DIR__/audetic.log</string>
  <key>StandardErrorPath</key><string>__LOG_DIR__/audetic.log</string>
</dict></plist>
```

---

## 3. Installer / packaging

### 3a. `release/cli/latest.sh`

- `release/cli/latest.sh:69` — `require_cmd sha256sum`. **Not present on macOS by default.** macOS has `shasum -a 256`. Fix: detect and alias, or compute hash via a shell function.
- `release/cli/latest.sh:71-91` — `detect_target()` explicitly rejects non-Linux. Needs Darwin branch returning `macos-aarch64` / `macos-x86_64`.
- `release/cli/latest.sh:107` — `mktemp -d -t audetic-install.XXXXXX` — works on macOS but the `-t` arg has different semantics (it's a prefix, not a template). The current form happens to work but is worth testing.
- `release/cli/latest.sh:125` — `find … -perm -u+x` works on BSD `find` (macOS) too.

### 3b. `release/cli/uninstall.sh`

- `release/cli/uninstall.sh:26-31` — hardcodes `XDG_CONFIG_HOME`/`XDG_DATA_HOME`. On macOS need to also clean `~/Library/Application Support/audetic`, `~/Library/Logs/Audetic`, `~/Library/LaunchAgents/com.audetic.service.plist`.
- `release/cli/uninstall.sh:123-125` — `systemctl` gating. Add `launchctl` branch.

### 3c. Build & publish — `scripts/release/deploy.ts`

- `scripts/release/deploy.ts:26-31` — `TARGET_LOOKUP` already includes `aarch64-apple-darwin` and `x86_64-apple-darwin`. Free.
- `scripts/release/deploy.ts:59` — `ensureCommands(["cross", "cargo", "tar"])` — `cross` won't help for macOS targets (no Docker image). Either build natively on a macOS runner, or skip `cross` for darwin targets and require host=darwin when building those.
- `release/cli/releases/0.1.25/manifest.json` — currently only `linux-x86_64-gnu`. Manifest schema accepts arbitrary target ids, no schema work needed.

### 3d. Code signing / notarization

Not done today (Linux doesn't need it). For Mac distribution outside a managed environment:

- Developer ID Application cert
- `codesign --deep --options runtime --entitlements audetic.entitlements ...`
- Notarize with `notarytool submit … --wait`
- Staple with `xcrun stapler staple`

Entitlements file will need `com.apple.security.device.audio-input` at minimum, plus `com.apple.security.device.audio-capture` and Accessibility if we go native for text injection / system audio.

Scope decision needed: do we want notarized binaries from day one, or ship unsigned with `xattr -d com.apple.quarantine` instructions for power users?

### 3e. Makefile

- `Makefile` service targets (`start`/`stop`/`status`/`logs`) hardcode `systemctl --user` and `journalctl`. Needs `uname -s` dispatch or a shim script.

---

## 4. File system paths

`crates/audetic/src/global/mod.rs:6-40` uses `dirs::config_dir()` / `dirs::data_dir()`. On macOS these both resolve to `~/Library/Application Support`. That's fine but unusual — every artifact (config, db, updates, lock) ends up under one tree. Verify nothing assumes `data_dir != config_dir`.

- `crates/audetic/src/app/mod.rs:254-259` — `resolve_meetings_dir()` falls back to `/tmp/audetic/meetings`. `/tmp` exists on macOS but is a symlink to `/private/tmp`; fine.

No hardcoded `/etc`, `/var/log`, or `/usr/share` in the Rust code outside the audio-feedback paths in `ui/mod.rs` (which we're rewriting anyway).

---

## 5. TCC permissions

macOS permission prompts the user will see:

| Prompt | Triggered by | Code site |
|---|---|---|
| Microphone | cpal `default_input_device()` | `audio/mic_source.rs:31`, `audio/audio_stream_manager.rs:39` |
| Screen Recording | only if we adopt ScreenCaptureKit for system audio | not yet |
| Accessibility | only if we adopt `CGEventPost` for text injection | not yet |
| Input Monitoring | only if we register global hotkeys natively | not yet |

If we stick with `osascript` for text injection and notifications, we avoid the Accessibility prompt — `osascript` runs with its own granted permissions and the user's existing trust of the system's Script Editor binary handles a lot. But: scripting events in another app may still trigger an Automation prompt the first time.

Two things to wire up regardless of approach:
1. **Info.plist `NSMicrophoneUsageDescription`** — required for the mic prompt to even appear. Without it the daemon will be killed.
2. **A way for the user to recover from a denied prompt.** Today the code paths just error; on macOS denial is sticky and only resettable via `tccutil reset Microphone com.audetic.service` or System Settings. Surface a "Permission denied — open System Settings" path.

---

## 6. Notifications

- `crates/audetic/src/ui/mod.rs:41,54,73,85` — `hyprland_notify()` calls.
- `crates/audetic/src/ui/mod.rs:93-98` — shells out to `hyprctl notify -1 3000 <color> <title>`.

macOS replacements, in increasing order of investment:

1. `osascript -e 'display notification "msg" with title "Audetic"'` — works, ugly, no icon control, drops in immediately.
2. `terminal-notifier` — better UX, requires a separate install (brew) or bundling the binary.
3. `UNUserNotificationCenter` via Objective‑C bridge — native, requires a notification permission grant.

Start with (1). Move to (3) when there's a reason.

---

## 7. Tray / desktop integration

The web UI is the primary surface (`http://127.0.0.1:3737`), so no real GUI shell work blocks the port.

Linux-specific integrations that just become inert on macOS:
- `crates/audetic/src/config/mod.rs:36-43` — `WaybarConfig` struct + Nerd Font glyphs at `:90-93`. Harmless on macOS, ignored.
- `docs/waybar-integration.md` — Linux-only doc; leave.

A native macOS menu bar item would be nice-to-have but is not blocking. Don't build it on the critical path.

---

## 8. Process / signals / IPC

No `/proc`, `inotify`, D-Bus, or Linux-specific signal handling in the Rust code. The daemon relies on its supervisor (systemd / launchd) for restarts. Nothing to port here.

---

## 9. Build & toolchain

### 9a. `crates/audetic/Cargo.toml`

- `:22` `cpal = "0.15"` — cross-platform, fine.
- `:23` `hound = "3.5"` — fine.
- `:40` `arboard = { version = "3.3", features = ["wayland-data-control"] }` — the `wayland-data-control` feature pulls Wayland deps that won't build on macOS unconditionally. Verify with `cargo build --target aarch64-apple-darwin`; if it breaks, gate it:
  ```toml
  [target.'cfg(target_os = "linux")'.dependencies]
  arboard = { version = "3.3", features = ["wayland-data-control"] }

  [target.'cfg(not(target_os = "linux"))'.dependencies]
  arboard = "3.3"
  ```
- `:85` `ffmpeg-sidecar = "2.5"` — already documented (`:82-84`) as downloading evermeet builds on macOS. Free.
- `:47` `dirs = "5.0"`, `:50` `which = "6.0"` — both cross-platform.

### 9b. `crates/audetic/build.rs`

Only invokes `bun` to build the web UI. Bun is fine on macOS.

### 9c. Native deps

No `libasound2-dev` / `libxkbcommon-dev` equivalents needed on macOS — CoreAudio is a system framework, linked automatically by cpal's `coreaudio-sys` build script.

---

## 10. CI/CD

- `.github/workflows/rust.yml:14` — `runs-on: ubuntu-latest`. Single OS.
- `.github/workflows/rust.yml:22` — `sudo apt-get install -y libasound2-dev libxkbcommon-dev ffmpeg`. Linux-only.
- `.github/workflows/rust.yml:23-26` — bun install. Cross-platform.
- `.github/workflows/rust.yml:33-34` — `cargo build` / `cargo test`. Cross-platform.

Minimal change: add `macos-latest` to a `strategy.matrix.os` and gate the apt step on `runner.os == 'Linux'`. No mic in CI runners, so audio tests must already tolerate "no device" (verify).

---

## 11. External commands inventory

Every `Command::new(…)` (or shell-out from scripts) that hits Linux-only tooling:

| Command | Where | macOS replacement |
|---|---|---|
| `pactl` | `audio/system_source.rs:36` | (none — see §1b) |
| `pw-cat` | `audio/system_source.rs:70,99` | (none — see §1b) |
| `aplay` | `ui/mod.rs:145` | `afplay` |
| `speaker-test` | `ui/mod.rs:163` | drop |
| `paplay` | `ui/mod.rs:191` | `afplay` |
| `beep` | `ui/mod.rs:183` | drop |
| `wtype` | `text_io/mod.rs:172-183` | `osascript` keystroke |
| `ydotool` | `text_io/mod.rs:186-203` | `osascript` keystroke |
| `xdotool` | `text_io/mod.rs:~235` | `osascript` keystroke |
| `wl-copy` | `text_io/mod.rs:~326` | `pbcopy` |
| `xclip` | `text_io/mod.rs:~330` | `pbcopy` |
| `xsel` | `text_io/mod.rs:~336` | `pbcopy` |
| `qdbus` | `text_io/mod.rs:~245` | drop |
| `xdg-open` | `install/mod.rs:180` | `open` |
| `systemctl` | `install/mod.rs:132,142`; `Makefile`; `logs/mod.rs:49`; `uninstall.sh:123` | `launchctl` |
| `journalctl` | `logs/mod.rs:49` | file-based logs |
| `hyprctl` | `ui/mod.rs:94` | `osascript` notification |
| `sha256sum` | `release/cli/latest.sh:69` | `shasum -a 256` |

Pattern: most of these live in two modules (`ui/mod.rs`, `text_io/mod.rs`). Worth introducing a small platform trait per concern (notifier, beeper, text injector, clipboard fallback) rather than `cfg!()` scattered across call sites.

---

## 12. Keybinds — Hyprland module (blocker)

Whole `crates/audetic/src/keybind/` is Hyprland config editing:
- `keybind/discovery.rs:8-12` — looks in `~/.config/hypr/{bindings,keybinds,hyprland}.conf`.
- `keybind/parser.rs` — parses `bindd = MOD, KEY, DESC, exec, CMD`.
- `keybind/writer.rs` — writes the same.
- `cli/keybind.rs:18-30` — interactive setup wizard.
- `api/routes/keybind.rs` — HTTP endpoints exposing the above.

This whole flow doesn't translate. macOS has no equivalent of "edit my window manager's config to push to talk." Options:

1. **No-op the keybind module on macOS.** Setup wizard prints "On macOS, configure a hotkey in System Settings → Keyboard → Shortcuts → Services that hits `curl http://127.0.0.1:3737/api/…`". Cheapest, real users will hate it.
2. **Native global hotkey** via the `global-hotkey` crate or similar. Requires Input Monitoring TCC. Decent UX but the in-app "set hotkey" flow has to be rebuilt — no config file to edit.
3. **Ship a small `audetic-hotkey-agent`** as a separate LaunchAgent that registers global hotkeys and pokes the daemon's HTTP API. Cleaner separation but more moving parts.

Recommend (1) for v1 plus a documented Shortcuts.app recipe, (2) when there's appetite.

API routes (`api/routes/keybind.rs`) should return a "not supported on this platform" response on macOS rather than 500ing.

---

## 13. Suggested implementation order

1. **Make it compile on `aarch64-apple-darwin`** — feature-gate `arboard`'s Wayland flag, verify cpal links. (1 day)
2. **Paths + service mgmt** — `ServiceManager` trait, launchd plist template, file-based logging, `open` instead of `xdg-open`. (2-3 days)
3. **Installer scripts** — `latest.sh` Darwin detection, `shasum` fallback, `uninstall.sh` macOS paths. (1 day)
4. **CI** — add `macos-latest` to the matrix. (half day)
5. **Run-time platform shims** — notifier, beeper, clipboard fallback as platform traits. (1-2 days)
6. **Text injection via `osascript`**, including TCC handling for first-paste denials. (1-2 days, longer if we go `CGEventPost`)
7. **Keybind module no-op + documented Shortcuts.app recipe.** (half day)
8. **Mic-only smoke test end-to-end on a real Mac.** (1 day, hardware-dependent)
9. **(Stretch) System audio via ScreenCaptureKit or BlackHole docs.** Spike before committing.
10. **(Stretch) Code signing + notarization** if we want shareable binaries.

Steps 1-8 are the path to "works on my Mac." Steps 9-10 are productization.

---

## Open questions for product/scope

- System audio on macOS: ship mic-only and treat full capture as a follow-up, or block v1 on a ScreenCaptureKit implementation?
- Distribution: signed + notarized `.dmg` from day one, or unsigned tarball + power-user instructions?
- Keybinds: accept "user binds a Shortcuts.app hotkey" as the v1 UX, or invest in a native global-hotkey path?
- Do we want a menu bar item, or is the web UI sufficient for v1?
