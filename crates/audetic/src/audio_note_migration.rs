//! Explicit, offline conversion of the former meetings/workflows database.
//!
//! No service or repository calls this on connection open. A SQLite-consistent
//! backup precedes an exclusive transaction; data_version closes the gap
//! between the backup snapshot and acquiring the migration writer lock.

use anyhow::{ensure, Context, Result};
use base64::Engine;
use rusqlite::{params, types::ValueRef, Connection, OpenFlags, Transaction, TransactionBehavior};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::db::init::{create_schema, has_legacy_schema, table_exists, SCHEMA_VERSION};

#[derive(Debug, Clone, Serialize)]
pub struct SourceMapping {
    pub source_table: String,
    pub source_id: i64,
    pub note_id: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MissingAudio {
    pub source_table: String,
    pub source_id: i64,
    pub audio_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisabledSubscription {
    pub id: i64,
    pub name: String,
    pub event: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub database: PathBuf,
    pub dry_run: bool,
    pub already_migrated: bool,
    pub schema_version: i64,
    pub backup_path: Option<PathBuf>,
    pub meetings: usize,
    pub workflows: usize,
    pub artifacts: usize,
    pub notes: usize,
    pub source_mappings: Vec<SourceMapping>,
    pub missing_audio: Vec<MissingAudio>,
    pub disabled_subscriptions: Vec<DisabledSubscription>,
    pub warnings: Vec<String>,
}

impl fmt::Display for MigrationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let json = serde_json::to_string_pretty(self).map_err(|_| fmt::Error)?;
        f.write_str(&json)
    }
}

/// Convert a stopped daemon's database. Dry-run opens SQLite read-only and
/// takes one read snapshot; it never creates a backup or changes source rows.
pub fn migrate_audio_notes(database: &Path, dry_run: bool) -> Result<MigrationReport> {
    ensure!(
        database.is_file(),
        "Database does not exist: {}",
        database.display()
    );
    let flags = if dry_run {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = Connection::open_with_flags(database, flags)
        .context("Failed to open migration database")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    if dry_run {
        let tx = conn.unchecked_transaction()?;
        validate_integrity(&tx)?;
        return inspect(&tx, database, true);
    }

    // Snapshot inspection is short and read-only. Recheck everything after
    // obtaining the migration lock; an old daemon may still be writing.
    {
        let tx = conn.unchecked_transaction()?;
        validate_integrity(&tx)?;
        let report = inspect(&tx, database, false)?;
        if report.already_migrated {
            return Ok(report);
        }
    }
    let version_before: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
    let backup = backup_database(&conn, database)?;
    let result = convert_backed_up(&conn, database, version_before).map(|mut report| {
        report.backup_path = Some(backup.clone());
        report
    });
    result.with_context(|| format!("Audio Notes migration failed; conversion transaction rolled back. Pre-migration backup: {}", backup.display()))
}

/// Keep the lock and version check in one seam so no caller can accidentally
/// convert against a source newer than the snapshot that was backed up.
fn convert_backed_up(
    conn: &Connection,
    database: &Path,
    version_before: i64,
) -> Result<MigrationReport> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Exclusive)
            .context("Cannot acquire exclusive SQLite migration transaction. Stop all Audetic processes and retry")?;
    let version_locked: i64 = tx.query_row("PRAGMA data_version", [], |r| r.get(0))?;
    ensure!(version_locked == version_before,
            "Another process wrote to the database while backing it up. No conversion was performed; stop the writer and retry");
    validate_integrity(&tx)?;
    let report = inspect(&tx, database, false)?;
    ensure!(
        !report.already_migrated,
        "Database changed while migration was preparing"
    );
    convert(&tx, &report)?;
    validate_integrity(&tx)?;
    tx.commit()
        .context("Failed to commit Audio Notes conversion")?;
    Ok(report)
}

fn backup_database(conn: &Connection, database: &Path) -> Result<PathBuf> {
    let filename = database
        .file_name()
        .context("Database has no filename")?
        .to_string_lossy();
    let backup = database.with_file_name(format!(
        "{filename}.pre-audio-notes-{}.sqlite3",
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
    let result = rusqlite::backup::Backup::new(source, &mut destination)?.step(-1)?;
    // This is offline tooling: copy all pages under one source read lock and
    // fail on contention instead of retrying indefinitely against a live writer.
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
        "SQLite foreign-key check failed; repair dangling legacy references before migrating"
    );
    Ok(())
}

fn count(conn: &Connection, table: &str) -> Result<usize> {
    if !table_exists(conn, table)? {
        return Ok(0);
    }
    let count: i64 =
        conn.query_row(&format!("SELECT COUNT(*) FROM {}", quote(table)), [], |r| {
            r.get(0)
        })?;
    Ok(usize::try_from(count)?)
}

fn inspect(conn: &Connection, database: &Path, dry_run: bool) -> Result<MigrationReport> {
    let legacy = has_legacy_schema(conn)?;
    let unified = table_exists(conn, "audio_notes_schema")?;
    ensure!(
        !(legacy && unified),
        "Mixed legacy and unified schemas detected; refusing an ambiguous conversion"
    );
    ensure!(
        legacy || unified,
        "No recognized legacy or Audio Notes schema in {}",
        database.display()
    );
    if unified {
        let version: i64 = conn.query_row(
            "SELECT version FROM audio_notes_schema WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            version == SCHEMA_VERSION,
            "Unsupported Audio Notes schema version {version}"
        );
    } else {
        ensure!(
            !table_exists(conn, "audio_notes")? && !table_exists(conn, "audio_note_artifacts")?,
            "Unmarked unified tables already exist; refusing to overwrite them"
        );
    }
    let meetings = count(conn, "meetings")?;
    let workflows = count(conn, "workflows")?;
    let artifacts = count(
        conn,
        if unified {
            "audio_note_artifacts"
        } else {
            "meeting_artifacts"
        },
    )?;
    let mut report = MigrationReport {
        database: database.to_path_buf(),
        dry_run,
        already_migrated: unified,
        schema_version: SCHEMA_VERSION,
        backup_path: None,
        meetings,
        workflows,
        artifacts,
        notes: if unified {
            count(conn, "audio_notes")?
        } else {
            meetings + workflows
        },
        source_mappings: Vec::new(),
        missing_audio: Vec::new(),
        disabled_subscriptions: Vec::new(),
        warnings: Vec::new(),
    };
    if unified {
        return Ok(report);
    }
    let mut max_id = 0;
    for table in ["meetings", "workflows"] {
        if !table_exists(conn, table)? {
            continue;
        }
        let mut stmt = conn.prepare(&format!("SELECT id,audio_path FROM {table} ORDER BY id"))?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (id, path) = row?;
            let note_id = if table == "meetings" {
                max_id = max_id.max(id);
                id
            } else {
                max_id = max_id
                    .checked_add(1)
                    .context("Audio Note ID space exhausted")?;
                max_id
            };
            report.source_mappings.push(SourceMapping {
                source_table: table.into(),
                source_id: id,
                note_id,
            });
            let path = path.unwrap_or_default();
            if path.is_empty() || !Path::new(&path).is_file() {
                report.missing_audio.push(MissingAudio {
                    source_table: table.into(),
                    source_id: id,
                    audio_path: path,
                });
            }
        }
    }
    if table_exists(conn, "meeting_artifacts")? {
        ensure!(
            table_exists(conn, "meetings")?,
            "Legacy artifacts exist without their meetings table"
        );
        let orphaned: i64 = conn.query_row("SELECT COUNT(*) FROM meeting_artifacts a LEFT JOIN meetings m ON m.id=a.meeting_id WHERE m.id IS NULL",[],|r|r.get(0))?;
        ensure!(
            orphaned == 0,
            "{orphaned} legacy artifacts have missing parents; refusing data loss"
        );
    }
    if table_exists(conn, "post_processing_jobs")? {
        let mut stmt = conn.prepare("SELECT id,name,event FROM post_processing_jobs WHERE enabled<>0 AND event IN ('dictation.completed','meeting.completed') ORDER BY id")?;
        report.disabled_subscriptions = stmt
            .query_map([], |r| {
                Ok(DisabledSubscription {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    event: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    if !report.missing_audio.is_empty() {
        report.warnings.push("Missing audio is reported only: every transcript and original path is retained; no audio is moved.".into());
    }
    if !report.disabled_subscriptions.is_empty() {
        report.warnings.push("Legacy event subscriptions will be disabled, not retargeted. Review commands and explicitly subscribe to audio_note.completed with its new payload.".into());
    }
    Ok(report)
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", quote(table)))?;
    let rows = stmt.query_map([], |r| r.get(1))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn select_column(columns: &HashSet<String>, name: &str, fallback: &str) -> String {
    if columns.contains(name) {
        quote(name)
    } else {
        fallback.into()
    }
}

fn convert(conn: &Connection, report: &MigrationReport) -> Result<()> {
    create_schema(conn)?;
    // Lossless provenance (including columns unknown to this release) survives
    // removal of the legacy runtime tables. Blobs use an explicit base64 tag.
    conn.execute_batch(
        "CREATE TABLE audio_note_migration_sources (
        source_table TEXT NOT NULL, source_id INTEGER NOT NULL,
        note_id INTEGER REFERENCES audio_notes(id), source_json TEXT NOT NULL,
        PRIMARY KEY(source_table,source_id)
    );",
    )?;
    for table in ["meetings", "workflows"] {
        if !table_exists(conn, table)? {
            continue;
        }
        let cols = columns(conn, table)?;
        if table == "meetings" {
            let names = [
                "id",
                "title",
                "title_source",
                "title_updated_at",
                "title_version",
                "status",
                "audio_path",
                "source_filename",
                "transcript_path",
                "transcript_text",
                "transcript_segments",
                "duration_seconds",
                "started_at",
                "completed_at",
                "error",
                "created_at",
                "deleted_at",
            ];
            let projections = names
                .iter()
                .map(|name| match *name {
                    "title" => format!("NULLIF(trim({}), '')", select_column(&cols, name, "NULL")),
                    "title_source" if !cols.contains(*name) => {
                        "CASE WHEN title IS NULL OR trim(title)='' THEN NULL ELSE 'manual' END"
                            .into()
                    }
                    "title_version" => format!("COALESCE({},0)", select_column(&cols, name, "0")),
                    "started_at" | "created_at" => format!(
                        "COALESCE({},CURRENT_TIMESTAMP)",
                        select_column(&cols, name, "NULL")
                    ),
                    "audio_path" => "COALESCE(audio_path,'')".into(),
                    _ => select_column(&cols, name, "NULL"),
                })
                .collect::<Vec<_>>();
            let source = if cols.contains("source_filename") {
                "CASE WHEN source_filename IS NOT NULL THEN 'import' ELSE 'microphone_and_system' END"
            } else {
                "'microphone_and_system'"
            };
            conn.execute(
                &format!(
                    "INSERT INTO audio_notes({},capture_source) SELECT {},{source} FROM meetings",
                    names.join(","),
                    projections.join(",")
                ),
                [],
            )?;
        } else {
            let created = select_column(&cols, "created_at", "NULL");
            for mapping in report
                .source_mappings
                .iter()
                .filter(|m| m.source_table == table)
            {
                conn.execute(&format!("INSERT INTO audio_notes(id,status,audio_path,transcript_text,started_at,completed_at,created_at,capture_source) \
                    SELECT ?1,'completed',COALESCE(audio_path,''),text,COALESCE({created},CURRENT_TIMESTAMP),{created},COALESCE({created},CURRENT_TIMESTAMP),'microphone' \
                    FROM workflows WHERE id=?2"),params![mapping.note_id,mapping.source_id])?;
            }
        }
        archive_rows(conn, table, report)?;
    }
    if table_exists(conn, "meeting_artifacts")? {
        let cols = columns(conn, "meeting_artifacts")?;
        let names = [
            "id",
            "kind",
            "title",
            "template_id",
            "agent_profile_id",
            "status",
            "content_markdown",
            "error",
            "stdout",
            "stderr",
            "created_at",
            "updated_at",
            "completed_at",
        ];
        let projection = names
            .iter()
            .map(|name| {
                select_column(
                    &cols,
                    name,
                    if matches!(*name, "created_at" | "updated_at") {
                        "CURRENT_TIMESTAMP"
                    } else {
                        "NULL"
                    },
                )
            })
            .collect::<Vec<_>>();
        conn.execute(&format!("INSERT INTO audio_note_artifacts(note_id,{}) SELECT meeting_id,{} FROM meeting_artifacts",names.join(","),projection.join(",")),[])?;
        archive_rows(conn, "meeting_artifacts", report)?;
        let changed_relationships: i64 = conn.query_row("SELECT COUNT(*) FROM meeting_artifacts old LEFT JOIN audio_note_artifacts new ON new.id=old.id AND new.note_id=old.meeting_id WHERE new.id IS NULL",[],|r|r.get(0))?;
        ensure!(
            changed_relationships == 0,
            "Artifact relationships were not preserved"
        );
    }
    // Preserve all hook definitions, including the original enabled state in
    // the archive; do not silently broaden a shell command's subscriptions.
    archive_rows(conn, "post_processing_jobs", report)?;
    conn.execute("UPDATE post_processing_jobs SET enabled=0,updated_at=CURRENT_TIMESTAMP WHERE event IN ('dictation.completed','meeting.completed') AND enabled<>0",[])?;
    ensure!(
        count(conn, "audio_notes")? == report.notes,
        "Audio Note count mismatch"
    );
    ensure!(
        count(conn, "audio_note_artifacts")? == report.artifacts,
        "Artifact count mismatch"
    );
    let mappings: i64 = conn.query_row("SELECT COUNT(*) FROM audio_note_migration_sources WHERE source_table IN ('meetings','workflows') AND note_id IS NOT NULL",[],|r|r.get(0))?;
    ensure!(
        usize::try_from(mappings)? == report.notes,
        "Source mapping count mismatch"
    );
    for table in ["meeting_artifacts", "meetings", "workflows"] {
        conn.execute(&format!("DROP TABLE IF EXISTS {table}"), [])?;
    }
    Ok(())
}

fn archive_rows(conn: &Connection, table: &str, report: &MigrationReport) -> Result<()> {
    let note_ids: HashMap<i64, i64> = report
        .source_mappings
        .iter()
        .filter(|mapping| mapping.source_table == table)
        .map(|mapping| (mapping.source_id, mapping.note_id))
        .collect();
    let mut stmt = conn.prepare(&format!("SELECT * FROM {}", quote(table)))?;
    let names = stmt
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get("id")?;
        let mut object = serde_json::Map::new();
        for (i, name) in names.iter().enumerate() {
            let value = match row.get_ref(i)? {
                ValueRef::Null => serde_json::Value::Null,
                ValueRef::Integer(n) => n.into(),
                ValueRef::Real(n) if n.is_finite() => serde_json::json!(n),
                ValueRef::Real(n) => serde_json::json!({"sqlite_real": n.to_string()}),
                ValueRef::Text(bytes) => std::str::from_utf8(bytes)?.into(),
                ValueRef::Blob(bytes) => {
                    serde_json::json!({"sqlite_blob_base64":base64::engine::general_purpose::STANDARD.encode(bytes)})
                }
            };
            object.insert(name.clone(), value);
        }
        let note_id = if table == "meeting_artifacts" {
            Some(row.get::<_, i64>("meeting_id")?)
        } else {
            note_ids.get(&id).copied()
        };
        conn.execute("INSERT INTO audio_note_migration_sources(source_table,source_id,note_id,source_json) VALUES(?1,?2,?3,?4)",params![table,id,note_id,serde_json::to_string(&object)?])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::audio_notes::AudioNoteRepository;

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

    #[test]
    fn old_vintage_mixed_ids_preserves_text_timestamps_and_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        drop(fixture(&path));
        let report = migrate_audio_notes(&path, false).unwrap();
        assert_eq!((report.meetings, report.workflows, report.notes), (2, 2, 4));
        assert_eq!(report.missing_audio.len(), 4);
        assert_eq!(
            report
                .source_mappings
                .iter()
                .map(|m| m.note_id)
                .collect::<Vec<_>>(),
            vec![1, 8, 9, 10]
        );
        let conn = crate::db::init_db_at(&path).unwrap();
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
        assert!(!has_legacy_schema(&conn).unwrap());
        let raw: String = conn.query_row("SELECT source_json FROM audio_note_migration_sources WHERE source_table='workflows' AND source_id=7",[],|r|r.get(0)).unwrap();
        assert!(raw.contains("FutureLegacyType"));
        assert!(report.backup_path.as_ref().unwrap().is_file());
        let second = migrate_audio_notes(&path, false).unwrap();
        assert!(second.already_migrated);
        assert!(second.backup_path.is_none());
        assert_eq!(second.notes, 4);
    }

    #[test]
    fn dry_run_does_not_write_or_create_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        drop(fixture(&path));
        let before = std::fs::read(&path).unwrap();
        let report = migrate_audio_notes(&path, true).unwrap();
        assert_eq!(report.notes, 4);
        assert!(report.backup_path.is_none());
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
        let report = migrate_audio_notes(&path, false).unwrap();
        assert_eq!(report.disabled_subscriptions.len(), 1);
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
    fn conversion_failure_rolls_back_and_backup_is_restorable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        // Valid SQLite, but invalid artifact data: failure must occur inside
        // conversion, after creating tables and copying notes, and roll it all back.
        conn.execute_batch("CREATE TABLE meeting_artifacts(id INTEGER PRIMARY KEY,meeting_id INTEGER,kind TEXT,title TEXT,status TEXT);
            INSERT INTO meeting_artifacts VALUES(1,1,'summary',NULL,'completed');").unwrap();
        drop(conn);
        let error = migrate_audio_notes(&path, false).unwrap_err();
        assert!(format!("{error:#}").contains("rolled back"));
        let conn = Connection::open(&path).unwrap();
        assert_eq!(count(&conn, "meetings").unwrap(), 2);
        assert_eq!(count(&conn, "workflows").unwrap(), 2);
        assert!(!table_exists(&conn, "audio_notes").unwrap());
        assert!(!table_exists(&conn, "audio_notes_schema").unwrap());
        let backup_path = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.to_string_lossy().contains("pre-audio-notes"))
            .unwrap();
        let backup = Connection::open(&backup_path).unwrap();
        let restore = dir.path().join("restored.sqlite3");
        backup
            .execute("VACUUM INTO ?1", [restore.to_str().unwrap()])
            .unwrap();
        let restored = Connection::open(&restore).unwrap();
        assert_eq!(count(&restored, "workflows").unwrap(), 2);
        assert_eq!(count(&restored, "meeting_artifacts").unwrap(), 1);
        validate_integrity(&restored).unwrap();
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
        let report = migrate_audio_notes(&path, false).unwrap();
        let backup_path = report.backup_path.unwrap();
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
        let backup = Connection::open(backup_path).unwrap();
        let text: String = backup
            .query_row("SELECT text FROM workflows WHERE id=99", [], |r| r.get(0))
            .unwrap();
        assert_eq!(text, "WAL text");
        assert_eq!(report.notes, 5);
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
            assert!(migrate_audio_notes(&path, dry_run)
                .unwrap_err()
                .to_string()
                .contains("missing parents"));
        }
        assert_eq!(count(&conn, "meeting_artifacts").unwrap(), 1);
    }

    #[test]
    fn active_writer_prevents_conversion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.sqlite3");
        let conn = fixture(&path);
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let error = migrate_audio_notes(&path, false).unwrap_err();
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
        let report = migrate_audio_notes(&path, false).unwrap();
        assert_eq!(report.notes, 2);
        assert_eq!(report.meetings, 0);
        assert_eq!(report.source_mappings[0].note_id, 1);
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
        assert!(migrate_audio_notes(&path, false)
            .unwrap_err()
            .to_string()
            .contains("ID space exhausted"));
        assert!(!table_exists(&conn, "audio_notes").unwrap());
        assert_eq!(count(&conn, "workflows").unwrap(), 2);
    }

    #[test]
    fn external_commit_after_backup_rejects_conversion() {
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
        let error = convert_backed_up(&conn, &path, before).unwrap_err();
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
            assert!(migrate_audio_notes(&path, dry_run).is_err());
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
        let error = migrate_audio_notes(&path, false).unwrap_err();
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
        migrate_audio_notes(&path, false).unwrap();
        let raw: String = conn.query_row("SELECT source_json FROM audio_note_migration_sources WHERE source_table='workflows' AND source_id=1", [], |r| r.get(0)).unwrap();
        let archived: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(archived["future_blob"]["sqlite_blob_base64"], "AID/");
        assert_eq!(archived["future_real"]["sqlite_real"], "inf");
        assert_eq!(archived["text"], "Dictation text");
    }
}
