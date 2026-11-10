//! Persistence for authenticated ingress and external audio imports.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressAccessKey {
    pub id: String,
    pub name: String,
    pub scope: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalImport {
    pub id: i64,
    pub provider: String,
    pub source_instance: String,
    pub external_id: String,
    pub status: String,
    pub audio_note_id: Option<i64>,
    pub recorded_at: Option<String>,
    pub source_filename: Option<String>,
    pub error: Option<String>,
    pub attempt_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportClaim {
    Claimed(i64),
    Accepted { import_id: i64, note_id: i64 },
    InProgress { import_id: i64 },
}

/// How a recording source delivers audio. A later provider picks one of these;
/// the table already stores both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMode {
    Poll,
    Webhook,
}

impl SyncMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::Webhook => "webhook",
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "poll" => Ok(Self::Poll),
            "webhook" => Ok(Self::Webhook),
            other => bail!("Unknown recording source sync mode {other}"),
        }
    }
}

/// One configured recording source. Provider-specific settings live in `config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingSource {
    pub provider: String,
    pub source_instance: String,
    pub sync_mode: SyncMode,
    pub enabled: bool,
    pub interval_minutes: Option<i64>,
    pub import_after: Option<String>,
    pub config: Option<String>,
    pub running: bool,
    pub last_started_at: Option<String>,
    pub last_completed_at: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

pub struct IntegrationRepository;

impl IntegrationRepository {
    pub fn insert_access_key(
        conn: &Connection,
        id: &str,
        name: &str,
        scope: &str,
        secret_hash: &[u8],
    ) -> Result<IngressAccessKey> {
        let name = name.trim();
        if name.is_empty() {
            bail!("Ingress access key name must not be empty");
        }
        validate_scope(scope)?;
        validate_hash(secret_hash)?;
        conn.execute(
            "INSERT INTO ingress_access_keys(id, name, scope, secret_hash) VALUES(?1, ?2, ?3, ?4)",
            params![id, name, scope, secret_hash],
        )
        .context("Failed to insert ingress access key")?;
        Self::access_key(conn, id)?.context("Inserted ingress access key disappeared")
    }

    pub fn list_access_keys(conn: &Connection) -> Result<Vec<IngressAccessKey>> {
        let mut statement = conn.prepare(
            "SELECT id, name, scope, created_at, last_used_at, revoked_at
             FROM ingress_access_keys ORDER BY created_at DESC, id DESC",
        )?;
        let keys = statement
            .query_map([], row_to_access_key)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to list ingress access keys")?;
        Ok(keys)
    }

    pub fn access_key(conn: &Connection, id: &str) -> Result<Option<IngressAccessKey>> {
        conn.query_row(
            "SELECT id, name, scope, created_at, last_used_at, revoked_at
             FROM ingress_access_keys WHERE id = ?1",
            [id],
            row_to_access_key,
        )
        .optional()
        .context("Failed to read ingress access key")
    }

    pub fn active_access_key_hash(
        conn: &Connection,
        id: &str,
        scope: &str,
    ) -> Result<Option<Vec<u8>>> {
        validate_scope(scope)?;
        conn.query_row(
            "SELECT secret_hash FROM ingress_access_keys
             WHERE id = ?1 AND scope = ?2 AND revoked_at IS NULL",
            params![id, scope],
            |row| row.get(0),
        )
        .optional()
        .context("Failed to authenticate ingress access key")
    }

    pub fn touch_access_key(conn: &Connection, id: &str) -> Result<()> {
        conn.execute(
            "UPDATE ingress_access_keys SET last_used_at = CURRENT_TIMESTAMP
             WHERE id = ?1 AND revoked_at IS NULL",
            [id],
        )?;
        Ok(())
    }

    pub fn revoke_access_key(conn: &Connection, id: &str) -> Result<Option<IngressAccessKey>> {
        conn.execute(
            "UPDATE ingress_access_keys
             SET revoked_at = COALESCE(revoked_at, CURRENT_TIMESTAMP)
             WHERE id = ?1",
            [id],
        )?;
        Self::access_key(conn, id)
    }

    pub fn claim_external_import(
        conn: &Connection,
        provider: &str,
        source_instance: &str,
        external_id: &str,
        recorded_at: Option<&str>,
        source_filename: Option<&str>,
    ) -> Result<ImportClaim> {
        for (label, value) in [
            ("provider", provider),
            ("source instance", source_instance),
            ("external ID", external_id),
        ] {
            if value.trim().is_empty() {
                bail!("External import {label} must not be empty");
            }
        }

        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let existing = external_import_by_identity(&tx, provider, source_instance, external_id)?;
        let claim = match existing {
            Some(existing) if existing.status == "accepted" => ImportClaim::Accepted {
                import_id: existing.id,
                note_id: existing
                    .audio_note_id
                    .context("Accepted external import has no Audio Note")?,
            },
            Some(existing) if existing.status == "pending" => ImportClaim::InProgress {
                import_id: existing.id,
            },
            Some(existing) => {
                tx.execute(
                    "UPDATE external_imports
                     SET status = 'pending', audio_note_id = NULL, error = NULL,
                         recorded_at = ?1, source_filename = ?2,
                         attempt_count = attempt_count + 1, updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?3",
                    params![recorded_at, source_filename, existing.id],
                )?;
                ImportClaim::Claimed(existing.id)
            }
            None => {
                tx.execute(
                    "INSERT INTO external_imports
                     (provider, source_instance, external_id, status, recorded_at, source_filename)
                     VALUES(?1, ?2, ?3, 'pending', ?4, ?5)",
                    params![
                        provider,
                        source_instance,
                        external_id,
                        recorded_at,
                        source_filename
                    ],
                )?;
                ImportClaim::Claimed(tx.last_insert_rowid())
            }
        };
        tx.commit()?;
        Ok(claim)
    }

    pub fn accept_external_import(
        conn: &Connection,
        import_id: i64,
        audio_note_id: i64,
    ) -> Result<()> {
        let changed = conn.execute(
            "UPDATE external_imports
             SET status = 'accepted', audio_note_id = ?1, error = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2 AND status = 'pending'",
            params![audio_note_id, import_id],
        )?;
        if changed != 1 {
            bail!("External import is not pending");
        }
        Ok(())
    }

    pub fn fail_external_import(conn: &Connection, import_id: i64, error: &str) -> Result<()> {
        conn.execute(
            "UPDATE external_imports
             SET status = 'failed', error = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?2 AND status = 'pending'",
            params![error, import_id],
        )?;
        Ok(())
    }

    pub fn recent_external_imports(conn: &Connection, limit: usize) -> Result<Vec<ExternalImport>> {
        let limit = i64::try_from(limit.min(100)).unwrap_or(100);
        let mut statement = conn.prepare(
            "SELECT id, provider, source_instance, external_id, status, audio_note_id,
                    recorded_at, source_filename, error, attempt_count, created_at, updated_at
             FROM external_imports ORDER BY updated_at DESC, id DESC LIMIT ?1",
        )?;
        let imports = statement
            .query_map([limit], row_to_external_import)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to list external imports")?;
        Ok(imports)
    }

    pub fn accepted_external_import_note_id(
        conn: &Connection,
        provider: &str,
        source_instance: &str,
        external_id: &str,
    ) -> Result<Option<i64>> {
        conn.query_row(
            "SELECT audio_note_id FROM external_imports
             WHERE provider = ?1 AND source_instance = ?2 AND external_id = ?3
               AND status = 'accepted'",
            params![provider, source_instance, external_id],
            |row| row.get(0),
        )
        .optional()
        .context("Failed to check external import identity")
    }

    pub fn recording_source(
        conn: &Connection,
        provider: &str,
        source_instance: &str,
    ) -> Result<RecordingSource> {
        conn.query_row(
            &format!("SELECT {RECORDING_SOURCE_COLUMNS} FROM recording_sources WHERE provider = ?1 AND source_instance = ?2"),
            params![provider, source_instance],
            row_to_recording_source,
        )
        .optional()
        .context("Failed to read recording source")?
        .with_context(|| format!("Recording source {provider}/{source_instance} is not configured"))
    }

    pub fn enabled_poll_sources(conn: &Connection) -> Result<Vec<RecordingSource>> {
        let mut statement = conn.prepare(&format!(
            "SELECT {RECORDING_SOURCE_COLUMNS} FROM recording_sources
             WHERE sync_mode = 'poll' AND enabled = 1
             ORDER BY provider, source_instance"
        ))?;
        let sources = statement
            .query_map([], row_to_recording_source)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to list recording sources")?;
        Ok(sources)
    }

    pub fn update_recording_source_settings(
        conn: &Connection,
        provider: &str,
        source_instance: &str,
        enabled: bool,
        interval_minutes: Option<i64>,
        import_after: Option<&str>,
    ) -> Result<()> {
        if interval_minutes.is_some_and(|minutes| minutes <= 0) {
            bail!("Recording source poll interval must be greater than zero");
        }
        let changed = conn.execute(
            "UPDATE recording_sources
             SET enabled = ?1, interval_minutes = ?2, import_after = ?3,
                 updated_at = CURRENT_TIMESTAMP
             WHERE provider = ?4 AND source_instance = ?5",
            params![
                enabled,
                interval_minutes,
                import_after,
                provider,
                source_instance
            ],
        )?;
        if changed != 1 {
            bail!("Recording source {provider}/{source_instance} is not configured");
        }
        Ok(())
    }

    pub fn start_sync(conn: &Connection, provider: &str, source_instance: &str) -> Result<bool> {
        Ok(conn.execute(
            "UPDATE recording_sources
             SET running = 1, last_started_at = CURRENT_TIMESTAMP,
                 last_error = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE provider = ?1 AND source_instance = ?2 AND running = 0",
            params![provider, source_instance],
        )? == 1)
    }

    pub fn update_import_cursor(
        conn: &Connection,
        provider: &str,
        source_instance: &str,
        import_after: &str,
    ) -> Result<()> {
        let changed = conn.execute(
            "UPDATE recording_sources
             SET import_after = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE provider = ?2 AND source_instance = ?3",
            params![import_after, provider, source_instance],
        )?;
        if changed != 1 {
            bail!("Recording source {provider}/{source_instance} is not configured");
        }
        Ok(())
    }

    pub fn finish_sync(
        conn: &Connection,
        provider: &str,
        source_instance: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let changed = conn.execute(
            "UPDATE recording_sources
             SET running = 0,
                 last_completed_at = CASE WHEN ?1 IS NULL THEN CURRENT_TIMESTAMP ELSE last_completed_at END,
                 last_error = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE provider = ?2 AND source_instance = ?3",
            params![error, provider, source_instance],
        )?;
        if changed != 1 {
            bail!("Recording source {provider}/{source_instance} is not configured");
        }
        Ok(())
    }

    pub fn sweep_interrupted_imports(conn: &Connection) -> Result<()> {
        conn.execute(
            "UPDATE external_imports
             SET status = 'failed', error = 'Interrupted: the Audetic daemon stopped during import',
                 updated_at = CURRENT_TIMESTAMP
             WHERE status = 'pending'",
            [],
        )?;
        Ok(())
    }

    pub fn sweep_interrupted_syncs(conn: &Connection) -> Result<()> {
        conn.execute(
            "UPDATE recording_sources
             SET running = 0,
                 last_error = 'Interrupted: the Audetic daemon stopped during synchronization',
                 updated_at = CURRENT_TIMESTAMP
             WHERE running = 1",
            [],
        )?;
        Ok(())
    }
}

const RECORDING_SOURCE_COLUMNS: &str =
    "provider, source_instance, sync_mode, enabled, interval_minutes, \
     import_after, config, running, last_started_at, last_completed_at, last_error, updated_at";

fn external_import_by_identity(
    conn: &Connection,
    provider: &str,
    source_instance: &str,
    external_id: &str,
) -> Result<Option<ExternalImport>> {
    conn.query_row(
        "SELECT id, provider, source_instance, external_id, status, audio_note_id,
                recorded_at, source_filename, error, attempt_count, created_at, updated_at
         FROM external_imports
         WHERE provider = ?1 AND source_instance = ?2 AND external_id = ?3",
        params![provider, source_instance, external_id],
        row_to_external_import,
    )
    .optional()
    .context("Failed to read external import")
}

fn row_to_access_key(row: &Row<'_>) -> rusqlite::Result<IngressAccessKey> {
    Ok(IngressAccessKey {
        id: row.get(0)?,
        name: row.get(1)?,
        scope: row.get(2)?,
        created_at: row.get(3)?,
        last_used_at: row.get(4)?,
        revoked_at: row.get(5)?,
    })
}

fn row_to_external_import(row: &Row<'_>) -> rusqlite::Result<ExternalImport> {
    Ok(ExternalImport {
        id: row.get(0)?,
        provider: row.get(1)?,
        source_instance: row.get(2)?,
        external_id: row.get(3)?,
        status: row.get(4)?,
        audio_note_id: row.get(5)?,
        recorded_at: row.get(6)?,
        source_filename: row.get(7)?,
        error: row.get(8)?,
        attempt_count: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn row_to_recording_source(row: &Row<'_>) -> rusqlite::Result<RecordingSource> {
    let sync_mode: String = row.get(2)?;
    Ok(RecordingSource {
        provider: row.get(0)?,
        source_instance: row.get(1)?,
        sync_mode: SyncMode::parse(&sync_mode).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, error.into())
        })?,
        enabled: row.get(3)?,
        interval_minutes: row.get(4)?,
        import_after: row.get(5)?,
        config: row.get(6)?,
        running: row.get(7)?,
        last_started_at: row.get(8)?,
        last_completed_at: row.get(9)?,
        last_error: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn validate_scope(scope: &str) -> Result<()> {
    if !matches!(scope, "index" | "generic") {
        bail!("Unknown ingress access key scope: {scope}");
    }
    Ok(())
}

fn validate_hash(secret_hash: &[u8]) -> Result<()> {
    if secret_hash.len() != 32 {
        bail!("Ingress access key hash must contain exactly 32 bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::ensure_current(&conn).unwrap();
        conn
    }

    #[test]
    fn access_keys_persist_only_hashes_and_revocation_disables_authentication() {
        let conn = setup();
        let hash = [7_u8; 32];
        let key =
            IntegrationRepository::insert_access_key(&conn, "key-id", "Index ring", "index", &hash)
                .unwrap();
        assert_eq!(key.name, "Index ring");
        assert_eq!(
            IntegrationRepository::active_access_key_hash(&conn, "key-id", "index").unwrap(),
            Some(hash.to_vec())
        );
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(ingress_access_keys)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column == "secret"));

        IntegrationRepository::revoke_access_key(&conn, "key-id").unwrap();
        assert!(
            IntegrationRepository::active_access_key_hash(&conn, "key-id", "index")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn external_import_claims_are_idempotent_and_failed_claims_retry() {
        let conn = setup();
        let claim = IntegrationRepository::claim_external_import(
            &conn,
            "pebble_index",
            "key-id",
            "recording-id",
            Some("2026-09-30 12:00:00"),
            Some("recording-id.m4a"),
        )
        .unwrap();
        let ImportClaim::Claimed(import_id) = claim else {
            panic!("first request must claim the import")
        };
        assert_eq!(
            IntegrationRepository::claim_external_import(
                &conn,
                "pebble_index",
                "key-id",
                "recording-id",
                None,
                None,
            )
            .unwrap(),
            ImportClaim::InProgress { import_id }
        );

        IntegrationRepository::fail_external_import(&conn, import_id, "network").unwrap();
        assert_eq!(
            IntegrationRepository::claim_external_import(
                &conn,
                "pebble_index",
                "key-id",
                "recording-id",
                None,
                None,
            )
            .unwrap(),
            ImportClaim::Claimed(import_id)
        );
        conn.execute(
            "INSERT INTO audio_notes(audio_path) VALUES('/tmp/import.m4a')",
            [],
        )
        .unwrap();
        let note_id = conn.last_insert_rowid();
        IntegrationRepository::accept_external_import(&conn, import_id, note_id).unwrap();
        assert_eq!(
            IntegrationRepository::claim_external_import(
                &conn,
                "pebble_index",
                "key-id",
                "recording-id",
                None,
                None,
            )
            .unwrap(),
            ImportClaim::Accepted { import_id, note_id }
        );
    }

    #[test]
    fn startup_recovery_releases_interrupted_work() {
        let conn = setup();
        IntegrationRepository::claim_external_import(
            &conn,
            "plaud",
            "default",
            "recording-id",
            None,
            None,
        )
        .unwrap();
        assert!(IntegrationRepository::start_sync(&conn, "plaud", "default").unwrap());

        IntegrationRepository::sweep_interrupted_imports(&conn).unwrap();
        IntegrationRepository::sweep_interrupted_syncs(&conn).unwrap();

        let import = external_import_by_identity(&conn, "plaud", "default", "recording-id")
            .unwrap()
            .unwrap();
        assert_eq!(import.status, "failed");
        assert!(import.error.unwrap().contains("daemon stopped"));
        let state = IntegrationRepository::recording_source(&conn, "plaud", "default").unwrap();
        assert_eq!(state.sync_mode, SyncMode::Poll);
        assert!(!state.running);
        assert!(state.last_error.unwrap().contains("daemon stopped"));
    }
}
