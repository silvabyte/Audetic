# Audetic Installation Guide

Complete installation instructions for different operating systems and environments.

## Install

Audetic is built from source — there are no hosted binaries. Clone the repo and
run `make install`, **never with sudo**:

```bash
git clone https://github.com/silvabyte/Audetic.git
cd Audetic
make install
```

`make install` dispatches on `uname -s` and:

- Builds the workspace in release mode (on macOS, also assembles and ad-hoc signs `Audetic.app`).
- Synchronizes the web UI dependencies with `bun install --frozen-lockfile` before
  each UI rebuild, including when an older `node_modules` directory already exists.
- Hands off to `audeticd install`, which copies the daemon to `~/.local/share/audetic/bin/audeticd` (macOS: `~/Applications/Audetic.app`) and puts the standalone `audetic` CLI on your PATH at `~/.local/bin/audetic`.
- Registers the background service — a systemd **user** unit at `~/.config/systemd/user/audeticd.service` on Linux, a LaunchAgent (`ai.audetic.daemon`) on macOS — and starts it.
- Waits for the daemon to bind `127.0.0.1:3737`, then offers the terminal setup flow (`audetic setup`), the web Setup Center (`http://127.0.0.1:3737/settings/setup`), or setup later. Non-interactive installs print both follow-up paths.
- Everything lives under `$HOME` — no `/usr/local/bin`, no sudo.

It is idempotent, so it doubles as the upgrade path — see [Updating](#updating).

macOS has extra prerequisites (full Xcode, `cmake`, `ffmpeg`) and a permissions
story of its own: see the **[macOS Install Guide](./macos-install.md)**.

After install:
1. The service is already enabled and started. Confirm with `make status`.
2. Run `audetic setup` or open `http://127.0.0.1:3737/settings/setup` to check provider, optional text delivery, shortcuts, and system audio support.
3. Preview and install microphone and microphone + system audio keybinds from either setup flow surface.
4. Edit `~/.config/audetic/config.toml` if you need custom providers, models, or behavior tweaks.

## Prerequisites and system dependencies

`make install` needs these present before it can build.

### Prerequisites

All systems require:
- **Current stable Rust toolchain** (`rustup update stable`; older distribution toolchains may not build current dependencies)
- **Bun** (builds the bundled web Setup Center)
- **CMake, pkg-config, a C/C++ build toolchain, and Clang/libclang** (native bindings)
- **Linux development libraries**: ALSA and libxkbcommon headers
- **curl** for API communication

Linux service installation also needs a running **systemd user manager**. Run it
from a terminal in your logged-in desktop session. Prerequisites are checked
before compilation, including with `make -j install`.

Transcription provider setup, text injection (`wtype` / `ydotool`), clipboard
tools (`wl-clipboard` on Wayland; `xclip` / `xsel` on X11), FFmpeg, and system-audio
tools are diagnosed in Setup Center. These are capability-specific, not all
mandatory to install or use Audetic. A separate Whisper CLI is needed only when
choosing that provider; on-device engines are built into Audetic.

### System Dependencies

#### Omarchy / Arch Linux

```bash
sudo pacman -S --needed base-devel rustup bun cmake clang pkgconf alsa-lib libxkbcommon curl
rustup default stable   # first-time Rust setup, if no default toolchain is selected
```

Optional text-delivery and system-audio tools (`wtype` or `ydotool`, `wl-clipboard`,
`pipewire-audio`, and `libpulse`) are diagnosed after installation. The Setup
Center produces one minimal `pacman` command for whatever is missing; Audetic
never runs that privileged command from the browser.

#### Ubuntu/Debian

```bash
sudo apt update
sudo apt install libasound2-dev libxkbcommon-dev clang libclang-dev pkg-config curl cmake build-essential
```

Install current stable Rust with [rustup](https://rustup.rs/) and
[Bun](https://bun.sh/docs/installation), then check `rustc --version`,
`cargo --version`, and `bun --version` in the terminal that will run `make install`.

#### Fedora

```bash
sudo dnf install clang clang-devel cmake gcc-c++ pkgconf-pkg-config alsa-lib-devel libxkbcommon-devel curl
```

Install current stable Rust and Bun as above. `xdg-utils` is optional for opening
the browser automatically; the installer always prints the setup URL.

The default Linux build is CPU-only. Opting into `--features vulkan` (also enabled
by `cargo clippy --all-targets --all-features -- -D warnings`) additionally needs
Vulkan headers, the Vulkan loader, and `glslc`. On Arch: `sudo pacman -S --needed
vulkan-headers vulkan-icd-loader shaderc`. GPU use also needs the driver for your
hardware; none of these are prerequisites for the default `make install`.

### Text Injection Setup

Text injection is optional and off by default. To enable automatic paste, see the
[Text Injection Setup Guide](./text-injection-setup.md) for detailed configuration.

**Quick setup for ydotool (recommended):**

```bash
# Enable ydotool user service
systemctl --user enable --now ydotool.service

# Add to shell profile
echo 'export YDOTOOL_SOCKET="/run/user/$(id -u)/.ydotool_socket"' >> ~/.bashrc
source ~/.bashrc
```

## Whisper Installation Options

Audetic supports multiple Whisper implementations:

### Option 1: Optimized whisper.cpp (Recommended)

Use the optimized fork with automatic build:

```bash
git clone https://github.com/matsilva/whisper.git ~/.local/share/audetic/whisper
cd ~/.local/share/audetic/whisper
./build.sh
```

This downloads and quantizes the large-v3-turbo model automatically.

### Option 2: OpenAI Whisper (Python)

```bash
pip install -U openai-whisper
```

### Option 3: Standard whisper.cpp

```bash
git clone https://github.com/ggerganov/whisper.cpp.git
cd whisper.cpp
make
./models/download-ggml-model.sh base
```

## Building without installing

`make install` builds for you. To build alone — while hacking on Audetic — use
the normal cargo targets:

```bash
make build       # debug
make release     # optimized
make run         # run the daemon in the foreground, no service registration
```

`make install` is just those plus the handoff to `audeticd install`, so there is
no separate "manual install" path to keep in step.

## Configuration

Create the configuration directory and file:

```bash
mkdir -p ~/.config/audetic
```

Audetic will create a default config on first run, or you can create one manually:

### Quick Start (Audetic API - Recommended)

Zero-config cloud transcription - no API key or local setup required:

```toml
[whisper]
provider = "audetic-api"  # Default: hosted service, no setup needed
language = "en"

[wayland]
input_method = "ydotool"  # Recommended (auto-detected first)

[behavior]
auto_paste = false
preserve_clipboard = false
audio_feedback = true
```

### Advanced: Local Processing

#### For OpenAI Whisper (CLI)

```toml
[whisper]
provider = "openai-cli"
model = "base"
language = "en"
# command_path is auto-detected if whisper is in PATH

[wayland]
input_method = "ydotool"  # Recommended (auto-detected first)

[behavior]
auto_paste = false
preserve_clipboard = false
audio_feedback = true
```

#### For Optimized Whisper.cpp

```toml
[whisper]
provider = "whisper-cpp"
model = "large-v3-turbo"
language = "en"
command_path = "/home/user/.local/share/audetic/whisper/build/bin/whisper-cli"
model_path = "/home/user/.local/share/audetic/whisper/models/ggml-large-v3-turbo-q5_1.bin"

[wayland]
input_method = "ydotool"  # Recommended (auto-detected first)

[behavior]
auto_paste = false
preserve_clipboard = false
audio_feedback = true
```

## Systemd Service (Linux)

`make install` (via `audeticd install`) sets this up for you: it writes a
systemd **user** unit to `~/.config/systemd/user/audeticd.service` with
`ExecStart` pointed at `~/.local/share/audetic/bin/audeticd`, runs
`systemctl --user daemon-reload`, imports the current desktop environment, and
re-enables/restarts `audeticd.service`. Re-enabling removes the legacy
`default.target.wants` link on upgrades. The unit template lives at
`crates/audetic/src/install/audetic.service.tmpl` — edit it there, not by hand
in `~/.config`, or the next `make install` will overwrite your changes.

Autostart is attached to **`graphical-session.target`**, with
`PartOf=graphical-session.target` to stop at logout. This matters on Wayland:
starting at the user manager's `default.target` can precede the compositor and
leave the daemon without `WAYLAND_DISPLAY` for its entire lifetime. An
`After=graphical-session.target` ordering line alone cannot wait for a target
that is not part of the startup transaction. Omarchy's UWSM session (and
systemd-integrated GNOME/KDE sessions) imports the environment before starting
the graphical session target.

For a manually launched compositor that does **not** manage that target, use
its session-start hook to import the available desktop variables and then
`systemctl --user restart audeticd.service`; stop Audetic in the session-exit
hook. For example, run these inside the compositor session (omit variables that
are unset):

```sh
systemctl --user import-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE
systemctl --user restart audeticd.service
```

Include `DISPLAY`/`XAUTHORITY` for X11, `HYPRLAND_INSTANCE_SIGNATURE` for raw
Hyprland, and any custom `YDOTOOL_SOCKET`, `PULSE_SERVER`, or `PIPEWIRE_REMOTE`.
Omarchy/UWSM users do not need an extra Hyprland startup hook. A headless daemon
can still be started explicitly with `make start`; persistent headless startup
can be opted into with `systemctl --user add-wants default.target audeticd.service`
(reapply after reinstalling). Desktop delivery requires a graphical session.

Day to day, use the Make targets rather than `systemctl` directly; they
dispatch to systemd or launchd so the same words work on both platforms:

```bash
make start      # enable + start
make stop
make restart
make status     # supervisor state + a live probe of the API
make logs       # follow
```

> **Audio groups:** User services cannot add supplemental groups the account does not already have. Most setups that use PipeWire/ALSA through the desktop stack work without any extra privileges. If you need direct ALSA device access, add yourself to the `audio` group (followed by a re-login) or add `SupplementaryGroups=audio` via a systemd drop-in.

## Hyprland Integration

Add to your Hyprland config (`~/.config/hypr/hyprland.conf`):

```
bindd = SUPER, R, Audetic Note, exec, curl -X POST http://127.0.0.1:3737/api/audio-notes/toggle -H 'Content-Type: application/json' -d '{"capture_source":"microphone"}'
```

On upgrade, remove the obsolete Audetic managed shortcut sections before
reinstalling with `audetic keybind install --target note` and
`audetic keybind install --target system-note`. The installer deliberately
reports conflicting old/custom bindings rather than overwriting unrelated
shortcuts. The new actions differ only by capture source, not classification.

For Omarchy users:
```
bindd = SUPER SHIFT, R, Audetic System Note, exec, curl -X POST http://127.0.0.1:3737/api/audio-notes/toggle -H 'Content-Type: application/json' -d '{"capture_source":"microphone_and_system"}'
```

## GNOME + Wayland Setup

GNOME requires special setup due to security restrictions:

### 1. Install ydotool and setup daemon

```bash
sudo pacman -S ydotool  # or appropriate package manager

# Create user service
mkdir -p ~/.config/systemd/user
```

Create `~/.config/systemd/user/ydotoold.service`:

```ini
[Unit]
Description=ydotoold user daemon
After=graphical-session.target

[Service]
Type=simple
ExecStart=/usr/bin/ydotoold -P 660

[Install]
WantedBy=default.target
```

```bash
# Add environment variable
echo 'export YDOTOOL_SOCKET="/run/user/$(id -u)/.ydotool_socket"' >> ~/.bashrc
source ~/.bashrc

# Enable services
systemctl --user daemon-reload
systemctl --user enable --now ydotoold.service
systemctl --user enable --now audeticd.service
```

### 2. Configure Audetic for GNOME

```toml
[wayland]
input_method = "ydotool"  # Recommended (auto-detected first)
```

### 3. Create GNOME Keyboard Shortcut

1. Open GNOME Settings
2. Go to Keyboard → Keyboard Shortcuts → View and Customize Shortcuts
3. Go to Custom Shortcuts
4. Add a microphone shortcut: `audetic notes toggle`; optionally add a system-audio shortcut: `audetic notes toggle --capture-source microphone_and_system`.
5. Set your preferred key combination (e.g., Super+R)

## Testing Installation

1. **Test service**: `make status` (or `systemctl --user status audeticd.service`)
2. **Test API**: `audetic notes toggle`
3. **Test provider**: `audetic provider test` (validates transcription setup)
4. **Test recording**: Press your configured keybind
5. **Check logs**: `make logs` or `journalctl --user -u audeticd.service -f`

## Troubleshooting

### Source build cannot resolve a web UI package

`make install` and plain `cargo build` synchronize locked dependencies before
building the UI. For a standalone Vite build, use `make ui-build` (also syncs
dependencies), or `make ui-install` before `bun run build`. A frozen-lockfile
failure indicates a manifest/lockfile mismatch or an install/download failure;
resolve that error rather than externalizing the missing package in Vite.
Missing Bun is a build error. `AUDETIC_SKIP_UI_BUILD=1` is an explicit
backend-only/prebuilt-UI escape hatch, not a desktop installation workaround.

### Service fails to start
- Check logs: `make logs` or `journalctl --user -u audeticd.service -e`
- Check status: `make status`
- Verify binary path: `which audetic` (the CLI) and `ls ~/.local/share/audetic/bin/audeticd` (the daemon)
- Rebuild and reinstall: `make install`

### Recording doesn't work
- Check microphone permissions
- Verify audio device: `arecord -l`
- Ensure the desired input device is set as the system default (Audetic uses whatever CPAL reports as default)

### Text injection fails
- Check `systemctl --user is-active graphical-session.target` and reinstall with
  `make install` to migrate older boot-time units. Importing variables into the
  user manager does not update an already running daemon; restart it afterwards.
- Verify ydotool service: `systemctl --user status ydotool.service`
- Check socket: `ls -la /run/user/$(id -u)/.ydotool_socket`
- See [Text Injection Setup](./text-injection-setup.md)

### Memory issues
- Large Whisper models need 3-5GB RAM
- Adjust `MemoryMax` in the service file (or remove it entirely)
- Use smaller models if needed

### GNOME-specific issues
- Ensure ydotoold is running as user service (not system)
- Verify YDOTOOL_SOCKET environment variable
- wtype will NOT work on GNOME - use ydotool only

## Updating

There is no auto-updater and no release channel. Rebuilding from source *is*
the update:

```bash
git pull && make install
```

`make install` is idempotent — it overwrites the installed binary, refreshes the
service definition, and restarts the daemon. Your config and transcription
history are untouched.

Version is whatever `Cargo.toml` says; `audetic version` reports it. See
[ADR 0001](./adr/0001-source-only-distribution.md) for why the hosted-release
and auto-update machinery was removed.

## Uninstalling

```bash
make uninstall
```

This stops and deregisters the service, then removes what `install` put on
disk. It prints the full plan and asks for confirmation first.

### Uninstall options

Pass flags through `ARGS`:

```bash
# Preview what will be removed (changes nothing)
make uninstall ARGS="--dry-run"

# Skip the confirmation prompt
make uninstall ARGS="--yes"

# Keep your config and transcription history
make uninstall ARGS="--keep-config --keep-database"
```

`make uninstall` prefers the *installed* daemon, so it works on a fresh clone
that was never built. You can also call it directly:
`audeticd uninstall --help`.

### What gets removed

By default:

- `~/.local/share/audetic/bin/` (the daemon)
- `~/.local/bin/audetic` (the CLI on your PATH)
- `~/.config/systemd/user/audeticd.service` (Linux) — or `~/Applications/Audetic.app`, both LaunchAgent plists, and `~/Library/Logs/Audetic/` (macOS)
- `~/.config/audetic/config.toml`
- `~/.local/share/audetic/audetic.db*` (transcription history)
- `~/.local/share/audetic/` state: `audio-notes/`, retained legacy audio directories, `models/`, `agent-runs/`, `keybind-backups/`, `config-backups/`
- Any leftovers from the retired auto-updater (`updates/`, `update.lock`, `update_state.json`)

`--keep-config` preserves `config.toml`; `--keep-database` preserves
`audetic.db*`. Everything else goes.

Temp recordings in `/tmp` are not touched — `make clean` removes those.

macOS TCC grants (Microphone, Screen Recording) are deliberately left alone,
since resetting them would also affect any other build of Audetic on the
machine. The [macOS guide](./macos-install.md#permissions) shows how to reset
them by hand if you want a clean slate.
