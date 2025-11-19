# Audetic Installation Guide

Complete installation instructions for different operating systems and environments.

## Quick Install (Recommended)

Audetic now ships verified binaries for Linux and macOS. Install or reinstall the service with one command—no Rust toolchain, git clone, or manual builds required:

```bash
curl -fsSL https://install.audetic.ai/cli/latest.sh | bash
```

The installer:

- Detects your OS/architecture and selects the matching artifact.
- Verifies SHA-256 (and optional signatures) before extracting.
- Installs the `audetic` binary into `/usr/local/bin` (or a custom prefix).
- Drops the systemd user unit plus config scaffolding under `~/.config/audetic`.
- Seeds `update_state.json` so the built-in auto-updater can take over.
- Is idempotent—rerun anytime to repair, reinstall, or switch channels.

### Useful flags

```
latest.sh --prefix "$HOME/.local"   # install without sudo
latest.sh --system                  # install as a system-level service
latest.sh --channel beta            # jump to another release channel
latest.sh --clean                   # remove previous binaries/services before reinstalling
latest.sh --dry-run                 # fetch & verify artifacts without touching the system
latest.sh --uninstall [--clean]     # remove Audetic (optionally purge config/cache)
```

After install:
1. The installer automatically enables/starts the systemd **user** service (unless `--no-start` was set). Use `systemctl --user status audetic.service` to confirm.
2. Add a keybind in Hyprland (or your compositor) that calls `curl -X POST http://127.0.0.1:3737/toggle`.
3. Edit `~/.config/audetic/config.toml` if you need custom providers, models, or behavior tweaks.

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

# Install binary
sudo cp target/release/audetic /usr/local/bin/
sudo chmod +x /usr/local/bin/audetic
```

## Configuration

Create the configuration directory and file:

```bash
mkdir -p ~/.config/audetic
```

Audetic will create a default config on first run, or you can create one manually:

### For Optimized Whisper.cpp

```toml
[whisper]
model = "large-v3-turbo"
language = "en"
command_path = "/home/user/.local/share/audetic/whisper/build/bin/whisper-cli"
model_path = "/home/user/.local/share/audetic/whisper/models/ggml-large-v3-turbo-q5_1.bin"

[ui]
notification_color = "rgb(ff1744)"

[wayland]
input_method = "ydotool"

[behavior]
auto_paste = true
preserve_clipboard = false
delete_audio_files = true
audio_feedback = true
```

### For OpenAI Whisper

```toml
[whisper]
model = "base"
language = "en"
# command_path is auto-detected if whisper is in PATH

[ui]
notification_color = "rgb(ff1744)"

[wayland]
input_method = "ydotool"

[behavior]
auto_paste = true
preserve_clipboard = false
delete_audio_files = true
audio_feedback = true
```

## Systemd Service Setup

Create a user service for automatic startup:

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
ExecStart=/usr/local/bin/audetic
Restart=always
RestartSec=5
Environment="RUST_LOG=info"
MemoryLimit=6G
CPUQuota=80%

[Install]
WantedBy=default.target
```

Enable and start the service:

```bash
systemctl --user daemon-reload
systemctl --user enable --now audetic.service
```

## Hyprland Integration

Add to your Hyprland config (`~/.config/hypr/hyprland.conf`):

```
bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle
```

For Omarchy users:
```
bindd = SUPER, R, Audetic, exec, $terminal -e curl -X POST http://127.0.0.1:3737/toggle
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
input_method = "ydotool"
```

### 3. Create GNOME Keyboard Shortcut

1. Open GNOME Settings
2. Go to Keyboard → Keyboard Shortcuts → View and Customize Shortcuts
3. Go to Custom Shortcuts
4. Add new shortcut with command: `curl -X POST http://127.0.0.1:3737/toggle`
5. Set your preferred key combination (e.g., Super+R)

## Testing Installation

1. **Test service**: `systemctl --user status audetic.service`
2. **Test API**: `curl -X POST http://127.0.0.1:3737/toggle`
3. **Test recording**: Press your configured keybind
4. **Check logs**: `make logs`

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
- Adjust `MemoryLimit` in service file
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

Because `latest.sh` is idempotent, you can also rerun it at any time to jump to a specific channel or repair a broken install:

```bash
curl -fsSL https://install.audetic.ai/cli/latest.sh | bash -s -- --channel beta --clean
```

## Uninstalling

The installer doubles as the uninstaller:

```bash
curl -fsSL https://install.audetic.ai/cli/latest.sh | bash -s -- --uninstall
```

Add `--clean` if you also want to purge `~/.config/audetic` and caches under `~/.local/share/audetic`.

Manual teardown remains the same as before (stop the systemd service, delete the binary, remove config directories) if you prefer to handle it yourself.