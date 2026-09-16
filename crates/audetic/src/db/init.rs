use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use uuid::Uuid;

use std::path::Path;
use std::time::Duration;

use super::sync::SyncRepository;

pub fn init_db() -> Result<Connection> {
    let db_path = crate::global::db_file()?;

    init_db_at(&db_path)
}

pub fn init_db_at(db_path: &Path) -> Result<Connection> {
    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }

    let conn = Connection::open(db_path).context("Failed to open database connection")?;

    // The daemon opens a fresh connection per request, so recording-history
    // writes, meeting writes, and API reads overlap. Wait for the write lock
    // instead of failing immediately with SQLITE_BUSY.
    conn.busy_timeout(Duration::from_secs(5))
        .context("Failed to set SQLite busy timeout")?;

    migrate(&conn)?;

    Ok(conn)
}

pub fn migrate(conn: &Connection) -> Result<()> {
    let node_id = SyncRepository::ensure_node_id(conn)?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS workflows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_type TEXT NOT NULL,
            text TEXT NOT NULL,
            audio_path TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            sync_id TEXT NOT NULL CHECK (trim(sync_id) <> ''),
            sync_revision INTEGER NOT NULL DEFAULT 0 CHECK (sync_revision >= 0),
            origin_node_id TEXT NOT NULL CHECK (trim(origin_node_id) <> '')
        )",
        [],
    )
    .context("Failed to create workflows table")?;

    add_column_if_missing(conn, "workflows", "sync_id", "TEXT")?;
    add_column_if_missing(
        conn,
        "workflows",
        "sync_revision",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "workflows", "origin_node_id", "TEXT")?;

    // Create index for faster text searches
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_workflows_created_at ON workflows(created_at DESC)",
        [],
    )
    .context("Failed to create index on created_at")?;

    // Meetings table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS meetings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT,
            title_source TEXT,
            title_updated_at TIMESTAMP,
            title_version INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'recording',
            audio_path TEXT NOT NULL,
            source_filename TEXT,
            transcript_path TEXT,
            transcript_text TEXT,
            transcript_segments TEXT,
            duration_seconds INTEGER,
            started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP,
            error TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            deleted_at TIMESTAMP,
            sync_id TEXT NOT NULL CHECK (trim(sync_id) <> ''),
            sync_revision INTEGER NOT NULL DEFAULT 0 CHECK (sync_revision >= 0),
            origin_node_id TEXT NOT NULL CHECK (trim(origin_node_id) <> '')
        )",
        [],
    )
    .context("Failed to create meetings table")?;

    // Soft-delete column for meetings created before `deleted_at` existed.
    // `CREATE TABLE IF NOT EXISTS` above is a no-op on those DBs, so backfill
    // the column here. Idempotent — skips the ALTER if it's already present.
    add_column_if_missing(conn, "meetings", "deleted_at", "TIMESTAMP")?;

    // Per-segment timestamps (JSON array of {start,end,text}) for clickable
    // transcript lines. Backfilled for meetings created before this column —
    // older rows just have NULL and the UI falls back to plain text.
    add_column_if_missing(conn, "meetings", "transcript_segments", "TEXT")?;

    // Meeting Titles gained persisted provenance after the initial meetings
    // schema shipped. Existing non-empty titles are user-owned Manual Titles;
    // whitespace-only legacy values are absence, not titles.
    add_column_if_missing(conn, "meetings", "title_source", "TEXT")?;
    add_column_if_missing(conn, "meetings", "title_updated_at", "TIMESTAMP")?;
    add_column_if_missing(
        conn,
        "meetings",
        "title_version",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "meetings", "source_filename", "TEXT")?;
    add_column_if_missing(conn, "meetings", "sync_id", "TEXT")?;
    add_column_if_missing(
        conn,
        "meetings",
        "sync_revision",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "meetings", "origin_node_id", "TEXT")?;
    conn.execute(
        "UPDATE meetings SET title = NULL, title_source = NULL \
         WHERE (title IS NOT NULL AND trim(title) = '') \
            OR (title IS NULL AND title_source IS NOT NULL)",
        [],
    )
    .context("Failed to normalize absent meeting titles")?;
    conn.execute(
        "UPDATE meetings SET title = trim(title), title_source = 'manual' \
         WHERE title IS NOT NULL AND title_source IS NULL",
        [],
    )
    .context("Failed to migrate legacy meeting titles to manual provenance")?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_meetings_started_at ON meetings(started_at DESC)",
        [],
    )
    .context("Failed to create meetings started_at index")?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_meetings_status ON meetings(status)",
        [],
    )
    .context("Failed to create meetings status index")?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_meetings_deleted_at ON meetings(deleted_at)",
        [],
    )
    .context("Failed to create meetings deleted_at index")?;

    // Post-processing jobs: user-defined commands fired on daemon events
    // (e.g. dictation.completed, meeting.completed). `action_config` is a
    // serialized JSON blob whose shape depends on `action_type`; future
    // action types (webhook, etc.) reuse the same row.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS post_processing_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            event TEXT NOT NULL,
            action_type TEXT NOT NULL,
            action_config TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )
    .context("Failed to create post_processing_jobs table")?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_pp_jobs_event_enabled \
         ON post_processing_jobs(event) WHERE enabled = 1",
        [],
    )
    .context("Failed to create post_processing_jobs event index")?;

    // Agent profiles describe local coding-agent CLIs (Claude Code, Codex,
    // OpenCode, Cursor Agent, etc.) that can turn a meeting transcript into a
    // persisted artifact. The args are stored as JSON argv tokens — not a shell
    // command — so execution can avoid `sh -c` quoting/injection hazards.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS agent_profiles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            executable TEXT NOT NULL,
            args_json TEXT NOT NULL,
            prompt_mode TEXT NOT NULL DEFAULT 'stdin',
            default_profile INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(kind, executable)
        )",
        [],
    )
    .context("Failed to create agent_profiles table")?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_agent_profiles_enabled \
         ON agent_profiles(enabled)",
        [],
    )
    .context("Failed to create agent_profiles enabled index")?;

    // Durable outputs generated from meetings (summaries, action-item reports,
    // notes). Agent runs move pending → running → completed/error so the UI can
    // show useful failures and preserve stdout/stderr for debugging.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS meeting_artifacts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            meeting_id INTEGER NOT NULL,
            kind TEXT NOT NULL,
            title TEXT NOT NULL,
            template_id TEXT,
            agent_profile_id INTEGER,
            status TEXT NOT NULL,
            content_markdown TEXT,
            error TEXT,
            stdout TEXT,
            stderr TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP,
            FOREIGN KEY(meeting_id) REFERENCES meetings(id),
            FOREIGN KEY(agent_profile_id) REFERENCES agent_profiles(id)
        )",
        [],
    )
    .context("Failed to create meeting_artifacts table")?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_meeting_artifacts_meeting_created \
         ON meeting_artifacts(meeting_id, created_at DESC)",
        [],
    )
    .context("Failed to create meeting_artifacts meeting index")?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_meeting_artifacts_status \
         ON meeting_artifacts(status)",
        [],
    )
    .context("Failed to create meeting_artifacts status index")?;

    migrate_entity_sync_identity(conn, &node_id)?;

    Ok(())
}

fn migrate_entity_sync_identity(conn: &Connection, node_id: &str) -> Result<()> {
    if SyncRepository::entity_identity_version(conn)? >= SyncRepository::ENTITY_IDENTITY_VERSION {
        return Ok(());
    }

    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .context("Failed to begin sync identity migration")?;

    if SyncRepository::entity_identity_version(&tx)? >= SyncRepository::ENTITY_IDENTITY_VERSION {
        return tx
            .commit()
            .context("Failed to finish concurrent sync identity migration");
    }

    backfill_entity_sync_identity(&tx, "workflows", node_id)?;
    backfill_entity_sync_identity(&tx, "meetings", node_id)?;

    tx.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_workflows_sync_id ON workflows(sync_id)",
        [],
    )
    .context("Failed to create workflows sync identity index")?;
    tx.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_meetings_sync_id ON meetings(sync_id)",
        [],
    )
    .context("Failed to create meetings sync identity index")?;
    create_sync_identity_triggers(&tx, "workflows")?;
    create_sync_identity_triggers(&tx, "meetings")?;

    SyncRepository::set_entity_identity_version(&tx, SyncRepository::ENTITY_IDENTITY_VERSION)?;

    tx.commit()
        .context("Failed to commit sync identity migration")
}

fn create_sync_identity_triggers(tx: &Transaction<'_>, table: &str) -> Result<()> {
    let invalid = "NEW.sync_id IS NULL OR trim(NEW.sync_id) = '' \
                   OR NEW.sync_revision IS NULL OR NEW.sync_revision < 0 \
                   OR NEW.origin_node_id IS NULL OR trim(NEW.origin_node_id) = ''";
    for (suffix, event) in [
        ("insert", "INSERT"),
        ("update", "UPDATE OF sync_id, sync_revision, origin_node_id"),
    ] {
        tx.execute(
            &format!(
                "CREATE TRIGGER IF NOT EXISTS {table}_sync_identity_{suffix} \
                 BEFORE {event} ON {table} WHEN {invalid} BEGIN \
                 SELECT RAISE(ABORT, 'invalid {table} sync identity'); END"
            ),
            [],
        )
        .with_context(|| format!("Failed to create {table} sync identity {suffix} trigger"))?;
    }
    Ok(())
}

fn backfill_entity_sync_identity(tx: &Transaction<'_>, table: &str, node_id: &str) -> Result<()> {
    let ids = {
        let mut stmt = tx
            .prepare(&format!(
                "SELECT id FROM {table} WHERE sync_id IS NULL OR trim(sync_id) = ''"
            ))
            .with_context(|| format!("Failed to prepare {table} sync identity backfill"))?;
        let ids = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .with_context(|| format!("Failed to query {table} sync identity backfill"))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("Failed to read {table} rows for sync identity backfill"))?;
        ids
    };

    for id in ids {
        tx.execute(
            &format!(
                "UPDATE {table} SET sync_id = ?1 \
                 WHERE id = ?2 AND (sync_id IS NULL OR trim(sync_id) = '')"
            ),
            params![Uuid::new_v4().to_string(), id],
        )
        .with_context(|| format!("Failed to backfill {table} sync identity"))?;
    }

    tx.execute(
        &format!(
            "UPDATE {table} SET origin_node_id = ?1 \
             WHERE origin_node_id IS NULL OR trim(origin_node_id) = ''"
        ),
        params![node_id],
    )
    .with_context(|| format!("Failed to backfill {table} sync origin"))?;

    let missing: i64 = tx
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM {table} \
                 WHERE sync_id IS NULL OR trim(sync_id) = '' \
                    OR origin_node_id IS NULL OR trim(origin_node_id) = ''"
            ),
            [],
            |row| row.get(0),
        )
        .with_context(|| format!("Failed to verify {table} sync identity"))?;
    if missing != 0 {
        bail!("{table} contains {missing} rows without synchronization identity");
    }

    Ok(())
}

/// Add `column` to `table` only if it isn't already there. SQLite has no
/// `ADD COLUMN IF NOT EXISTS`, and there's no versioned-migration system here,
/// so we inspect `PRAGMA table_info` first and `ALTER` only when missing —
/// keeping `migrate()` safe to run on every startup against any DB vintage.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("Failed to inspect columns of {table}"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .with_context(|| format!("Failed to read columns of {table}"))?
        .filter_map(|c| c.ok())
        .any(|c| c == column);

    if exists {
        return Ok(());
    }

    match conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
        [],
    ) {
        Ok(_) => Ok(()),
        // SQLite has no `ADD COLUMN IF NOT EXISTS`, so the check above and the
        // ALTER below can't be one atomic step: another connection opening the
        // same database (a second daemon, the CLI, concurrent tests) can land
        // its migration in between. Losing that race means the column is now
        // there, which is all we wanted — so treat it as success rather than
        // failing startup.
        Err(err) if is_duplicate_column(&err) => Ok(()),
        Err(err) => Err(err).with_context(|| format!("Failed to add column {column} to {table}")),
    }
}

/// Whether a rusqlite error is SQLite's "duplicate column name" — i.e. the
/// column we were about to add already exists.
fn is_duplicate_column(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(_, Some(msg)) if msg.contains("duplicate column name")
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn migration_marks_legacy_non_empty_titles_manual_and_clears_blank_titles() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meetings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT,
                status TEXT NOT NULL DEFAULT 'recording',
                audio_path TEXT NOT NULL,
                transcript_path TEXT,
                transcript_text TEXT,
                duration_seconds INTEGER,
                started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                completed_at TIMESTAMP,
                error TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );
            INSERT INTO meetings (title, audio_path) VALUES ('  Legacy Planning  ', '/tmp/one.wav');
            INSERT INTO meetings (title, audio_path) VALUES ('   ', '/tmp/two.wav');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let legacy = crate::db::meetings::MeetingRepository::get(&conn, 1)
            .unwrap()
            .unwrap();
        assert_eq!(legacy.title.as_deref(), Some("Legacy Planning"));
        assert_eq!(legacy.title_source.as_deref(), Some("manual"));
        let blank = crate::db::meetings::MeetingRepository::get(&conn, 2)
            .unwrap()
            .unwrap();
        assert_eq!(blank.title, None);
        assert_eq!(blank.title_source, None);
    }

    #[test]
    fn migration_backfills_stable_sync_identity_for_legacy_entities() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE workflows (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_type TEXT NOT NULL,
                text TEXT NOT NULL,
                audio_path TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );
            INSERT INTO workflows (workflow_type, text, audio_path)
            VALUES ('VoiceToText', 'First', '/tmp/first.wav');
            INSERT INTO workflows (workflow_type, text, audio_path)
            VALUES ('VoiceToText', 'Second', '/tmp/second.wav');

            CREATE TABLE meetings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT,
                status TEXT NOT NULL DEFAULT 'recording',
                audio_path TEXT NOT NULL,
                transcript_path TEXT,
                transcript_text TEXT,
                duration_seconds INTEGER,
                started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                completed_at TIMESTAMP,
                error TEXT,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );
            INSERT INTO meetings (title, audio_path) VALUES ('First', '/tmp/first.wav');
            INSERT INTO meetings (title, audio_path) VALUES ('Second', '/tmp/second.wav');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let node_id = SyncRepository::node_id(&conn).unwrap();
        Uuid::parse_str(&node_id).unwrap();
        let node_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_metadata", [], |row| row.get(0))
            .unwrap();
        assert_eq!(node_count, 1);

        let first_pass = sync_rows(&conn);
        assert_eq!(first_pass.len(), 4);
        assert_eq!(
            first_pass
                .iter()
                .map(|(_, id, _, _)| id)
                .collect::<HashSet<_>>()
                .len(),
            4
        );
        for (_, sync_id, revision, origin_node_id) in &first_pass {
            Uuid::parse_str(sync_id).unwrap();
            assert_eq!(*revision, 0);
            assert_eq!(origin_node_id, &node_id);
        }

        migrate(&conn).unwrap();

        assert_eq!(SyncRepository::node_id(&conn).unwrap(), node_id);
        assert_eq!(sync_rows(&conn), first_pass);
        assert_eq!(
            SyncRepository::entity_identity_version(&conn).unwrap(),
            SyncRepository::ENTITY_IDENTITY_VERSION
        );
        for table in ["workflows", "meetings"] {
            let ids: Vec<i64> = conn
                .prepare(&format!("SELECT id FROM {table} ORDER BY id"))
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert_eq!(ids, vec![1, 2]);
        }

        assert!(conn
            .execute(
                "INSERT INTO workflows (workflow_type, text, audio_path) \
                 VALUES ('VoiceToText', 'Missing identity', '/tmp/missing.wav')",
                [],
            )
            .is_err());
        assert!(conn
            .execute(
                "INSERT INTO meetings (audio_path) VALUES ('/tmp/missing.wav')",
                [],
            )
            .is_err());
    }

    #[test]
    fn node_identity_survives_database_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audetic.db");

        let first = init_db_at(&path).unwrap();
        let node_id = SyncRepository::node_id(&first).unwrap();
        drop(first);

        let reopened = init_db_at(&path).unwrap();
        assert_eq!(SyncRepository::node_id(&reopened).unwrap(), node_id);
    }

    fn sync_rows(conn: &Connection) -> Vec<(String, String, i64, String)> {
        let mut rows = Vec::new();
        for table in ["workflows", "meetings"] {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT sync_id, sync_revision, origin_node_id FROM {table} ORDER BY id"
                ))
                .unwrap();
            rows.extend(
                stmt.query_map([], |row| {
                    Ok((table.to_string(), row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap(),
            );
        }
        rows
    }
}
