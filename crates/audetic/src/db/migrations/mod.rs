//! Versioned schema migrations.
//!
//! `detect_version` is the only place that decides what version a database is
//! at. `MIGRATIONS` lists every step in order; the runner applies the steps
//! between the detected version and `LATEST`. Released steps are frozen:
//! change the schema by appending a step. Fresh databases replay the whole
//! chain, so no second copy of the schema can drift from it.
//!
//! Databases holding data are upgraded only by [`migrate_database`], which the
//! daemon runs at startup under its instance lock. It takes a SQLite-consistent
//! backup, then applies every pending step in one exclusive transaction, so a
//! failure leaves the database exactly as it was.

mod v1;
mod v2;
mod v3;

use anyhow::{ensure, Context, Result};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::fmt;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::db::init::table_exists;

struct Migration {
    version: i64,
    description: &'static str,
    /// Returns human-readable notes about what the step changed.
    apply: fn(&Connection) -> Result<Vec<String>>,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "Create Audio Notes and import legacy meetings and dictations",
        apply: v1::apply,
    },
    Migration {
        version: 2,
        description: "Add external imports, ingress keys, and Plaud sync",
        apply: v2::apply,
    },
    Migration {
        version: 3,
        description: "Add classification kind override",
        apply: v3::apply,
    },
];

pub const LATEST: i64 = MIGRATIONS[MIGRATIONS.len() - 1].version;

#[derive(Debug)]
pub struct MigrationOutcome {
    pub database: PathBuf,
    pub dry_run: bool,
    pub from: i64,
    pub to: i64,
    pub backup_path: Option<PathBuf>,
    pub steps: Vec<StepOutcome>,
}

#[derive(Debug)]
pub struct StepOutcome {
    pub version: i64,
    pub description: &'static str,
    pub notes: Vec<String>,
}

impl fmt::Display for MigrationOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.steps.is_empty() {
            return write!(
                f,
                "Database schema is current (version {}): {}",
                self.to,
                self.database.display()
            );
        }
        let verb = if self.dry_run {
            "Would migrate"
        } else {
            "Migrated"
        };
        write!(
            f,
            "{verb} database schema from version {} to {}: {}",
            self.from,
            self.to,
            self.database.display()
        )?;
        for step in &self.steps {
            write!(f, "\n  v{}: {}", step.version, step.description)?;
            for note in &step.notes {
                write!(f, "\n      {note}")?;
            }
        }
        if let Some(backup) = &self.backup_path {
            write!(f, "\n  Backup: {}", backup.display())?;
        }
        Ok(())
    }
}

/// Version 0 means no Audio Notes marker: an empty database, or one written by
/// a pre-Audio-Notes release. Step 1 imports whatever legacy tables exist.
fn detect_version(conn: &Connection) -> Result<i64> {
    if table_exists(conn, "audio_notes_schema")? {
        ensure!(
            !v1::has_legacy_tables(conn)?,
            "Mixed legacy and Audio Notes schemas detected; refusing an ambiguous migration"
        );
        let version: i64 = conn.query_row(
            "SELECT version FROM audio_notes_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        ensure!(
            (1..=LATEST).contains(&version),
            "Unsupported Audio Notes schema version {version}; this build supports up to {LATEST}"
        );
        return Ok(version);
    }
    ensure!(
        !table_exists(conn, "audio_notes")? && !table_exists(conn, "audio_note_artifacts")?,
        "Unversioned Audio Notes tables detected; refusing to guess their schema. Restore a known database backup or inspect the schema offline."
    );
    Ok(0)
}

fn is_empty(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%')",
        [],
        |row| row.get(0),
    )?)
}

/// Per-connection check. An empty database is brought to `LATEST` in place
/// (there is nothing to back up); any other outdated database is refused, so
/// opening a connection never rewrites existing data.
pub(crate) fn ensure_current(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", true)?;
    let version = detect_version(conn)?;
    if version == LATEST {
        return Ok(());
    }
    ensure!(
        is_empty(conn)?,
        "Database schema is at version {version}, but this build needs version {LATEST}. \
         Restart the Audetic daemon to upgrade it; it backs the database up first."
    );
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Exclusive)?;
    run_steps(&tx, version, LATEST)?;
    tx.commit().context("Failed to commit new database schema")
}

/// Upgrade the database at `path` to `LATEST`. The caller must hold the daemon
/// instance lock. A dry run applies the steps to an in-memory copy instead, so
/// it reports exactly what the real run would do without writing anything.
pub fn migrate_database(path: &Path, dry_run: bool) -> Result<MigrationOutcome> {
    ensure!(
        path.is_file(),
        "Database does not exist: {}",
        path.display()
    );
    let flags = if dry_run {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = Connection::open_with_flags(path, flags).context("Failed to open database")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    validate_integrity(&conn)?;
    let from = detect_version(&conn)?;
    let mut outcome = MigrationOutcome {
        database: path.to_path_buf(),
        dry_run,
        from,
        to: LATEST,
        backup_path: None,
        steps: Vec::new(),
    };
    if from == LATEST {
        return Ok(outcome);
    }

    if dry_run {
        let mut scratch = Connection::open_in_memory()?;
        copy_snapshot(&conn, &mut scratch)?;
        scratch.pragma_update(None, "foreign_keys", true)?;
        let tx = Transaction::new_unchecked(&scratch, TransactionBehavior::Exclusive)?;
        outcome.steps = run_steps(&tx, from, LATEST)?;
        return Ok(outcome);
    }

    let version_before: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
    let backup = if is_empty(&conn)? {
        None
    } else {
        Some(backup_database(&conn, path)?)
    };
    match apply_backed_up(&conn, from, version_before) {
        Ok(steps) => {
            outcome.steps = steps;
            outcome.backup_path = backup;
            Ok(outcome)
        }
        Err(err) => {
            // The transaction rolled back, so the backup duplicates the
            // database. Dropping it keeps a crash-looping daemon from filling
            // the disk with one backup per restart.
            if let Some(backup) = &backup {
                if detect_version(&conn).ok() == Some(from) {
                    let _ = std::fs::remove_file(backup);
                }
            }
            Err(err.context(format!(
                "Schema migration from version {from} to {LATEST} failed and was rolled back; the database is unchanged"
            )))
        }
    }
}

/// Keep the lock and version check in one seam so no caller can migrate a
/// database that changed after its backup was taken.
fn apply_backed_up(conn: &Connection, from: i64, version_before: i64) -> Result<Vec<StepOutcome>> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Exclusive).context(
        "Cannot acquire exclusive SQLite migration transaction. Stop all Audetic processes and retry",
    )?;
    let version_locked: i64 = tx.query_row("PRAGMA data_version", [], |r| r.get(0))?;
    ensure!(
        version_locked == version_before,
        "Another process wrote to the database while backing it up. No migration was performed; stop the writer and retry"
    );
    ensure!(
        detect_version(&tx)? == from,
        "Database changed while the migration was preparing"
    );
    let steps = run_steps(&tx, from, LATEST)?;
    validate_integrity(&tx)?;
    tx.commit().context("Failed to commit schema migration")?;
    Ok(steps)
}

fn run_steps(conn: &Connection, from: i64, to: i64) -> Result<Vec<StepOutcome>> {
    MIGRATIONS
        .iter()
        .filter(|m| m.version > from && m.version <= to)
        .map(|m| {
            let notes = (m.apply)(conn)
                .with_context(|| format!("Migration to schema version {} failed", m.version))?;
            conn.execute(
                "INSERT INTO audio_notes_schema(singleton, version) VALUES(1, ?1)
                 ON CONFLICT(singleton) DO UPDATE
                 SET version = excluded.version, migrated_at = CURRENT_TIMESTAMP",
                [m.version],
            )?;
            Ok(StepOutcome {
                version: m.version,
                description: m.description,
                notes,
            })
        })
        .collect()
}

fn backup_database(conn: &Connection, database: &Path) -> Result<PathBuf> {
    let filename = database
        .file_name()
        .context("Database has no filename")?
        .to_string_lossy();
    let backup = database.with_file_name(format!(
        "{filename}.pre-v{LATEST}-{}.sqlite3",
        uuid::Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&backup)
        .context("Failed to reserve private backup file")?;
    copy_sqlite_snapshot(conn, &backup)
        .with_context(|| format!("Failed to create SQLite backup at {}", backup.display()))?;
    file.sync_all().context("Failed to sync SQLite backup")?;
    #[cfg(unix)]
    {
        let parent = backup
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::File::open(parent)?
            .sync_all()
            .context("Failed to sync SQLite backup directory")?;
    }
    let backup_conn = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    validate_integrity(&backup_conn).context("Backup integrity validation failed")?;
    Ok(backup)
}

fn copy_sqlite_snapshot(source: &Connection, destination: &Path) -> Result<()> {
    // Keep the create_new reservation and its private permissions. macOS's
    // SQLite can reject even an empty existing VACUUM INTO destination; the
    // backup API explicitly supports an existing destination connection and
    // copies committed WAL content as part of a consistent snapshot.
    let mut destination =
        Connection::open_with_flags(destination, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .context("Failed to open reserved backup database")?;
    destination.busy_timeout(Duration::from_secs(5))?;
    copy_snapshot(source, &mut destination)?;
    // The copied header can carry WAL mode from the source. Materialize a
    // standalone rollback-journal backup while it is still writable: macOS's
    // SQLite cannot always reopen a WAL-mode file read-only without sidecars.
    destination.pragma_update(None, "journal_mode", "DELETE")?;
    Ok(())
}

fn copy_snapshot(source: &Connection, destination: &mut Connection) -> Result<()> {
    let result = rusqlite::backup::Backup::new(source, destination)?.step(-1)?;
    // Copy all pages under one source read lock and fail on contention
    // instead of retrying indefinitely against a live writer.
    ensure!(
        result == rusqlite::backup::StepResult::Done,
        "Cannot finish SQLite backup ({result:?}). Stop all Audetic processes and retry"
    );
    Ok(())
}

fn validate_integrity(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA integrity_check")?;
    let messages = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        messages == ["ok"],
        "SQLite integrity check failed: {}",
        messages.join("; ")
    );
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    ensure!(
        stmt.query([])?.next()?.is_none(),
        "SQLite foreign-key check failed; repair dangling references before migrating"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::audio_notes::AudioNoteRepository;
    use v1::count;

    fn fixture(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        // Original schema vintage: no title provenance, segments, filename,
        // deleted_at, profiles or artifacts. Workflows intentionally collide.
        conn.execute_batch("CREATE TABLE meetings (
            id INTEGER PRIMARY KEY, title TEXT, status TEXT NOT NULL, audio_path TEXT NOT NULL,
            transcript_path TEXT, transcript_text TEXT, duration_seconds INTEGER,
            started_at TEXT, completed_at TEXT, error TEXT, created_at TEXT
        );
        CREATE TABLE workflows(id INTEGER PRIMARY KEY, workflow_type TEXT, text TEXT, audio_path TEXT, created_at TEXT);
        INSERT INTO meetings VALUES(1,'  Legacy title  ','completed','/missing/legacy-audio.wav','text.txt','Meeting text',30,'2025-01-01 01:02:03','2025-01-01 01:02:33',NULL,'2025-01-01 01:02:03');
        INSERT INTO meetings VALUES(8,'   ','error','','',NULL,12,'2025-02-01 00:00:00',NULL,'Original error','2025-02-01 00:00:00');
        INSERT INTO workflows VALUES(1,'VoiceToText','Dictation text','/missing/quick.wav','2024-01-01 00:00:00');
        INSERT INTO workflows VALUES(7,'FutureLegacyType','Unknown kind text','','2026-01-01 00:00:00');").unwrap();
        conn
    }

    fn schema_version(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT version FROM audio_notes_schema WHERE singleton = 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn backups_in(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains(".pre-v"))
            .collect()
    }

    #[test]
    fn migrations_are_contiguous_from_one() {
        let versions: Vec<i64> = MIGRATIONS.iter().map(|m| m.version).collect();
        assert_eq!(versions, (1..=LATEST).collect::<Vec<_>>());
    }

    #[test]
    fn fresh_connection_replays_the_chain_without_a_backup() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_current(&conn).unwrap();
        assert_eq!(schema_version(&conn), LATEST);
        assert!(!v1::has_legacy_tables(&conn).unwrap());
        for table in [
            "audio_note_artifacts",
            "post_processing_jobs",
            "external_imports",
            "plaud_sync_state",
        ] {
            assert!(table_exists(&conn, table).unwrap(), "{table}");
        }
        assert!(!table_exists(&conn, "audio_note_migration_sources").unwrap());
    }

    #[test]
    fn every_older_version_upgrades_to_latest() {
        for start in 1..LATEST {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("audetic.db");
            let conn = Connection::open(&path).unwrap();
            run_steps(&conn, 0, start).unwrap();
            conn.execute(
                "INSERT INTO audio_notes(title, audio_path) VALUES('Keep me', '/tmp/keep.wav')",
                [],
            )
            .unwrap();
            drop(conn);

            let outcome = migrate_database(&path, false).unwrap();

            assert_eq!((outcome.from, outcome.to), (start, LATEST));
            assert!(outcome.backup_path.unwrap().is_file());
            let conn = crate::db::init_db_at(&path).unwrap();
            assert_eq!(schema_version(&conn), LATEST);
            let title: String = conn
                .query_row("SELECT title FROM audio_notes WHERE id = 1", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(title, "Keep me");
        }
    }

    #[test]
    fn connection_open_refuses_outdated_data_without_changing_it() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE workflows(id INTEGER PRIMARY KEY);")
            .unwrap();
        let err = ensure_current(&conn).unwrap_err().to_string();
        assert!(err.contains("version 0"), "{err}");
        assert!(err.contains("Restart the Audetic daemon"), "{err}");
        assert!(!table_exists(&conn, "audio_notes").unwrap());
    }

    #[test]
    fn unknown_schema_versions_are_not_silently_accepted() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_current(&conn).unwrap();
        conn.execute("UPDATE audio_notes_schema SET version=999", [])
            .unwrap();
        assert!(ensure_current(&conn)
            .unwrap_err()
            .to_string()
            .contains("Unsupported"));
        conn.execute("DROP TABLE audio_notes_schema", []).unwrap();
        assert!(ensure_current(&conn)
            .unwrap_err()
            .to_string()
            .contains("Unversioned"));
        assert!(!table_exists(&conn, "audio_notes_schema").unwrap());
    }

    #[test]
    fn version_two_classification_survives_kind_override_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audetic.db");
        let conn = Connection::open(&path).unwrap();
        run_steps(&conn, 0, 2).unwrap();
        conn.execute(
            "INSERT INTO audio_notes(audio_path, classification) VALUES('/tmp/keep.wav', '{\"kind\":\"meeting\"}')",
            [],
        )
        .unwrap();
        drop(conn);

        migrate_database(&path, false).unwrap();

        let conn = crate::db::init_db_at(&path).unwrap();
        let row: (String, Option<String>) = conn
            .query_row(
                "SELECT classification, classification_kind_override FROM audio_notes",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (r#"{"kind":"meeting"}"#.into(), None));
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

    #[test]
    fn old_vintage_mixed_ids_preserves_text_timestamps_and_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        drop(fixture(&path));
        let outcome = migrate_database(&path, false).unwrap();
        assert_eq!((outcome.from, outcome.to), (0, LATEST));
        let notes = &outcome.steps[0].notes;
        assert!(notes[0].contains("Imported 4 notes (2 meetings, 2 dictations)"));
        assert!(notes[1].contains("4 notes have no audio file"));
        let conn = crate::db::init_db_at(&path).unwrap();
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM audio_notes ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(ids, vec![1, 8, 9, 10]);
        let note = AudioNoteRepository::get(&conn, 1).unwrap().unwrap();
        assert_eq!(note.title.as_deref(), Some("Legacy title"));
        assert_eq!(note.title_source.as_deref(), Some("manual"));
        assert_eq!(note.transcript_text.as_deref(), Some("Meeting text"));
        let dictation = AudioNoteRepository::get(&conn, 9).unwrap().unwrap();
        assert_eq!(dictation.transcript_text.as_deref(), Some("Dictation text"));
        assert_eq!(dictation.started_at, "2024-01-01 00:00:00");
        assert_eq!(dictation.capture_source, "microphone");
        assert!(AudioNoteRepository::get(&conn, 8)
            .unwrap()
            .unwrap()
            .title
            .is_none());
        assert!(!v1::has_legacy_tables(&conn).unwrap());
        let raw: String = conn.query_row("SELECT source_json FROM audio_note_migration_sources WHERE source_table='workflows' AND source_id=7",[],|r|r.get(0)).unwrap();
        assert!(raw.contains("FutureLegacyType"));
        assert!(outcome.backup_path.as_ref().unwrap().is_file());
        let second = migrate_database(&path, false).unwrap();
        assert!(second.steps.is_empty());
        assert!(second.backup_path.is_none());
        assert_eq!(count(&conn, "audio_notes").unwrap(), 4);
    }

    #[test]
    fn dry_run_does_not_write_or_create_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        drop(fixture(&path));
        let before = std::fs::read(&path).unwrap();
        let outcome = migrate_database(&path, true).unwrap();
        assert_eq!(outcome.steps.len(), LATEST as usize);
        assert!(outcome.steps[0].notes[0].contains("Imported 4 notes"));
        assert!(outcome.backup_path.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let conn = Connection::open(&path).unwrap();
        assert!(!table_exists(&conn, "audio_notes").unwrap());
    }

    #[test]
    fn modern_columns_artifact_relationships_deleted_state_and_hooks_survive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        conn.execute_batch("ALTER TABLE meetings ADD COLUMN title_source TEXT;
            ALTER TABLE meetings ADD COLUMN title_version INTEGER DEFAULT 0;
            ALTER TABLE meetings ADD COLUMN title_updated_at TEXT;
            ALTER TABLE meetings ADD COLUMN source_filename TEXT;
            ALTER TABLE meetings ADD COLUMN deleted_at TEXT;
            ALTER TABLE meetings ADD COLUMN transcript_segments TEXT;
            ALTER TABLE meetings ADD COLUMN future_metadata TEXT;
            UPDATE meetings SET title_source='generated',title_version=3,title_updated_at='2025-03-01',source_filename='source.mp3',deleted_at='2026-01-01',transcript_segments='[{\"start\":0,\"end\":1,\"text\":\"Hi\"}]',future_metadata='keep me' WHERE id=1;
            CREATE TABLE agent_profiles(id INTEGER PRIMARY KEY,name TEXT,kind TEXT,executable TEXT,args_json TEXT,prompt_mode TEXT,default_profile INTEGER,enabled INTEGER,created_at TEXT,updated_at TEXT);
            INSERT INTO agent_profiles VALUES(3,'Agent','custom','test','[]','stdin',1,1,'then','then');
            CREATE TABLE meeting_artifacts(id INTEGER PRIMARY KEY,meeting_id INTEGER NOT NULL REFERENCES meetings(id),kind TEXT,title TEXT,template_id TEXT,agent_profile_id INTEGER REFERENCES agent_profiles(id),status TEXT,content_markdown TEXT,error TEXT,stdout TEXT,stderr TEXT,created_at TEXT,updated_at TEXT,completed_at TEXT);
            INSERT INTO meeting_artifacts VALUES(42,1,'summary','Summary','standard',3,'completed','# Original',NULL,'output','warnings','then','later','later');
            CREATE TABLE post_processing_jobs(id INTEGER PRIMARY KEY,name TEXT,event TEXT,action_type TEXT,action_config TEXT,enabled INTEGER,created_at TEXT,updated_at TEXT);
            INSERT INTO post_processing_jobs VALUES(1,'old hook','meeting.completed','shell','{\"command\":\"true\"}',1,'then','then');
            INSERT INTO post_processing_jobs VALUES(2,'new hook','audio_note.completed','shell','{}',1,'then','then');").unwrap();
        drop(conn);
        let outcome = migrate_database(&path, false).unwrap();
        let disabled: Vec<_> = outcome.steps[0]
            .notes
            .iter()
            .filter(|n| n.starts_with("Disabled hook"))
            .collect();
        assert_eq!(disabled.len(), 1);
        assert!(disabled[0].contains("old hook"));
        let conn = crate::db::init_db_at(&path).unwrap();
        assert!(AudioNoteRepository::get(&conn, 1).unwrap().is_none());
        let row: (i64,i64,String) = conn.query_row("SELECT note_id,agent_profile_id,content_markdown FROM audio_note_artifacts WHERE id=42",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
        assert_eq!(row, (1, 3, "# Original".into()));
        let row: (String,i64,String,String) = conn.query_row("SELECT title_source,title_version,source_filename,deleted_at FROM audio_notes WHERE id=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(
            row,
            (
                "generated".into(),
                3,
                "source.mp3".into(),
                "2026-01-01".into()
            )
        );
        let enabled: Vec<i64> = conn
            .prepare("SELECT enabled FROM post_processing_jobs ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(enabled, vec![0, 1]);
        let original: String = conn.query_row("SELECT source_json FROM audio_note_migration_sources WHERE source_table='meetings' AND source_id=1",[],|r|r.get(0)).unwrap();
        assert!(original.contains("keep me"));
        validate_integrity(&conn).unwrap();
    }

    #[test]
    fn failed_migration_rolls_back_and_drops_its_redundant_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        // Valid SQLite, but invalid artifact data: failure must occur inside
        // conversion, after creating tables and copying notes, and roll it all back.
        conn.execute_batch("CREATE TABLE meeting_artifacts(id INTEGER PRIMARY KEY,meeting_id INTEGER,kind TEXT,title TEXT,status TEXT);
            INSERT INTO meeting_artifacts VALUES(1,1,'summary',NULL,'completed');").unwrap();
        drop(conn);
        let error = migrate_database(&path, false).unwrap_err();
        assert!(format!("{error:#}").contains("rolled back"));
        let conn = Connection::open(&path).unwrap();
        assert_eq!(count(&conn, "meetings").unwrap(), 2);
        assert_eq!(count(&conn, "workflows").unwrap(), 2);
        assert_eq!(count(&conn, "meeting_artifacts").unwrap(), 1);
        assert!(!table_exists(&conn, "audio_notes").unwrap());
        assert!(!table_exists(&conn, "audio_notes_schema").unwrap());
        validate_integrity(&conn).unwrap();
        assert!(backups_in(dir.path()).is_empty());
    }

    #[test]
    fn backup_includes_committed_wal_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute(
            "INSERT INTO workflows VALUES(99,'VoiceToText','WAL text','','2026-01-01')",
            [],
        )
        .unwrap();
        // Keep the source connection open so SQLite cannot remove/checkpoint
        // the WAL on last-close before the backup is taken.
        let outcome = migrate_database(&path, false).unwrap();
        let backup_path = outcome.backup_path.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&backup_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let backup =
            Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let journal_mode: String = backup
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            journal_mode, "delete",
            "backup must not require WAL sidecars"
        );
        let text: String = backup
            .query_row("SELECT text FROM workflows WHERE id=99", [], |r| r.get(0))
            .unwrap();
        assert_eq!(text, "WAL text");
        assert!(outcome.steps[0].notes[0].contains("Imported 5 notes"));
        // Verify independence from sidecars by restoring ONLY the backup file.
        // Some SQLite builds retain an inert journal after closing it; mere
        // presence does not tell us whether the backup depends on that file.
        let restore_dir = tempfile::tempdir().unwrap();
        let restore_path = restore_dir.path().join("standalone.sqlite3");
        std::fs::copy(&backup_path, &restore_path).unwrap();
        let restored =
            Connection::open_with_flags(&restore_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        validate_integrity(&restored).unwrap();
        let restored_text: String = restored
            .query_row("SELECT text FROM workflows WHERE id=99", [], |r| r.get(0))
            .unwrap();
        assert_eq!(restored_text, "WAL text");
        let source_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            source_mode, "wal",
            "backup must not change the source journal mode"
        );
    }

    #[test]
    fn backup_accepts_a_reserved_sqlite_destination() {
        let dir = tempfile::tempdir().unwrap();
        let source = Connection::open_in_memory().unwrap();
        source.execute_batch("CREATE TABLE original(text TEXT); INSERT INTO original VALUES('preserved'); PRAGMA user_version = 7;").unwrap();
        let destination = dir.path().join("reserved.sqlite3");
        let reserved = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .unwrap();
        // Exercise SQLite's existing-output rejection on every platform. An
        // initialized destination also must work with the backup API, rather
        // than depending on VACUUM's platform-specific empty-file handling.
        let destination_conn = Connection::open(&destination).unwrap();
        destination_conn
            .pragma_update(None, "user_version", 99)
            .unwrap();
        drop(destination_conn);

        copy_sqlite_snapshot(&source, &destination).unwrap();
        reserved.sync_all().unwrap();
        let backup = Connection::open(&destination).unwrap();
        let text: String = backup
            .query_row("SELECT text FROM original", [], |r| r.get(0))
            .unwrap();
        assert_eq!(text, "preserved");
        let version: i64 = backup
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 7);
        validate_integrity(&backup).unwrap();
    }

    #[test]
    fn refuses_orphaned_artifacts_instead_of_dropping_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        conn.execute_batch("CREATE TABLE meeting_artifacts(id INTEGER PRIMARY KEY,meeting_id INTEGER); INSERT INTO meeting_artifacts VALUES(1,999);").unwrap();
        for dry_run in [true, false] {
            assert!(
                format!("{:#}", migrate_database(&path, dry_run).unwrap_err())
                    .contains("missing parents")
            );
        }
        assert_eq!(count(&conn, "meeting_artifacts").unwrap(), 1);
    }

    #[test]
    fn active_writer_prevents_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let error = migrate_database(&path, false).unwrap_err();
        assert!(format!("{error:#}").contains("exclusive SQLite migration transaction"));
        assert!(!table_exists(&conn, "audio_notes").unwrap());
        conn.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn workflows_only_installation_is_supported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        conn.execute("DROP TABLE meetings", []).unwrap();
        drop(conn);
        let outcome = migrate_database(&path, false).unwrap();
        assert!(outcome.steps[0].notes[0].contains("Imported 2 notes (0 meetings, 2 dictations)"));
        let conn = crate::db::init_db_at(&path).unwrap();
        assert_eq!(
            AudioNoteRepository::get(&conn, 1)
                .unwrap()
                .unwrap()
                .transcript_text
                .as_deref(),
            Some("Dictation text")
        );
    }

    #[test]
    fn overflowing_id_mapping_refuses_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        conn.execute("UPDATE meetings SET id=?1 WHERE id=8", [i64::MAX])
            .unwrap();
        assert!(format!("{:#}", migrate_database(&path, false).unwrap_err())
            .contains("ID space exhausted"));
        assert!(!table_exists(&conn, "audio_notes").unwrap());
        assert_eq!(count(&conn, "workflows").unwrap(), 2);
    }

    #[test]
    fn external_commit_after_backup_rejects_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        let before: i64 = conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        let backup_path = backup_database(&conn, &path).unwrap();
        let other_writer = Connection::open(&path).unwrap();
        other_writer
            .execute(
                "UPDATE workflows SET text='changed after backup' WHERE id=1",
                [],
            )
            .unwrap();
        let error = apply_backed_up(&conn, 0, before).unwrap_err();
        assert!(error.to_string().contains("Another process wrote"));
        assert!(!table_exists(&conn, "audio_notes").unwrap());
        let current: String = conn
            .query_row("SELECT text FROM workflows WHERE id=1", [], |r| r.get(0))
            .unwrap();
        let backup = Connection::open(backup_path).unwrap();
        let original: String = backup
            .query_row("SELECT text FROM workflows WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(current, "changed after backup");
        assert_eq!(original, "Dictation text");
    }

    #[test]
    fn corrupt_database_is_rejected_without_rewriting_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.sqlite3");
        let invalid = b"Not a SQLite database; preserve these bytes for recovery";
        std::fs::write(&path, invalid).unwrap();
        for dry_run in [true, false] {
            assert!(migrate_database(&path, dry_run).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), invalid);
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn invalid_legacy_text_rolls_back_instead_of_lossy_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        conn.execute_batch(
            "ALTER TABLE workflows ADD COLUMN unexpected TEXT;
            UPDATE workflows SET unexpected=CAST(X'80' AS TEXT) WHERE id=1;",
        )
        .unwrap();
        let error = migrate_database(&path, false).unwrap_err();
        assert!(format!("{error:#}").contains("utf-8"));
        assert!(!table_exists(&conn, "audio_notes_schema").unwrap());
        assert!(!table_exists(&conn, "audio_notes").unwrap());
        assert_eq!(count(&conn, "meetings").unwrap(), 2);
        let unchanged: String = conn
            .query_row(
                "SELECT hex(unexpected) FROM workflows WHERE id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unchanged, "80");
    }

    #[test]
    fn unknown_blob_and_nonfinite_columns_are_archived_without_json_loss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        conn.execute_batch(
            "ALTER TABLE workflows ADD COLUMN future_blob BLOB;
            ALTER TABLE workflows ADD COLUMN future_real REAL;
            UPDATE workflows SET future_blob=X'0080FF',future_real=1e999 WHERE id=1;",
        )
        .unwrap();
        migrate_database(&path, false).unwrap();
        let raw: String = conn.query_row("SELECT source_json FROM audio_note_migration_sources WHERE source_table='workflows' AND source_id=1", [], |r| r.get(0)).unwrap();
        let archived: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(archived["future_blob"]["sqlite_blob_base64"], "AID/");
        assert_eq!(archived["future_real"]["sqlite_real"], "inf");
        assert_eq!(archived["text"], "Dictation text");
    }
}
