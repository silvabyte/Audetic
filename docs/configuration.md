# Audetic Configuration Guide

Audetic is configured via a single TOML file located at `~/.config/audetic/config.toml`. This guide covers everything you need to know about configuring Audetic for your needs.

## Quick Start

The minimal configuration to get started:

```toml
[whisper]
# Auto-detection (recommended) - Audetic will automatically choose the best provider
language = "en"

# For OpenAI API access, add your key to config:
# provider = "openai-api"
# api_key = "sk-your-api-key-here"
```

Audetic will create a default configuration file on first run if none exists.

## Complete Configuration Example

Here's a full configuration file with all available options:

```toml
[whisper]
provider = "openai-api"         # Transcription provider (see Providers section)
api_key = "sk-your-api-key-here" # API key for API providers
model = "whisper-1"             # Model name (provider-specific)
language = "en"                 # Language code (ISO 639-1)
command_path = "/usr/bin/whisper"  # Custom CLI tool path (optional)
model_path = "/path/to/model.bin"  # Custom model file path (optional)
api_endpoint = "https://api.openai.com/v1/audio/transcriptions"  # Custom API endpoint (optional)

[ui]
notification_color = "rgb(ff1744)"  # Hyprland notification color

[ui.waybar]
idle_text = "󰑊"                # Icon shown when idle (ready to record)
recording_text = "󰻃"           # Icon shown when recording
idle_tooltip = "Press Super+R to record"                    # Tooltip for idle state
recording_tooltip = "Recording... Press Super+R to stop"     # Tooltip for recording state

[wayland]
input_method = "wtype"          # Text injection method

[behavior]
auto_paste = true               # Automatically paste transcribed text
preserve_clipboard = false      # Keep clipboard content after pasting
delete_audio_files = true       # Delete temporary audio files after processing
audio_feedback = true           # Play audio feedback sounds
```

## Configuration Sections

### [whisper] - Transcription Settings

Configures speech-to-text transcription providers and models.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `provider` | string | auto-detect | Transcription provider: `"openai-api"`, `"openai-cli"`, `"whisper-cpp"`, or omit for auto-detection |
| `api_key` | string | none | API key for API-based providers (required for openai-api) |
| `model` | string | `"base"` | Model name (provider-specific, see Providers section) |
| `language` | string | `"en"` | Language code (ISO 639-1 format) |
| `command_path` | string | auto-detect | Custom path to whisper CLI tool (optional) |
| `model_path` | string | auto-detect | Custom path to model file (whisper.cpp only) |
| `api_endpoint` | string | OpenAI API | Custom API endpoint URL (API providers only) |

#### Providers

Audetic supports multiple transcription providers:

**OpenAI API** (`provider = "openai-api"`)
- **Best for:** High accuracy, no local setup
- **Requirements:** API key in config, internet connection  
- **Models:** `"whisper-1"` (only available model)
- **Cost:** ~$0.006 per minute of audio

**OpenAI Whisper CLI** (`provider = "openai-cli"`)
- **Best for:** Local processing, no API costs, privacy
- **Requirements:** `pip install openai-whisper`
- **Models:** `"tiny"`, `"base"`, `"small"`, `"medium"`, `"large-v3"`
- **Cost:** Free (local processing)

**whisper.cpp** (`provider = "whisper-cpp"`)
- **Best for:** Resource-constrained systems, CPU-only inference
- **Requirements:** Build from source or install via package manager
- **Models:** `"tiny"`, `"base"`, `"small"`, `"medium"`, `"large"`
- **Status:** Experimental
- **Cost:** Free (local processing)

**Auto-Detection** (omit `provider`)
- Audetic automatically selects the best available provider:
  1. OpenAI Whisper CLI (if installed)
  2. whisper.cpp (fallback)
- Note: API providers require explicit configuration with api_key

#### Language Codes

Common language codes (ISO 639-1):

| Code | Language | Code | Language | Code | Language |
|------|----------|------|----------|------|----------|
| `en` | English | `es` | Spanish | `fr` | French |
| `de` | German | `it` | Italian | `pt` | Portuguese |
| `ru` | Russian | `zh` | Chinese | `ja` | Japanese |
| `ko` | Korean | `ar` | Arabic | `auto` | Auto-detect* |

*Auto-detection only works with OpenAI API

For the complete list, see [ISO 639-1 codes](https://en.wikipedia.org/wiki/List_of_ISO_639-1_codes).

### [ui] - User Interface Settings

Controls visual indicators and desktop notifications.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `notification_color` | string | `"rgb(ff1744)"` | Hyprland notification color for `hyprctl notify` |

#### [ui.waybar] - Waybar Integration

Customize icons and tooltips for Waybar status display. See [Waybar Integration](./waybar-integration.md) for setup instructions.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `idle_text` | string | `"󰑊"` | Icon shown when idle (ready to record) - Nerd Font icon |
| `recording_text` | string | `"󰻃"` | Icon shown when actively recording - Nerd Font icon |
| `idle_tooltip` | string | `"Press Super+R to record"` | Tooltip text when hovering over idle state |
| `recording_tooltip` | string | `"Recording... Press Super+R to stop"` | Tooltip text when hovering during recording |

**Icon Tips:**
- Uses Nerd Font icons for consistency with other Waybar modules
- Icons inherit colors from your Waybar theme via CSS classes
- All styling is controlled by CSS, not inline styles
- Custom icons can be any Unicode character or Nerd Font glyph

### [wayland] - Wayland Integration

Configures integration with Wayland desktop environments.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `input_method` | string | `"wtype"` | Text injection method: `"wtype"`, `"clipboard"` |

**Text Injection Methods:**
- `"wtype"` - Direct text typing (fast, works in most apps)
- `"clipboard"` - Via clipboard (universal compatibility, slower)

### [behavior] - Application Behavior

Controls how Audetic handles transcribed text and temporary files.

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `auto_paste` | bool | `true` | Automatically paste/type transcribed text |
| `preserve_clipboard` | bool | `false` | Keep existing clipboard content when using clipboard injection |
| `delete_audio_files` | bool | `true` | Delete temporary audio recordings after processing |
| `audio_feedback` | bool | `true` | Play audio feedback sounds (start/stop recording) |

## Configuration File Location

Audetic looks for its configuration file at:

- **Linux:** `~/.config/audetic/config.toml`
- **macOS:** `~/Library/Application Support/audetic/config.toml`
- **Windows:** `%APPDATA%\audetic\config.toml`

## Environment Variables

Audetic respects these environment variables:

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Logging level (`error`, `warn`, `info`, `debug`, `trace`) |

## Common Configuration Scenarios

### For OpenAI API Users
```toml
[whisper]
provider = "openai-api"
api_key = "sk-your-api-key-here"  # Your OpenAI API key
model = "whisper-1"
language = "en"  # or "auto" for automatic detection
```

### For Local Processing (Privacy-Focused)
```toml
[whisper]
provider = "openai-cli"
model = "small"  # Good balance of speed and accuracy
language = "en"

# No API key needed - everything runs locally
```

### For Multiple Languages
```toml
[whisper]
provider = "openai-api"
model = "whisper-1" 
language = "auto"  # Automatically detect language

# Or set a specific language code like "es" for Spanish
```

### For Low-Resource Systems
```toml
[whisper]
provider = "openai-cli"
model = "tiny"       # Smallest, fastest model
language = "en"

[behavior]
delete_audio_files = true  # Clean up temp files
```

### For High Accuracy Transcription
```toml
[whisper]
provider = "openai-cli"
model = "large-v3"   # Most accurate model
language = "en"

[behavior]
audio_feedback = false  # Reduce distractions
```

## Migrating from Earlier Versions

If you're upgrading from an earlier version that used `use_api = true/false`, update your config:

**Old format:**
```toml
[whisper]
use_api = true
model = "whisper-1"
```

**New format:**
```toml
[whisper]
provider = "openai-api"
model = "whisper-1"
```

**Migration mapping:**
- `use_api = true` → `provider = "openai-api"`
- `use_api = false` → `provider = "openai-cli"` (or `"whisper-cpp"`)

## Troubleshooting Configuration

### Config File Issues

**"Failed to parse config file"**
- Check TOML syntax with an online validator
- Ensure strings are quoted: `language = "en"` not `language = en`
- Verify boolean values: `true`/`false` not `"true"`/`"false"`

**"Config file not found"**
- Audetic will create a default config on first run
- Manually create the config directory: `mkdir -p ~/.config/audetic`

### Provider Issues

**"No transcription provider available"**
- Install a provider: `pip install openai-whisper` 
- Or set OpenAI API key: `export OPENAI_API_KEY="sk-..."`
- Check provider installation: `whisper --help`

**"OPENAI_API_KEY environment variable required"**
- Set your API key: `export OPENAI_API_KEY="sk-your-key"`
- Get an API key from https://platform.openai.com/api-keys

### Audio Issues

**"No audio input detected"**
- Check `device = "default"` in config
- List devices: `arecord -l`
- Test audio: `arecord -f cd test.wav` (Ctrl+C to stop, `aplay test.wav` to playback)

### Validation

Test your configuration:
```bash
# Start Audetic with verbose logging
RUST_LOG=debug audetic

# Look for these log messages:
# "Loaded config from ..."
# "Using [Provider] for transcription"
```

For more troubleshooting, see the [Whisper Transcription Setup](./whisper-transcription-setup.md) guide.