# Audetic Menu Bar (macOS)

A native SwiftUI `MenuBarExtra` agent that surfaces Audetic's status in the
macOS menu bar and provides global keyboard shortcuts. It is the macOS analog
of the Hyprland keybind on Linux.

## What it does

- Shows live status: daemon up/down, dictation recording, meeting active.
- Point-and-click toggles for **dictation** and **meeting**.
- "Open Audetic" opens the web UI (`http://127.0.0.1:3737/`) in the browser.
- User-customizable **global** keyboard shortcuts (Settings window) to toggle
  dictation and meetings from any app, via
  [`sindresorhus/KeyboardShortcuts`](https://github.com/sindresorhus/KeyboardShortcuts).

No default shortcuts are shipped (so we never steal a user's existing
hotkeys) — assign them in Settings.

## Architecture

The app is an **independent HTTP consumer** of the daemon, exactly like the
`audetic` CLI. It never reaches into daemon state; it only calls the public
API:

| Action            | Endpoint                       |
| ----------------- | ------------------------------ |
| Toggle dictation  | `POST /api/toggle`             |
| Toggle meeting    | `POST /api/meetings/toggle`    |
| Dictation status  | `GET /api/status`              |
| Meeting status    | `GET /api/meetings/status`     |
| Open web UI       | `http://127.0.0.1:3737/`       |

The host/port constants in `DaemonClient.swift` mirror
`crates/audetic-core/src/url.rs` (`HOST` / `DEFAULT_PORT` / `API_PREFIX`).
Keep them in sync.

When the daemon is down, status polls fail, the icon goes to its offline
glyph, and the toggle items are disabled.

## Build

Requires the Swift toolchain (Xcode Command Line Tools).

```sh
# Build + assemble the signed bundle:
make macos-menubar

# Or just compile for iteration:
cd apps/menubar-macos && swift build
```

`make macos-app` builds this and embeds the resulting
`Audetic Menu Bar.app` inside `Audetic.app/Contents/Library/LoginItems/`.
The Rust install flow (`audeticd install`) registers it as a per-user
LaunchAgent (`ai.audetic.menubar`) so it starts on login.
