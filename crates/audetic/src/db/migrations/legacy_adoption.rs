use anyhow::{bail, Context, Result};
use rusqlite::Connection;

use super::table_exists;

// Databases released before `user_version` used this private chain. These
// steps only bridge that schema to numbered migration 1; they are not new
// numbered migrations and must run only during daemon startup.
const BASELINE_VERSION: i64 = 3;
pub(super) const V1_TO_V2: &str = "
    ALTER TABLE audio_notes ADD COLUMN source_provider TEXT;
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
    INSERT INTO plaud_sync_state(singleton) VALUES(1);
";
const V2_TO_V3: &str = "
    ALTER TABLE audio_notes ADD COLUMN classification_kind_override TEXT CHECK(
        classification_kind_override IS NULL OR (
            length(classification_kind_override) BETWEEN 1 AND 64
            AND substr(classification_kind_override, 1, 1) GLOB '[a-z]'
            AND classification_kind_override NOT GLOB '*[^a-z0-9_-]*'
        )
    );
    DROP INDEX IF EXISTS idx_audio_notes_kind;
    CREATE INDEX idx_audio_notes_effective_kind ON audio_notes(
        COALESCE(
            classification_kind_override,
            json_extract(CASE WHEN json_valid(classification) THEN classification END, '$.kind')
        )
    );
";

#[derive(Clone, Copy)]
pub(super) enum LegacyVersion {
    V1,
    V2,
    V3,
}

impl LegacyVersion {
    pub(super) fn number(self) -> i64 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
            Self::V3 => 3,
        }
    }
}

pub(super) fn version_for_adoption(
    conn: &Connection,
    user_version: i64,
    empty: bool,
) -> Result<Option<LegacyVersion>> {
    if user_version != 0 || empty || !table_exists(conn, "audio_notes_schema")? {
        return Ok(None);
    }
    let version: i64 = conn
        .query_row(
            "SELECT version FROM audio_notes_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .context("Failed to read legacy schema version")?;
    let version = match version {
        1 => LegacyVersion::V1,
        2 => LegacyVersion::V2,
        BASELINE_VERSION => LegacyVersion::V3,
        _ => bail!(
            "Unsupported legacy schema version {version}; expected a version from 1 to {BASELINE_VERSION}"
        ),
    };
    Ok(Some(version))
}

pub(super) fn apply_in_open_transaction(conn: &Connection, version: LegacyVersion) -> Result<()> {
    match version {
        LegacyVersion::V1 => {
            conn.execute_batch(V1_TO_V2)
                .context("Failed to migrate legacy schema from version 1 to 2")?;
            conn.execute_batch(V2_TO_V3)
                .context("Failed to migrate legacy schema from version 2 to 3")?;
        }
        LegacyVersion::V2 => conn
            .execute_batch(V2_TO_V3)
            .context("Failed to migrate legacy schema from version 2 to 3")?,
        LegacyVersion::V3 => {}
    }
    conn.execute_batch(
        "DROP TABLE IF EXISTS audio_notes_schema;
         DROP TABLE IF EXISTS audio_note_migration_sources;",
    )
    .context("Failed to remove legacy schema markers")
}
