<img src="./assets/banner.png" alt="Audetic" />
Basically superwhisper for Omarchy, Audetic is a voice to text application for Wayland/Hyprland. Press a keybind to toggle recording, get automatic transcription and inject text into the focused application/clipboard...

## Quickstart Video

[![Audetic Quickstart](https://img.youtube.com/vi/8gQLqz_mosI/hqdefault.jpg)](https://youtu.be/8gQLqz_mosI)

- **[View Documentation](./docs/index.md)** - Detailed guides and configuration

## Quick Install (Recommended)

Audetic ships pre-built, signed binaries for Linux and macOS.

```bash
curl -fsSL https://install.audetic.ai/cli/latest.sh | bash
```

The installer detects your platform and hands off to `audetic install`.
Everything lives under `$HOME` — no sudo.

### Linux

Copies the binary to `~/.local/share/audetic/bin/`, installs a systemd
**user** service at `~/.config/systemd/user/audetic.service`,
`enable --now`s it, waits for it to bind `127.0.0.1:3737`, and opens the
web UI in your browser. Pass `--no-launch` to skip opening the browser.

### macOS

Unpacks the signed and notarized `Audetic.app` to `~/Applications/`,
drops a LaunchAgent plist at
`~/Library/LaunchAgents/ai.audetic.daemon.plist`, `launchctl bootstrap`s
it, waits for `127.0.0.1:3737`, and opens the web UI.

Two permission prompts on first run:

- **Microphone** — needed for voice-to-text and meeting mic capture.
  Fires automatically the first time the daemon opens the mic.
- **Screen Recording** (called *Screen & System Audio Recording* on
  macOS 15+) — needed for meeting *system* audio capture. The daemon
  triggers this prompt on first launch via `CGRequestScreenCaptureAccess`.
  After you click Allow, the daemon **auto-restarts** to pick up the
  fresh permission (launchd's `KeepAlive` does the heavy lifting); no
  manual restart needed.

If you ever need to revoke or reset permissions, open **System
Settings → Privacy & Security**, find Audetic, and toggle as needed.
To force a fresh prompt:

```bash
tccutil reset Microphone ai.audetic.daemon
tccutil reset ScreenCapture ai.audetic.daemon
launchctl kickstart -k gui/$(id -u)/ai.audetic.daemon
```

System audio capture requires macOS **14.6 or later** (Core Audio Tap
API). On older versions, meetings fall back to mic-only.

**After installation:**

1. Finish provider and ffmpeg setup in the web UI the installer opened (or visit `http://127.0.0.1:3737/`).
2. Add a keybind:
   - Hyprland: `bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/api/toggle`
   - macOS: System Settings → Keyboard → Keyboard Shortcuts → Services / Shortcuts.app calling the same `curl` command.
3. Press the keybind to start/stop recording!

## Web UI

The daemon serves a web UI at `http://127.0.0.1:3737/` for onboarding, provider
configuration, and browsing transcription history. The HTTP API lives under
`http://127.0.0.1:3737/api/*` (e.g. `POST /api/toggle`, `GET /api/status`).

## Configuration

Default config at `~/.config/audetic/config.toml`. See [Configuration Guide](./docs/configuration.md) for details.

### Provider CLI

Audetic ships an interactive helper so you can switch transcription providers without editing TOML by hand:

```bash
audetic provider show        # inspect current provider (secrets masked)
audetic provider configure   # interactive wizard (requires a TTY)
audetic provider test        # validate the stored provider
```

## Transcribe Media Files

Transcribe audio or video files using the audetic cloud transcription service:

```bash
# Basic transcription (output to stdout)
audetic transcribe recording.mp4

# Specify language and output file
audetic transcribe meeting.mkv -l en -o meeting.txt

# JSON output with timestamps
audetic transcribe podcast.mp3 -f json --timestamps -o podcast.json

# SRT subtitle format
audetic transcribe video.mp4 -f srt -o subtitles.srt

# Copy result to clipboard
audetic transcribe voice-memo.m4a --copy

# Use custom API endpoint
audetic transcribe audio.wav --api-url http://localhost:3141/api/v1/jobs
```

**Supported formats:**

- Audio: wav, mp3, m4a, flac, ogg, opus
- Video: mp4, mkv, webm, avi, mov

Files are automatically compressed to MP3 before upload for efficient transfer.
Files already in MP3 or Opus format are sent as-is. Use `--no-compress` to skip.

**Options:**

- `-l, --language <LANG>` - Language code (e.g., 'en', 'es', or 'auto' for detection)
- `-o, --output <FILE>` - Write transcription to file (default: stdout)
- `-f, --format <FORMAT>` - Output format: text (default), json, srt
- `--timestamps` - Include timestamps in text output
- `--no-progress` - Disable progress indicator
- `-c, --copy` - Copy result to clipboard
- `--no-compress` - Skip compression (send file in original format)
- `--api-url <URL>` - Override transcription API URL

## Updates

Audetic includes an auto-updater plus manual controls:

```bash
audetic update
```

## Uninstall — macOS

```bash
launchctl bootout gui/$(id -u)/ai.audetic.daemon
rm -rf ~/Applications/Audetic.app
rm  ~/Library/LaunchAgents/ai.audetic.daemon.plist
rm -rf "~/Library/Application Support/audetic"
rm -rf ~/Library/Logs/Audetic
tccutil reset Microphone ai.audetic.daemon
tccutil reset ScreenCapture ai.audetic.daemon
```

## Uninstall

```bash
curl -fsSL https://install.audetic.ai/cli/uninstall.sh | bash
```

Use `--dry-run` to preview, or `--keep-database` to preserve transcription history. See [Installation Guide](./docs/installation.md#uninstalling) for all options.

## License

MIT
