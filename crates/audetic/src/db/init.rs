use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

pub(crate) const SCHEMA_VERSION: i64 = 1;

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
        if version != SCHEMA_VERSION {
            bail!("Unsupported Audio Notes schema version {version}; expected {SCHEMA_VERSION}");
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
            classification TEXT CHECK(classification IS NULL OR json_valid(classification)),
            enrichment_status TEXT NOT NULL DEFAULT 'pending'
                CHECK(enrichment_status IN ('pending', 'running', 'completed', 'error')),
            enrichment_error TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_audio_notes_stream ON audio_notes(started_at DESC, id DESC)
            WHERE deleted_at IS NULL;
        CREATE INDEX IF NOT EXISTS idx_audio_notes_status ON audio_notes(status);
        CREATE INDEX IF NOT EXISTS idx_audio_notes_kind ON audio_notes(json_extract(classification, '$.kind'));
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
        INSERT OR IGNORE INTO audio_notes_schema(singleton, version) VALUES(1, 1);",
    ).context("Failed to create unified Audio Notes schema")?;
    Ok(())
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
}
