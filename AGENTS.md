# AGENTS.md

## Build/Test/Lint Commands

- `make build` - Debug build
- `make release` - Release build
- `make test` - Run all tests (`cargo test`)
- `cargo test test_name` - Run a single test by name
- `make lint` - Run clippy with warnings as errors
- `make fmt` - Format code with rustfmt
- `make check` - Quick compilation check

## Code Style

- **Imports**: External crates first, then std, then `crate::`, then `super::`
- **Naming**: snake_case for modules/functions, PascalCase for types/enums
- **Error handling**: Use `anyhow::Result` with `.context("message")?` for context
- **Async**: tokio runtime, async/await throughout
- **Logging**: Use `tracing` macros (`info!`, `debug!`, `error!`)

## Pre-commit Checks (run before committing)

1. `cargo fmt -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`

## Project Structure

- `src/api/` - HTTP API routes (axum)
- `src/cli/` - CLI commands (clap)
- `src/transcription/` - Transcription providers
- `src/db/` - SQLite database operations

## bd - Dependency-Aware Issue Tracker

Issues chained together like beads.

GETTING STARTED
  bd init   Initialize bd in your project
            Creates .beads/ directory with project-specific database
            Auto-detects prefix from directory name (e.g., myapp-1, myapp-2)

  bd init --prefix api   Initialize with custom prefix
            Issues will be named: api-1, api-2, ...

CREATING ISSUES
  bd create "Fix login bug"
  bd create "Add auth" -p 0 -t feature
  bd create "Write tests" -d "Unit tests for auth" --assignee alice

VIEWING ISSUES
  bd list       List all issues
  bd list --status open  List by status
  bd list --priority 0  List by priority (0-4, 0=highest)
  bd show bd-1       Show issue details

MANAGING DEPENDENCIES
  bd dep add bd-1 bd-2     Add dependency (bd-2 blocks bd-1)
  bd dep tree bd-1  Visualize dependency tree
  bd dep cycles      Detect circular dependencies

DEPENDENCY TYPES
  blocks  Task B must complete before task A
  related  Soft connection, doesn't block progress
  parent-child  Epic/subtask hierarchical relationship
  discovered-from  Auto-created when AI discovers related work

READY WORK
  bd ready       Show issues ready to work on
            Ready = status is 'open' AND no blocking dependencies
            Perfect for agents to claim next work!

UPDATING ISSUES
  bd update bd-1 --status in_progress
  bd update bd-1 --priority 0
  bd update bd-1 --assignee bob

CLOSING ISSUES
  bd close bd-1
  bd close bd-2 bd-3 --reason "Fixed in PR #42"

AGENT INTEGRATION
  bd is designed for AI-supervised workflows:
    • Agents create issues when discovering new work
    • bd ready shows unblocked work ready to claim
    • Use --json flags for programmatic parsing
    • Dependencies prevent agents from duplicating effort
