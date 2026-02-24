use anyhow::{Context, Result};
use rusqlite::Connection;

pub fn init_db() -> Result<Connection> {
    let db_path = crate::global::db_file()?;

    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }

    let conn = Connection::open(&db_path).context("Failed to open database connection")?;

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
            duration_seconds INTEGER,
            started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP,
            error TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )
    .context("Failed to create meetings table")?;

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

    Ok(())
}
