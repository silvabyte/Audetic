---
name: audetic-feature-planning
description: Plan new features in the Audetic codebase using the project's orthogonal layering across its workspace crates (shared core, daemon, CLI) and the web UI. Use BEFORE writing any non-trivial code or producing an implementation plan when the work touches the daemon, the HTTP API, the CLI, or the web UI. Triggers - "plan a feature", "add X to Audetic", "implement Y", "let's design", "how should I structure", any request that would touch more than one of db / core / api / cli / web-ui. Skip for typo fixes, single-file refactors with no transport touch, or pure CSS/layout tweaks in the web UI.
---

# Audetic feature planning — orthogonal layering across the workspace

Audetic is a Cargo workspace with three crates plus a web UI:

- **`audetic-core`** (`crates/audetic-core/`) — shared, daemon-independent library: config, compression, clipboard, ffmpeg, `jobs_client`, and the URL surface (`url.rs`). Both other crates depend on it.
- **`audetic`** (`crates/audetic/`) — the daemon. Owns all runtime state and the SQLite database, exposes everything over an HTTP API, and serves the bundled web UI.
- **`audetic-cli`** (`crates/audetic-cli/`) — the CLI binary. A *consumer* of the daemon. It cannot import the daemon crate, so it reaches daemon-owned data and state over HTTP and shares pure logic via `audetic-core`.

The daemon owns the runtime state and the database; the CLI and web UI are independent consumers that reach it over HTTP. Design features so each consumer stays independently usable and testable.

## The layering — design in this order

When planning, walk these layers top-to-bottom. Each layer depends only on the ones above it.

| # | Layer | Path | Knows about |
|---|---|---|---|
| 1 | DB / repository | `crates/audetic/src/db/<table>.rs` | SQLite only (daemon-owned) |
| 2 | Shared core library | `crates/audetic-core/src/<module>.rs` | std/deps only — pure logic, used by daemon AND CLI |
| 3 | Daemon domain module | `crates/audetic/src/<domain>/` | DB + `audetic-core`; daemon-internal, NOT importable by the CLI |
| 4 | HTTP route + utoipa | `crates/audetic/src/api/routes/<domain>.rs` | Domain modules |
| 5 | OpenAPI registration | `crates/audetic/src/api/docs.rs` (`ApiDoc`) | Route handlers |
| 6 | URL constants | `crates/audetic-core/src/url.rs` (`paths::*`), re-exported as `crate::api::url` | Path strings only |
| 7 | Web UI types (generated) | `apps/web-ui/src/api/schema.ts` (`bun run codegen`) | OpenAPI spec |
| 8 | Web UI consumer | `apps/web-ui/src/stores/*` + `routes/*` | `daemon` client + `schema.ts` |
| 9 | CLI consumer | `crates/audetic-cli/src/<domain>.rs` | HTTP via `api_url(paths::*)` + `reqwest`; `audetic_core::` for pure logic — see "CLI transport" below |

**Library code at layers 2–3 must not import `axum::` (HTTP server) or `clap::` (CLI parsing).** If it does, the boundary leaked. (Outbound `reqwest` from `audetic-core` is fine — e.g. `jobs_client` calling an external API.)

## Hard rules (these break tests or break consumers)

1. **No inlined daemon URLs.** Never hardcode `http://127.0.0.1:3737/api/...`. Use `api_url(paths::FOO)` (from `audetic_core::url`) in Rust, the `daemon` client in TypeScript. The dev vite proxy and prod same-origin only work if the web UI uses the typed client. `crates/audetic-cli/src/post_processing.rs` is the exemplar; some older commands still build URLs with raw `format!("{}/x", base_url())` and should be migrated.

2. **The CLI is a separate crate — it talks to the daemon over HTTP.** `audetic-cli` cannot `use crate::history` or otherwise import the `audetic` daemon crate. It reaches daemon-owned data and state via HTTP (`api_url(paths::*)` + `reqwest`), and shares pure, daemon-independent logic through `audetic-core`. See *CLI transport* for the decision.

3. **Never reach daemon runtime state except through HTTP.** The CLI must not depend on the `audetic` crate to touch `MeetingMachine`, `RecordingMachine`, or any `*StatusHandle`. State machines and shared handles are daemon-internal — a second process touching them would create two owners of the same in-memory state. The crate boundary enforces this; do not work around it by adding `audetic` as a CLI dependency.

4. **OpenAPI is the source of truth for web UI types.** Every new route needs `#[utoipa::path(...)]` plus registration in `api/docs.rs` (`ApiDoc::paths` and `components`). After changes, run `bun run codegen` from `apps/web-ui/` (or `make codegen` if available) and commit `schema.ts`.

5. **Well-known endpoints get a `paths::` constant.** Any path referenced from more than one place (CLI + web UI, install scripts, keybind installer) needs an entry in `audetic_core::url::paths`. Tests in `crates/audetic-core/src/url.rs` enforce that every `paths::*` constant resolves to a real OpenAPI operation — drift fails CI.

6. **Repository methods don't know about HTTP.** SQL stays in `crates/audetic/src/db/`. Route handlers call repository methods; they do not write SQL inline.

7. **Domain modules don't know about CLI flags.** Argument parsing belongs in `crates/audetic-cli/src/args.rs`. Core and domain functions take typed structs, not `clap::ArgMatches`.

## CLI transport

The CLI never reaches into the daemon. Pick how a command gets its work done:

| Pattern | Example commands | Trigger |
|---|---|---|
| **Daemon HTTP** (`api_url(paths::*)` + `reqwest`) | `history`, `keybind`, `logs`, `provider`, `update`, `meeting`, `post-processing` | Anything the daemon owns: the SQLite database, runtime-managed config, active recording/meeting, watched jobs, anything behind a `*StatusHandle`. The daemon must be running. In practice this is **every stateful command** — even durable reads like `history` go through `GET /history` because the daemon owns the DB connection. |
| **Shared core** (`audetic_core::*`) | pure helpers used inside `transcribe` and others — config parsing, compression, clipboard, ffmpeg | Pure logic with no daemon dependency. Lives in `audetic-core` so both crates share one implementation. |
| **External API** | `transcribe` | Talks to a third-party service (Audetic Cloud) via `audetic_core::jobs_client`. Not the daemon. |

There is **no** "CLI reads the DB directly" path: the daemon owns the SQLite connection, so durable data is read over HTTP. When planning a new CLI command, the default is HTTP; only pure, daemon-independent logic belongs in `audetic-core`.

## Planning checklist — include in every feature plan

For each feature, the plan should explicitly answer:

- [ ] **DB**: New table or column? Migration written (`crates/audetic/src/db/`)? Repository method (signature)?
- [ ] **Core vs daemon library**: Pure, cross-crate logic → `crates/audetic-core/src/`. Daemon-internal business logic → `crates/audetic/src/<domain>/`. Name the function and what it takes/returns.
- [ ] **HTTP route**: Method, path, request/response types (all `#[derive(ToSchema)]`), `#[utoipa::path]` annotation, error mapping via `api::error`.
- [ ] **OpenAPI registration**: Add handler to `ApiDoc::paths(...)`, add new types to `components(schemas(...))`.
- [ ] **`paths::` constant**: Add to `audetic_core::url::paths` IF any other code (CLI, install, web UI components that build URLs by hand, keybind) needs to reference it.
- [ ] **Codegen**: `bun run codegen` in `apps/web-ui/` to refresh `schema.ts`.
- [ ] **Web UI**: Which store owns the state? Which route/component renders it? Calls go through `daemon` from `api/client.ts`.
- [ ] **CLI**: Subcommand wiring in `crates/audetic-cli/src/args.rs` + handler in `crates/audetic-cli/src/<domain>.rs`. Transport: daemon HTTP via `api_url(paths::*)` for daemon-owned ops; `audetic_core::` for pure logic (see *CLI transport*).
- [ ] **Tests**: Unit tests at the core/domain layer; route-level tests where useful; verify `cargo test` passes the `url::tests::*` invariants in `audetic-core`.
- [ ] **Verification**: How to test end-to-end — `make build && make test`, manual flow through CLI, manual flow through web UI (chrome-devtools MCP if applicable).

If a checkbox doesn't apply, say so explicitly in the plan with one sentence ("No new DB schema — uses existing `meetings` table"). Silent skips hide coupling.

## Orthogonality red flags — call these out in plan review

- Core or domain module (layers 2–3) imports `axum::` or `clap::` → push HTTP concerns out to `api/routes/`, CLI parsing out to `crates/audetic-cli/src/args.rs`.
- Route handler contains a SQL string or `rusqlite::` call → push down into `crates/audetic/src/db/`.
- Route handler body exceeds ~20 lines of business logic → extract into a domain module.
- Web UI does `fetch("/api/...")` or `fetch("http://127.0.0.1:3737/...")` → use `daemon.GET("/path", ...)` from `api/client.ts`.
- CLI adds the `audetic` daemon crate as a dependency to reach a state handle, machine, or DB → wrong (rule 3); reach it over HTTP, or move shared pure logic to `audetic-core`.
- CLI builds a daemon URL with raw `format!("{}/x", base_url())` instead of `api_url(paths::X)` → use the constant (`post_processing.rs` is the pattern; `history`/`keybind`/`logs`/`provider`/`update` still need migrating).
- Two consumers (CLI + web UI) compute the same URL with different string concatenation → add a `paths::` constant or a helper like `post_processing_job_path(id)`.
- A new endpoint exists but `schema.ts` wasn't regenerated → run `bun run codegen` before claiming the web UI is wired up.

## When it's OK to skip layers

- **Pure web UI work** (component restyling, layout, copy): layers 1–6 untouched.
- **Internal refactor inside one core/domain module**: layers 4–9 untouched if the public function signature didn't change.
- **CLI-local concern** (e.g. output formatting, `--json` flag handling that just transforms an existing response): no new route needed.
- **One-shot script** in `scripts/` that hits an existing endpoint: no new layer work, just call `api_url(paths::*)`.

## Concrete reference points

Use these as templates when designing a new feature. Read whichever is closest before writing the plan.

- **CLI over HTTP, done right** (uses `api_url(paths::*)` + helpers): `crates/audetic/src/post_processing/` → `crates/audetic/src/api/routes/post_processing.rs` → `crates/audetic-cli/src/post_processing.rs`.
- **CLI reading durable data over HTTP**: `crates/audetic/src/api/routes/history.rs` (`GET /history`) → `crates/audetic-cli/src/history.rs` (note: still uses raw `base_url()` strings; migrate to `paths::HISTORY`).
- **State-machine domain across all consumers**: `crates/audetic/src/meeting/` → `crates/audetic/src/api/routes/meetings.rs` → `crates/audetic-cli/src/meeting.rs` (HTTP) → `apps/web-ui/src/stores/meeting-store.ts` and `routes/meetings.tsx`.
- **Shared pure logic across crates**: `crates/audetic-core/src/` (config, compression, url) used by both daemon and CLI; `transcribe` uses `audetic_core::jobs_client` + the external API.
- **Well-known path with parameterization**: `audetic_core::url::post_processing_job_path(id)` shows how to build a `paths::X/{id}/sub` helper alongside the base constant.

## Verification before declaring a plan complete

A finished plan should let someone (or future-you) implement without re-deciding. Before handing off:

1. Every checklist item above is answered or explicitly N/A.
2. File paths are concrete and crate-qualified (`crates/audetic/src/api/routes/foo.rs`, not "the routes file").
3. The reused functions are named (e.g., `MeetingRepository::update_title`, not "the repository").
4. Test plan covers: `cargo test` passes (especially `audetic_core::url::tests`), `bun run typecheck` in the web UI passes, and a manual end-to-end walkthrough is described for at least one consumer.
