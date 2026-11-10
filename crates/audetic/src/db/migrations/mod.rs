//! Schema version lives in `PRAGMA user_version`. Migration N is
//! `MIGRATIONS[N - 1]`. Released entries are never edited.
//!
//! Opening a connection (`ensure_current`) creates an empty database and
//! refuses anything else that is not already current. Rewriting a database
//! that already has tables happens only in `upgrade`, which the daemon calls
//! under the instance lock.

mod legacy_adoption;

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
    let (version, legacy_version, pending) = migration_plan(conn, latest)?;
    if let Some(legacy_version) = legacy_version {
        bail!(
            "Database uses legacy schema version {legacy_version}. \
             Restart the daemon to upgrade"
        );
    }
    match pending {
        Plan::Ready => Ok(()),
        Plan::Create => apply_pending(conn, version, migrations),
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
    let (version, legacy_version, pending) = migration_plan(&conn, latest)?;
    if let Some(legacy_version) = legacy_version {
        let backup = pre_upgrade_backup_path(db_path, latest);
        write_backup(&conn, &backup, version)?;
        return adopt_legacy_and_apply_pending(&conn, legacy_version, migrations);
    }
    match pending {
        Plan::Ready => Ok(()),
        Plan::Create => apply_pending(&conn, version, migrations),
        Plan::Upgrade => {
            let backup = pre_upgrade_backup_path(db_path, latest);
            write_backup(&conn, &backup, version)?;
            apply_pending(&conn, version, migrations)
        }
    }
}

fn migration_plan(conn: &Connection, latest: i64) -> Result<(i64, Option<i64>, Plan)> {
    conn.pragma_update(None, "foreign_keys", true)
        .context("Failed to enable foreign keys")?;
    let version = user_version(conn)?;
    let empty = database_is_empty(conn)?;
    if let Some(legacy_version) = legacy_adoption::version_for_adoption(conn, version, empty)? {
        return Ok((version, Some(legacy_version), Plan::Upgrade));
    }
    Ok((version, None, plan(version, latest, empty)?))
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
    // Recognized pre-user_version schemas are handled before this point.
    // Applying the baseline on any other unknown schema would hide it.
    if version == 0 {
        bail!(
            "Refusing to open a non-empty database at user_version 0. \
             The baseline is not applied on top of an unversioned database"
        );
    }
    Ok(Plan::Upgrade)
}

fn apply_pending(conn: &Connection, from: i64, migrations: &[&str]) -> Result<()> {
    apply_migration_transaction(conn, from, None, migrations)
}

fn adopt_legacy_and_apply_pending(
    conn: &Connection,
    legacy_version: i64,
    migrations: &[&str],
) -> Result<()> {
    apply_migration_transaction(conn, 1, Some(legacy_version), migrations)
}

fn apply_migration_transaction(
    conn: &Connection,
    from: i64,
    legacy_version: Option<i64>,
    migrations: &[&str],
) -> Result<()> {
    let latest = migrations.len() as i64;
    if from >= latest && legacy_version.is_none() {
        return Ok(());
    }
    conn.execute_batch("BEGIN EXCLUSIVE")
        .context("Failed to begin exclusive migration transaction")?;
    let applied = (|| {
        if let Some(version) = legacy_version {
            legacy_adoption::apply_in_open_transaction(conn, version)?;
        }
        apply_in_open_transaction(conn, from, migrations)
    })();
    match applied {
        Ok(versions) => {
            conn.execute_batch("COMMIT")
                .context("Failed to commit database migration")?;
            if let Some(version) = legacy_version {
                info!("Adopted legacy schema version {version} as migration 1");
            }
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

    fn schema_snapshot_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/db/schema.sql")
    }

    fn schema_snapshot(conn: &Connection) -> Result<String> {
        let version = user_version(conn)?;
        let mut stmt = conn.prepare(
            "SELECT sql FROM sqlite_master
             WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
             ORDER BY rowid",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = format!("-- user_version {version}\n");
        for sql in rows {
            let sql = sql?;
            out.push_str(&sql);
            if !sql.ends_with(';') {
                out.push(';');
            }
            out.push('\n');
        }
        Ok(out)
    }

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

    const LEGACY_MARKER_SCHEMA: &str = "
        CREATE TABLE audio_notes_schema (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            version INTEGER NOT NULL,
            migrated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
    ";

    const LEGACY_V1_SCHEMA: &str = "
        CREATE TABLE audio_notes (
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
        CREATE INDEX idx_audio_notes_stream ON audio_notes(started_at DESC, id DESC)
            WHERE deleted_at IS NULL;
        CREATE INDEX idx_audio_notes_status ON audio_notes(status);
        CREATE INDEX idx_audio_notes_kind ON audio_notes(json_extract(classification, '$.kind'));
        CREATE TABLE post_processing_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL, event TEXT NOT NULL, action_type TEXT NOT NULL,
            action_config TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX idx_pp_jobs_event_enabled
            ON post_processing_jobs(event) WHERE enabled = 1;
        CREATE TABLE agent_profiles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL, kind TEXT NOT NULL, executable TEXT NOT NULL,
            args_json TEXT NOT NULL, prompt_mode TEXT NOT NULL DEFAULT 'stdin',
            default_profile INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(kind, executable)
        );
        CREATE INDEX idx_agent_profiles_enabled ON agent_profiles(enabled);
        CREATE TABLE audio_note_artifacts (
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
        CREATE INDEX idx_audio_note_artifacts_note_created
            ON audio_note_artifacts(note_id, created_at DESC);
        CREATE INDEX idx_audio_note_artifacts_status ON audio_note_artifacts(status);
    ";

    fn seed_legacy(path: &Path, version: i64) -> Connection {
        let conn = Connection::open(path).unwrap();
        if version == 1 {
            conn.execute_batch(LEGACY_V1_SCHEMA).unwrap();
        } else {
            conn.execute_batch(MIGRATIONS[0]).unwrap();
        }
        conn.execute_batch(LEGACY_MARKER_SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO audio_notes(title, audio_path, transcript_text)
             VALUES('Keep me', '/tmp/keep.wav', 'real transcript')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO audio_notes_schema(singleton, version) VALUES(1, ?1)",
            [version],
        )
        .unwrap();
        conn
    }

    fn assert_legacy_upgrade(path: &Path, legacy_version: i64, enabled: i64) {
        let live = Connection::open(path).unwrap();
        assert_eq!(user_version(&live).unwrap(), LATEST);
        let note: (String, String) = live
            .query_row(
                "SELECT title, transcript_text FROM audio_notes WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(note, ("Keep me".into(), "real transcript".into()));
        let source: (String, String, i64) = live
            .query_row(
                "SELECT provider, source_instance, enabled FROM recording_sources",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(source, ("plaud".into(), "default".into(), enabled));
        assert!(!table_exists(&live, "audio_notes_schema").unwrap());
        assert!(!table_exists(&live, "audio_note_migration_sources").unwrap());
        assert!(!table_exists(&live, "plaud_sync_state").unwrap());

        let backup_path = pre_upgrade_backup_path(path, LATEST);
        assert!(backup_path.exists(), "pre-upgrade backup was not written");
        let backup = Connection::open(backup_path).unwrap();
        assert_eq!(user_version(&backup).unwrap(), 0);
        let saved_version: i64 = backup
            .query_row(
                "SELECT version FROM audio_notes_schema WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(saved_version, legacy_version);
        let saved_title: String = backup
            .query_row("SELECT title FROM audio_notes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(saved_title, "Keep me");
    }

    #[test]
    fn fresh_replay_matches_schema_snapshot() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_current(&conn).unwrap();
        let actual = schema_snapshot(&conn).unwrap();
        assert!(
            !actual.contains("audio_notes_schema"),
            "baseline schema must not recreate the marker table"
        );
        assert!(
            !actual.contains("plaud_sync_state"),
            "provider tables belong in a migration that removes them, not the snapshot"
        );
        assert!(actual.contains("CREATE TABLE recording_sources"));
        assert!(actual.starts_with(&format!("-- user_version {LATEST}\n")));
        let path = schema_snapshot_path();
        let expected = fs::read_to_string(&path).unwrap_or_default();
        if actual != expected {
            fs::write(&path, &actual).unwrap();
            panic!(
                "schema snapshot regenerated at {}. Review the diff and commit schema.sql",
                path.display()
            );
        }
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
    fn upgrades_legacy_v3_database_and_preserves_data_and_sync_state() {
        let (_dir, path) = temp_db();
        {
            let conn = seed_legacy(&path, 3);
            conn.execute(
                "UPDATE plaud_sync_state
                 SET enabled = 1, interval_minutes = 30, last_error = 'keep me'
                 WHERE singleton = 1",
                [],
            )
            .unwrap();
        }

        upgrade(&path).unwrap();

        assert_legacy_upgrade(&path, 3, 1);
        let live = Connection::open(&path).unwrap();
        let state: (i64, String) = live
            .query_row(
                "SELECT interval_minutes, last_error FROM recording_sources",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (30, "keep me".into()));
    }

    #[test]
    fn upgrades_legacy_v1_database_through_old_and_numbered_chains() {
        let (_dir, path) = temp_db();
        drop(seed_legacy(&path, 1));

        upgrade(&path).unwrap();

        assert_legacy_upgrade(&path, 1, 0);
        let live = Connection::open(&path).unwrap();
        assert!(table_exists(&live, "ingress_access_keys").unwrap());
        assert!(table_exists(&live, "external_imports").unwrap());
        let override_value: Option<String> = live
            .query_row(
                "SELECT classification_kind_override FROM audio_notes WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(override_value, None);
    }

    #[test]
    fn adoption_drops_the_optional_legacy_import_marker() {
        let (_dir, path) = temp_db();
        {
            let conn = seed_legacy(&path, 3);
            conn.execute_batch(
                "CREATE TABLE audio_note_migration_sources (
                    source TEXT PRIMARY KEY
                 );
                 INSERT INTO audio_note_migration_sources(source) VALUES('old-import');",
            )
            .unwrap();
        }

        upgrade(&path).unwrap();

        assert_legacy_upgrade(&path, 3, 0);
        let backup = Connection::open(pre_upgrade_backup_path(&path, LATEST)).unwrap();
        assert!(table_exists(&backup, "audio_note_migration_sources").unwrap());
    }

    #[test]
    fn ensure_current_refuses_legacy_database_without_modifying_it() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(LEGACY_MARKER_SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO audio_notes(title, audio_path) VALUES('Keep me', '/tmp/keep.wav')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO audio_notes_schema(singleton, version) VALUES(1, 3)",
            [],
        )
        .unwrap();

        let error = ensure_current(&conn).unwrap_err().to_string();

        assert!(error.contains("Restart the daemon to upgrade"), "{error}");
        assert_eq!(user_version(&conn).unwrap(), 0);
        assert!(table_exists(&conn, "audio_notes_schema").unwrap());
        assert!(table_exists(&conn, "plaud_sync_state").unwrap());
        assert!(!table_exists(&conn, "recording_sources").unwrap());
        let title: String = conn
            .query_row("SELECT title FROM audio_notes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "Keep me");
    }

    #[test]
    fn refuses_unknown_legacy_schema_version_without_backup_or_rewrite() {
        let (_dir, path) = temp_db();
        drop(seed_legacy(&path, 99));

        let error = upgrade(&path).unwrap_err().to_string();

        assert!(
            error.contains("Unsupported legacy schema version 99"),
            "{error}"
        );
        assert!(!pre_upgrade_backup_path(&path, LATEST).exists());
        let conn = Connection::open(&path).unwrap();
        assert_eq!(user_version(&conn).unwrap(), 0);
        assert!(table_exists(&conn, "audio_notes_schema").unwrap());
        assert!(!table_exists(&conn, "recording_sources").unwrap());
        let title: String = conn
            .query_row("SELECT title FROM audio_notes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(title, "Keep me");
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
    fn upgrade_moves_plaud_sync_state_into_recording_sources_and_writes_a_backup() {
        let (_dir, path) = temp_db();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "UPDATE plaud_sync_state
                 SET enabled = 1, interval_minutes = 30,
                     import_after = '2026-01-02 03:04:05',
                     running = 1, last_started_at = '2026-01-02 03:00:00',
                     last_completed_at = '2026-01-01 00:00:00',
                     last_error = 'keep me', updated_at = '2026-01-02 03:00:01'
                 WHERE singleton = 1",
                [],
            )
            .unwrap();
        }

        upgrade(&path).unwrap();

        let backup = pre_upgrade_backup_path(&path, LATEST);
        assert!(backup.exists(), "pre-upgrade backup was not written");
        let saved = Connection::open(&backup).unwrap();
        assert_eq!(user_version(&saved).unwrap(), 1);
        let saved_row: (i64, i64, String) = saved
            .query_row(
                "SELECT enabled, interval_minutes, last_error
                 FROM plaud_sync_state WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(saved_row, (1, 30, "keep me".into()));
        let saved_sources: i64 = saved
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'recording_sources'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(saved_sources, 0);

        let live = Connection::open(&path).unwrap();
        assert_eq!(user_version(&live).unwrap(), LATEST);
        let old_table: i64 = live
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'plaud_sync_state'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_table, 0);
        #[derive(Debug, PartialEq, Eq)]
        struct Carried {
            provider: String,
            source_instance: String,
            sync_mode: String,
            enabled: i64,
            interval_minutes: i64,
            import_after: String,
            running: i64,
            last_started_at: String,
            last_completed_at: String,
            last_error: String,
            updated_at: String,
            config: Option<String>,
        }
        let row = live
            .query_row(
                "SELECT provider, source_instance, sync_mode, enabled, interval_minutes,
                        import_after, running, last_started_at, last_completed_at, last_error,
                        updated_at, config
                 FROM recording_sources",
                [],
                |row| {
                    Ok(Carried {
                        provider: row.get(0)?,
                        source_instance: row.get(1)?,
                        sync_mode: row.get(2)?,
                        enabled: row.get(3)?,
                        interval_minutes: row.get(4)?,
                        import_after: row.get(5)?,
                        running: row.get(6)?,
                        last_started_at: row.get(7)?,
                        last_completed_at: row.get(8)?,
                        last_error: row.get(9)?,
                        updated_at: row.get(10)?,
                        config: row.get(11)?,
                    })
                },
            )
            .unwrap();
        assert_eq!(
            row,
            Carried {
                provider: "plaud".into(),
                source_instance: "default".into(),
                sync_mode: "poll".into(),
                enabled: 1,
                interval_minutes: 30,
                import_after: "2026-01-02 03:04:05".into(),
                running: 1,
                last_started_at: "2026-01-02 03:00:00".into(),
                last_completed_at: "2026-01-01 00:00:00".into(),
                last_error: "keep me".into(),
                updated_at: "2026-01-02 03:00:01".into(),
                config: None,
            }
        );
    }

    #[test]
    fn upgrade_of_a_missing_database_does_not_create_one() {
        let (_dir, path) = temp_db();
        upgrade(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn init_db_at_creates_an_empty_database_at_latest() {
        let (_dir, path) = temp_db();
        let conn = crate::db::init_db_at(&path).unwrap();
        assert_eq!(user_version(&conn).unwrap(), LATEST);
        let notes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'audio_notes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(notes, 1);
    }
}
