# GitHub Copilot Instructions for Audetic

## Project Overview

**Audetic** is a voice-to-text transcription tool for Linux. It captures audio, transcribes it using various providers, and injects the text into applications.

**Key Features:**

- Multiple transcription providers (OpenAI, AssemblyAI, Whisper.cpp, Audetic API)
- System tray integration
- Configurable keybindings
- Text injection via ydotool/xdotool

## Tech Stack

- **Language**: Rust
- **Async Runtime**: Tokio
- **CLI Framework**: Clap
- **HTTP Server**: Axum
- **Database**: SQLite (via rusqlite)
- **Audio**: cpal for audio capture
- **Testing**: Rust standard testing

## Coding Guidelines

### Testing

- Run `make test` or `cargo test` before committing
- Use `cargo test test_name` for single tests

### Code Style

- Run `make fmt` (rustfmt) before committing
- Run `make lint` (clippy) before committing
- Follow existing patterns in `src/`

### Git Workflow

- Always commit `.beads/issues.jsonl` with code changes
- Run `bd sync` at end of work sessions

## Issue Tracking with bd

**CRITICAL**: This project uses **bd** for ALL task tracking. Do NOT create markdown TODO lists.

### Essential Commands

```bash
# Find work
bd ready --json                    # Unblocked issues
bd list --status open --json       # All open issues

# Create and manage
bd create "Title" -t bug|feature|task -p 0-4 --json
bd create "Subtask" --parent <epic-id> --json  # Hierarchical subtask
bd update <id> --status in_progress --json
bd close <id> --reason "Done" --json

# Sync (CRITICAL at end of session!)
bd sync  # Force immediate export/commit/push
```

### Workflow

1. **Check ready work**: `bd ready --json`
2. **Claim task**: `bd update <id> --status in_progress`
3. **Work on it**: Implement, test, document
4. **Discover new work?** `bd create "Found bug" -p 1 --deps discovered-from:<parent-id> --json`
5. **Complete**: `bd close <id> --reason "Done" --json`
6. **Sync**: `bd sync` (flushes changes to git immediately)

### Priorities

- `0` - Critical (security, data loss, broken builds)
- `1` - High (major features, important bugs)
- `2` - Medium (default, nice-to-have)
- `3` - Low (polish, optimization)
- `4` - Backlog (future ideas)

## Project Structure

```
Audetic/
├── src/
│   ├── api/           # HTTP API routes (axum)
│   ├── cli/           # CLI commands (clap)
│   ├── transcription/ # Transcription providers
│   ├── db/            # SQLite database operations
│   ├── audio/         # Audio capture and streaming
│   ├── config/        # Configuration handling
│   ├── keybind/       # Keybinding management
│   ├── text_io/       # Text injection
│   └── ui/            # System tray UI
├── release/           # Release artifacts
├── docs/              # Documentation
└── .beads/
    ├── beads.db       # SQLite database (DO NOT COMMIT)
    └── issues.jsonl   # Git-synced issue storage
```

## Build Commands

- `make build` - Debug build
- `make release` - Release build
- `make test` - Run all tests
- `make lint` - Run clippy
- `make fmt` - Format code
- `make check` - Quick compilation check

## CLI Help

Run `bd <command> --help` to see all available flags for any command.

## Important Rules

- Use bd for ALL task tracking
- Always use `--json` flag for programmatic use
- Run `bd sync` at end of sessions
- Do NOT create markdown TODO lists
- Do NOT commit `.beads/beads.db` (JSONL only)

---

**For detailed workflows, see [AGENTS.md](../AGENTS.md)**
