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

## Agent skills

### Issue tracker

Issues and PRDs are tracked as Fizzy cards on the Audetic board; external PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Triage uses Fizzy tags with the canonical default role strings: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Domain docs use a multi-context layout with root `CONTEXT-MAP.md` pointing to context-specific `CONTEXT.md` files. See `docs/agents/domain.md`.

## Issue Tracking with Fizzy

This project uses **Fizzy** for issue tracking. Do not use beads, markdown TODO files, or GitHub Issues unless the user explicitly asks.

### Board

- Account: `6100722`
- Board: `Audetic`
- Board ID: `03ge61jjq6tmkyjv39tu6eom3`
- URL: `https://app.fizzy.do/6100722/boards/03ge61jjq6tmkyjv39tu6eom3`

### Common Commands

```bash
fizzy card list --account 6100722 --board 03ge61jjq6tmkyjv39tu6eom3 --all
fizzy card show <number> --account 6100722
fizzy card create --account 6100722 --board 03ge61jjq6tmkyjv39tu6eom3 --title "Title" --description "<p>Description</p>"
fizzy card close <number> --account 6100722
```

### Workflow for AI Agents

1. Check existing work with `fizzy card list`.
2. Create new work as Fizzy cards on the Audetic board.
3. Use card numbers, not internal card IDs, for card commands.
4. Track triage state with Fizzy tags matching `docs/agents/triage-labels.md`.
5. Add implementation notes as card comments.
6. Close cards when work is complete.

### Important Rules

- Use Fizzy cards for project work tracking.
- Use the `--account 6100722` flag unless `FIZZY_ACCOUNT` is configured.
- Use the Audetic board ID for new project cards.
- Do not create `.scratch/` issue files for project work.
- Do not duplicate issues in beads or GitHub Issues.
- If the `fizzy` shim is unavailable, do not modify global `mise` config without asking the user first.

### Managing AI-Generated Planning Documents

AI assistants often create planning and design documents during development:

- PLAN.md, IMPLEMENTATION.md, ARCHITECTURE.md
- DESIGN.md, CODEBASE_SUMMARY.md, INTEGRATION_PLAN.md
- TESTING_GUIDE.md, TECHNICAL_DESIGN.md, and similar files

**Best Practice: Use a dedicated directory for these ephemeral files**

**Recommended approach:**

- Create a `history/` directory in the project root
- Store ALL AI-generated planning/design docs in `history/`
- Keep the repository root clean and focused on permanent project files
- Only access `history/` when explicitly asked to review past planning

**Example .gitignore entry (optional):**

```
# AI planning documents (ephemeral)
history/
```

**Benefits:**

- Clean repository root
- Clear separation between ephemeral and permanent documentation
- Easy to exclude from version control if desired
- Preserves planning history for archeological research
- Reduces noise when browsing the project
