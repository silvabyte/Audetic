<img src="./assets/banner.png" alt="Audetic" />
Voice to Text application for Wayland/Hyprland. Press a keybind to toggle recording, get automatic transcription via Whisper, and inject text into the focused application... Basically superwhisper for Omarchy.

**[View Documentation](./docs/index.md)** - Detailed guides and configuration

## Quick Install (Recommended)

Audetic ships pre-built, signed binaries.

```bash
curl -fsSL https://install.audetic.ai/cli/latest.sh | bash
```

**After installation:**

1. Confirm the service: `audetic` - streams the logs
2. Add a keybind in Hyprland (or your compositor): `bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle`
3. Press the keybind to start/stop recording!

## Configuration

Default config at `~/.config/audetic/config.toml`. See [Configuration Guide](./docs/configuration.md) for details.

## Updates

Audetic includes an auto-updater plus manual controls:

```bash
audetic update
```

## License

MIT
