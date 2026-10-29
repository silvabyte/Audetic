//! Version 2: external audio imports, ingress access keys, and Plaud sync.

use anyhow::{Context, Result};
use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> Result<Vec<String>> {
    conn.execute_batch(
        "ALTER TABLE audio_notes ADD COLUMN source_provider TEXT;
         ALTER TABLE audio_notes ADD COLUMN source_external_id TEXT;
         ALTER TABLE audio_notes ADD COLUMN source_recorded_at TIMESTAMP;
         CREATE INDEX idx_audio_notes_source ON audio_notes(source_provider, source_external_id);
         CREATE TABLE ingress_access_keys (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL CHECK(trim(name) <> ''),
            scope TEXT NOT NULL CHECK(scope IN ('index', 'generic')),
            secret_hash BLOB NOT NULL UNIQUE
                CHECK(typeof(secret_hash) = 'blob' AND length(secret_hash) = 32),
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_used_at TIMESTAMP,
            revoked_at TIMESTAMP
         );
         CREATE INDEX idx_ingress_access_keys_active
            ON ingress_access_keys(scope, revoked_at);
         CREATE TABLE external_imports (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider TEXT NOT NULL CHECK(trim(provider) <> ''),
            source_instance TEXT NOT NULL CHECK(trim(source_instance) <> ''),
            external_id TEXT NOT NULL CHECK(trim(external_id) <> ''),
            status TEXT NOT NULL CHECK(status IN ('pending', 'accepted', 'failed')),
            audio_note_id INTEGER REFERENCES audio_notes(id),
            recorded_at TIMESTAMP,
            source_filename TEXT,
            error TEXT,
            attempt_count INTEGER NOT NULL DEFAULT 1 CHECK(attempt_count > 0),
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(provider, source_instance, external_id)
         );
         CREATE INDEX idx_external_imports_recent
            ON external_imports(created_at DESC, id DESC);
         CREATE INDEX idx_external_imports_note ON external_imports(audio_note_id);
         CREATE TABLE plaud_sync_state (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0, 1)),
            interval_minutes INTEGER NOT NULL DEFAULT 15 CHECK(interval_minutes BETWEEN 5 AND 1440),
            import_after TIMESTAMP,
            running INTEGER NOT NULL DEFAULT 0 CHECK(running IN (0, 1)),
            last_started_at TIMESTAMP,
            last_completed_at TIMESTAMP,
            last_error TEXT,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         INSERT INTO plaud_sync_state(singleton) VALUES(1);",
    )
    .context("Failed to add external import tables")?;
    Ok(Vec::new())
}
