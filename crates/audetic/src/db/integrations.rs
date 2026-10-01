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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaudSyncState {
    pub enabled: bool,
    pub interval_minutes: i64,
    pub import_after: Option<String>,
    pub running: bool,
    pub last_started_at: Option<String>,
    pub last_completed_at: Option<String>,
    pub last_error: Option<String>,
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

    pub fn plaud_state(conn: &Connection) -> Result<PlaudSyncState> {
        conn.query_row(
            "SELECT enabled, interval_minutes, import_after, running,
                    last_started_at, last_completed_at, last_error
             FROM plaud_sync_state WHERE singleton = 1",
            [],
            |row| {
                Ok(PlaudSyncState {
                    enabled: row.get(0)?,
                    interval_minutes: row.get(1)?,
                    import_after: row.get(2)?,
                    running: row.get(3)?,
                    last_started_at: row.get(4)?,
                    last_completed_at: row.get(5)?,
                    last_error: row.get(6)?,
                })
            },
        )
        .context("Failed to read Plaud synchronization state")
    }

    pub fn update_plaud_settings(
        conn: &Connection,
        enabled: bool,
        interval_minutes: i64,
        import_after: Option<&str>,
    ) -> Result<PlaudSyncState> {
        if !(5..=1440).contains(&interval_minutes) {
            bail!("Plaud sync interval must be between 5 and 1440 minutes");
        }
        conn.execute(
            "UPDATE plaud_sync_state
             SET enabled = ?1, interval_minutes = ?2, import_after = ?3,
                 updated_at = CURRENT_TIMESTAMP
             WHERE singleton = 1",
            params![enabled, interval_minutes, import_after],
        )?;
        Self::plaud_state(conn)
    }

    pub fn start_plaud_sync(conn: &Connection) -> Result<bool> {
        Ok(conn.execute(
            "UPDATE plaud_sync_state
             SET running = 1, last_started_at = CURRENT_TIMESTAMP,
                 last_error = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE singleton = 1 AND running = 0",
            [],
        )? == 1)
    }

    pub fn update_plaud_cursor(conn: &Connection, import_after: &str) -> Result<()> {
        conn.execute(
            "UPDATE plaud_sync_state
             SET import_after = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE singleton = 1",
            [import_after],
        )?;
        Ok(())
    }

    pub fn finish_plaud_sync(conn: &Connection, error: Option<&str>) -> Result<()> {
        conn.execute(
            "UPDATE plaud_sync_state
             SET running = 0,
                 last_completed_at = CASE WHEN ?1 IS NULL THEN CURRENT_TIMESTAMP ELSE last_completed_at END,
                 last_error = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE singleton = 1",
            [error],
        )?;
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

    pub fn sweep_interrupted_plaud_sync(conn: &Connection) -> Result<()> {
        conn.execute(
            "UPDATE plaud_sync_state
             SET running = 0,
                 last_error = 'Interrupted: the Audetic daemon stopped during Plaud synchronization',
                 updated_at = CURRENT_TIMESTAMP
             WHERE singleton = 1 AND running = 1",
            [],
        )?;
        Ok(())
    }
}

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
        crate::db::migrate(&conn).unwrap();
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
        assert!(IntegrationRepository::start_plaud_sync(&conn).unwrap());

        IntegrationRepository::sweep_interrupted_imports(&conn).unwrap();
        IntegrationRepository::sweep_interrupted_plaud_sync(&conn).unwrap();

        let import = external_import_by_identity(&conn, "plaud", "default", "recording-id")
            .unwrap()
            .unwrap();
        assert_eq!(import.status, "failed");
        assert!(import.error.unwrap().contains("daemon stopped"));
        let state = IntegrationRepository::plaud_state(&conn).unwrap();
        assert!(!state.running);
        assert!(state.last_error.unwrap().contains("daemon stopped"));
    }
}
