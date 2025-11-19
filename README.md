<div align="center">
  <img src="./assets/banner.png" alt="Audetic" />

  <br/>
  <br/>

  Voice to Text application for Wayland/Hyprland. Press a keybind to toggle recording, get automatic transcription via Whisper, and inject text into the focused application... Basically superwhisper for Omarchy.
</div>

<br/>

**[View Documentation](./docs/index.md)** - Detailed guides and configuration

## Quick Install (Recommended)

Audetic ships pre-built, signed binaries. Install or repair the service—no Rust or git required:

```bash
curl -fsSL https://install.audetic.ai/cli/latest.sh | bash
```

Highlights:
- Detects OS/arch, downloads the matching artifact, and verifies SHA-256 (plus optional signature).
- Installs `audetic` into `/usr/local/bin` (or a custom `--prefix`).
- Sets up the user systemd service + default config under `~/.config/audetic/`.
- Idempotent: rerun anytime with flags like `--clean`, `--channel beta`, `--dry-run`, or `--uninstall`.

**After installation:**

1. Confirm the service: `systemctl --user status audetic.service`
2. Add a keybind in Hyprland (or your compositor): `bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle`
3. Press the keybind to start/stop recording!

## Features

- 🎤 Keybind-activated voice recording
- 🔴 Visual recording indicators  
- 🎯 Automatic text injection into focused apps
- 📋 Intelligent clipboard fallback
- ⚡ Optimized for Wayland/Hyprland
- 🔧 Configurable via TOML

## Manual Installation

Need to hack on Audetic or build for an unsupported platform? Follow the [Installation Guide](./docs/installation.md) for the full toolchain-based workflow (Rust, Whisper builds, etc.).

## Configuration

Default config at `~/.config/audetic/config.toml`. See [Configuration Guide](./docs/configuration.md) for details.

## Development

Audetic uses a Makefile for common tasks:

```bash
make build      # Build debug binary
make release    # Build optimized release
make test       # Run tests
make lint       # Run clippy linter
make fmt        # Check formatting
make fix        # Fix formatting and simple issues

make start      # Enable and start service
make logs       # Show service logs
make restart    # Restart service
make status     # Check service status
make clean      # Clean build artifacts
```

## Troubleshooting

- **Recording issues**: Check [Configuration Guide](./docs/configuration.md)
- **Text injection fails**: See [Text Injection Setup](./docs/text-injection-setup.md)
- **Service problems**: View logs with `make logs`

## Updates

Audetic includes an auto-updater plus manual controls:

```bash
audetic update --check        # Compare installed vs remote without installing
audetic update --force        # Force install immediately
audetic update --channel beta # Switch release channels
audetic update --disable      # Turn off background updates
audetic update --enable       # Re-enable background updates
```

The daemon periodically polls `https://install.audetic.ai/cli/version`, downloads new binaries into `~/.local/share/audetic/updates`, verifies checksums, and swaps them atomically. Set `AUDETIC_DISABLE_AUTO_UPDATE=1` to opt out.

## License

MIT
