//! Applied migrations are recorded with SHA-256 checksums in
//! `schema_migrations`. Migration N is `MIGRATIONS[N - 1]`. Released entries
//! are never edited because startup refuses a changed applied migration.
//!
//! Opening a connection (`ensure_current`) creates an empty database and
//! refuses anything else that is not already current. Rewriting a database
//! that already has tables happens only in `upgrade`, which the daemon calls
//! under the instance lock.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use tracing::info;

use std::fmt::Write;
use std::path::{Path, PathBuf};

const SCHEMA_MIGRATIONS_DDL: &str = "CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY CHECK(version > 0),
    checksum TEXT NOT NULL CHECK(length(checksum) = 64),
    applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);";

#[derive(Clone, Copy)]
struct Migration {
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "0001_baseline.sql",
        sql: include_str!("0001_baseline.sql"),
    },
    Migration {
        name: "0002_recording_sources.sql",
        sql: include_str!("0002_recording_sources.sql"),
    },
];
#[cfg(test)]
const LATEST: usize = MIGRATIONS.len();

pub fn ensure_current(conn: &Connection) -> Result<()> {
    ensure_with(conn, MIGRATIONS)
}

pub(crate) fn upgrade(db_path: &Path) -> Result<()> {
    upgrade_with(db_path, MIGRATIONS)
}

fn ensure_with(conn: &Connection, migrations: &[Migration]) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)
        .context("Failed to enable foreign keys")?;
    match applied_count(conn)? {
        Some(applied) if applied == migrations.len() => Ok(()),
        Some(applied) if applied > migrations.len() => bail!(
            "Database was written by a newer build ({applied} migrations applied; this build supports {})",
            migrations.len()
        ),
        Some(applied) => bail!(
            "Database has {applied} migrations applied and this build requires {}. \
             Restart the daemon to upgrade",
            migrations.len()
        ),
        None if database_is_empty(conn)? => {
            apply_migration_transaction(conn, 0, true, migrations)
        }
        None => bail!(
            "Refusing to open a non-empty database without schema_migrations; run scripts/stamp-legacy-db.sh."
        ),
    }
}

fn upgrade_with(db_path: &Path, migrations: &[Migration]) -> Result<()> {
    if !db_path.exists() {
        return Ok(());
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", true)
        .context("Failed to enable foreign keys")?;
    let Some(_) = applied_count(&conn)? else {
        if database_is_empty(&conn)? {
            return apply_migration_transaction(&conn, 0, true, migrations);
        }
        bail!(
            "Refusing to open a non-empty database without schema_migrations; run scripts/stamp-legacy-db.sh."
        );
    };
    let applied = verify_applied_migrations(&conn, migrations)?;
    if applied == migrations.len() {
        return Ok(());
    }
    let backup = pre_upgrade_backup_path(db_path, migrations.len());
    write_backup(&conn, &backup, applied)?;
    apply_migration_transaction(&conn, applied, false, migrations)
}

fn apply_migration_transaction(
    conn: &Connection,
    from: usize,
    create_tracking_table: bool,
    migrations: &[Migration],
) -> Result<()> {
    if from >= migrations.len() && !create_tracking_table {
        return Ok(());
    }
    conn.execute_batch("BEGIN EXCLUSIVE")
        .context("Failed to begin exclusive migration transaction")?;
    let applied = apply_in_open_transaction(conn, from, create_tracking_table, migrations);
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
    from: usize,
    create_tracking_table: bool,
    migrations: &[Migration],
) -> Result<Vec<usize>> {
    if create_tracking_table {
        conn.execute_batch(SCHEMA_MIGRATIONS_DDL)
            .context("Failed to create schema_migrations")?;
    }
    let mut applied = Vec::new();
    for (index, migration) in migrations.iter().enumerate().skip(from) {
        let version = index + 1;
        conn.execute_batch(migration.sql)
            .with_context(|| format!("Failed to apply migration {version}"))?;
        let checksum = migration_checksum(migration);
        conn.execute(
            "INSERT INTO schema_migrations(version, checksum) VALUES (?1, ?2)",
            params![version as i64, checksum],
        )
        .with_context(|| format!("Failed to record migration {version}"))?;
        applied.push(version);
    }
    integrity_check(conn)?;
    foreign_key_check(conn)?;
    Ok(applied)
}

fn applied_count(conn: &Connection) -> Result<Option<usize>> {
    match conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(count) => Ok(Some(
            usize::try_from(count).context("schema_migrations row count is negative")?,
        )),
        Err(error) if missing_schema_migrations_error(&error) => {
            if table_exists(conn, "schema_migrations")? {
                Err(error).context("Failed to count applied migrations")
            } else {
                Ok(None)
            }
        }
        Err(error) => Err(error).context("Failed to count applied migrations"),
    }
}

fn missing_schema_migrations_error(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(_, Some(message))
            if message.contains("no such table: schema_migrations")
    )
}

fn verify_applied_migrations(conn: &Connection, migrations: &[Migration]) -> Result<usize> {
    let mut stmt = conn
        .prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")
        .context("Failed to read applied migrations")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let applied = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if applied.len() > migrations.len() {
        bail!(
            "Database was written by a newer build ({} migrations applied; this build supports {})",
            applied.len(),
            migrations.len()
        );
    }
    for (index, (version, checksum)) in applied.iter().enumerate() {
        let expected_version = index + 1;
        if *version != expected_version as i64 {
            bail!(
                "Invalid schema_migrations sequence: expected version {expected_version}, found {version}"
            );
        }
        let migration = &migrations[index];
        if checksum != &migration_checksum(migration) {
            bail!(
                "migration {expected_version} ({}) changed after it was applied to this database",
                migration.name
            );
        }
    }
    Ok(applied.len())
}

fn migration_checksum(migration: &Migration) -> String {
    let digest = Sha256::digest(migration.sql.as_bytes());
    let mut checksum = String::with_capacity(64);
    for byte in digest {
        write!(&mut checksum, "{byte:02x}").expect("writing to a String cannot fail");
    }
    checksum
}

fn write_backup(conn: &Connection, backup: &Path, applied: usize) -> Result<()> {
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
        "Backing up database with {applied} applied migrations to {}",
        backup.display()
    );
    conn.execute_batch(&format!("VACUUM INTO '{literal}'"))
        .with_context(|| format!("Failed to back up database to {}", backup.display()))?;
    std::fs::rename(&temp, backup)
        .with_context(|| format!("Failed to install database backup at {}", backup.display()))
}

fn pre_upgrade_backup_path(db_path: &Path, target_version: usize) -> PathBuf {
    let file_name = db_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audetic.db".to_string());
    db_path.with_file_name(format!("{file_name}.pre-v{target_version}.sqlite3"))
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

    const TEST_MIGRATIONS: &[Migration] = &[
        Migration {
            name: "0001_notes.sql",
            sql: "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT NOT NULL);",
        },
        Migration {
            name: "0002_note_body.sql",
            sql: "ALTER TABLE notes ADD COLUMN body TEXT;",
        },
    ];

    fn temp_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audetic.db");
        (dir, path)
    }

    fn seed_applied(path: &Path, migrations: &[Migration], count: usize) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(SCHEMA_MIGRATIONS_DDL).unwrap();
        for (index, migration) in migrations.iter().enumerate().take(count) {
            conn.execute_batch(migration.sql).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations(version, checksum) VALUES (?1, ?2)",
                params![(index + 1) as i64, migration_checksum(migration)],
            )
            .unwrap();
        }
        conn
    }

    fn migration_rows(conn: &Connection) -> Vec<(i64, String)> {
        let mut stmt = conn
            .prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn fresh_replay_records_every_migration_checksum() {
        let (_dir, path) = temp_db();
        let conn = crate::db::init_db_at(&path).unwrap();
        assert!(table_exists(&conn, "audio_notes").unwrap());
        assert!(table_exists(&conn, "recording_sources").unwrap());
        assert!(!table_exists(&conn, "audio_notes_schema").unwrap());
        assert!(!table_exists(&conn, "plaud_sync_state").unwrap());
        assert_eq!(
            migration_rows(&conn),
            MIGRATIONS
                .iter()
                .enumerate()
                .map(|(index, migration)| { ((index + 1) as i64, migration_checksum(migration)) })
                .collect::<Vec<_>>()
        );
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
        assert_eq!(migration_rows(&conn).len(), LATEST);
    }

    #[test]
    fn refuses_gaps_extra_rows_and_newer_databases() {
        let (_dir, path) = temp_db();
        let file = seed_applied(&path, MIGRATIONS, LATEST);
        file.execute(
            "UPDATE schema_migrations SET version = 3 WHERE version = 2",
            [],
        )
        .unwrap();
        drop(file);
        let error = upgrade(&path).unwrap_err().to_string();
        assert!(error.contains("expected version 2, found 3"), "{error}");

        let (_dir, path) = temp_db();
        let file = seed_applied(&path, MIGRATIONS, LATEST);
        file.execute(
            "INSERT INTO schema_migrations(version, checksum) VALUES (3, ?1)",
            ["0".repeat(64)],
        )
        .unwrap();
        drop(file);
        let opened = Connection::open(&path).unwrap();
        let error = ensure_current(&opened).unwrap_err().to_string();
        assert!(error.contains("written by a newer build"), "{error}");
        drop(opened);
        let error = upgrade(&path).unwrap_err().to_string();
        assert!(error.contains("written by a newer build"), "{error}");
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
            error.contains("non-empty database without schema_migrations"),
            "{error}"
        );
        assert!(error.contains("scripts/stamp-legacy-db.sh"), "{error}");
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
            error.contains("non-empty database without schema_migrations"),
            "{error}"
        );
        let backup = pre_upgrade_backup_path(&path, LATEST);
        assert!(!backup.exists());
    }

    #[test]
    fn ensure_current_neither_upgrades_nor_verifies_checksums() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_MIGRATIONS_DDL).unwrap();
        conn.execute_batch(TEST_MIGRATIONS[0].sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations(version, checksum) VALUES (1, ?1)",
            ["0".repeat(64)],
        )
        .unwrap();
        conn.execute("INSERT INTO notes(title) VALUES('keep')", [])
            .unwrap();

        let error = ensure_with(&conn, TEST_MIGRATIONS).unwrap_err().to_string();
        assert!(error.contains("Restart the daemon to upgrade"), "{error}");
        assert_eq!(migration_rows(&conn), vec![(1, "0".repeat(64))]);
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

        conn.execute_batch(TEST_MIGRATIONS[1].sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations(version, checksum) VALUES (2, ?1)",
            ["0".repeat(64)],
        )
        .unwrap();
        ensure_with(&conn, TEST_MIGRATIONS).unwrap();
    }

    #[test]
    fn upgrade_backs_up_database_that_has_data() {
        let (_dir, path) = temp_db();
        {
            let conn = seed_applied(&path, TEST_MIGRATIONS, 1);
            conn.execute("INSERT INTO notes(title) VALUES('keep')", [])
                .unwrap();
        }

        upgrade_with(&path, TEST_MIGRATIONS).unwrap();

        let backup = pre_upgrade_backup_path(&path, 2);
        assert_eq!(
            backup.file_name().unwrap().to_str().unwrap(),
            "audetic.db.pre-v2.sqlite3"
        );
        let saved = Connection::open(&backup).unwrap();
        assert_eq!(migration_rows(&saved).len(), 1);
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
        assert_eq!(migration_rows(&live).len(), 2);
        let row: (String, Option<String>) = live
            .query_row("SELECT title, body FROM notes", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row, ("keep".into(), None));
    }

    #[test]
    fn checksum_mismatch_is_refused_without_changes_or_backup() {
        let (_dir, path) = temp_db();
        {
            let conn = seed_applied(&path, TEST_MIGRATIONS, 1);
            conn.execute("INSERT INTO notes(title) VALUES('keep')", [])
                .unwrap();
            conn.execute(
                "UPDATE schema_migrations SET checksum = ?1 WHERE version = 1",
                ["0".repeat(64)],
            )
            .unwrap();
        }

        let error = upgrade_with(&path, TEST_MIGRATIONS)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(
                "migration 1 (0001_notes.sql) changed after it was applied to this database"
            ),
            "{error}"
        );
        assert!(!pre_upgrade_backup_path(&path, 2).exists());
        let conn = Connection::open(&path).unwrap();
        assert_eq!(migration_rows(&conn), vec![(1, "0".repeat(64))]);
        let title: String = conn
            .query_row("SELECT title FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(title, "keep");
    }

    #[test]
    fn failing_step_rolls_back_and_leaves_version_unchanged() {
        let (_dir, path) = temp_db();
        let migrations = &[
            Migration {
                name: "0001_notes.sql",
                sql: "CREATE TABLE notes(id INTEGER PRIMARY KEY, title TEXT NOT NULL);",
            },
            Migration {
                name: "0002_broken.sql",
                sql: "ALTER TABLE missing_table ADD COLUMN body TEXT;",
            },
        ];
        {
            let conn = seed_applied(&path, migrations, 1);
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
        assert_eq!(migration_rows(&conn).len(), 1);
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
        {
            let conn = seed_applied(&path, TEST_MIGRATIONS, 1);
            conn.execute("INSERT INTO notes(title) VALUES('keep')", [])
                .unwrap();
        }
        let backup = pre_upgrade_backup_path(&path, 2);
        fs::write(&backup, b"not a database").unwrap();

        upgrade_with(&path, TEST_MIGRATIONS).unwrap();

        let saved = Connection::open(&backup).unwrap();
        assert_eq!(migration_rows(&saved).len(), 1);
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
