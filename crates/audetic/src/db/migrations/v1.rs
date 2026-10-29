//! Version 1: the unified Audio Notes schema, plus a lossless import of any
//! pre-Audio-Notes `meetings`/`workflows` tables. An empty database simply
//! has nothing to import.

use anyhow::{ensure, Context, Result};
use base64::Engine;
use rusqlite::{params, types::ValueRef, Connection};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::db::init::table_exists;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS audio_notes (
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
    CREATE INDEX IF NOT EXISTS idx_audio_notes_stream ON audio_notes(started_at DESC, id DESC)
        WHERE deleted_at IS NULL;
    CREATE INDEX IF NOT EXISTS idx_audio_notes_status ON audio_notes(status);
    CREATE INDEX IF NOT EXISTS idx_audio_notes_kind ON audio_notes(json_extract(classification, '$.kind'));
    CREATE TABLE IF NOT EXISTS post_processing_jobs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL, event TEXT NOT NULL, action_type TEXT NOT NULL,
        action_config TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
        created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
    CREATE INDEX IF NOT EXISTS idx_pp_jobs_event_enabled ON post_processing_jobs(event) WHERE enabled = 1;
    CREATE TABLE IF NOT EXISTS agent_profiles (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL, kind TEXT NOT NULL, executable TEXT NOT NULL, args_json TEXT NOT NULL,
        prompt_mode TEXT NOT NULL DEFAULT 'stdin', default_profile INTEGER NOT NULL DEFAULT 0,
        enabled INTEGER NOT NULL DEFAULT 1,
        created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
        UNIQUE(kind, executable)
    );
    CREATE INDEX IF NOT EXISTS idx_agent_profiles_enabled ON agent_profiles(enabled);
    CREATE TABLE IF NOT EXISTS audio_note_artifacts (
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
    CREATE INDEX IF NOT EXISTS idx_audio_note_artifacts_note_created ON audio_note_artifacts(note_id, created_at DESC);
    CREATE INDEX IF NOT EXISTS idx_audio_note_artifacts_status ON audio_note_artifacts(status);
    CREATE TABLE IF NOT EXISTS audio_notes_schema (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        version INTEGER NOT NULL, migrated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
    INSERT OR IGNORE INTO audio_notes_schema(singleton, version) VALUES(1, 1);";

pub(super) fn apply(conn: &Connection) -> Result<Vec<String>> {
    // Inspect before creating anything so data-loss refusals happen up front.
    let import = if has_legacy_tables(conn)? {
        Some(inspect(conn)?)
    } else {
        None
    };
    conn.execute_batch(SCHEMA)
        .context("Failed to create Audio Notes schema")?;
    let Some(import) = import else {
        return Ok(Vec::new());
    };
    convert(conn, &import)?;
    Ok(import.summary())
}

pub(super) fn has_legacy_tables(conn: &Connection) -> Result<bool> {
    Ok(table_exists(conn, "meetings")?
        || table_exists(conn, "workflows")?
        || table_exists(conn, "meeting_artifacts")?)
}

struct SourceMapping {
    source_table: &'static str,
    source_id: i64,
    note_id: i64,
}

struct DisabledSubscription {
    name: String,
    event: String,
}

struct LegacyImport {
    meetings: usize,
    workflows: usize,
    artifacts: usize,
    notes: usize,
    source_mappings: Vec<SourceMapping>,
    missing_audio: usize,
    disabled_subscriptions: Vec<DisabledSubscription>,
}

impl LegacyImport {
    fn summary(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "Imported {} notes ({} meetings, {} dictations) and {} artifacts",
            self.notes, self.meetings, self.workflows, self.artifacts
        )];
        if self.missing_audio > 0 {
            lines.push(format!(
                "{} notes have no audio file on disk; their transcripts were kept",
                self.missing_audio
            ));
        }
        for sub in &self.disabled_subscriptions {
            lines.push(format!(
                "Disabled hook \"{}\" ({}); recreate it for audio_note.completed",
                sub.name, sub.event
            ));
        }
        lines
    }
}

fn inspect(conn: &Connection) -> Result<LegacyImport> {
    let meetings = count(conn, "meetings")?;
    let workflows = count(conn, "workflows")?;
    let mut import = LegacyImport {
        meetings,
        workflows,
        artifacts: count(conn, "meeting_artifacts")?,
        notes: meetings + workflows,
        source_mappings: Vec::new(),
        missing_audio: 0,
        disabled_subscriptions: Vec::new(),
    };
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
            import.source_mappings.push(SourceMapping {
                source_table: table,
                source_id: id,
                note_id,
            });
            let path = path.unwrap_or_default();
            if path.is_empty() || !Path::new(&path).is_file() {
                import.missing_audio += 1;
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
        let mut stmt = conn.prepare("SELECT name,event FROM post_processing_jobs WHERE enabled<>0 AND event IN ('dictation.completed','meeting.completed') ORDER BY id")?;
        import.disabled_subscriptions = stmt
            .query_map([], |r| {
                Ok(DisabledSubscription {
                    name: r.get(0)?,
                    event: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(import)
}

pub(super) fn count(conn: &Connection, table: &str) -> Result<usize> {
    if !table_exists(conn, table)? {
        return Ok(0);
    }
    let count: i64 =
        conn.query_row(&format!("SELECT COUNT(*) FROM {}", quote(table)), [], |r| {
            r.get(0)
        })?;
    Ok(usize::try_from(count)?)
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

fn convert(conn: &Connection, import: &LegacyImport) -> Result<()> {
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
            for mapping in import
                .source_mappings
                .iter()
                .filter(|m| m.source_table == table)
            {
                conn.execute(&format!("INSERT INTO audio_notes(id,status,audio_path,transcript_text,started_at,completed_at,created_at,capture_source) \
                    SELECT ?1,'completed',COALESCE(audio_path,''),text,COALESCE({created},CURRENT_TIMESTAMP),{created},COALESCE({created},CURRENT_TIMESTAMP),'microphone' \
                    FROM workflows WHERE id=?2"),params![mapping.note_id,mapping.source_id])?;
            }
        }
        archive_rows(conn, table, import)?;
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
        archive_rows(conn, "meeting_artifacts", import)?;
        let changed_relationships: i64 = conn.query_row("SELECT COUNT(*) FROM meeting_artifacts old LEFT JOIN audio_note_artifacts new ON new.id=old.id AND new.note_id=old.meeting_id WHERE new.id IS NULL",[],|r|r.get(0))?;
        ensure!(
            changed_relationships == 0,
            "Artifact relationships were not preserved"
        );
    }
    // Preserve all hook definitions, including the original enabled state in
    // the archive; do not silently broaden a shell command's subscriptions.
    archive_rows(conn, "post_processing_jobs", import)?;
    conn.execute("UPDATE post_processing_jobs SET enabled=0,updated_at=CURRENT_TIMESTAMP WHERE event IN ('dictation.completed','meeting.completed') AND enabled<>0",[])?;
    ensure!(
        count(conn, "audio_notes")? == import.notes,
        "Audio Note count mismatch"
    );
    ensure!(
        count(conn, "audio_note_artifacts")? == import.artifacts,
        "Artifact count mismatch"
    );
    let mappings: i64 = conn.query_row("SELECT COUNT(*) FROM audio_note_migration_sources WHERE source_table IN ('meetings','workflows') AND note_id IS NOT NULL",[],|r|r.get(0))?;
    ensure!(
        usize::try_from(mappings)? == import.notes,
        "Source mapping count mismatch"
    );
    for table in ["meeting_artifacts", "meetings", "workflows"] {
        conn.execute(&format!("DROP TABLE IF EXISTS {table}"), [])?;
    }
    Ok(())
}

fn archive_rows(conn: &Connection, table: &str, import: &LegacyImport) -> Result<()> {
    let note_ids: HashMap<i64, i64> = import
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
