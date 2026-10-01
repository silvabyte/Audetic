# Audio Notes

All captured and imported audio belongs to one chronological stream. An Audio
Note owns durable audio references, the raw transcript and optional timed
segments, title/provenance, capture timestamps/source, and processing state.
“Meeting,” “dictation,” “conversation,” and “request” are classifications, not
separate models, endpoints, or capture machines.

## Workflow

```text
microphone / microphone + system / imported media
                       ↓
              durable Audio Note
                       ↓
            optional review and trim
                       ↓
         transcribe → persist raw transcript
                       ├─ optional immediate paste/copy
                       ↓
              AI classification
                       ↓
             processor / template
                       ↓
          durable artifact + completion event
```

Choose **Record note** or **Import audio / video** in `/audio-notes`. Capture
options select sources, review, and delivery behavior; they do not preselect the
note's meaning. Review lets you play, trim, confirm, or discard a recording.

Automatic paste is **off by default**. Settings → Capture & delivery saves the
daemon preference, also represented by `[behavior].auto_paste` in config.toml.
Each capture can override it. If enabled, the persisted raw transcript is pasted
immediately; later AI results never replace it or get pasted into another window.
Clipboard copying is a separate opt-in capture option or explicit note action.

### External imports

Audetic accepts external recordings through a dedicated loopback listener on
`127.0.0.1:3739`. It serves only authenticated `POST /v1/index` and
`POST /v1/audio`; the local API and SPA remain on `127.0.0.1:3737`. Configure
the `ingest.audetic.link` tunnel to target port 3739 only.

Create and revoke scoped, one-time bearer keys in Settings → Integrations or
with `audetic integrations keys`. The generic endpoint accepts multipart fields
`audio`, `external_id`, and optional RFC 3339 `recorded_at` and `title`. Repeated
provider/key/external-ID deliveries are idempotent. Public webhook uploads are
limited to 50 MiB.

For Pebble Index, configure the generated key as an `Authorization` header with
the value `Bearer <key>`, and add `X-Index-Webhook-Version` with the value `1`.
Use `https://ingest.audetic.link/v1/index` as the webhook URL and enable audio.

Plaud synchronization uses the official `@plaud-ai/cli`. Install it, run
`plaud login`, then enable incremental sync or explicitly import history:

```sh
audetic integrations plaud status
audetic integrations plaud enable --interval-minutes 15
audetic integrations plaud sync
audetic integrations plaud backfill
```

Plaud downloads stream to disk with a 1 GiB limit and a 30-minute timeout so
long recordings do not inherit the public webhook limit.

External recordings enter the same import, transcription, enrichment, and
post-processing pipeline as manual imports. Their provider, external ID, and
original recording time remain attached to the Audio Note.

Raw capture/transcription status and enrichment status are independent. A missing
or failed agent does not hide a successful transcription. The note exposes the
error and a **Retry AI processing** action. Restart marks interrupted processing
as an error with retry guidance. Transcription retry uses retained audio; AI retry
uses the saved transcript. Completed enrichment requests are idempotent.

Select a signed-in, installed local agent in Settings → Capture & delivery.
Profiles support Claude Code, Codex, OpenCode, and Cursor Agent. Installation
checks cannot prove authentication: agent execution errors remain visible on the
note. The default agent is used for automatic processing; manual artifacts can
choose another agent/template.

## Classification and processors

`note_intelligence/classification.rs` validates a version-1 JSON contract with
`kind`, `confidence`, `title`, `topics`, `participants`, and extensible `metadata`.
Kinds are lowercase slugs, not a closed Rust enum. Unknown kinds remain valid and
use the general-note processor. Manual titles take precedence over generated titles.

`note_intelligence/registry.rs` maps classification kinds to artifact kinds and
templates. Defaults cover meeting summaries/participants/decisions/actions,
cleaned dictation, conversation summaries, general notes, task intent, and shopping
items. `enrich_audio_note_with_registry` accepts an alternative registry. To add a
processor, register its kind/template and output contract; do not add a new core
entity or duplicate the capture pipeline.

Artifacts retain Markdown, optional structured JSON, status, agent/template
provenance, and diagnostic output. Request artifacts contain extracted intent,
items, or actions. They do not themselves execute an inferred command or update
an external shopping application. Configured post-processing jobs receive
`audio_note.completed` with the note ID, transcript, audio references, duration,
title, and classification and can route these results onward.

The event is emitted after successful enrichment. Completion is persisted before
dispatch, preventing duplicate dispatch on ordinary retries. It is not a durable
outbox: a process crash between commit and dispatch can lose the notification.

## Architecture and API

- `db/audio_notes.rs`: SQL and one `AudioNoteRepository`.
- `audio_notes/`: one capture machine plus shared import/transcribe/retry pipeline.
- `note_intelligence/`: classification validation and processor selection.
- `audio_note_artifacts/`: template/agent execution and durable outputs.
- `api/routes/audio_notes.rs`: thin HTTP consumer of the domain.
- `audetic-core/url.rs`: shared daemon URL constants for CLI and integrations.
- `apps/web-ui/src/api/schema.ts`: generated from daemon OpenAPI, never maintained separately.

The CLI and browser reach the daemon over HTTP; neither owns SQLite or capture
state. Tests inject temporary database and media paths.

The collection is `GET /api/audio-notes`, with `query`, `kind`, `limit`, and
`offset`; ordering is descending capture time, then ID. Detail, playback, titles,
retry, AI processing, and artifacts live under `/api/audio-notes/{id}`. Capture
operations are `/start`, `/stop`, `/toggle`, `/confirm`, `/cancel`, `/status`;
multipart imports use `/import`. See `/api/openapi.json` for exact contracts.

```sh
audetic notes import call.wav --title "Planning discussion"
audetic notes list --kind meeting --limit 20
audetic notes show 42
audetic notes process 42
audetic notes retry 42
audetic notes delete 42
```

Deletion hides a note and its artifacts from all normal reads; audio remains on
disk. Historical records with missing audio retain their transcripts.

## Development and upgrades

Use the [offline migration guide](audio-notes-migration.md) to convert legacy
storage before starting the updated daemon. Old `/meetings`, `/history`, `/toggle`,
and `/transcribe` API routes and meeting/history/transcribe CLI commands are removed.

For an isolated Linux development instance, set separate `XDG_CONFIG_HOME` and
`XDG_DATA_HOME`, and set `AUDETIC_PORT=3837` for both daemon and CLI. Set
`AUDETIC_INGRESS_PORT=3839` on the daemon when the production ingress listener
is already running. Create its
provider configuration and install/provide model assets there before recording.
The alternate port remains loopback-only. Never point QA fixtures or migrations
at your personal database. Without the environment override the port is 3737.
