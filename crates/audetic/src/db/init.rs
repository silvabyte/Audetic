use anyhow::{Context, Result};
use rusqlite::Connection;
use std::time::Duration;

pub fn init_db() -> Result<Connection> {
    let db_path = crate::global::db_file()?;

    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }

    let conn = Connection::open(&db_path).context("Failed to open database connection")?;

    // The daemon opens a fresh connection per request, so recording-history
    // writes, meeting writes, and API reads overlap. Wait for the write lock
    // instead of failing immediately with SQLITE_BUSY.
    conn.busy_timeout(Duration::from_secs(5))
        .context("Failed to set SQLite busy timeout")?;

    migrate(&conn)?;

    Ok(conn)
}

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS workflows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_type TEXT NOT NULL,
            text TEXT NOT NULL,
            audio_path TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )
    .context("Failed to create workflows table")?;

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
            status TEXT NOT NULL DEFAULT 'recording',
            audio_path TEXT NOT NULL,
            transcript_path TEXT,
            transcript_text TEXT,
            transcript_segments TEXT,
            duration_seconds INTEGER,
            started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP,
            error TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            deleted_at TIMESTAMP
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

    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )
        .with_context(|| format!("Failed to add column {column} to {table}"))?;
    }
    Ok(())
}
