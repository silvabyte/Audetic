# Notebook and remote access

Status: Planned

Owner: Mat, 12 November 2026.

This is the feature plan for a server-side notebook and for reaching the Audetic
web UI from Mat's other devices. It records the decision and the shape of the
work. It does not implement either one.

The canonical notebook lives in the Audetic daemon (`audeticd`) that already
runs as a systemd user service on the always-on Beelink (Arch Linux). Notes are
rows in that daemon's SQLite database, plus files beside it. Other devices are
clients. A private network is how those clients reach the daemon. Passkeys are
how a remote browser proves it may act. Quire is the inspiration for the
editor. The store, and any later export, stay Audetic's.

## Goals

- One Note. An Audio Note is a Note that has audio. A Note may also be typed
  Markdown or an imported document, with no audio.
- Edit Markdown (source and preview), search, switch notes quickly, file them
  in folders, tag them, and link them with `[[wiki links]]` that survive
  renames and expose backlinks.
- Keep audio first-class. Each audio source keeps its file, raw transcript,
  timed segments, and provenance. Classification and generated artifacts stay
  on the Note.
- Use the existing web UI (`apps/web-ui`, served by the daemon) as an
  installable PWA on the Mac, the phone, and the Beelink.
- Authenticate remote browsers with passkeys (WebAuthn). A Bitwarden passkey
  and a platform authenticator are both fine. Neither is named in the schema.
- Let a local agent read and search notes through an MCP server that is
  read-only unless a tool is explicitly enabled.
- Keep a Markdown folder export and import so a note can leave Audetic. The
  daemon database remains the canonical copy.
- Take optional snapshots of the database and media files onto a mounted NAS
  directory.

## Non-goals

- Adopting Quire, its desktop app, its Android app, or its S3 sync as the
  notes layer.
- Using Syncthing, MinIO, or an SMB share as the live store.
- A public hostname, a Cloudflare tunnel, or a DNS change. Those stay a
  separate owner decision.
- Replacing capture on the machine that runs the daemon. ADR 0002 still
  governs microphone and system-audio sessions there.
- A second notebook on the Mac, or daemon-to-daemon sync.
- Vendor names in schema, migrations, or core types. Plaud, Quire, Tailscale,
  Bitwarden, Synology, and MinIO appear in deployment prose only.
- Hosted distribution. ADR 0001 stands: install remains `git pull && make install`.
- Write-capable MCP tools in the first MCP slice.
- Password login, a vendor identity provider, and multi-user accounts. There
  is one operator.

## Language

Use the Audio Notes glossary in `crates/audetic/src/audio_notes/CONTEXT.md`.
This plan adds four terms. It does not reuse the words that glossary sets
aside.

**Note**:
The durable document. It has a Note Title, a Markdown body, and zero or more
audio sources. An Audio Note is a Note with at least one audio source.
_Avoid_: note type (the glossary reserves that phrase away from Artifact Kind)

**Folder**:
A named container of Notes. A person chooses it. It may be nested. It does not
classify a recording.
_Avoid_: collection, Effective Classification, Inferred Classification

**Tag**:
A `#tag` token indexed from the Markdown body.
_Avoid_: Manual Classification, kind

**Audio source**:
One captured or imported media file on a Note, with its own transcript,
segments, provenance, and processing state.
_Avoid_: Audio Note, when the file itself is what is meant

Effective Classification, Manual Title, Generated Title, and Artifact Kind keep
their current meanings. Classification describes the Note. A Folder is not a
classification, and a Tag is not a classification.

## What exists today

`audeticd` binds `127.0.0.1:3737` (`audetic_core::url::HOST` and
`DEFAULT_PORT`) and serves the SPA and `/api/*` with no session. A process
that can open that port can record, read transcripts, change the provider,
install a keybind, and read logs. Browser requests are limited by
`reject_disallowed_origin` in `crates/audetic/src/api/mod.rs`, which allowlists
localhost and the Vite dev ports. A request with no `Origin` header, which is
how the CLI and curl call the daemon, is allowed. That allowance is safe while
the listener is loopback and nothing proxies onto it.

Ingress is a separate listener on `127.0.0.1:3739`. It accepts `POST /v1/index`
and `POST /v1/audio` with a scoped bearer key. The key is stored as a hash and
works until it is revoked. `docs/audio-notes.md` already says an ingest tunnel
targets port 3739 only.

Audio notes are the `audio_notes` table plus `audio_note_artifacts`.
`audio_path` is `NOT NULL`, so the current row cannot represent a note without
audio. Search is `AudioNoteRepository::search`, an `instr` scan over the title
and the transcript. The web UI lists notes at `/audio-notes` and renders
generated Markdown with `ReactMarkdown` in
`apps/web-ui/src/components/artifact-content.tsx`. Local agents are CLI
profiles run by `crates/audetic/src/agents/`. The HTTP surface is
`/agent-profiles`, which only configures those CLIs. There is no MCP server
and no service worker.

File import goes through `import_audio_note_file` and does not take the live
capture handle. A phone or browser upload belongs on that path.

The next migration is `0003`. `0001_baseline.sql` and
`0002_recording_sources.sql` are already applied and must not be edited.
Startup upgrades the database under the instance lock after a `VACUUM INTO`
backup. An older binary refuses a database that has more migrations than it
knows (`newer_build` in `crates/audetic/src/db/migrations/mod.rs`).

## Alternatives

### A. A Markdown folder, Syncthing, and Quire as the editor

A folder of `.md` files is the notes layer. Syncthing copies it across the
Mac, the Beelink, the NAS, and Android. Quire edits the folder. Audetic
projects a note into the folder under an ownership rule: it writes the file
once, stores the hash it wrote, and rewrites the file only while the on-disk
hash still matches. After any other edit, the file is the person's.

The files stay portable, and Quire already has the editor, folders, tags,
links, and a read-only MCP server. Syncthing still produces conflicts when a
phone was offline. Audio, transcripts, timed segments, classification, and
artifacts are not Markdown files. They would live in a second store, or be
flattened and lose structure. Audetic and Quire would share a directory. The
ownership rule stops Audetic from overwriting an edit, and it also means
Audetic stops updating a note as soon as the file changes. Search, capture,
and the agent profiles would read a projection of a folder the database
already knows how to own.

### B. Quire object-store sync, with Audetic as a third client

Quire syncs a notes folder to S3-compatible storage, including a self-hosted
MinIO, using the format in
[SYNC.md](https://github.com/bjarneo/quire/blob/main/SYNC.md). A third client
may implement that format. Remote state is `.quire/manifest.json` (or a sealed
`manifest.bin`) plus one object per file. The reference fork is
[silvabyte/quire](https://github.com/silvabyte/quire); the format is the
upstream document.

Devices that already run Quire would see the same files, with no Syncthing.
The cost is protocol coupling. Audetic would implement Quire's manifest,
conflict copies (`<name> (conflict).md`), encryption header, and provider
table, and it would have to follow upstream changes. A sync reads the
manifest, changes it, reads it again, and writes it. The second read is
best-effort. The format does not use compare-and-swap, so two writers can
drop each other's entries. Quire runs that sync inside the desktop and
Android apps. There is no headless sync process to run beside `audeticd`.
SYNC.md carries `.md` files and a fixed list of image types up to 50 MB.
Audio, transcripts, and artifacts are outside the format. A locked note is a
Quire-specific file header, which would put a vendor layout in Audetic's
files.

### C. The NAS share as the working copy

The Synology share is mounted on each device and holds the notes.

There is one directory and no sync protocol. Phones mount SMB poorly, offline
use stops when the share is unreachable, and file locking across macOS,
Linux, and Android is a weak canonical store for SQLite. The share is a
reasonable place to put a snapshot. It is a poor place to put the live
database.

### Chosen direction

The live store stays the Beelink daemon: SQLite plus media files. The web UI
becomes the editor on every device, secured with passkeys, reached over a
private network. The bar for that editor is Quire's behavior: source and
preview, search, a quick switcher, folders, `#tags`, `[[wiki links]]` with
rename rewriting, attachments, and a read-only MCP server. Upstream Quire is
[bjarneo/quire](https://github.com/bjarneo/quire).

Alternative A remains available as an optional export and import. It stays off
until a directory is configured, and it uses the ownership rule above. The NAS
is an optional snapshot target: a copy of the same kind of database backup the
migration runner already writes, plus the media directory, to a configured
path. If the path is missing, the daemon logs and skips. Capture does not wait
on the NAS.

This leaves ADR 0001 and ADR 0002 in place. Distribution stays source-only.
Hot-swap capture stays on the daemon that owns the microphone. A recording
made in the browser enters through import, on the same path as a file upload.

## User flows

**Capture on the phone.** The installed PWA records with `MediaRecorder` and
uploads the blob through the existing import pipeline. The Beelink transcribes
and classifies it. The new Note shows up in the stream on every signed-in
device. The phone does not start the Beelink microphone.

**Capture on the Mac.** The early slices treat the Mac as a browser. It
records or shares a file and uploads it, the same way the phone does. System
audio, the global keybind, and paste into the focused app still need a daemon
on that Mac. If that daemon stays, it is a capture appliance: it finishes a
recording and imports the file to the Beelink. It does not keep a second
notebook. Whether to keep it is an open question below.

**Capture on the Beelink.** Unchanged. `audetic notes start` and the local UI
record through the Capture Backend, ADR 0002 included.

**A typed note.** The person creates a Note with no audio, writes Markdown,
and files it. A recording can be attached later. The raw transcript stays on
the audio source. Editing the body does not overwrite the transcript, and a
generated artifact still does not replace it.

**Organize and recall.** Search covers the title, the body, the transcript,
and artifact Markdown. The quick switcher opens a note by title. Folders group
notes. Tag filters and Effective Classification filters are separate.
`[[links]]` resolve to a note id. Renaming a note rewrites the visible label
and leaves backlinks in place.

**A bot.** On the Beelink, `audetic mcp` speaks MCP on stdio and reads the
daemon over HTTP. The default tools search and read. Write tools are off.
Folders the person has hidden are omitted. The existing agent profiles still
run classification and templates. MCP does not replace them.

## Data model

Names stay provider-agnostic. The tables below are the planned `0003` shape.
A later slice may add a column in its own migration. Rust types live in the
daemon crate.

| Table | Role |
| --- | --- |
| `notes` | id; title fields with today's rules (a Manual Title wins, `title_version`); `body_markdown`; `body_version`; folder id; classification JSON; manual kind override; enrichment status and error; timestamps; `deleted_at` |
| `note_audio_sources` | One row per media file. Carries the file fields of today's `audio_notes` row: path, capture source, transcript text and segments, duration, status, error, `source_provider`, `source_external_id`, `source_recorded_at`. `source_provider` stays an opaque string. |
| `note_folders` | id, parent id, name unique among siblings, position, hidden flag. MCP search and listings omit a hidden folder. |
| `note_links` | source note, target note, unresolved label. Rebuilt from the body on save. |
| `note_tags` | note id and tag text. Rebuilt from the body on save. |
| `note_attachments` | note id, relative path, media type, byte size |
| `webauthn_credentials` | credential id, public key, sign counter, label, transports, created and last-used timestamps, revoked timestamp |
| `web_sessions` | hash of the session id, expiry, credential id |

`audio_note_artifacts.note_id` keeps meaning "the Note id". Copying rows onto
the same integer ids makes that true without rewriting artifact rows.

The first migration allows zero or more audio sources so a later "attach
another recording" slice does not move columns again. Capture still creates
exactly one source. Classification reads that source's transcript. A rule for
several sources waits until the UI can attach a second one.

**Wiki links.** The body is the text the person edits. On save, the daemon
parses `[[...]]`. A unique title resolves to a note id and is stored as
`[[id|Title]]`. A rename rewrites the label and updates `note_links`. An
ambiguous or unknown title stays unresolved and is listed on the note.
Creating a note by following an unresolved link can wait until links ship.
Backlinks are a query on `note_links`, not a scan of every body.

**Tags** are tokens in the body. They do not write
`classification_kind_override`.

**Attachments** are files under the daemon data directory. Markdown in the
product points at `/api/notes/{id}/attachments/{attachment_id}`. Export
rewrites those URLs to relative paths. The daemon does not fetch arbitrary
remote images into the library.

**Saves.** A body or title save sends the version the client edited. A
mismatch returns 409 and the current document. The first conflict UI shows
both copies. It does not merge them.

**Deletion** keeps today's meaning. A deleted Note disappears from ordinary
reads. Files stay on disk until a separate cleanup exists.

## Migration

Add `crates/audetic/src/db/migrations/0003_notes.sql` and append it to
`MIGRATIONS`. Do not edit `0001` or `0002`. A changed checksum refuses
startup.

The SQL copies each `audio_notes` row into `notes` with the same id, an empty
`body_markdown`, and the current title and classification columns, and into
one `note_audio_sources` row with the file and the transcript. The empty body
is deliberate. The transcript remains the raw transcript.

The release that applies `0003` also points `AudioNoteRepository` reads and
writes at the new tables and keeps the current JSON for `/api/audio-notes`.
Capture looks the same in that release. The `audio_notes` table stays on disk,
unused by those writers, until a later cleanup migration drops it. Rollback is
the `VACUUM INTO` backup taken before the upgrade. The previous binary will
refuse the upgraded file, so running that binary on the new file is not a
rollback.

A fresh database replays the same chain. The copy is a no-op when
`audio_notes` is empty.

## Notebook behavior

Quire opens a folder of Markdown, switches between source and preview,
searches, offers a quick switcher, files notes in folders, indexes `#tags`,
rewrites `[[Note name]]` when a note is renamed, stores images under
`attachments/`, and exposes a read-only MCP server. Audetic does those jobs
on the Note model. It does not embed Quire, and it does not open a Quire
folder as its library.

**Source and preview.** The first editor can be a text area plus the existing
`ReactMarkdown` renderer. Highlighting the source (CodeMirror or another
component) is an open choice and is not required before folders ship. The
preview keeps today's rule that raw HTML stays inert.

**Quick switcher.** A palette over titles. Typing `Folder/Title` opens or
creates the note in that folder. It lives in the web UI and calls the notes
API.

**Search.** The notebook query should move from the `instr` scan to SQLite
FTS5, once it is confirmed that the SQLite linked by `rusqlite` 0.37 includes
FTS5. The dependency currently enables the `backup` feature only. If FTS5 is
absent, enable it on that dependency. Do not add another search service.
Until that slice, `AudioNoteRepository::search` keeps treating the query as
literal text, including characters that would be wildcards in `LIKE`.

**MCP.** `audetic mcp` in `audetic-cli` is a stdio server. It calls the daemon
with `api_url(paths::*)` and does not open SQLite. The first tools are search,
read one note, and list a folder. Write tools stay behind a flag that
defaults off. There is no new TCP port. Quire's optional HTTP MCP on
`127.0.0.1:3917` is a useful comparison and is not the plan. Remote MCP waits
until remote browser auth has already shipped.

**Export ownership.** When a directory is configured, Audetic writes each
exported note once and stores the SHA-256 it wrote, outside the Markdown.
A later export rewrites the file only when the hash still matches. A mismatch
leaves the file alone and marks the note diverged. Import of a diverged file
is an explicit action and comes back as a new body version. Audio files are
copied beside the Markdown. They are not forced through SYNC.md.

## Access

### Two listeners

`127.0.0.1:3737` stays the local admin API. The CLI, the local browser,
keybinds, provider setup, logs, and ingress-key management stay there, with
today's trust model: loopback, and no session.

Remote clients use a second listener. Every request on it requires a passkey
session, including a request that sends no `Origin`. Do not reverse-proxy the
remote network onto port 3737. Proxied traffic would look like the CLI, and
the missing-`Origin` allowance would become full admin access.

The remote listener serves the same SPA and a smaller route set.

| Capability | Loopback `:3737` | Remote listener |
| --- | --- | --- |
| Read and edit notes, play audio, upload an import | yes | yes, with a session |
| Browser or phone capture upload | yes | yes, with a session |
| Start or stop the daemon's microphone | yes | no |
| Provider, keybind, logs, config file, system install, ingress keys | yes | no |
| Enroll an additional passkey | with a one-time enroll token | an existing session may add one |
| OpenAPI document | yes | no |

Ingress stays on port 3739 with its own bearer keys. An ingest tunnel still
targets 3739 only.

### Passkeys

The daemon is the relying party. Credentials live in `webauthn_credentials`.
There is no external account provider.

A passkey is bound to the site's rpId. A credential created at
`http://127.0.0.1:3737` will not assert on the private-network hostname.
Enrollment for remote use happens on that hostname.

1. On the Beelink, `audetic auth enroll` asks the loopback API for a one-time
   token and prints a URL on the canonical HTTPS origin.
2. The person opens that URL, redeems the token, and creates a passkey. The
   rpId is the hostname they will keep using.
3. The daemon stores the credential and starts a session.

Further passkeys (a phone authenticator, a Bitwarden passkey, a hardware key)
are added from an existing session. Each credential has a label and can be
revoked. Revoking a credential revokes the sessions that came from it. The
sign counter is stored, and a regression rejects the assertion.

The remote listener sets an `HttpOnly`, `Secure`, `SameSite=Lax` session
cookie for the HTTPS origin. The session id is random, and only its hash is
stored. Sessions expire. The loopback listener does not set or require the
cookie.

Losing every passkey is recovered on the Beelink by running
`audetic auth enroll` again. A single-use recovery code, stored as a hash and
printed once at first enrollment, may redeem that same step. Daily login stays
the passkey. The recovery code only reopens enrollment.

### CSRF and origins

Add the one configured remote origin to the allowlist behind
`is_allowed_browser_origin`. The match is the full origin, HTTPS, exact
string. A mutating request on the remote listener must carry that `Origin`.
The loopback listener keeps today's allowlist. CORS remains a browser
response policy. The origin middleware remains the request check, which is
what `crates/audetic/src/api/mod.rs` already documents.

### How a device reaches the daemon

The preferred path is a private network such as Tailscale. Tailscale Serve, or
an equivalent, terminates HTTPS for the remote hostname and forwards only to
the remote listener. The daemon binds loopback for the admin listener and a
separate socket for the remote listener. WebAuthn and a PWA install need a
secure context once the origin leaves localhost, so that private path is HTTPS.

Cloudflare Tunnel, public DNS, and any change that puts the daemon on the
public internet are outside this plan. They could reuse the remote listener
later. They need an explicit decision from Mat before anyone configures them.

The auth design needs a stable HTTPS hostname before the first remote passkey
is registered. It does not require a particular private-network product.
Changing the hostname later strands the passkeys created for the old rpId.

### PWA

The daemon serves a web app manifest and a service worker with the SPA. The
app is installable on the phone and the Mac. The scope is the remote origin.

**Offline read.** The worker caches the shell and notes opened recently.
Cached API responses are dropped on logout. Admin routes are not cached.

**Offline write.** Drafts and queued uploads live in the browser (IndexedDB).
A queued save still carries `body_version`. On reconnect, a version mismatch
shows both copies. The phone does not keep its own SQLite database.

**Mobile audio.** `MediaRecorder` produces a blob. The client posts it to the
import route the daemon already uses for files. That path has to accept the
container the browser records. WebM/Opus and MP4/AAC are the likely pair.
ffmpeg is already on the transcribe path. This flow does not call the cpal
Capture Backend and does not implement ADR 0002 on the phone. System audio
from the phone is out of scope.

**Share target.** The manifest declares a share target for audio, other
files, and plain text. Audio and files go to import. Text becomes a Note
body. A share while offline joins the same queue. The share request requires
a session.

## Architecture

The feature-planning skill's order is the implementation order. Names below
are the planned seams. The storage slice is the one that moves data. Later
slices add routes on top.

| Layer | Plan |
| --- | --- |
| DB | `0003_notes.sql`. `NotesRepository` in `crates/audetic/src/db/notes.rs` with `insert`, `get`, `save_body(id, body, expected_version)`, and `search`. Audio-source queries sit in that module or in `note_audio_sources.rs`. `AudioNoteRepository` becomes an adapter over the new tables until callers move. A later cleanup deletes the adapter and the old table. |
| Core | Link and tag parsing live in `crates/audetic-core` so the daemon and the export command share one implementation. That module does not import `axum` or `clap`. The WebAuthn ceremony stays in the daemon. The CLI only starts it over HTTP. |
| Domain | `crates/audetic/src/notes/` owns save, link rebuild, and the export hash check. Capture and import stay in `crates/audetic/src/audio_notes/` and call the note repository when they create a source. `import_audio_note_file` remains the upload entry. |
| HTTP | `crates/audetic/src/api/routes/notes.rs` for the notebook. `crates/audetic/src/api/routes/auth.rs` for enroll, assert, logout, and the credential list. Choosing which routes exist on the remote listener belongs next to `ApiServer::start` in `crates/audetic/src/api/mod.rs`. Capture routes stay in `audio_notes.rs`. |
| OpenAPI | Register new handlers and `ToSchema` types in `crates/audetic/src/api/docs.rs`. |
| `paths::` | Add note, attachment, and auth paths to `crates/audetic-core/src/url.rs` when the CLI or MCP calls them. `url::tests` must keep resolving every constant to an operation. |
| Codegen | The slice that adds routes runs `bun run codegen` in `apps/web-ui` and commits `apps/web-ui/src/api/schema.ts`. |
| Web UI | A notes store next to the audio-notes store. The detail route gains the editor. The stream stays one list. Calls go through the `daemon` client in `api/client.ts`. The manifest and service worker ship as daemon static assets with the SPA. MobX keeps the `<Observer>` render-prop boundary. |
| CLI | `audetic notes` stays on HTTP. `audetic auth enroll` and `audetic mcp` are handlers under `crates/audetic-cli/src/`, wired from `args.rs`, using `api_url(paths::*)`. The CLI crate does not depend on the daemon crate. |
| Tests | The migration copy keeps ids, transcripts, an empty body, and loadable artifacts. A stale `body_version` returns 409. The origin allowlist accepts the configured HTTPS origin and rejects any other. The remote router rejects a missing session and rejects microphone start. Renaming a note keeps backlinks. `cargo test`, including `audetic_core::url::tests`. Web UI slices also run `bun run typecheck` and store tests for save and the offline queue. |
| Verification | `make test` for the Rust slices. For a UI slice, capture on loopback, edit a body, reload, and confirm the transcript is unchanged. For the remote slice, enroll a passkey on the HTTPS name with a one-time token, open the PWA from another device, and confirm a provider route answers 403. Do not point a public tunnel at either port as part of the test. |

Slices that do not add HTTP routes skip codegen. The MCP slice does not add a
browser route. No other layer in this table is optional once its slice starts.

## Roadmap

Each slice can ship alone, and loopback Audetic still works afterward. No
slice is in progress.

1. **Storage move.** `0003`, the repository, and unchanged `/api/audio-notes`
   JSON. Tests prove the copy.
2. **Body.** `PUT` the body with `body_version`. A text area and preview on
   the existing detail page. The transcript still comes from the audio source.
3. **Notes without audio.** Create one from the UI and from `audetic notes`.
   The stream lists it.
4. **Organization.** Folders, tags, wiki links, backlinks, and the quick
   switcher.
5. **Search.** FTS5 over title, body, transcript, and artifact Markdown, after
   the SQLite build is confirmed.
6. **Attachments.** Upload, render, and a size cap.
7. **Passkeys, listener still local.** The enroll token, the credential
   table, the session cookie, and tests against a temporary listener. This
   slice does not change which address the Beelink serves.
8. **Private HTTPS.** Bind the remote listener and put the private-network
   proxy in front of it. Update the origin allowlist. Leave admin routes off
   that listener. This slice waits on the hostname.
9. **PWA shell.** Manifest, install, an offline read cache, and a draft queue.
10. **Phone capture and share.** `MediaRecorder` and the share target, both
    uploading through import.
11. **MCP.** `audetic mcp`, with search and read. Write tools stay off.
12. **A way out.** Optional Markdown export and import with the ownership
    hash. Optional snapshot to a configured directory.

After slice 1 has run on the real database and nothing reads `audio_notes`, a
separate migration drops that table. That cleanup is not part of `0003`.

## Open questions

- Is a private network such as Tailscale already installed on the Beelink, the
  Mac, and the phone? Which HTTPS hostname is the WebAuthn rpId? The first
  remote passkey is stuck to that name.
- Which NAS directory should receive snapshots, and on what schedule? The
  plan needs a filesystem path and nothing about the NAS vendor.
- Does the linked SQLite include FTS5? `crates/audetic/Cargo.toml` depends on
  `rusqlite` 0.37.0 with the `backup` feature only.
- Is a plain text area enough until folders ship, or does source highlighting
  have to come first? Either way the preview can reuse `artifact-content.tsx`.
- Should the Mac keep a local daemon for system audio, keybinds, and paste,
  and import finished files to the Beelink? The early slices assume the Mac
  client is the browser.
- When a Note has several audio sources, which transcript is classified?
  Deferred until a second source can be attached.
- Slice 7 can store a printed recovery code, or it can rely on enroll from
  the Beelink alone. Which of those should ship first?

## Risks

**The admin API is open on loopback.** Binding that same listener past
loopback, or proxying a tunnel onto port 3737, gives away recordings,
provider secrets, and control of the host. Slice 8 waits for slice 7. The
remote listener is a different socket and a smaller route table.

**The passkey is tied to the hostname.** An enroll against the wrong origin
creates a credential the other devices cannot use. The enroll command prints
the origin it is using.

**The migration is the only copy of the notes.** `0003` moves that data. The
pre-upgrade backup is the safety net. The slice is finished only after a test
upgrades a fixture database and reads every transcript back. A NAS snapshot
does not replace that backup.

**Two offline edits.** Two devices can edit one note while one of them is
offline. The version check refuses the stale save, and the person reconciles
the two copies. The folder alternatives lose text when the last write wins.
The canonical store uses the version check so that does not happen here.

**Export can still overwrite a file.** An export writer that skips the hash
check will replace a Quire or Syncthing edit on the next save. The writer
compares the file to the stored hash and skips the file when they differ.

**An MCP write is a second editor.** A write tool that runs because a client
asked has no version dialog in front of the person. The default set of tools
is read-only.

**The service worker holds a copy.** A cached transcript on a shared phone is
a copy of the notebook. Logout clears the cache. The worker does not cache
the loopback admin origin.

**A rejected upload can drop audio.** If import rejects a `MediaRecorder`
blob and the client has already discarded it, the recording is gone. The
client keeps the blob until the daemon has accepted the file.

## Tracking

This file is the plan. A slice becomes a Fizzy card on the Audetic board when
someone starts it, with a tag from `docs/agents/triage-labels.md`. No slice is
in progress. The feature-planning skill does not ask for a beads issue, and
this repo tracks work in Fizzy rather than beads.
