# Database schema migrations

The daemon's SQLite schema is versioned. `crates/audetic/src/db/migrations/`
holds one frozen step per version; the runner reads the database's version and
applies every step between it and the latest. **This happens automatically when
the daemon starts**, so upgrading is just `make install` (or `audeticd install`).

| Version | Step |
|---|---|
| 0 | No schema marker: an empty database, or one from a pre-Audio-Notes release |
| 1 | Create Audio Notes and import legacy `meetings`/`workflows` |
| 2 | External imports, ingress access keys, Plaud sync |
| 3 | Classification kind override |

Fresh databases replay the same chain from version 0, so there is no separate
"current schema" to drift from the steps. To change the schema, append a step
to `MIGRATIONS`; never edit a released one.

## What startup does

1. Takes the daemon instance lock, so no other daemon can write.
2. Checks SQLite integrity and foreign keys, then reads the schema version.
3. If steps are pending and the database holds data, writes a private backup
   beside it: `audetic.db.pre-v<LATEST>-<UUID>.sqlite3`.
4. Applies every pending step in **one exclusive transaction**, stamping the
   version after each, then validates integrity and commits.
5. Logs a summary: steps applied, imported note counts, notes with missing
   audio, any disabled hooks, and the backup path.

If anything fails, the whole transaction rolls back and the database is exactly
as it was. The backup is then deleted, because it duplicates the database and a
restarting service would otherwise create one per attempt. The daemon exits
with the error, and `audeticd install` prints the service's recent log lines.

Opening a connection never upgrades existing data. A connection only creates a
brand-new empty database or confirms the schema is current; anything else is
refused with a message to restart the daemon.

## Running it manually

Use the `audeticd` binary (not the HTTP-only `audetic` CLI). Stop the daemon
first; the command takes the same instance lock and refuses if a daemon holds it.

```sh
audeticd migrate --dry-run   # apply the steps to an in-memory copy and report
audeticd migrate             # upgrade for real
```

`--database <PATH>` selects another file. The default is
`~/Library/Application Support/audetic/audetic.db` on macOS and
`$XDG_DATA_HOME/audetic/audetic.db` (or `~/.local/share/audetic/audetic.db`) on
Linux. `migrate-audio-notes` still works as an alias.

Dry run opens the database read-only and runs the real steps against an
in-memory copy, so it reports exactly what would happen, including validation
failures, without writing anything. Re-running on a current database is a no-op.

## Safety rules

1. SQLite's backup API creates a consistent snapshot, including committed data
   in a WAL, into an exclusively reserved owner-only file. The backup is
   converted to standalone rollback-journal mode, synced, and integrity
   checked; it needs no WAL/SHM sidecars. The source journal mode is preserved.
   Treat backups as private: they contain transcripts and hook configuration.
2. Steps run in an **exclusive SQLite transaction**. A `data_version` check
   rejects any external commit between taking the backup and acquiring the
   lock. An active writer fails after the lock timeout instead of racing.
3. Unknown versions (for example, a database from a newer build), unversioned
   Audio Notes tables, and mixed legacy/unified schemas are rejected.

## The legacy import (version 1)

1. Meeting IDs are retained. Workflow IDs are assigned sequentially above the
   maximum meeting ID, with stable source-ID ordering, so no two records merge.
   Workflow text becomes the raw transcript, with the original creation time
   used for the note timestamps.
2. Deleted notes stay deleted. Titles retain ownership/version metadata;
   pre-provenance nonblank titles become manual titles. Segment JSON,
   durations, errors, file paths, and artifact IDs/parent/profile relationships
   are preserved. Legacy meetings use `microphone_and_system`, imports with a
   filename use `import`, and legacy dictations use `microphone`. No historical
   semantic classification is fabricated.
3. `audio_note_migration_sources` keeps every original row as JSON
   (`source_table`, `source_id`, `note_id`, `source_json`), including artifact
   rows, hook definitions, and columns unknown to this release. BLOBs are tagged
   `sqlite_blob_base64` and nonfinite reals `sqlite_real`. Invalid UTF-8 text
   fails the migration instead of being rewritten lossily.
4. Note, artifact, and mapping counts and parent relationships are checked
   before the legacy tables are dropped. Orphaned artifacts fail the migration
   rather than being discarded.
5. Missing audio is **not** a failure: the transcript, original path, and
   timestamps remain. Audio files are never moved or deleted.

### Event hooks need deliberate reconfiguration

Enabled subscriptions to `dictation.completed` and `meeting.completed` are
disabled and named in the migration summary. Their configuration remains in
SQLite and the provenance archive. **They are not retargeted** to
`audio_note.completed`: an old shell command could then run for a wider set of
recordings or receive a different payload. Recreate each intended job through
Settings or the post-processing API, using the unified event and its `note_id`,
transcript, audio, duration, title, and classification fields, and test it
before enabling.

Historical notes begin with pending enrichment; migration does not run agents.
Use Process when you want classification or artifacts generated.

## Restoring a pre-migration backup

A restore brings back the **old schema**: run the previous daemon version with
it. Notes created after the migration are not in the backup; keep a copy of the
current database if you may need them.

Stop every daemon. With no open connections, checkpoint the current database,
snapshot it, then restore:

```sh
sqlite3 "/absolute/path/to/audetic.db" 'PRAGMA wal_checkpoint(TRUNCATE);'
sqlite3 "/absolute/path/to/audetic.db" ".backup '/absolute/path/to/current-safety.sqlite3'"
sqlite3 "/absolute/path/to/audetic.db" ".restore '/absolute/path/to/audetic.db.pre-v3-UUID.sqlite3'"
sqlite3 "/absolute/path/to/audetic.db" 'PRAGMA integrity_check; PRAGMA foreign_key_check;'
```

Use SQLite's `.restore`, not a file copy over an open database or one with
stale WAL sidecars. Integrity output should be `ok`, with no foreign-key rows.
Backups from before this runner existed are named
`audetic.db.pre-audio-notes-<UUID>.sqlite3` and restore the same way.
