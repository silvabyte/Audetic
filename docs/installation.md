# Audetic Installation Guide

Complete installation instructions for different operating systems and environments.

## Quick Install (Recommended)

Audetic ships verified, pre-built binaries. Install with one command—no Rust toolchain, git clone, or manual builds required, and no sudo:

```bash
curl -fsSL https://install.audetic.ai/cli/latest.sh | bash
```

The installer:

- Detects your OS/architecture, downloads the matching artifact, and verifies its SHA-256.
- Hands off to `audetic install`, which copies the binary to `~/.local/share/audetic/bin/audetic`.
- Writes a systemd **user** unit at `~/.config/systemd/user/audetic.service` and `enable --now`s it.
- Waits for the daemon to bind `127.0.0.1:3737`, then opens the web UI (`http://127.0.0.1:3737/`) in your default browser so you can finish onboarding (ffmpeg install, provider config) in the SPA.
- Everything lives under `$HOME` — no `/usr/local/bin`, no sudo.

### Useful flags

```
latest.sh --channel beta            # jump to another release channel
latest.sh --version <v>             # pin a specific version
latest.sh --no-launch               # don't open the web UI in a browser after install
```

After install:
1. The installer already enabled/started the systemd **user** service. Use `systemctl --user status audetic.service` to confirm.
2. Finish provider and ffmpeg setup in the web UI the installer opened (or visit `http://127.0.0.1:3737/`).
3. Add a keybind in Hyprland (or your compositor) that calls `curl -X POST http://127.0.0.1:3737/api/toggle`.
4. Edit `~/.config/audetic/config.toml` if you need custom providers, models, or behavior tweaks.

## Manual Installation

> **When should I use this?**  
> Only when you need to hack on Audetic itself or build for a platform that doesn't have pre-built binaries yet. Everyone else should stick with the `latest.sh` installer above.

### Prerequisites

All systems require:
- **Rust toolchain** (1.70+)
- **Whisper implementation** (see [Whisper Installation Options](#whisper-installation-options))
- **Text injection tool**: `ydotool` (recommended) or `wtype`
- **Clipboard tools**: `wl-clipboard` (Wayland) or `xclip`/`xsel` (X11)
- **Audio dependencies**: ALSA libraries
- **curl** for API communication

### System Dependencies

#### Arch Linux

```bash
sudo pacman -S rust ydotool wtype wl-clipboard alsa-lib curl cmake make gcc
```

#### Ubuntu/Debian

```bash
sudo apt update
sudo apt install cargo libasound2-dev wl-clipboard curl cmake build-essential

# Install ydotool (may need to compile from source)
sudo apt install ydotool || {
    git clone https://github.com/ReimuNotMoe/ydotool.git
    cd ydotool && mkdir build && cd build
    cmake .. && make -j$(nproc)
    sudo make install
}
```

#### Fedora

```bash
sudo dnf install rust cargo ydotool cmake gcc-c++ alsa-lib-devel curl openssl-devel
```

### Text Injection Setup

Audetic requires a text injection method. See the [Text Injection Setup Guide](./text-injection-setup.md) for detailed configuration.

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

## Building Audetic

```bash
# Clone the repository
git clone https://github.com/silvabyte/Audetic.git
cd Audetic

# Build release version
cargo build --release

# Install into the user-local layout (~/.local/share/audetic/bin + systemd
# user unit). No sudo needed — this is the same path `latest.sh` takes.
./target/release/audetic install
```

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
auto_paste = true
preserve_clipboard = false
delete_audio_files = true
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
auto_paste = true
preserve_clipboard = false
delete_audio_files = true
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
auto_paste = true
preserve_clipboard = false
delete_audio_files = true
audio_feedback = true
```

## Systemd Service Setup

The `latest.sh` installer (via `audetic install`) already sets this up: it
writes a systemd **user** unit to `~/.config/systemd/user/audetic.service` with
`ExecStart` pointed at `~/.local/share/audetic/bin/audetic`, runs
`systemctl --user daemon-reload`, and `systemctl --user enable --now audetic.service`.

If you built Audetic manually and want to wire up the service by hand:

```bash
mkdir -p ~/.config/systemd/user
```

Create `~/.config/systemd/user/audetic.service`:

```ini
[Unit]
Description=Audetic Voice Transcription Service
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/share/audetic/bin/audetic
Restart=always
RestartSec=5
Environment="RUST_LOG=info"
MemoryMax=6G
CPUQuota=80%

[Install]
WantedBy=default.target
```

Enable and start the service:

```bash
systemctl --user daemon-reload
systemctl --user enable --now audetic.service
```

> **Audio groups:** User services cannot add supplemental groups the account does not already have. Most setups that use PipeWire/ALSA through the desktop stack work without any extra privileges. If you need direct ALSA device access, add yourself to the `audio` group (followed by a re-login) or add `SupplementaryGroups=audio` via a systemd drop-in.

## Hyprland Integration

Add to your Hyprland config (`~/.config/hypr/hyprland.conf`):

```
bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/api/toggle
```

For Omarchy users:
```
bindd = SUPER, R, Audetic, exec, $terminal -e curl -X POST http://127.0.0.1:3737/api/toggle
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
systemctl --user enable --now audetic.service
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
4. Add new shortcut with command: `curl -X POST http://127.0.0.1:3737/api/toggle`
5. Set your preferred key combination (e.g., Super+R)

## Testing Installation

1. **Test service**: `systemctl --user status audetic.service`
2. **Test API**: `curl -X POST http://127.0.0.1:3737/api/toggle`
3. **Test provider**: `audetic provider test` (validates transcription setup)
4. **Test recording**: Press your configured keybind
5. **Check logs**: `make logs` or `journalctl --user -u audetic.service -f`

## Troubleshooting

### Service fails to start
- Check logs: `make logs` or `journalctl --user -u audetic.service -e`
- Check status: `make status`
- Verify binary path: `which audetic`
- Test config: `audetic --verbose`

### Recording doesn't work
- Check microphone permissions
- Verify audio device: `arecord -l`
- Ensure the desired input device is set as the system default (Audetic uses whatever CPAL reports as default)

### Text injection fails
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

Audetic now includes two parallel update paths:

1. **Background auto-updater**: runs inside the daemon, checks `https://install.audetic.ai/cli/version` every few hours, downloads new binaries into `~/.local/share/audetic/updates`, swaps them atomically, and restarts the service (unless `AUDETIC_DISABLE_AUTO_RESTART=1` is set). Auto-updates respect `~/.config/audetic/update_state.json` and can be disabled.

2. **Manual CLI control** via the built-in subcommand:

```bash
# Show current vs remote version without installing
audetic update --check

# Force an immediate install (even if versions appear equal)
audetic update --force

# Switch channels for subsequent checks
audetic update --channel beta

# Toggle background updates
audetic update --disable
audetic update --enable
```

You can also rerun the installer at any time to jump to a specific channel or repair a broken install:

```bash
curl -fsSL https://install.audetic.ai/cli/latest.sh | bash -s -- --channel beta
```

## Uninstalling

Remove Audetic with the dedicated uninstall script:

```bash
curl -fsSL https://install.audetic.ai/cli/uninstall.sh | bash
```

### Uninstall options

```bash
# Preview what will be removed (no changes made)
curl -fsSL https://install.audetic.ai/cli/uninstall.sh | bash -s -- --dry-run

# Skip confirmation prompt
curl -fsSL https://install.audetic.ai/cli/uninstall.sh | bash -s -- --yes

# Keep your config and transcription history
curl -fsSL https://install.audetic.ai/cli/uninstall.sh | bash -s -- --keep-config --keep-database

# Also remove temp audio files from /tmp
curl -fsSL https://install.audetic.ai/cli/uninstall.sh | bash -s -- --remove-temp
```

### What gets removed

By default, the uninstaller removes:
- `~/.local/share/audetic/bin/` (binary + `audetic-*.bak` files from auto-updates)
- `~/.config/systemd/user/audetic.service` (systemd unit)
- `~/.config/audetic/` (config and update state)
- `~/.local/share/audetic/audetic.db*` (transcription history)
- `~/.local/share/audetic/updates/`, `update.lock` (auto-update cache)
- `~/.local/share/audetic/meetings/`, `keybind-backups/`

Use `--keep-config`, `--keep-database`, or `--keep-updates` to preserve specific artifacts.