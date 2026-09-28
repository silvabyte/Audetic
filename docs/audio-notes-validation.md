# Audio Notes refactor validation

Validated on Linux on 2026-09-28. All runtime checks used isolated XDG config/data
directories under `/tmp/opencode/audio-notes-qa` and loopback ports 3837/3838.
The existing personal installation on port 3737 and its database were not upgraded.

## Automated gates

- `cargo test --workspace`: **311 passed, 1 ignored**. This includes 254 daemon
  unit tests, 9 capture integration tests, 4 compression tests, 12 CLI unit
  tests, 4 CLI integration tests, and 28 shared-core tests.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo build --workspace`: passed with the real bundled frontend.
- Frontend `bun run codegen`, `bun run lint`, `bun run typecheck`,
  `bun run test`, and `bun run build`: passed. **30 frontend tests** (19 behavior,
  11 Observer-boundary lint tests).

The ignored Rust test is the opt-in long-audio Parakeet test. Short speech was
transcribed with the real installed Parakeet model during runtime QA instead.
Native macOS consumer code and XCTest coverage were updated, but AppKit/SwiftUI
build/tests were not executable on this Linux host.

## Runtime and browser walkthrough

The bundled production SPA was inspected through Chrome DevTools, with real
daemon HTTP calls, FFmpeg, Parakeet transcription, and signed-in Codex processing.

| Flow | Observed result |
| --- | --- |
| CLI imports the 11-second speech fixture | Durable note created, exact recognizable speech transcribed, timed segment retained |
| AI processing after transcription | Valid classification stored; template-selected summary persisted separately from raw transcript |
| Browser multipart import | Import accepted, stream updated, generated title/classification and cleaned-text artifact appeared |
| Microphone capture → stop → review | Real capture opened; review playback and frozen duration appeared |
| Review trim → confirm | Four-second capture trimmed from 1s to 3s; resulting note duration was 2s |
| Silent captured audio | Audio retained; empty transcript and explanatory enrichment error visible; UI explicitly says no speech detected |
| Microphone + system capture → cancel | Both sources reported active; cancellation persisted without transcription |
| Paste configuration | Off by default; browser save survived reload; explicit per-recording off overrode saved on; preference restored off after QA |
| Default AI agent | Codex selected through the real API and shown in Settings; preference survived restart |
| Playback and segment seeking | Browser media ready state 4, duration 11s, seek/play successful, no media error |
| HTTP range playback | `Range: bytes=0-99` returned 206 and exactly 100 bytes |
| Title edit and raw-text copy | Browser title save persisted manual ownership, visible via CLI; copy action succeeded |
| Search and classification filter | Query + `shopping-list` displayed only the expected migrated note |
| Deletion | Browser deletion returned to the stream; soft-deleted note hidden from API reads |
| Removed APIs | `/api/meetings`, `/api/history`, `/api/toggle`, `/api/transcribe` returned 404 |

The delivery ordering and default-off behavior are additionally verified by
isolated pipeline tests: successful database persistence precedes delivery,
completion, and enrichment; failed persistence cannot paste or report success.
Real focused-desktop text injection was not exercised using synthetic speech.

## Migration and classification across former record types

Ran the actual `audeticd migrate-audio-notes` CLI against a synthetic legacy
SQLite fixture containing a meeting and workflow with colliding ID 1, plus an
existing artifact. Dry-run reported two notes without modification; conversion
created a private backup and mapped the workflow to note 2; rerun reported
`already_migrated: true` without duplicates.

Started the application against that migrated database and verified in Chrome:

- Former meeting and dictation appeared together in descending capture order.
- Original artifact survived, and the manual meeting title retained precedence.
- Real AI classification recognized the meeting, extracted Alice/Bob and release
  decisions/actions, and generated a meeting artifact.
- “Add milk and two dozen eggs to my shopping list” became `shopping-list`, with
  structured milk and eggs items (`quantity: 2`, `unit: "dozen"`). No external
  shopping action was executed.
- Missing legacy audio produced the expected playback-unavailable message and
  404, while the transcript remained readable and successfully processable.

Automated migration tests also cover old column vintages, deleted records,
artifact foreign keys, missing audio, disabled retired hooks, WAL-consistent
backup, rollback/restore, active writers, post-backup concurrent writes, corrupt
databases, invalid text, unknown legacy columns, and idempotency.

## Findings resolved during QA

- Initialized built-in agent profiles before the first capture, rather than
  depending on a prior UI profile-list request.
- Added default-agent selection and Codex's non-repository execution argument.
- Distinguished silent completed recordings from transcripts still pending.
- Removed a misleading completed-enrichment “Reprocess” control.
- Stopped treating cancelled/failed captures as pending enrichment polling work.

No unexpected browser console errors or failed requests were observed on the
final successful import/detail/title workflow. Missing legacy audio intentionally
returns 404; service restarts during QA temporarily interrupted polling.

## Static analysis

Ran UBS on all staged Rust, TypeScript/JavaScript, and Swift changes. Its critical
matches were reviewed: 12 Rust failure assertions in tests and 4 deliberate
JavaScript nullish comparisons. Warnings include test unwrap/assert patterns,
typed store accesses, and loopback HTTP URLs. These were not treated as runtime
defects or removed merely to silence the scanner. Clippy and frontend lint/type
checks pass. The frontend build retains a non-blocking bundle-size warning.
