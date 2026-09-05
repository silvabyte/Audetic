use anyhow::{bail, Context, Result};
use rusqlite::Connection;

use std::path::Path;
use std::time::Duration;

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const LATEST_SCHEMA_VERSION: i64 = 7;

/// Open the application database and run pending migrations.
///
/// This remains the compatibility entry point for callers that historically
/// expected `init_db` to initialize an empty database. New runtime callers
/// should run [`migrate_db`] once during startup and use [`open_db`] afterward.
pub fn init_db() -> Result<Connection> {
    migrate_db()
}

/// Open a database at `db_path` and run pending migrations.
///
/// Prefer [`migrate_db_at`] in startup code and [`open_db_at`] in code that
/// only needs a connection to an already-migrated database.
pub fn init_db_at(db_path: &Path) -> Result<Connection> {
    migrate_db_at(db_path)
}

/// Open the application database without running data migrations.
pub fn open_db() -> Result<Connection> {
    let db_path = crate::global::db_file()?;
    open_db_at(&db_path)
}

/// Open an already-migrated database and apply connection-local settings.
pub fn open_db_at(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create database directory")?;
    }

    let conn = Connection::open(db_path).context("Failed to open database connection")?;
    configure_connection(&conn)?;
    Ok(conn)
}

/// Open the application database and apply all pending migrations.
///
/// The daemon should call this exactly once while constructing startup state.
pub fn migrate_db() -> Result<Connection> {
    let db_path = crate::global::db_file()?;
    migrate_db_at(&db_path)
}

/// Open `db_path` and apply all pending migrations under one exclusive
/// transaction.
pub fn migrate_db_at(db_path: &Path) -> Result<Connection> {
    let conn = open_db_at(db_path)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Apply settings that SQLite scopes to an individual connection.
pub fn configure_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)
        .context("Failed to set SQLite busy timeout")?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("Failed to enable SQLite foreign keys")?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .context("Failed to configure SQLite synchronous mode")?;
    Ok(())
}

/// Apply numbered schema migrations.
///
/// Migrations are serialized by an exclusive transaction and recorded only
/// after their complete schema/data change succeeds. Keeping this function
/// public preserves the existing in-memory test setup API; production code
/// should invoke it only through the startup migration entry points above.
pub fn migrate(conn: &Connection) -> Result<()> {
    configure_connection(conn)?;

    // WAL is database-persistent rather than connection-local, so set it only
    // on the startup migration path. In-memory databases retain their `memory`
    // journal mode, which is expected.
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("Failed to configure SQLite journal mode")?;

    conn.execute_batch("BEGIN EXCLUSIVE TRANSACTION")
        .context("Failed to begin exclusive database migration")?;

    let result = apply_pending_migrations(conn);
    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .context("Failed to commit database migrations"),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn apply_pending_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .context("Failed to create schema_migrations table")?;

    let newest: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .context("Failed to read current schema version")?;
    if let Some(version) = newest {
        if version > LATEST_SCHEMA_VERSION {
            bail!(
                "Database schema version {version} is newer than supported version \
                 {LATEST_SCHEMA_VERSION}"
            );
        }
    }

    apply_migration(conn, 1, "baseline", migrate_baseline)?;
    apply_migration(
        conn,
        2,
        "sync_identity_and_settings",
        migrate_sync_foundation,
    )?;
    apply_migration(
        conn,
        3,
        "sync_serve_ownership",
        migrate_sync_serve_ownership,
    )?;
    apply_migration(
        conn,
        4,
        "dictation_shared_library",
        migrate_dictation_shared_library,
    )?;
    apply_migration(
        conn,
        5,
        "meeting_artifact_shared_library",
        migrate_meeting_artifact_shared_library,
    )?;
    apply_migration(
        conn,
        6,
        "recording_payload_sync",
        migrate_recording_payload_sync,
    )?;
    apply_migration(
        conn,
        7,
        "payload_staging_failures",
        migrate_payload_staging_failures,
    )?;
    Ok(())
}

fn apply_migration(
    conn: &Connection,
    version: i64,
    name: &str,
    migration: fn(&Connection) -> Result<()>,
) -> Result<()> {
    let already_applied: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
            [version],
            |row| row.get(0),
        )
        .with_context(|| format!("Failed to inspect database migration {version}"))?;
    if already_applied {
        return Ok(());
    }

    migration(conn).with_context(|| format!("Failed to apply database migration {version}"))?;
    conn.execute(
        "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
        rusqlite::params![version, name],
    )
    .with_context(|| format!("Failed to record database migration {version}"))?;
    Ok(())
}

fn migrate_baseline(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS workflows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_type TEXT NOT NULL,
            text TEXT NOT NULL,
            audio_path TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_workflows_created_at
            ON workflows(created_at DESC);

        CREATE TABLE IF NOT EXISTS meetings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT,
            title_source TEXT,
            title_updated_at TIMESTAMP,
            title_version INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'recording',
            audio_path TEXT NOT NULL,
            source_filename TEXT,
            transcript_path TEXT,
            transcript_text TEXT,
            transcript_segments TEXT,
            duration_seconds INTEGER,
            started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP,
            error TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            deleted_at TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS post_processing_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            event TEXT NOT NULL,
            action_type TEXT NOT NULL,
            action_config TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_pp_jobs_event_enabled
            ON post_processing_jobs(event) WHERE enabled = 1;

        CREATE TABLE IF NOT EXISTS agent_profiles (
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
        );
        CREATE INDEX IF NOT EXISTS idx_agent_profiles_enabled
            ON agent_profiles(enabled);

        CREATE TABLE IF NOT EXISTS meeting_artifacts (
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
        );
        CREATE INDEX IF NOT EXISTS idx_meeting_artifacts_meeting_created
            ON meeting_artifacts(meeting_id, created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_meeting_artifacts_status
            ON meeting_artifacts(status);",
    )
    .context("Failed to create baseline database schema")?;

    // These checks absorb every known pre-numbered schema vintage. Once
    // migration 1 is recorded, normal startups never repeat them.
    add_column_if_missing(conn, "meetings", "deleted_at", "TIMESTAMP")?;
    add_column_if_missing(conn, "meetings", "transcript_segments", "TEXT")?;
    add_column_if_missing(conn, "meetings", "title_source", "TEXT")?;
    add_column_if_missing(conn, "meetings", "title_updated_at", "TIMESTAMP")?;
    add_column_if_missing(
        conn,
        "meetings",
        "title_version",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(conn, "meetings", "source_filename", "TEXT")?;

    conn.execute_batch(
        "UPDATE meetings SET title = NULL, title_source = NULL
         WHERE (title IS NOT NULL AND trim(title) = '')
            OR (title IS NULL AND title_source IS NOT NULL);
         UPDATE meetings SET title = trim(title), title_source = 'manual'
         WHERE title IS NOT NULL AND title_source IS NULL;

         CREATE INDEX IF NOT EXISTS idx_meetings_started_at
            ON meetings(started_at DESC);
         CREATE INDEX IF NOT EXISTS idx_meetings_status ON meetings(status);
         CREATE INDEX IF NOT EXISTS idx_meetings_deleted_at ON meetings(deleted_at);",
    )
    .context("Failed to migrate legacy meeting data")?;
    Ok(())
}

fn migrate_sync_foundation(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_identity (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            device_id TEXT NOT NULL UNIQUE,
            hub_id TEXT UNIQUE,
            owner_login TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            CHECK ((hub_id IS NULL) = (owner_login IS NULL))
        );

        CREATE TABLE IF NOT EXISTS sync_settings (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            role TEXT NOT NULL DEFAULT 'standalone'
                CHECK (role IN ('standalone', 'home_hub', 'connected_device')),
            device_name TEXT,
            hub_url TEXT,
            hub_id TEXT,
            hub_owner_login TEXT,
            upload_recording_payloads INTEGER NOT NULL DEFAULT 0
                CHECK (upload_recording_payloads IN (0, 1)),
            cache_level TEXT NOT NULL DEFAULT 'live_only'
                CHECK (cache_level IN (
                    'live_only',
                    'text_for_offline_use',
                    'text_and_available_audio'
                )),
            shared_config_enabled INTEGER NOT NULL DEFAULT 0
                CHECK (shared_config_enabled IN (0, 1)),
            change_cursor INTEGER NOT NULL DEFAULT 0 CHECK (change_cursor >= 0),
            shared_config_version INTEGER CHECK (shared_config_version >= 0),
            last_contact_at TEXT,
            last_error TEXT,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            CHECK (
                (role = 'connected_device' AND hub_url IS NOT NULL
                    AND hub_id IS NOT NULL AND hub_owner_login IS NOT NULL)
                OR (role != 'connected_device' AND hub_url IS NULL
                    AND hub_id IS NULL AND hub_owner_login IS NULL)
            )
        );",
    )
    .context("Failed to create sync identity and settings tables")?;
    Ok(())
}

fn migrate_sync_serve_ownership(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_serve_ownership (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            https_port INTEGER NOT NULL CHECK (https_port BETWEEN 1 AND 65535),
            mount_path TEXT NOT NULL,
            proxy_url TEXT NOT NULL,
            configured_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );",
    )
    .context("Failed to create sync Serve ownership table")?;
    Ok(())
}

fn migrate_dictation_shared_library(conn: &Connection) -> Result<()> {
    // Migration 2 intentionally did not create an identity until first use.
    // Dictation provenance is non-null, so establish that stable identity
    // before rebuilding the operational table.
    let device_id = conn
        .query_row(
            "SELECT device_id FROM sync_identity WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| uuid::Uuid::new_v4().hyphenated().to_string());
    conn.execute(
        "INSERT OR IGNORE INTO sync_identity (singleton, device_id) VALUES (1, ?1)",
        [&device_id],
    )
    .context("Failed to establish device identity for dictation migration")?;

    conn.execute_batch(
        "CREATE TABLE workflows_v4 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_type TEXT NOT NULL,
            text TEXT NOT NULL,
            audio_path TEXT NOT NULL,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            sync_id TEXT NOT NULL UNIQUE,
            origin_device_id TEXT NOT NULL,
            sync_version INTEGER NOT NULL DEFAULT 1 CHECK (sync_version >= 1),
            deleted_at TIMESTAMP
        );",
    )
    .context("Failed to create UUID-backed workflows table")?;

    let legacy_rows = {
        let mut statement = conn
            .prepare(
                "SELECT id, workflow_type, text, audio_path, created_at
                 FROM workflows ORDER BY id",
            )
            .context("Failed to inspect legacy workflows")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .context("Failed to read legacy workflows")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to map legacy workflows")?;
        rows
    };
    for (id, workflow_type, text, audio_path, created_at) in legacy_rows {
        conn.execute(
            "INSERT INTO workflows_v4
                (id, workflow_type, text, audio_path, created_at, sync_id, origin_device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                workflow_type,
                text,
                audio_path,
                created_at,
                uuid::Uuid::new_v4().hyphenated().to_string(),
                device_id,
            ],
        )
        .context("Failed to backfill a workflow UUID")?;
    }
    conn.execute_batch(
        "DROP TABLE workflows;
         ALTER TABLE workflows_v4 RENAME TO workflows;
         CREATE INDEX idx_workflows_created_at ON workflows(created_at DESC);
         CREATE INDEX idx_workflows_visible ON workflows(deleted_at, created_at DESC);

         CREATE TABLE shared_record_index (
            record_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL CHECK (kind = 'dictation'),
            origin_device_id TEXT,
            authoritative_revision INTEGER NOT NULL DEFAULT 0 CHECK (authoritative_revision >= 0),
            deleted_at TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );

         CREATE TABLE shared_dictations (
            record_id TEXT PRIMARY KEY,
            origin_device_id TEXT NOT NULL,
            text TEXT NOT NULL,
            source_created_at TEXT NOT NULL,
            source_updated_at TEXT NOT NULL,
            local_version INTEGER NOT NULL CHECK (local_version >= 1),
            authoritative_revision INTEGER NOT NULL CHECK (authoritative_revision >= 1),
            deleted_at TEXT,
            FOREIGN KEY(record_id) REFERENCES shared_record_index(record_id)
         );
         CREATE INDEX idx_shared_dictations_visible
            ON shared_dictations(deleted_at, source_created_at DESC, record_id DESC);

         CREATE TABLE sync_outbox_items (
            record_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK (kind = 'dictation'),
            local_version INTEGER NOT NULL CHECK (local_version >= 1),
            snapshot_json TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'pending'
                CHECK (state IN ('pending', 'uploading', 'synced', 'needs_attention')),
            accepted_hub_revision INTEGER,
            attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
            lease_owner TEXT,
            lease_expires_at TEXT,
            next_attempt_at TEXT,
            last_error TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(record_id, kind)
         );
         CREATE INDEX idx_sync_outbox_claim
            ON sync_outbox_items(state, next_attempt_at, lease_expires_at, created_at);

         CREATE TABLE sync_tombstones (
            record_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL CHECK (kind = 'dictation'),
            deleted_version INTEGER NOT NULL CHECK (deleted_version >= 1),
            deleted_at TEXT NOT NULL
         );

         CREATE TABLE shared_library_changes (
            cursor INTEGER PRIMARY KEY AUTOINCREMENT,
            operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
            kind TEXT NOT NULL CHECK (kind = 'dictation'),
            record_id TEXT NOT NULL,
            authoritative_revision INTEGER NOT NULL,
            change_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         CREATE INDEX idx_shared_library_changes_record
            ON shared_library_changes(record_id, cursor);",
    )
    .context("Failed to create dictation Shared Library schema")?;
    Ok(())
}

fn migrate_meeting_artifact_shared_library(conn: &Connection) -> Result<()> {
    let device_id: String = conn
        .query_row(
            "SELECT device_id FROM sync_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .context("Missing device identity for meeting migration")?;

    conn.execute_batch(
        "CREATE TABLE meetings_v5 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT,
            title_source TEXT,
            title_updated_at TIMESTAMP,
            title_version INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'recording',
            audio_path TEXT NOT NULL,
            source_filename TEXT,
            transcript_path TEXT,
            transcript_text TEXT,
            transcript_segments TEXT,
            duration_seconds INTEGER,
            started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP,
            error TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            deleted_at TIMESTAMP,
            sync_id TEXT NOT NULL UNIQUE,
            origin_device_id TEXT NOT NULL,
            sync_version INTEGER NOT NULL DEFAULT 1 CHECK (sync_version >= 1)
        );
        CREATE TABLE meeting_artifacts_v5 (
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
            sync_id TEXT NOT NULL UNIQUE,
            origin_device_id TEXT NOT NULL,
            sync_version INTEGER NOT NULL DEFAULT 1 CHECK (sync_version >= 1),
            FOREIGN KEY(meeting_id) REFERENCES meetings_v5(id),
            FOREIGN KEY(agent_profile_id) REFERENCES agent_profiles(id)
        );",
    )
    .context("Failed to create UUID-backed meeting tables")?;

    let meetings = {
        let mut statement = conn.prepare(
            "SELECT id, title, title_source, title_updated_at, title_version, status, audio_path,
                    source_filename, transcript_path, transcript_text, transcript_segments,
                    duration_seconds, started_at, completed_at, error, created_at, deleted_at
             FROM meetings ORDER BY id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, Option<String>>(16)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for row in meetings {
        conn.execute(
            "INSERT INTO meetings_v5 VALUES
             (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,1)",
            rusqlite::params![
                row.0,
                row.1,
                row.2,
                row.3,
                row.4,
                row.5,
                row.6,
                row.7,
                row.8,
                row.9,
                row.10,
                row.11,
                row.12,
                row.13,
                row.14,
                row.15,
                row.16,
                uuid::Uuid::new_v4().hyphenated().to_string(),
                device_id
            ],
        )?;
    }
    let artifacts = {
        let mut statement = conn.prepare(
            "SELECT id, meeting_id, kind, title, template_id, agent_profile_id, status,
                    content_markdown, error, stdout, stderr, created_at, updated_at, completed_at
             FROM meeting_artifacts ORDER BY id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, Option<String>>(13)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for row in artifacts {
        conn.execute(
            "INSERT INTO meeting_artifacts_v5 VALUES
             (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,1)",
            rusqlite::params![
                row.0,
                row.1,
                row.2,
                row.3,
                row.4,
                row.5,
                row.6,
                row.7,
                row.8,
                row.9,
                row.10,
                row.11,
                row.12,
                row.13,
                uuid::Uuid::new_v4().hyphenated().to_string(),
                device_id
            ],
        )?;
    }

    conn.execute_batch(
        "DROP TABLE meeting_artifacts;
         DROP TABLE meetings;
         ALTER TABLE meetings_v5 RENAME TO meetings;
         ALTER TABLE meeting_artifacts_v5 RENAME TO meeting_artifacts;
         CREATE INDEX idx_meetings_started_at ON meetings(started_at DESC);
         CREATE INDEX idx_meetings_status ON meetings(status);
         CREATE INDEX idx_meetings_deleted_at ON meetings(deleted_at);
         CREATE INDEX idx_meetings_visible_uuid ON meetings(deleted_at, sync_id);
         CREATE INDEX idx_meeting_artifacts_meeting_created ON meeting_artifacts(meeting_id, created_at DESC);
         CREATE INDEX idx_meeting_artifacts_status ON meeting_artifacts(status);

         ALTER TABLE shared_dictations RENAME TO shared_dictations_v4;
         ALTER TABLE shared_record_index RENAME TO shared_record_index_v4;
         CREATE TABLE shared_record_index (
            record_id TEXT PRIMARY KEY,
            kind TEXT NOT NULL CHECK (kind IN ('dictation','meeting','artifact')),
            origin_device_id TEXT,
            authoritative_revision INTEGER NOT NULL DEFAULT 0 CHECK (authoritative_revision >= 0),
            deleted_at TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );
         INSERT INTO shared_record_index SELECT * FROM shared_record_index_v4;
         CREATE TABLE shared_dictations (
            record_id TEXT PRIMARY KEY, origin_device_id TEXT NOT NULL, text TEXT NOT NULL,
            source_created_at TEXT NOT NULL, source_updated_at TEXT NOT NULL,
            local_version INTEGER NOT NULL CHECK(local_version >= 1),
            authoritative_revision INTEGER NOT NULL CHECK(authoritative_revision >= 1), deleted_at TEXT,
            FOREIGN KEY(record_id) REFERENCES shared_record_index(record_id)
         );
         INSERT INTO shared_dictations SELECT * FROM shared_dictations_v4;
         DROP TABLE shared_dictations_v4;
         DROP TABLE shared_record_index_v4;
         CREATE INDEX idx_shared_dictations_visible ON shared_dictations(deleted_at, source_created_at DESC, record_id DESC);

         CREATE TABLE shared_meetings (
            record_id TEXT PRIMARY KEY,
            origin_device_id TEXT NOT NULL,
            title TEXT,
            title_source TEXT,
            title_version INTEGER NOT NULL DEFAULT 0,
            title_authority TEXT NOT NULL DEFAULT 'origin' CHECK(title_authority IN ('origin','hub')),
            source_filename TEXT,
            transcript_text TEXT NOT NULL,
            transcript_segments TEXT,
            duration_seconds INTEGER NOT NULL,
            status TEXT NOT NULL CHECK(status = 'completed'),
            source_created_at TEXT NOT NULL,
            source_updated_at TEXT NOT NULL,
            source_completed_at TEXT NOT NULL,
            local_version INTEGER NOT NULL CHECK(local_version >= 1),
            authoritative_revision INTEGER NOT NULL CHECK(authoritative_revision >= 1),
            deleted_at TEXT,
            FOREIGN KEY(record_id) REFERENCES shared_record_index(record_id)
         );
         CREATE INDEX idx_shared_meetings_visible ON shared_meetings(deleted_at, source_created_at DESC, record_id DESC);
         CREATE TABLE shared_artifacts (
            record_id TEXT PRIMARY KEY,
            parent_record_id TEXT NOT NULL,
            origin_device_id TEXT NOT NULL,
            artifact_kind TEXT NOT NULL,
            title TEXT NOT NULL,
            template_id TEXT,
            agent_profile_name TEXT,
            content_markdown TEXT NOT NULL,
            source_created_at TEXT NOT NULL,
            source_updated_at TEXT NOT NULL,
            source_completed_at TEXT NOT NULL,
            local_version INTEGER NOT NULL CHECK(local_version >= 1),
            authoritative_revision INTEGER NOT NULL CHECK(authoritative_revision >= 1),
            deleted_at TEXT,
            FOREIGN KEY(record_id) REFERENCES shared_record_index(record_id),
            FOREIGN KEY(parent_record_id) REFERENCES shared_meetings(record_id)
         );
         CREATE INDEX idx_shared_artifacts_parent ON shared_artifacts(parent_record_id, deleted_at, source_created_at DESC);
         CREATE TABLE sync_artifact_runs (
            run_id TEXT PRIMARY KEY,
            artifact_record_id TEXT NOT NULL UNIQUE,
            parent_record_id TEXT NOT NULL,
            origin_device_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            title TEXT NOT NULL,
            template_id TEXT,
            agent_profile_name TEXT,
            agent_profile_id INTEGER,
            status TEXT NOT NULL CHECK(status IN ('pending','running','completed','error')),
            content_markdown TEXT,
            error TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TEXT,
            FOREIGN KEY(agent_profile_id) REFERENCES agent_profiles(id)
         );

         ALTER TABLE sync_outbox_items RENAME TO sync_outbox_items_v4;
         CREATE TABLE sync_outbox_items (
            record_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('dictation','meeting','artifact')),
            local_version INTEGER NOT NULL CHECK(local_version >= 1), snapshot_json TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','uploading','synced','needs_attention')),
            accepted_hub_revision INTEGER, attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
            lease_owner TEXT, lease_expires_at TEXT, next_attempt_at TEXT, last_error TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(record_id,kind)
         );
         INSERT INTO sync_outbox_items SELECT * FROM sync_outbox_items_v4;
         DROP TABLE sync_outbox_items_v4;
         CREATE INDEX idx_sync_outbox_claim ON sync_outbox_items(state,next_attempt_at,lease_expires_at,created_at);

         ALTER TABLE sync_tombstones RENAME TO sync_tombstones_v4;
         CREATE TABLE sync_tombstones(record_id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK(kind IN ('dictation','meeting','artifact')), deleted_version INTEGER NOT NULL CHECK(deleted_version >= 1), deleted_at TEXT NOT NULL);
         INSERT INTO sync_tombstones SELECT * FROM sync_tombstones_v4;
         DROP TABLE sync_tombstones_v4;
         ALTER TABLE shared_library_changes RENAME TO shared_library_changes_v4;
         CREATE TABLE shared_library_changes(cursor INTEGER PRIMARY KEY AUTOINCREMENT, operation TEXT NOT NULL CHECK(operation IN ('upsert','delete')), kind TEXT NOT NULL CHECK(kind IN ('dictation','meeting','artifact')), record_id TEXT NOT NULL, authoritative_revision INTEGER NOT NULL, change_json TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
         INSERT INTO shared_library_changes SELECT * FROM shared_library_changes_v4;
         DROP TABLE shared_library_changes_v4;
         CREATE INDEX idx_shared_library_changes_record ON shared_library_changes(record_id,cursor);"
    ).context("Failed to install meeting Shared Library schema")?;
    Ok(())
}

fn migrate_recording_payload_sync(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE sync_outbox_blobs (
            record_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('dictation','meeting')),
            checksum TEXT,
            staged_path TEXT,
            byte_size INTEGER,
            media_type TEXT,
            payload_role TEXT NOT NULL DEFAULT 'recording' CHECK(payload_role = 'recording'),
            availability TEXT NOT NULL CHECK(availability IN ('pending','available','unavailable','needs_attention')),
            state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','uploading','synced','needs_attention')),
            attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
            lease_owner TEXT,
            lease_expires_at TEXT,
            next_attempt_at TEXT,
            last_error TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(record_id,payload_role),
            CHECK(
                (availability = 'unavailable' AND checksum IS NULL AND staged_path IS NULL
                    AND byte_size IS NULL AND media_type IS NULL AND state = 'synced')
                OR
                (availability != 'unavailable' AND checksum IS NOT NULL AND byte_size IS NOT NULL
                    AND byte_size > 0 AND media_type IS NOT NULL)
            )
        );
        CREATE INDEX idx_sync_outbox_blobs_claim
            ON sync_outbox_blobs(state,next_attempt_at,lease_expires_at,created_at);
        CREATE INDEX idx_sync_outbox_blobs_checksum
            ON sync_outbox_blobs(checksum,state);

        CREATE TABLE library_blobs (
            checksum TEXT PRIMARY KEY,
            canonical_path TEXT NOT NULL UNIQUE,
            byte_size INTEGER NOT NULL CHECK(byte_size > 0),
            media_type TEXT NOT NULL,
            verified INTEGER NOT NULL DEFAULT 1 CHECK(verified IN (0,1)),
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        CREATE TABLE library_record_blobs (
            record_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('dictation','meeting')),
            checksum TEXT,
            byte_size INTEGER,
            media_type TEXT,
            payload_role TEXT NOT NULL DEFAULT 'recording' CHECK(payload_role = 'recording'),
            availability TEXT NOT NULL CHECK(availability IN ('pending','available','unavailable','needs_attention')),
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(record_id,payload_role),
            FOREIGN KEY(record_id) REFERENCES shared_record_index(record_id) ON DELETE CASCADE,
            CHECK(
                (availability = 'unavailable' AND checksum IS NULL AND byte_size IS NULL AND media_type IS NULL)
                OR
                (availability != 'unavailable' AND checksum IS NOT NULL AND byte_size IS NOT NULL
                    AND byte_size > 0 AND media_type IS NOT NULL)
            )
        );
        CREATE INDEX idx_library_record_blobs_checksum
            ON library_record_blobs(checksum,availability);

        ALTER TABLE shared_library_changes RENAME TO shared_library_changes_v5;
        CREATE TABLE shared_library_changes(
            cursor INTEGER PRIMARY KEY AUTOINCREMENT,
            operation TEXT NOT NULL CHECK(operation IN ('upsert','delete','payload_availability')),
            kind TEXT NOT NULL CHECK(kind IN ('dictation','meeting','artifact')),
            record_id TEXT NOT NULL,
            authoritative_revision INTEGER NOT NULL,
            change_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );
        INSERT INTO shared_library_changes SELECT * FROM shared_library_changes_v5;
        DROP TABLE shared_library_changes_v5;
        CREATE INDEX idx_shared_library_changes_record
            ON shared_library_changes(record_id,cursor);",
    )
    .context("Failed to install Recording Payload schema")?;
    Ok(())
}

fn migrate_payload_staging_failures(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE sync_outbox_blobs RENAME TO sync_outbox_blobs_v6;
         CREATE TABLE sync_outbox_blobs (
            record_id TEXT NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('dictation','meeting')),
            checksum TEXT,
            staged_path TEXT,
            byte_size INTEGER,
            media_type TEXT,
            payload_role TEXT NOT NULL DEFAULT 'recording' CHECK(payload_role = 'recording'),
            availability TEXT NOT NULL CHECK(availability IN ('pending','available','unavailable','needs_attention')),
            state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','uploading','synced','needs_attention')),
            attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
            lease_owner TEXT,
            lease_expires_at TEXT,
            next_attempt_at TEXT,
            last_error TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(record_id,payload_role),
            CHECK(
                (availability = 'unavailable' AND checksum IS NULL AND staged_path IS NULL
                    AND byte_size IS NULL AND media_type IS NULL AND state = 'synced')
                OR
                (availability = 'needs_attention' AND checksum IS NULL AND staged_path IS NULL
                    AND byte_size IS NULL AND media_type IS NULL AND state = 'needs_attention')
                OR
                (availability != 'unavailable' AND checksum IS NOT NULL AND byte_size IS NOT NULL
                    AND byte_size > 0 AND media_type IS NOT NULL)
            )
         );
         INSERT INTO sync_outbox_blobs SELECT * FROM sync_outbox_blobs_v6;
         DROP TABLE sync_outbox_blobs_v6;
         CREATE INDEX idx_sync_outbox_blobs_claim
             ON sync_outbox_blobs(state,next_attempt_at,lease_expires_at,created_at);
         CREATE INDEX idx_sync_outbox_blobs_checksum
             ON sync_outbox_blobs(checksum,state);",
    )
    .context("Failed to allow payload staging failures without fabricated blob metadata")?;
    Ok(())
}

/// Add `column` to `table` only when a pre-numbered database lacks it.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .with_context(|| format!("Failed to inspect columns of {table}"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .with_context(|| format!("Failed to read columns of {table}"))?
        .filter_map(|column| column.ok())
        .any(|existing| existing == column);

    if exists {
        return Ok(());
    }

    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
        [],
    )
    .with_context(|| format!("Failed to add column {column} to {table}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_marks_legacy_non_empty_titles_manual_and_clears_blank_titles() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE meetings (
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
            );
            INSERT INTO meetings (title, audio_path) VALUES ('  Legacy Planning  ', '/tmp/one.wav');
            INSERT INTO meetings (title, audio_path) VALUES ('   ', '/tmp/two.wav');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let legacy = crate::db::meetings::MeetingRepository::get(&conn, 1)
            .unwrap()
            .unwrap();
        assert_eq!(legacy.title.as_deref(), Some("Legacy Planning"));
        assert_eq!(legacy.title_source.as_deref(), Some("manual"));
        let blank = crate::db::meetings::MeetingRepository::get(&conn, 2)
            .unwrap()
            .unwrap();
        assert_eq!(blank.title, None);
        assert_eq!(blank.title_source, None);
    }

    #[test]
    fn every_opened_connection_enforces_foreign_keys() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        let conn = open_db_at(&path).unwrap();

        let enabled: bool = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert!(enabled);
    }
}
