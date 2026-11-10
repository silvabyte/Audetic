use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

pub fn init_db() -> Result<Connection> {
    init_db_at(&crate::global::db_file()?)
}

/// Open a database and make sure it is safe to use.
///
/// An empty file is created at the current schema. A database that already
/// has tables is only accepted when its applied migration count matches this
/// build. Older populated databases are left untouched; the daemon upgrades
/// those from `run_service` under the instance lock.
pub fn init_db_at(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }
    let conn = Connection::open(db_path).context("Failed to open database connection")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    super::migrations::ensure_current(&conn)?;
    Ok(conn)
}
