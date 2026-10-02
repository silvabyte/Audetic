use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

pub(crate) const SCHEMA_VERSION: i64 = 3;

pub fn init_db() -> Result<Connection> {
    init_db_at(&crate::global::db_file()?)
}

pub fn init_db_at(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }
    let conn = Connection::open(db_path).context("Failed to open database connection")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    migrate(&conn)?;
    Ok(conn)
}

pub(crate) fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )?)
}

pub(crate) fn has_legacy_schema(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "meetings")?
        || table_exists(conn, "workflows")?
        || table_exists(conn, "meeting_artifacts")?)
}

/// Initialize a fresh database, or validate the unified schema. This runs on
/// every connection: crash recovery and business-state normalization do NOT
/// belong here. Legacy conversions require the explicit offline command.
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    if has_legacy_schema(conn)? {
        bail!("Legacy audio database detected. Stop Audetic, then run `audeticd migrate-audio-notes --database <PATH> --dry-run` and `audeticd migrate-audio-notes --database <PATH>`. See docs/audio-notes-migration.md.");
    }
    if table_exists(conn, "audio_notes_schema")? {
        let version: i64 = conn.query_row(
            "SELECT version FROM audio_notes_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        match version {
            1 => {
                migrate_v1_to_v2(conn)?;
                migrate_v2_to_v3(conn)?;
            }
            2 => migrate_v2_to_v3(conn)?,
            SCHEMA_VERSION => {}
            _ => {
                bail!("Unsupported Audio Notes schema version {version}; expected {SCHEMA_VERSION}")
            }
        }
        return Ok(());
    }
    if table_exists(conn, "audio_notes")? || table_exists(conn, "audio_note_artifacts")? {
        bail!("Unversioned Audio Notes tables detected; refusing to mark an unknown schema as current. Restore a known database backup or inspect the schema offline.");
    }
    let tx = conn.unchecked_transaction()?;
    create_schema(&tx)?;
    tx.commit().context("Failed to commit Audio Notes schema")
}

/// Called only inside the fresh-install or offline-conversion transaction.
pub(crate) fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS audio_notes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT, title_source TEXT, title_updated_at TIMESTAMP,
            title_version INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'recording',
            audio_path TEXT NOT NULL,
            source_filename TEXT,
            transcript_path TEXT, transcript_text TEXT, transcript_segments TEXT,
            duration_seconds INTEGER,
            started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP, error TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            deleted_at TIMESTAMP,
            capture_source TEXT NOT NULL DEFAULT 'microphone',
            source_provider TEXT,
            source_external_id TEXT,
            source_recorded_at TIMESTAMP,
            classification TEXT CHECK(classification IS NULL OR json_valid(classification)),
            classification_kind_override TEXT CHECK(
                classification_kind_override IS NULL OR (
                    length(classification_kind_override) BETWEEN 1 AND 64
                    AND substr(classification_kind_override, 1, 1) GLOB '[a-z]'
                    AND classification_kind_override NOT GLOB '*[^a-z0-9_-]*'
                )
            ),
            enrichment_status TEXT NOT NULL DEFAULT 'pending'
                CHECK(enrichment_status IN ('pending', 'running', 'completed', 'error')),
            enrichment_error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_audio_notes_stream ON audio_notes(started_at DESC, id DESC)
            WHERE deleted_at IS NULL;
        CREATE INDEX IF NOT EXISTS idx_audio_notes_status ON audio_notes(status);
        CREATE INDEX IF NOT EXISTS idx_audio_notes_effective_kind ON audio_notes(
            COALESCE(
                classification_kind_override,
                json_extract(CASE WHEN json_valid(classification) THEN classification END, '$.kind')
            )
        );
        CREATE INDEX IF NOT EXISTS idx_audio_notes_source ON audio_notes(source_provider, source_external_id);
        CREATE TABLE IF NOT EXISTS ingress_access_keys (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL CHECK(trim(name) <> ''),
            scope TEXT NOT NULL CHECK(scope IN ('index', 'generic')),
            secret_hash BLOB NOT NULL UNIQUE
                CHECK(typeof(secret_hash) = 'blob' AND length(secret_hash) = 32),
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_used_at TIMESTAMP,
            revoked_at TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_ingress_access_keys_active
            ON ingress_access_keys(scope, revoked_at);
        CREATE TABLE IF NOT EXISTS external_imports (
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
        CREATE INDEX IF NOT EXISTS idx_external_imports_recent
            ON external_imports(created_at DESC, id DESC);
        CREATE INDEX IF NOT EXISTS idx_external_imports_note
            ON external_imports(audio_note_id);
        CREATE TABLE IF NOT EXISTS plaud_sync_state (
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
        INSERT OR IGNORE INTO plaud_sync_state(singleton) VALUES(1);
        CREATE TABLE IF NOT EXISTS post_processing_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL, event TEXT NOT NULL, action_type TEXT NOT NULL,
            action_config TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_pp_jobs_event_enabled ON post_processing_jobs(event) WHERE enabled = 1;
        CREATE TABLE IF NOT EXISTS agent_profiles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL, kind TEXT NOT NULL, executable TEXT NOT NULL, args_json TEXT NOT NULL,
            prompt_mode TEXT NOT NULL DEFAULT 'stdin', default_profile INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(kind, executable)
        );
        CREATE INDEX IF NOT EXISTS idx_agent_profiles_enabled ON agent_profiles(enabled);
        CREATE TABLE IF NOT EXISTS audio_note_artifacts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            note_id INTEGER NOT NULL REFERENCES audio_notes(id),
            kind TEXT NOT NULL, title TEXT NOT NULL, template_id TEXT,
            agent_profile_id INTEGER REFERENCES agent_profiles(id),
            status TEXT NOT NULL, content_markdown TEXT, content_json TEXT,
            error TEXT, stdout TEXT, stderr TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_audio_note_artifacts_note_created ON audio_note_artifacts(note_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_audio_note_artifacts_status ON audio_note_artifacts(status);
        CREATE TABLE IF NOT EXISTS audio_notes_schema (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            version INTEGER NOT NULL, migrated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        INSERT OR IGNORE INTO audio_notes_schema(singleton, version) VALUES(1, 3);",
    ).context("Failed to create unified Audio Notes schema")?;
    Ok(())
}

fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
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
         INSERT INTO plaud_sync_state(singleton) VALUES(1);
         UPDATE audio_notes_schema
            SET version = 2, migrated_at = CURRENT_TIMESTAMP
            WHERE singleton = 1;",
    )
    .context("Failed to migrate Audio Notes schema from version 1 to 2")?;
    tx.commit()
        .context("Failed to commit Audio Notes schema version 2 migration")
}

fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "ALTER TABLE audio_notes ADD COLUMN classification_kind_override TEXT CHECK(
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
         UPDATE audio_notes_schema
            SET version = 3, migrated_at = CURRENT_TIMESTAMP
            WHERE singleton = 1;",
    )
    .context("Failed to migrate Audio Notes schema from version 2 to 3")?;
    tx.commit()
        .context("Failed to commit Audio Notes schema version 3 migration")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_install_has_no_legacy_tables() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        assert!(!has_legacy_schema(&conn).unwrap());
        assert!(table_exists(&conn, "audio_note_artifacts").unwrap());
        assert!(table_exists(&conn, "post_processing_jobs").unwrap());
    }

    #[test]
    fn refuses_legacy_without_changing_it() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE workflows(id INTEGER PRIMARY KEY);")
            .unwrap();
        assert!(migrate(&conn)
            .unwrap_err()
            .to_string()
            .contains("migrate-audio-notes"));
        assert!(!table_exists(&conn, "audio_notes").unwrap());
    }

    #[test]
    fn opening_connection_does_not_normalize_or_recover_business_state() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute_batch("INSERT INTO audio_notes(title,audio_path,enrichment_status) VALUES('  untouched  ','audio.wav','running');").unwrap();
        migrate(&conn).unwrap();
        let row: (String, String) = conn
            .query_row("SELECT title,enrichment_status FROM audio_notes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(row, ("  untouched  ".into(), "running".into()));
    }

    #[test]
    fn unknown_schema_versions_are_not_silently_accepted() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute("UPDATE audio_notes_schema SET version=999", [])
            .unwrap();
        assert!(migrate(&conn)
            .unwrap_err()
            .to_string()
            .contains("Unsupported"));
        conn.execute("DROP TABLE audio_notes_schema", []).unwrap();
        assert!(migrate(&conn)
            .unwrap_err()
            .to_string()
            .contains("Unversioned"));
        assert!(!table_exists(&conn, "audio_notes_schema").unwrap());
    }

    #[test]
    fn version_one_database_migrates_without_losing_audio_notes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE audio_notes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT, title_source TEXT, title_updated_at TIMESTAMP,
                title_version INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'recording',
                audio_path TEXT NOT NULL,
                source_filename TEXT,
                transcript_path TEXT, transcript_text TEXT, transcript_segments TEXT,
                duration_seconds INTEGER,
                started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                completed_at TIMESTAMP, error TEXT,
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                deleted_at TIMESTAMP,
                capture_source TEXT NOT NULL DEFAULT 'microphone',
                classification TEXT,
                enrichment_status TEXT NOT NULL DEFAULT 'pending',
                enrichment_error TEXT
            );
            CREATE TABLE audio_notes_schema (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                version INTEGER NOT NULL, migrated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            INSERT INTO audio_notes_schema(singleton, version) VALUES(1, 1);
            INSERT INTO audio_notes(title, audio_path, classification)
                VALUES('Keep me', '/tmp/keep.wav', 'legacy-not-json');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let version: i64 = conn
            .query_row(
                "SELECT version FROM audio_notes_schema WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let title: String = conn
            .query_row("SELECT title FROM audio_notes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, 3);
        assert_eq!(title, "Keep me");
        assert!(crate::db::audio_notes::AudioNoteRepository::get(&conn, 1)
            .unwrap()
            .unwrap()
            .classification
            .is_none());
        assert!(table_exists(&conn, "ingress_access_keys").unwrap());
        assert!(table_exists(&conn, "external_imports").unwrap());
        assert!(table_exists(&conn, "plaud_sync_state").unwrap());
    }

    #[test]
    fn version_two_database_migrates_classification_override_without_losing_inference() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE audio_notes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                audio_path TEXT NOT NULL,
                classification TEXT CHECK(classification IS NULL OR json_valid(classification)),
                deleted_at TIMESTAMP
            );
            CREATE INDEX idx_audio_notes_kind ON audio_notes(json_extract(classification, '$.kind'));
            CREATE TABLE audio_notes_schema (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                version INTEGER NOT NULL, migrated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            INSERT INTO audio_notes_schema(singleton, version) VALUES(1, 2);
            INSERT INTO audio_notes(audio_path, classification)
                VALUES('/tmp/keep.wav', '{\"kind\":\"meeting\"}');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let row: (i64, String, Option<String>) = conn
            .query_row(
                "SELECT s.version, n.classification, n.classification_kind_override
                 FROM audio_notes_schema s CROSS JOIN audio_notes n",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, (3, r#"{"kind":"meeting"}"#.into(), None));
        assert!(conn
            .execute(
                "UPDATE audio_notes SET classification_kind_override='custom-kind' WHERE id=1",
                [],
            )
            .is_ok());
        assert!(conn
            .execute(
                "UPDATE audio_notes SET classification_kind_override='Not valid' WHERE id=1",
                [],
            )
            .is_err());
        let index_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='index' AND name='idx_audio_notes_effective_kind'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 1);
    }
}
