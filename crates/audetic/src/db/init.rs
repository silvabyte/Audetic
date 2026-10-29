use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

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

/// Create a fresh database, or confirm an existing one is current. This runs on
/// every connection: crash recovery, business-state normalization, and
/// upgrades of existing data do NOT belong here.
pub fn migrate(conn: &Connection) -> Result<()> {
    crate::db::migrations::ensure_current(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
