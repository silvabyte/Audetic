//! Schema version lives in `PRAGMA user_version`. Migration N is
//! `MIGRATIONS[N - 1]`. Released entries are never edited.
//!
//! Opening a connection (`ensure_current`) creates an empty database and
//! refuses anything else that is not already current. Rewriting a database
//! that already has tables happens only in `upgrade`, which the daemon calls
//! under the instance lock.

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use tracing::info;

use std::path::{Path, PathBuf};

const MIGRATIONS: &[&str] = &[
    include_str!("0001_baseline.sql"),
    include_str!("0002_recording_sources.sql"),
];
#[cfg(test)]
const LATEST: i64 = MIGRATIONS.len() as i64;

pub fn ensure_current(conn: &Connection) -> Result<()> {
    ensure_with(conn, MIGRATIONS)
}

pub(crate) fn upgrade(db_path: &Path) -> Result<()> {
    upgrade_with(db_path, MIGRATIONS)
}

fn ensure_with(conn: &Connection, migrations: &[&str]) -> Result<()> {
    let latest = migrations.len() as i64;
    conn.pragma_update(None, "foreign_keys", true)
        .context("Failed to enable foreign keys")?;
    let version = user_version(conn)?;
    if version == latest {
        return Ok(());
    }
    match migration_plan_for_version(conn, version, latest)? {
        Plan::Ready => Ok(()),
        Plan::Create => apply_migration_transaction(conn, version, migrations),
        Plan::Upgrade => bail!(
            "Database is at user_version {version} and this build supports {latest}. \
             Restart the daemon to upgrade"
        ),
    }
}

fn upgrade_with(db_path: &Path, migrations: &[&str]) -> Result<()> {
    let latest = migrations.len() as i64;
    if !db_path.exists() {
        return Ok(());
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let (version, pending) = migration_plan(&conn, latest)?;
    match pending {
        Plan::Ready => return Ok(()),
        Plan::Create => return apply_migration_transaction(&conn, version, migrations),
        Plan::Upgrade => {}
    }
    let backup = pre_upgrade_backup_path(db_path, latest);
    write_backup(&conn, &backup, version)?;
    apply_migration_transaction(&conn, version, migrations)
}

fn migration_plan(conn: &Connection, latest: i64) -> Result<(i64, Plan)> {
    conn.pragma_update(None, "foreign_keys", true)
        .context("Failed to enable foreign keys")?;
    let version = user_version(conn)?;
    Ok((version, migration_plan_for_version(conn, version, latest)?))
}

fn migration_plan_for_version(conn: &Connection, version: i64, latest: i64) -> Result<Plan> {
    let empty = database_is_empty(conn)?;
    plan(version, latest, empty)
}

/// What opening or startup may do with this file.
enum Plan {
    /// `user_version` is already this build's schema.
    Ready,
    /// No user tables yet, so the pending chain may be applied in place.
    Create,
    /// User tables exist at an older version. Only `upgrade` may rewrite them.
    Upgrade,
}

fn plan(version: i64, latest: i64, empty: bool) -> Result<Plan> {
    if version == latest {
        return Ok(Plan::Ready);
    }
    if version > latest {
        bail!(
            "Database was written by a newer build (user_version {version}; this build supports {latest})"
        );
    }
    if empty {
        return Ok(Plan::Create);
    }
    if version == 0 {
        bail!("Refusing to open a non-empty database at user_version 0; run scripts/stamp-legacy-db.sh.");
    }
    Ok(Plan::Upgrade)
}

fn apply_migration_transaction(conn: &Connection, from: i64, migrations: &[&str]) -> Result<()> {
    let latest = migrations.len() as i64;
    if from >= latest {
        return Ok(());
    }
    conn.execute_batch("BEGIN EXCLUSIVE")
        .context("Failed to begin exclusive migration transaction")?;
    let applied = apply_in_open_transaction(conn, from, migrations);
    match applied {
        Ok(versions) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit database migration")?;
            for version in versions {
                info!("Applied database migration {version}");
            }
            Ok(())
        }
        Err(error) => match conn.execute_batch("ROLLBACK") {
            Ok(()) => Err(error),
            Err(rollback) => Err(error.context(format!(
                "database migration failed, and rolling back also failed: {rollback:#}"
            ))),
        },
    }
}

fn apply_in_open_transaction(
    conn: &Connection,
    from: i64,
    migrations: &[&str],
) -> Result<Vec<i64>> {
    let latest = migrations.len() as i64;
    let mut applied = Vec::new();
    for version in (from + 1)..=latest {
        let sql = migrations[(version - 1) as usize];
        conn.execute_batch(sql)
            .with_context(|| format!("Failed to apply migration {version}"))?;
        applied.push(version);
    }
    integrity_check(conn)?;
    foreign_key_check(conn)?;
    conn.pragma_update(None, "user_version", latest)
        .with_context(|| format!("Failed to set user_version to {latest}"))?;
    Ok(applied)
}

fn write_backup(conn: &Connection, backup: &Path, version: i64) -> Result<()> {
    let mut temp = backup.as_os_str().to_os_string();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    match std::fs::remove_file(&temp) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to remove stale backup at {}", temp.display()));
        }
    }
    let text = temp.to_str().context("Backup path is not valid UTF-8")?;
    let literal = text.replace('\'', "''");
    info!(
        "Backing up database at user_version {version} to {}",
        backup.display()
    );
    conn.execute_batch(&format!("VACUUM INTO '{literal}'"))
        .with_context(|| format!("Failed to back up database to {}", backup.display()))?;
    std::fs::rename(&temp, backup)
        .with_context(|| format!("Failed to install database backup at {}", backup.display()))
}

fn pre_upgrade_backup_path(db_path: &Path, target_version: i64) -> PathBuf {
    let file_name = db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audetic.db".to_string());
    db_path.with_file_name(format!("{file_name}.pre-v{target_version}.sqlite3"))
}

fn user_version(conn: &Connection) -> Result<i64> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("Failed to read user_version")
}

fn database_is_empty(conn: &Connection) -> Result<bool> {
    let tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok(tables == 0)
}

#[cfg(test)]
fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )?)
}

fn integrity_check(conn: &Connection) -> Result<()> {
    let status: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .context("Failed to run integrity_check")?;
    if status != "ok" {
        bail!("integrity_check failed: {status}");
    }
    Ok(())
}

fn foreign_key_check(conn: &Connection) -> Result<()> {
    let mut stmt = conn
        .prepare("PRAGMA foreign_key_check")
        .context("Failed to run foreign_key_check")?;
    let rows = stmt.query_map([], |row| {
        let table: String = row.get(0)?;
        let rowid: i64 = row.get(1)?;
        Ok(format!("{table} rowid {rowid}"))
    })?;
    let mut problems = Vec::new();
    for row in rows {
        problems.push(row?);
    }
    if !problems.is_empty() {
        bail!("foreign_key_check failed: {}", problems.join(", "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    fn temp_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audetic.db");
        (dir, path)
    }

    fn seed_version(path: &Path, sql: &str, version: i64) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(sql).unwrap();
        conn.pragma_update(None, "user_version", version).unwrap();
        conn
    }

    #[test]
    fn fresh_replay_reaches_latest() {
        let (_dir, path) = temp_db();
        let conn = crate::db::init_db_at(&path).unwrap();
        assert_eq!(user_version(&conn).unwrap(), LATEST);
        assert!(table_exists(&conn, "audio_notes").unwrap());
        assert!(table_exists(&conn, "recording_sources").unwrap());
        assert!(!table_exists(&conn, "audio_notes_schema").unwrap());
        assert!(!table_exists(&conn, "plaud_sync_state").unwrap());
    }

    #[test]
    fn opening_connection_does_not_normalize_or_recover_business_state() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_current(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO audio_notes(title,audio_path,enrichment_status) \
             VALUES('  untouched  ','audio.wav','running');",
        )
        .unwrap();
        ensure_current(&conn).unwrap();
        let row: (String, String) = conn
            .query_row(
                "SELECT title, enrichment_status FROM audio_notes",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, ("  untouched  ".into(), "running".into()));
        assert_eq!(user_version(&conn).unwrap(), LATEST);
    }

    #[test]
    fn refuses_database_written_by_a_newer_build() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_current(&conn).unwrap();
        conn.pragma_update(None, "user_version", LATEST + 1)
            .unwrap();
        let error = ensure_current(&conn).unwrap_err().to_string();
        assert!(error.contains("written by a newer build"), "{error}");
        assert_eq!(user_version(&conn).unwrap(), LATEST + 1);

        let (_dir, path) = temp_db();
        let file = Connection::open(&path).unwrap();
        file.execute_batch("CREATE TABLE notes(id INTEGER PRIMARY KEY);")
            .unwrap();
        file.pragma_update(None, "user_version", LATEST + 1)
            .unwrap();
        drop(file);
        let error = upgrade(&path).unwrap_err().to_string();
        assert!(error.contains("written by a newer build"), "{error}");
        assert_eq!(
            user_version(&Connection::open(&path).unwrap()).unwrap(),
            LATEST + 1
        );
    }

    #[test]
    fn refuses_nonempty_unversioned_database() {
        let (_dir, path) = temp_db();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE legacy(id INTEGER PRIMARY KEY);
             INSERT INTO legacy(id) VALUES (1);",
        )
        .unwrap();
        drop(conn);

        let opened = Connection::open(&path).unwrap();
        let error = ensure_current(&opened).unwrap_err().to_string();
        assert!(
            error.contains("non-empty database at user_version 0"),
            "{error}"
        );
        assert!(error.contains("scripts/stamp-legacy-db.sh"), "{error}");
        assert_eq!(user_version(&opened).unwrap(), 0);
        let legacy: i64 = opened
            .query_row("SELECT COUNT(*) FROM legacy", [], |row| row.get(0))
            .unwrap();
        assert_eq!(legacy, 1);
        assert!(!database_is_empty(&opened).unwrap());
        let audio_notes: i64 = opened
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'audio_notes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audio_notes, 0);
        drop(opened);

        let error = upgrade(&path).unwrap_err().to_string();
        assert!(
            error.contains("non-empty database at user_version 0"),
            "{error}"
        );
        let backup = pre_upgrade_backup_path(&path, LATEST);
        assert!(!backup.exists());
    }

    #[test]
    fn ensure_current_does_not_upgrade_a_populated_database() {
        let conn = Connection::open_in_memory().unwrap();
        let migrations = &[
            "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT NOT NULL);",
            "ALTER TABLE notes ADD COLUMN body TEXT;",
        ];
        conn.execute_batch(migrations[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute("INSERT INTO notes(title) VALUES('keep')", [])
            .unwrap();

        let error = ensure_with(&conn, migrations).unwrap_err().to_string();
        assert!(error.contains("Restart the daemon to upgrade"), "{error}");
        assert_eq!(user_version(&conn).unwrap(), 1);
        let title: String = conn
            .query_row("SELECT title FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "keep");
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(notes)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .map(|name| name.unwrap())
            .collect();
        assert_eq!(columns, vec!["id".to_string(), "title".to_string()]);
    }

    #[test]
    fn upgrade_backs_up_database_that_has_data() {
        let (_dir, path) = temp_db();
        let migrations = &[
            "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT NOT NULL);",
            "ALTER TABLE notes ADD COLUMN body TEXT;",
        ];
        {
            let conn = seed_version(&path, migrations[0], 1);
            conn.execute("INSERT INTO notes(title) VALUES('keep')", [])
                .unwrap();
        }

        upgrade_with(&path, migrations).unwrap();

        let backup = pre_upgrade_backup_path(&path, 2);
        assert_eq!(
            backup.file_name().unwrap().to_str().unwrap(),
            "audetic.db.pre-v2.sqlite3"
        );
        let saved = Connection::open(&backup).unwrap();
        assert_eq!(user_version(&saved).unwrap(), 1);
        let title: String = saved
            .query_row("SELECT title FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "keep");
        let saved_columns: i64 = saved
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('notes') WHERE name = 'body'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(saved_columns, 0);

        let live = Connection::open(&path).unwrap();
        assert_eq!(user_version(&live).unwrap(), 2);
        let row: (String, Option<String>) = live
            .query_row("SELECT title, body FROM notes", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row, ("keep".into(), None));
    }

    #[test]
    fn failing_step_rolls_back_and_leaves_version_unchanged() {
        let (_dir, path) = temp_db();
        let migrations = &[
            "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT NOT NULL);",
            "ALTER TABLE missing_table ADD COLUMN body TEXT;",
        ];
        {
            let conn = seed_version(&path, migrations[0], 1);
            conn.execute("INSERT INTO notes(title) VALUES('keep')", [])
                .unwrap();
        }

        let error = upgrade_with(&path, migrations).unwrap_err();
        assert!(
            error.to_string().contains("Failed to apply migration 2"),
            "{error:#}"
        );
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("UPDATE notes SET title = 'newer'", [])
                .unwrap();
        }
        let retried = upgrade_with(&path, migrations).unwrap_err();
        assert!(
            retried.to_string().contains("Failed to apply migration 2"),
            "{retried:#}"
        );

        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 1);
        let title: String = conn
            .query_row("SELECT title FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "newer");
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'notes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 1);

        let backup = Connection::open(pre_upgrade_backup_path(&path, 2)).unwrap();
        let backed_up_title: String = backup
            .query_row("SELECT title FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(backed_up_title, "newer");
    }

    #[test]
    fn preexisting_junk_backup_is_replaced_with_a_valid_database() {
        let (_dir, path) = temp_db();
        let migrations = &[
            "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT NOT NULL);",
            "ALTER TABLE notes ADD COLUMN body TEXT;",
        ];
        {
            let conn = seed_version(&path, migrations[0], 1);
            conn.execute("INSERT INTO notes(title) VALUES('keep')", [])
                .unwrap();
        }
        let backup = pre_upgrade_backup_path(&path, 2);
        fs::write(&backup, b"not a database").unwrap();

        upgrade_with(&path, migrations).unwrap();

        let saved = Connection::open(&backup).unwrap();
        assert_eq!(user_version(&saved).unwrap(), 1);
        let title: String = saved
            .query_row("SELECT title FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "keep");
    }

    #[test]
    fn upgrade_of_a_missing_database_does_not_create_one() {
        let (_dir, path) = temp_db();
        upgrade(&path).unwrap();
        assert!(!path.exists());
    }
}
