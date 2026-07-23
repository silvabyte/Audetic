# Install Audetic on macOS

Audetic is built from source on macOS. It's three commands once the
prerequisites are in place. No installer script, no agent, no sudo.

> **Apple Silicon only.** These steps assume an arm64 Mac (M1 or later).

## 1. One-time prerequisites

You only do this once per machine.

```bash
# Full Xcode (NOT just Command Line Tools — the menu-bar app needs the
# SwiftUI macro plugins that only ship with Xcode). Install from the App
# Store, then point the toolchain at it:
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
sudo xcodebuild -license accept

# Build + runtime deps
brew install cmake ffmpeg
```

- `cmake` — required, or the `whisper-rs-sys` build aborts with
  *"is cmake not installed?"*.
- `ffmpeg` — required at runtime for audio.
- Full Xcode — required for the SwiftUI menu-bar app. With only the
  Command Line Tools you'll hit
  *"external macro implementation type 'SwiftUIMacros.EntryMacro' could not
  be found"*.

Sanity check: `command -v swift` should resolve and `xcode-select -p`
should print a path inside `/Applications/Xcode.app`.

## 2. Build and install

From the repo root:

```bash
make install
```

**Run this as yourself — never with `sudo`.** It's a per-user LaunchAgent;
`sudo` installs into the wrong domain and leaves root-owned files that block
the daemon from starting (see [Troubleshooting](#troubleshooting)).

`make install` compiles the daemon (release), builds the SwiftUI menu-bar app,
embeds it inside `target/release/Audetic.app` as a login item, ad-hoc signs the
bundle, then hands off to `audeticd install` — which copies the bundle to
`~/Applications/`, puts the `audetic` CLI in `~/.local/bin/`, writes the
LaunchAgents, starts the daemon on `127.0.0.1:3737`, and opens the web UI.

That's it. To confirm it's up:

```bash
make status
```

## Updating later

Same command — it's idempotent:

```bash
git pull && make install
```

The bundle is ad-hoc signed (tied to its cdhash), so macOS may re-prompt for
Microphone / Screen Recording after a rebuild. There is no auto-updater, so a
rebuild is never clobbered by a remote release.

## Permissions

Two prompts appear the first time the daemon needs them:

- **Microphone** — voice-to-text and meeting mic capture. Fires the first
  time the daemon opens the mic.
- **Screen Recording** (*Screen & System Audio Recording* on macOS 15+) —
  meeting *system* audio. The daemon auto-restarts after you click Allow, so
  no manual restart is needed.

To force fresh prompts:

```bash
tccutil reset Microphone ai.audetic.daemon
tccutil reset ScreenCapture ai.audetic.daemon
launchctl kickstart -k gui/$(id -u)/ai.audetic.daemon
```

System audio capture needs macOS **14.6+** (Core Audio Tap API); older
versions fall back to mic-only.

## Local transcription model (optional)

To transcribe on-device instead of via the cloud, download a model and point
the config at it:

```bash
audetic models download parakeet-tdt-0.6b-v3
```

Then set in `~/Library/Application Support/audetic/config.toml`:

```toml
[whisper]
provider = "local"
model = "parakeet-tdt-0.6b-v3"
```

…and reload: `launchctl kickstart -k gui/$(id -u)/ai.audetic.daemon`.

> Model downloads come from `huggingface.co`. If that host is blocked on your
> network, fetch `parakeet-tdt-0.6b-v3` out of band and drop the files into
> `~/Library/Application Support/audetic/models/parakeet-tdt-0.6b-v3/`
> (`hf-mirror.com` works as a mirror). The daemon accepts manually-placed
> files as long as each is present at ≥90% of its expected size.

## Troubleshooting

**`swift toolchain not found` / `SwiftUIMacros.EntryMacro` not found**
You're on the Command Line Tools toolchain. Install full Xcode and run
`sudo xcode-select -s /Applications/Xcode.app/Contents/Developer`
(step 1), then rebuild.

**`Bootstrap failed: 5: Input/output error`**
Usually a stale launchd registration. Clear it and reinstall:

```bash
launchctl bootout gui/$(id -u)/ai.audetic.daemon 2>/dev/null
make install
```

**`Bootstrap failed: 125: Domain does not support specified action`**
You ran `install` with `sudo`. Don't. If you already did, reclaim ownership
of the files it created as root, then reinstall as yourself:

```bash
sudo chown -R "$(id -un):staff" \
  ~/Applications/Audetic.app \
  ~/.local/bin/audetic \
  ~/Library/LaunchAgents/ai.audetic.daemon.plist
make install
```

**Check what's running**

```bash
pgrep -lf 'audeticd|AudeticMenuBar'        # processes
lsof -nP -iTCP:3737 -sTCP:LISTEN           # daemon listening?
tail -f ~/Library/Logs/Audetic/audetic.log # logs
```

## Uninstall

```bash
make uninstall
```

Boots out both LaunchAgents, then removes `~/Applications/Audetic.app`, the two
plists, `~/Library/Logs/Audetic/`, the `audetic` CLI, and the state under
`~/Library/Application Support/audetic`. It prints the plan and asks before
touching anything — `make uninstall ARGS="--dry-run"` to just look, or
`ARGS="--keep-database"` to keep your transcription history.

It works even from a fresh clone: teardown runs from the *installed* daemon
when there is one.

TCC grants are left alone on purpose — resetting them would also clear
permissions for any other build of Audetic on the machine. To clear them
yourself:

```bash
tccutil reset Microphone ai.audetic.daemon
tccutil reset ScreenCapture ai.audetic.daemon
```
