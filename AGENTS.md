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
