---
name: audetic-feature-planning
description: Plan new features in the Audetic codebase using the project's library-first, orthogonal layering. Use BEFORE writing any non-trivial code or producing an implementation plan when the work touches the daemon, the HTTP API, the CLI, or the web UI. Triggers - "plan a feature", "add X to Audetic", "implement Y", "let's design", "how should I structure", any request that would touch more than one of db / library / api / cli / web-ui. Skip for typo fixes, single-file refactors with no transport touch, or pure CSS/layout tweaks in the web UI.
---

# Audetic feature planning — library-first, orthogonal layering

This project ships ONE daemon binary that exposes its functionality through multiple consumers (HTTP API, CLI, web UI). Features must be designed so each layer is independently usable and testable. Skipping or inverting layers leads to coupled code that one consumer can use but the others cannot.

## The layering — design in this order

When planning, walk these layers top-to-bottom. Each layer depends only on the ones above it.

| # | Layer | Path | Knows about |
|---|---|---|---|
| 1 | DB / repository | `crates/audetic/src/db/<table>.rs` | SQLite only |
| 2 | Domain library | `crates/audetic/src/<domain>/` | DB, pure logic |
| 3 | Transcription/service traits | inside domain module | Library only |
| 4 | HTTP route + utoipa | `crates/audetic/src/api/routes/<domain>.rs` | Library |
| 5 | OpenAPI registration | `crates/audetic/src/api/docs.rs` (`ApiDoc`) | Route handlers |
| 6 | URL constants | `crates/audetic/src/api/url.rs` (`paths::*`) | Path strings only |
| 7 | Web UI types (generated) | `apps/web-ui/src/api/schema.ts` (`bun run codegen`) | OpenAPI spec |
| 8 | Web UI consumer | `apps/web-ui/src/stores/*` + `routes/*` | `daemon` client + `schema.ts` |
| 9 | CLI consumer | `crates/audetic/src/cli/<domain>.rs` | Library directly OR `api_url(paths::*)` — see "When CLI uses HTTP vs library" below |

**Library code at layer 2 must not import `axum::`, `clap::`, or `reqwest::`.** If it does, the boundary leaked.

## Hard rules (these break tests or break consumers)

1. **No inlined daemon URLs.** Never hardcode `http://127.0.0.1:3737/api/...`. Use `api_url(paths::FOO)` in Rust, the `daemon` client in TypeScript. The dev vite proxy and prod same-origin only work if the web UI uses the typed client.

2. **Library first; HTTP only when the daemon owns the runtime state.** The CLI calls library functions directly for durable data, filesystem, and config; it goes through HTTP (`api_url(paths::*)`) only to coordinate with daemon-owned runtime state. See *When CLI uses HTTP vs library* for the decision.

3. **Never import a state machine into the CLI.** Whether the CLI uses the library directly or HTTP, it must NOT import `MeetingMachine`, `RecordingMachine`, or any `*StatusHandle`. State machines and shared handles are daemon-internal — touching them from a separate process would create two owners of the same in-memory state.

4. **OpenAPI is the source of truth for web UI types.** Every new route needs `#[utoipa::path(...)]` plus registration in `api/docs.rs` (`ApiDoc::paths` and `components`). After changes, run `bun run codegen` from `apps/web-ui/` (or `make codegen` if available) and commit `schema.ts`.

5. **Well-known endpoints get a `paths::` constant.** Any path referenced from more than one place (CLI + web UI, install scripts, keybind installer) needs an entry in `api::url::paths`. Tests in `api/url.rs` enforce that every `paths::*` constant resolves to a real OpenAPI operation — drift fails CI.

6. **Repository methods don't know about HTTP.** SQL stays in `db/`. Route handlers call repository methods; they do not write SQL inline.

7. **Domain modules don't know about CLI flags.** Argument parsing belongs in `cli/args.rs`. Library functions take typed structs, not `clap::ArgMatches`.

## When CLI uses HTTP vs library

| Pattern | Example commands | Trigger |
|---|---|---|
| **HTTP** (`reqwest` + `api_url(paths::*)`) | `meeting`, `post-processing` | Operation reads/mutates state the daemon owns at runtime (active recording, active meeting, jobs the daemon is watching, anything tracked by a `*StatusHandle`). The daemon must be running. |
| **Library direct** (`use crate::history;` etc.) | `history`, `install`, `keybind`, `logs`, `provider`, `update`, `compression` | Operation is durable storage, filesystem, config, or one-shot setup. Works whether or not the daemon is running. |
| **External API** | `transcribe` | Talks to a third-party service (e.g. Audetic Cloud). Neither library nor daemon HTTP. |

When planning a new CLI command, pick based on whether it would conflict with — or be invisible to — the running daemon. If in doubt: starting/stopping/inspecting active work → HTTP. Reading durable data or editing config → library.

## Planning checklist — include in every feature plan

For each feature, the plan should explicitly answer:

- [ ] **DB**: New table or column? Migration written? Repository method (signature)?
- [ ] **Library**: Domain function or method — where does it live? What does it take/return?
- [ ] **HTTP route**: Method, path, request/response types (all `#[derive(ToSchema)]`), `#[utoipa::path]` annotation, error mapping via `api::error`.
- [ ] **OpenAPI registration**: Add handler to `ApiDoc::paths(...)`, add new types to `components(schemas(...))`.
- [ ] **`paths::` constant**: Add to `api::url::paths` IF any other code (CLI, install, web UI components that build URLs by hand, keybind) needs to reference it.
- [ ] **Codegen**: `bun run codegen` in `apps/web-ui/` to refresh `schema.ts`.
- [ ] **Web UI**: Which store owns the state? Which route/component renders it? Calls go through `daemon` from `api/client.ts`.
- [ ] **CLI**: Subcommand wiring in `cli/args.rs` + handler in `cli/<domain>.rs`. Pick transport based on "When CLI uses HTTP vs library" — library direct for durable/config operations, HTTP for daemon-runtime state.
- [ ] **Tests**: Unit tests at the library layer; route-level tests where useful; verify `cargo test` passes the `api::url::tests::*` invariants.
- [ ] **Verification**: How to test end-to-end — `make build && make test`, manual flow through CLI, manual flow through web UI (chrome-devtools MCP if applicable).

If a checkbox doesn't apply, say so explicitly in the plan with one sentence ("No new DB schema — uses existing `meetings` table"). Silent skips hide coupling.

## Orthogonality red flags — call these out in plan review

- Domain module under `crates/audetic/src/<domain>/` imports `axum::` or `reqwest::` → push HTTP concerns out to `api/routes/`.
- Route handler contains a SQL string or `rusqlite::` call → push down into `db/`.
- Route handler body exceeds ~20 lines of business logic → extract into a library function.
- Web UI does `fetch("/api/...")` or `fetch("http://127.0.0.1:3737/...")` → use `daemon.GET("/path", ...)` from `api/client.ts`.
- CLI module imports a state handle (`MeetingStatusHandle`, `RecordingStatusHandle`) or domain machine (`MeetingMachine`, `RecordingMachine`) → wrong regardless of transport (rule 3); reach active state through HTTP instead.
- CLI command duplicates logic that lives in a library module (re-implements SQL, re-parses config) → call the library function, even if the daemon's HTTP route also calls it.
- Two consumers (CLI + web UI) compute the same URL with different string concatenation → add a `paths::` constant or a helper like `post_processing_job_path(id)`.
- A new endpoint exists but `schema.ts` wasn't regenerated → run `bun run codegen` before claiming the web UI is wired up.

## When it's OK to skip layers

- **Pure web UI work** (component restyling, layout, copy): layers 1–6 untouched.
- **Internal refactor inside one library module**: layers 4–9 untouched if the public function signature didn't change.
- **CLI-local concern** (e.g. output formatting, `--json` flag handling that just transforms an existing response): no new route needed.
- **One-shot script** in `scripts/` that hits an existing endpoint: no new layer work, just call `api_url(paths::*)`.

## Concrete reference points

Use these as templates when designing a new feature. Read whichever is closest before writing the plan.

- **CLI uses HTTP** (daemon-runtime state): `post_processing/` module → `api/routes/post_processing.rs` → `cli/post_processing.rs` shows the `reqwest` + `api_url(paths::*)` pattern.
- **CLI uses library directly** (durable data): `history/` module → `cli/history.rs` reads SQLite via `crate::history::*` without touching the daemon.
- **Multi-pipeline domain with state machine**: `meeting/` module → `api/routes/meetings.rs` → `cli/meeting.rs` (HTTP, because state machine) → `apps/web-ui/src/stores/meeting-store.ts` and `routes/meetings.tsx`.
- **Well-known path with parameterization**: `api::url::post_processing_job_path(id)` shows how to build a `paths::X/{id}/sub` helper alongside the base constant.

## Verification before declaring a plan complete

A finished plan should let someone (or future-you) implement without re-deciding. Before handing off:

1. Every checklist item above is answered or explicitly N/A.
2. File paths are concrete (`crates/audetic/src/api/routes/foo.rs`, not "the routes file").
3. The reused functions are named (e.g., `MeetingRepository::update_title`, not "the repository").
4. Test plan covers: `cargo test` passes (especially `api::url::tests`), `bun run typecheck` in web UI passes, and a manual end-to-end walkthrough is described for at least one consumer.
