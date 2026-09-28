# Migrating to Audio Notes

Audio Notes replaces the separate meeting and dictation-history tables with one
durable note stream. **Stop the daemon before migration.** Normal startup refuses
legacy databases and prints the offline migration command; merely opening a
database connection never converts legacy data or resets active processing.

Fresh installations create only `audio_notes`, `audio_note_artifacts`, the schema
marker, and the existing agent-profile and post-processing tables. They do not
need this migration.

## 1. Stop every daemon instance

Finish or cancel an active recording first. Quit the macOS menu-bar app if it
would restart the daemon. Stop any manually launched development daemon as well.

Linux user-service installation:

```sh
systemctl --user stop audeticd.service
systemctl --user is-active audeticd.service   # should report inactive
```

If an older installation also has an `audetic.service` unit, stop that unit too.
For an older system-service installation, stop its system unit instead. On macOS:

```sh
launchctl bootout gui/$(id -u)/ai.audetic.daemon
```

Do not run an old daemon against the converted database.

## 2. Inspect the dry-run report

Use the newly installed `audeticd` binary (not the HTTP-only `audetic` CLI).
The `migrate-audio-notes` daemon subcommand runs directly against the selected
database and exits; it does not start the service or require a running HTTP API.
Omitting `--database` uses the normal data-directory database. Typical paths are
`$XDG_DATA_HOME/audetic/audetic.db` (or `~/.local/share/audetic/audetic.db`) on
Linux and `~/Library/Application Support/audetic/audetic.db` on macOS.

```sh
audeticd migrate-audio-notes --database "/absolute/path/to/audetic.db" --dry-run
```

Dry-run opens the existing database read-only, validates SQLite integrity and
foreign keys, and reports counts, old-to-new IDs, missing audio, and obsolete
enabled event subscriptions. It does not create a backup or change source data.
It is an inventory/preflight check, not a guarantee that all rows satisfy the
new schema's constraints: the real conversion validates those transactionally.

Missing audio is **not** a migration failure. The transcript, original path, and
timestamps remain available. This command neither moves nor deletes audio files.
Resolve corrupt databases or orphaned artifact relationships before proceeding;
the migration deliberately refuses to discard those rows.

## 3. Convert

Ensure sufficient free space for another database copy and SQLite's transaction
journal, then run:

```sh
audeticd migrate-audio-notes --database "/absolute/path/to/audetic.db"
```

The printable JSON report includes `backup_path`. Save the report and backup.
The backup is named `audetic.db.pre-audio-notes-<UUID>.sqlite3` beside the database
and is created with owner-only permissions on Unix. Treat it as private: it
contains transcripts and agent/hook configuration.

### Safety and conversion rules

1. SQLite's backup API creates a consistent snapshot, including committed data
   in a WAL, into an exclusively reserved private file. This avoids the
   platform-dependent existing-file behavior of `VACUUM INTO`. The backup is
   synced and its integrity checked. A raw copy of just the live `.db` file would
   not provide this guarantee.
2. Conversion obtains an **exclusive SQLite transaction**. In WAL mode readers
   can continue, but no other writer can commit during conversion. A
   `data_version` check rejects any external commit between backup preparation
   and obtaining this lock. An active writer yields an actionable failure after
   the lock timeout: stop it and retry, rather than running two owners.
3. Meeting IDs are retained. Workflow IDs are assigned sequentially above the
   maximum meeting ID, with stable source-ID ordering. No collision can merge
   two records. Workflow text becomes the raw transcript, with the original
   creation time used for the note timestamps.
4. Deleted notes stay deleted. Titles retain ownership/version metadata;
   pre-provenance nonblank titles become manual titles. Original whitespace and
   any unknown legacy columns remain in the provenance archive. Segment JSON,
   durations, errors, file paths, and artifact IDs/parent/profile relationships
   are preserved. Legacy meetings use `microphone_and_system`, imports with a
   filename use `import`, and legacy dictations use `microphone`. No historical
   semantic classification is fabricated.
5. `audio_note_migration_sources` records `source_table`, `source_id`, `note_id`,
   and the full original `source_json` row. It also archives artifact rows and
   hook definitions (including original enabled flags). This is migration
   provenance, not an old runtime repository. SQLite BLOB values are tagged
   with `sqlite_blob_base64` in the archive; nonfinite real values are preserved
   with a `sqlite_real` tag rather than silently becoming JSON null. Invalid
   UTF-8 text is rejected with a rollback instead of being rewritten lossily.
6. Note/artifact/mapping counts and parent relationships are checked. Legacy
   tables are removed and `audio_notes_schema` version **1** is committed in the
   same transaction after integrity/foreign-key validation. Agent profiles and
   post-processing definitions remain. Any conversion failure rolls back the
   entire transaction, leaving source tables usable and the backup available.

Re-running the command on a migrated database is a no-op reporting
`already_migrated: true`; it does not create another backup or duplicate notes.
An unknown schema version or mixed legacy/unified schema is rejected.

### Existing event hooks require deliberate reconfiguration

Enabled subscriptions to `dictation.completed` and `meeting.completed` are
disabled and listed in `disabled_subscriptions`. Their commands/configuration
remain in SQLite and the provenance archive, but retired subscriptions are not
exposed as runnable jobs by the new API. **They are not automatically retargeted**
to `audio_note.completed`: doing so could unexpectedly run an old shell command
for a wider set of recordings or with a different payload.

Review each saved command and recreate the intended job through Settings or the
post-processing API using the unified event and its `note_id`, transcript, audio,
duration, title, and classification fields. Test it explicitly before enabling.

## 4. Restart and verify

Linux user service:

```sh
systemctl --user start audeticd.service
```

macOS, using the installed LaunchAgent plist:

```sh
launchctl bootstrap gui/$(id -u) "$HOME/Library/LaunchAgents/ai.audetic.daemon.plist"
```

Open Audio Notes and verify a former meeting, a former dictation, their
transcripts, and any generated artifacts. Deleted records should remain hidden.
Missing-audio notes can still be read and processed from their transcripts.
Historical notes begin with pending enrichment; migration does not run agents.
Use Process when you want classification/artifacts generated. Restart recovery
of interrupted capture/enrichment is performed explicitly by service startup,
not by the per-connection schema initializer. Pending/running artifact jobs from
the previous daemon become durable errors with retry guidance; partial output,
stdout, and stderr remain available. Generate those artifacts again to retry.

## Restoring the pre-migration database

A rollback restores the **old schema**: use the previous daemon version with it,
or repair the problem and rerun migration before starting the new daemon.
Any notes created after migration are not in the old backup; preserve the
converted database if you may need those records.

Stop every daemon again. With no open connections, checkpoint the current
database and create a separate safety snapshot using the SQLite CLI:

```sh
sqlite3 "/absolute/path/to/audetic.db" 'PRAGMA wal_checkpoint(TRUNCATE);'
sqlite3 "/absolute/path/to/audetic.db" ".backup '/absolute/path/to/converted-safety.sqlite3'"
sqlite3 "/absolute/path/to/audetic.db" ".restore '/absolute/path/to/audetic.db.pre-audio-notes-UUID.sqlite3'"
sqlite3 "/absolute/path/to/audetic.db" 'PRAGMA integrity_check; PRAGMA foreign_key_check;'
```

Use SQLite's `.restore`, not a file copy over an open database or one with stale
WAL sidecars. Integrity output should be `ok`, with no foreign-key violation rows.
The isolated migration tests exercise rollback, restoration into a separate
database, and backups that include committed WAL content. Never point test
fixtures at your normal data directory.
