//! SQLite persistence for synchronization identity, Hub devices, and Client pairing.

use anyhow::{bail, Context, Result};
use audetic_core::sync::{PairedHub, SyncDevice};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use uuid::Uuid;

use std::fmt;

pub struct SyncRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindDeviceOutcome {
    Bound(SyncDevice),
    AlreadyBound(SyncDevice),
    Conflict,
    NotFound,
    Revoked,
}

/// Authenticated identity available to the daemon sync service, never an API DTO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedDevice {
    pub device_id: String,
    pub client_node_id: Option<String>,
}

/// Private Client-side storage record containing the reusable bearer credential.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ClientPairing {
    pub(crate) hub_url: String,
    pub(crate) hub_node_id: String,
    pub(crate) device_id: String,
    pub(crate) protocol_version: u16,
    pub(crate) bearer_credential: String,
    pub(crate) paired_at: String,
}

impl fmt::Debug for ClientPairing {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientPairing")
            .field("hub_url", &self.hub_url)
            .field("hub_node_id", &self.hub_node_id)
            .field("device_id", &self.device_id)
            .field("protocol_version", &self.protocol_version)
            .field("bearer_credential", &"[REDACTED]")
            .field("paired_at", &self.paired_at)
            .finish()
    }
}

impl ClientPairing {
    pub(crate) fn public_projection(&self) -> PairedHub {
        PairedHub {
            hub_url: self.hub_url.clone(),
            hub_node_id: self.hub_node_id.clone(),
            device_id: self.device_id.clone(),
            protocol_version: self.protocol_version,
            paired_at: self.paired_at.clone(),
        }
    }
}

impl SyncRepository {
    pub(crate) const ENTITY_IDENTITY_VERSION: i64 = 1;

    pub(crate) fn migrate_pairing_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sync_devices (
                device_id TEXT PRIMARY KEY,
                name TEXT NOT NULL CHECK (trim(name) <> ''),
                client_node_id TEXT,
                credential_hash BLOB NOT NULL UNIQUE
                    CHECK (typeof(credential_hash) = 'blob' AND length(credential_hash) = 32),
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                paired_at TEXT,
                revoked_at TEXT,
                CHECK (
                    (client_node_id IS NULL AND paired_at IS NULL)
                    OR (client_node_id IS NOT NULL AND paired_at IS NOT NULL)
                )
            );
            CREATE TABLE IF NOT EXISTS sync_client_pairing (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                hub_url TEXT NOT NULL CHECK (trim(hub_url) <> ''),
                hub_node_id TEXT NOT NULL CHECK (trim(hub_node_id) <> ''),
                device_id TEXT NOT NULL CHECK (trim(device_id) <> ''),
                protocol_version INTEGER NOT NULL CHECK (protocol_version > 0),
                bearer_credential TEXT NOT NULL CHECK (trim(bearer_credential) <> ''),
                paired_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP CHECK (trim(paired_at) <> '')
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_sync_devices_active_client_node
                ON sync_devices(client_node_id)
                WHERE client_node_id IS NOT NULL AND revoked_at IS NULL;",
        )
        .context("Failed to create synchronization pairing tables")?;
        add_protocol_version_if_missing(conn)?;
        Ok(())
    }

    pub(crate) fn ensure_node_id(conn: &Connection) -> Result<String> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sync_metadata (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                node_id TEXT NOT NULL,
                entity_identity_version INTEGER NOT NULL DEFAULT 0
            )",
            [],
        )
        .context("Failed to create sync metadata table")?;

        if let Some(node_id) = Self::optional_node_id(conn)? {
            return Self::validate_node_id(node_id);
        }

        let candidate = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT OR IGNORE INTO sync_metadata (singleton, node_id) VALUES (1, ?1)",
            params![candidate],
        )
        .context("Failed to initialize sync node identity")?;

        Self::node_id(conn)
    }

    pub fn node_id(conn: &Connection) -> Result<String> {
        let node_id = Self::optional_node_id(conn)?
            .context("Sync metadata does not contain a node identity")?;
        Self::validate_node_id(node_id)
    }

    pub fn issue_device(
        conn: &Connection,
        name: &str,
        credential_hash: &[u8],
    ) -> Result<SyncDevice> {
        let name = name.trim();
        if name.is_empty() {
            bail!("Sync device name must not be empty");
        }
        validate_credential_hash(credential_hash)?;

        let device_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO sync_devices (device_id, name, credential_hash) VALUES (?1, ?2, ?3)",
            params![device_id, name, credential_hash],
        )
        .context("Failed to issue sync device credential")?;

        device_by_id(conn, &device_id)?.context("Issued sync device was not persisted")
    }

    pub fn list_devices(conn: &Connection) -> Result<Vec<SyncDevice>> {
        let mut stmt = conn
            .prepare(
                "SELECT device_id, name, client_node_id, created_at, paired_at, revoked_at
                 FROM sync_devices ORDER BY created_at ASC, device_id ASC",
            )
            .context("Failed to prepare sync device list")?;
        let rows = stmt
            .query_map([], row_to_device)
            .context("Failed to query sync devices")?;
        let mut devices = Vec::new();
        for row in rows {
            devices.push(row??);
        }
        Ok(devices)
    }

    pub fn authenticate_device(
        conn: &Connection,
        credential_hash: &[u8],
    ) -> Result<Option<AuthenticatedDevice>> {
        validate_credential_hash(credential_hash)?;

        conn.query_row(
            "SELECT device_id, client_node_id FROM sync_devices
             WHERE credential_hash = ?1 AND revoked_at IS NULL",
            params![credential_hash],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .context("Failed to authenticate sync device")?
        .map(|(device_id, client_node_id)| {
            validate_uuid(&device_id, "Stored sync device ID")?;
            if let Some(node_id) = &client_node_id {
                validate_uuid(node_id, "Stored Client node ID")?;
            }
            Ok(AuthenticatedDevice {
                device_id,
                client_node_id,
            })
        })
        .transpose()
    }

    pub fn bind_device(
        conn: &Connection,
        device_id: &str,
        client_node_id: &str,
    ) -> Result<BindDeviceOutcome> {
        validate_uuid(device_id, "Sync device ID")?;
        validate_uuid(client_node_id, "Client node ID")?;

        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .context("Failed to begin sync device binding")?;
        let Some(device) = device_by_id(&tx, device_id)? else {
            tx.commit()
                .context("Failed to finish missing sync device binding")?;
            return Ok(BindDeviceOutcome::NotFound);
        };

        if device.revoked {
            tx.commit()
                .context("Failed to finish revoked sync device binding")?;
            return Ok(BindDeviceOutcome::Revoked);
        }

        if let Some(bound_node_id) = &device.client_node_id {
            let outcome = if bound_node_id == client_node_id {
                BindDeviceOutcome::AlreadyBound(device)
            } else {
                BindDeviceOutcome::Conflict
            };
            tx.commit()
                .context("Failed to finish existing sync device binding")?;
            return Ok(outcome);
        }

        let active_binding_exists: bool = tx
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sync_devices
                    WHERE client_node_id = ?1 AND revoked_at IS NULL AND device_id <> ?2
                )",
                params![client_node_id, device_id],
                |row| row.get(0),
            )
            .context("Failed to check existing Client node binding")?;
        if active_binding_exists {
            tx.commit()
                .context("Failed to finish conflicting Client node binding")?;
            return Ok(BindDeviceOutcome::Conflict);
        }

        tx.execute(
            "UPDATE sync_devices
             SET client_node_id = ?1, paired_at = CURRENT_TIMESTAMP
             WHERE device_id = ?2 AND client_node_id IS NULL AND revoked_at IS NULL",
            params![client_node_id, device_id],
        )
        .context("Failed to bind sync device")?;
        let device = device_by_id(&tx, device_id)?.context("Bound sync device disappeared")?;
        tx.commit()
            .context("Failed to commit sync device binding")?;
        Ok(BindDeviceOutcome::Bound(device))
    }

    pub fn revoke_device(conn: &Connection, device_id: &str) -> Result<Option<SyncDevice>> {
        validate_uuid(device_id, "Sync device ID")?;
        conn.execute(
            "UPDATE sync_devices SET revoked_at = COALESCE(revoked_at, CURRENT_TIMESTAMP)
             WHERE device_id = ?1",
            params![device_id],
        )
        .context("Failed to revoke sync device")?;
        device_by_id(conn, device_id)
    }

    pub fn save_client_pairing(
        conn: &Connection,
        paired_hub: &PairedHub,
        bearer_credential: &str,
    ) -> Result<()> {
        if paired_hub.hub_url.trim().is_empty() {
            bail!("Hub URL must not be empty");
        }
        validate_uuid(&paired_hub.hub_node_id, "Hub node ID")?;
        validate_uuid(&paired_hub.device_id, "Sync device ID")?;
        if bearer_credential.trim().is_empty() {
            bail!("Bearer credential must not be empty");
        }
        if paired_hub.paired_at.trim().is_empty() {
            bail!("Pairing timestamp must not be empty");
        }

        conn.execute(
            "INSERT INTO sync_client_pairing
             (singleton, hub_url, hub_node_id, device_id, protocol_version, bearer_credential, paired_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                paired_hub.hub_url,
                paired_hub.hub_node_id,
                paired_hub.device_id,
                paired_hub.protocol_version,
                bearer_credential,
                paired_hub.paired_at,
            ],
        )
        .context("Failed to save Client sync pairing")?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn client_pairing(conn: &Connection) -> Result<Option<ClientPairing>> {
        conn.query_row(
            "SELECT hub_url, hub_node_id, device_id, protocol_version, bearer_credential, paired_at
             FROM sync_client_pairing WHERE singleton = 1",
            [],
            |row| {
                Ok(ClientPairing {
                    hub_url: row.get(0)?,
                    hub_node_id: row.get(1)?,
                    device_id: row.get(2)?,
                    protocol_version: row.get(3)?,
                    bearer_credential: row.get(4)?,
                    paired_at: row.get(5)?,
                })
            },
        )
        .optional()
        .context("Failed to read Client sync pairing")?
        .map(|pairing| {
            if pairing.hub_url.trim().is_empty() {
                bail!("Stored Hub URL is empty");
            }
            validate_uuid(&pairing.hub_node_id, "Stored Hub node ID")?;
            validate_uuid(&pairing.device_id, "Stored sync device ID")?;
            if pairing.protocol_version == 0 {
                bail!("Stored sync protocol version is invalid");
            }
            if pairing.bearer_credential.trim().is_empty() {
                bail!("Stored bearer credential is empty");
            }
            Ok(pairing)
        })
        .transpose()
    }

    pub fn paired_hub(conn: &Connection) -> Result<Option<PairedHub>> {
        Ok(Self::client_pairing(conn)?.map(|pairing| pairing.public_projection()))
    }

    pub fn delete_client_pairing(conn: &Connection) -> Result<bool> {
        let deleted = conn
            .execute("DELETE FROM sync_client_pairing WHERE singleton = 1", [])
            .context("Failed to delete Client sync pairing")?;
        Ok(deleted > 0)
    }

    pub(crate) fn entity_identity_version(conn: &Connection) -> Result<i64> {
        conn.query_row(
            "SELECT entity_identity_version FROM sync_metadata WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .context("Failed to read entity sync identity version")
    }

    pub(crate) fn set_entity_identity_version(conn: &Connection, version: i64) -> Result<()> {
        conn.execute(
            "UPDATE sync_metadata SET entity_identity_version = ?1 WHERE singleton = 1",
            params![version],
        )
        .context("Failed to record entity sync identity migration")?;
        Ok(())
    }

    fn optional_node_id(conn: &Connection) -> Result<Option<String>> {
        conn.query_row(
            "SELECT node_id FROM sync_metadata WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .context("Failed to read sync node identity")
    }

    fn validate_node_id(node_id: String) -> Result<String> {
        validate_uuid(&node_id, "Stored sync node identity")?;
        Ok(node_id)
    }
}

fn add_protocol_version_if_missing(conn: &Connection) -> Result<()> {
    let exists = conn
        .prepare("PRAGMA table_info(sync_client_pairing)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|column| column.ok())
        .any(|column| column == "protocol_version");
    if !exists {
        match conn.execute(
            "ALTER TABLE sync_client_pairing ADD COLUMN protocol_version INTEGER NOT NULL DEFAULT 1",
            [],
        ) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("duplicate column name") => {}
            Err(error) => {
                return Err(error).context("Failed to add sync pairing protocol version")
            }
        }
    }
    Ok(())
}

fn device_by_id(conn: &Connection, device_id: &str) -> Result<Option<SyncDevice>> {
    conn.query_row(
        "SELECT device_id, name, client_node_id, created_at, paired_at, revoked_at
         FROM sync_devices WHERE device_id = ?1",
        params![device_id],
        row_to_device,
    )
    .optional()
    .context("Failed to query sync device")?
    .transpose()
}

fn row_to_device(row: &Row<'_>) -> rusqlite::Result<Result<SyncDevice>> {
    let device_id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let client_node_id: Option<String> = row.get(2)?;
    let created_at: String = row.get(3)?;
    let paired_at: Option<String> = row.get(4)?;
    let revoked_at: Option<String> = row.get(5)?;

    Ok((|| {
        validate_uuid(&device_id, "Stored sync device ID")?;
        if name.trim().is_empty() {
            bail!("Stored sync device name is empty");
        }
        if let Some(node_id) = &client_node_id {
            validate_uuid(node_id, "Stored Client node ID")?;
        }
        if client_node_id.is_some() != paired_at.is_some() {
            bail!("Stored sync device binding is inconsistent");
        }

        Ok(SyncDevice {
            device_id,
            name,
            paired: client_node_id.is_some(),
            revoked: revoked_at.is_some(),
            client_node_id,
            created_at,
            paired_at,
            revoked_at,
        })
    })())
}

fn validate_uuid(value: &str, label: &str) -> Result<()> {
    Uuid::parse_str(value).with_context(|| format!("{label} is not a valid UUID"))?;
    Ok(())
}

fn validate_credential_hash(credential_hash: &[u8]) -> Result<()> {
    if credential_hash.len() != 32 {
        bail!("Sync credential hash must contain exactly 32 bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use audetic_core::sync::PairedHub;
    use rusqlite::Connection;

    use crate::db::{init_db_at, migrate};

    use super::*;

    const HASH: [u8; 32] = [7; 32];
    const OTHER_HASH: [u8; 32] = [8; 32];
    const CLIENT_NODE_ID: &str = "67e55044-10b1-426f-9247-bb680e5fe0c8";
    const OTHER_NODE_ID: &str = "350e8400-e29b-41d4-a716-446655440000";

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn pairing_table_migration_is_idempotent() {
        let conn = setup_db();

        migrate(&conn).unwrap();
        migrate(&conn).unwrap();

        for table in ["sync_devices", "sync_client_pairing"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "{table}");
        }
    }

    #[test]
    fn pairing_migration_backfills_protocol_version() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sync_client_pairing (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                hub_url TEXT NOT NULL,
                hub_node_id TEXT NOT NULL,
                device_id TEXT NOT NULL,
                bearer_credential TEXT NOT NULL,
                paired_at TEXT NOT NULL
            );
            INSERT INTO sync_client_pairing
                (singleton, hub_url, hub_node_id, device_id, bearer_credential, paired_at)
            VALUES
                (1, 'https://sync.example.com',
                 '350e8400-e29b-41d4-a716-446655440000',
                 '257dc5a6-d8d7-463b-8f2f-f95f616c3a15',
                 'private', '2026-09-16 18:00:00');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let pairing = SyncRepository::client_pairing(&conn).unwrap().unwrap();
        assert_eq!(
            pairing.protocol_version,
            audetic_core::sync::SYNC_PROTOCOL_VERSION
        );
    }

    #[test]
    fn issuing_devices_validates_input_and_persists_only_a_hash() {
        let conn = setup_db();

        assert!(SyncRepository::issue_device(&conn, "   ", &HASH).is_err());
        assert!(SyncRepository::issue_device(&conn, "Laptop", &[1; 31]).is_err());
        assert!(SyncRepository::issue_device(&conn, "Laptop", &[1; 33]).is_err());

        let device = SyncRepository::issue_device(&conn, "  Work laptop  ", &HASH).unwrap();
        Uuid::parse_str(&device.device_id).unwrap();
        assert_eq!(device.name, "Work laptop");
        assert!(!device.paired);
        assert!(!device.revoked);

        let (stored_hash, storage_type): (Vec<u8>, String) = conn
            .query_row(
                "SELECT credential_hash, typeof(credential_hash) FROM sync_devices",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored_hash, HASH);
        assert_eq!(stored_hash.len(), 32);
        assert_eq!(storage_type, "blob");
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(sync_devices)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column == "credential"));
    }

    #[test]
    fn binding_is_idempotent_for_the_same_node_and_conflicts_for_another() {
        let conn = setup_db();
        let device = SyncRepository::issue_device(&conn, "Laptop", &HASH).unwrap();

        let bound = SyncRepository::bind_device(&conn, &device.device_id, CLIENT_NODE_ID).unwrap();
        assert!(matches!(bound, BindDeviceOutcome::Bound(_)));

        let repeated =
            SyncRepository::bind_device(&conn, &device.device_id, CLIENT_NODE_ID).unwrap();
        assert!(matches!(repeated, BindDeviceOutcome::AlreadyBound(_)));

        let conflict =
            SyncRepository::bind_device(&conn, &device.device_id, OTHER_NODE_ID).unwrap();
        assert_eq!(conflict, BindDeviceOutcome::Conflict);

        let listed = SyncRepository::list_devices(&conn).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].client_node_id.as_deref(), Some(CLIENT_NODE_ID));
        assert!(listed[0].paired);
        assert!(listed[0].paired_at.is_some());
    }

    #[test]
    fn authentication_rejects_wrong_length_unknown_and_revoked_hashes() {
        let conn = setup_db();
        let device = SyncRepository::issue_device(&conn, "Laptop", &HASH).unwrap();

        assert!(SyncRepository::authenticate_device(&conn, &[0; 31]).is_err());
        assert!(SyncRepository::authenticate_device(&conn, &OTHER_HASH)
            .unwrap()
            .is_none());
        let authenticated = SyncRepository::authenticate_device(&conn, &HASH)
            .unwrap()
            .unwrap();
        assert_eq!(authenticated.device_id, device.device_id);
        assert_eq!(authenticated.client_node_id, None);

        let revoked = SyncRepository::revoke_device(&conn, &device.device_id)
            .unwrap()
            .unwrap();
        assert!(revoked.revoked);
        assert!(revoked.revoked_at.is_some());
        assert!(SyncRepository::authenticate_device(&conn, &HASH)
            .unwrap()
            .is_none());
        assert_eq!(
            SyncRepository::bind_device(&conn, &device.device_id, CLIENT_NODE_ID).unwrap(),
            BindDeviceOutcome::Revoked
        );
    }

    #[test]
    fn revocation_releases_the_client_node_for_a_replacement_credential() {
        let conn = setup_db();
        let first = SyncRepository::issue_device(&conn, "Old laptop", &HASH).unwrap();
        SyncRepository::bind_device(&conn, &first.device_id, CLIENT_NODE_ID).unwrap();

        let second = SyncRepository::issue_device(&conn, "Replacement", &OTHER_HASH).unwrap();
        assert_eq!(
            SyncRepository::bind_device(&conn, &second.device_id, CLIENT_NODE_ID).unwrap(),
            BindDeviceOutcome::Conflict
        );

        SyncRepository::revoke_device(&conn, &first.device_id).unwrap();
        assert!(matches!(
            SyncRepository::bind_device(&conn, &second.device_id, CLIENT_NODE_ID).unwrap(),
            BindDeviceOutcome::Bound(_)
        ));
    }

    #[test]
    fn repository_boundaries_validate_uuid_inputs() {
        let conn = setup_db();
        let device = SyncRepository::issue_device(&conn, "Laptop", &HASH).unwrap();

        assert!(SyncRepository::bind_device(&conn, "not-a-uuid", CLIENT_NODE_ID).is_err());
        assert!(SyncRepository::bind_device(&conn, &device.device_id, "not-a-uuid").is_err());
        assert!(SyncRepository::revoke_device(&conn, "not-a-uuid").is_err());
    }

    #[test]
    fn client_pairing_survives_reopen_and_can_be_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audetic.db");
        let credential = "audetic_sync_private-credential";
        let paired_hub = PairedHub {
            hub_url: "https://sync.example.com".to_string(),
            hub_node_id: OTHER_NODE_ID.to_string(),
            device_id: "257dc5a6-d8d7-463b-8f2f-f95f616c3a15".to_string(),
            protocol_version: audetic_core::sync::SYNC_PROTOCOL_VERSION,
            paired_at: "2026-09-16 18:00:00".to_string(),
        };

        let conn = init_db_at(&path).unwrap();
        SyncRepository::save_client_pairing(&conn, &paired_hub, credential).unwrap();
        drop(conn);

        let reopened = init_db_at(&path).unwrap();
        assert_eq!(
            SyncRepository::paired_hub(&reopened).unwrap(),
            Some(paired_hub)
        );
        let private = SyncRepository::client_pairing(&reopened).unwrap().unwrap();
        assert_eq!(private.bearer_credential, credential);
        let debug = format!("{private:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(credential));

        assert!(SyncRepository::delete_client_pairing(&reopened).unwrap());
        assert!(!SyncRepository::delete_client_pairing(&reopened).unwrap());
        assert!(SyncRepository::client_pairing(&reopened).unwrap().is_none());
    }

    #[test]
    fn client_pairing_rejects_invalid_identifiers_and_duplicate_state() {
        let conn = setup_db();
        let mut paired_hub = PairedHub {
            hub_url: "https://sync.example.com".to_string(),
            hub_node_id: OTHER_NODE_ID.to_string(),
            device_id: "257dc5a6-d8d7-463b-8f2f-f95f616c3a15".to_string(),
            protocol_version: audetic_core::sync::SYNC_PROTOCOL_VERSION,
            paired_at: "2026-09-16 18:00:00".to_string(),
        };

        paired_hub.hub_node_id = "invalid".to_string();
        assert!(SyncRepository::save_client_pairing(&conn, &paired_hub, "credential").is_err());
        paired_hub.hub_node_id = OTHER_NODE_ID.to_string();
        assert!(SyncRepository::save_client_pairing(&conn, &paired_hub, " ").is_err());
        SyncRepository::save_client_pairing(&conn, &paired_hub, "credential").unwrap();
        assert!(SyncRepository::save_client_pairing(&conn, &paired_hub, "other").is_err());
    }

    #[test]
    fn public_projections_do_not_serialize_private_storage_fields() {
        let conn = setup_db();
        SyncRepository::issue_device(&conn, "Laptop", &HASH).unwrap();

        let json = serde_json::to_value(SyncRepository::list_devices(&conn).unwrap()).unwrap();
        let serialized = json.to_string();
        assert!(!serialized.contains("credential"));
        assert!(!serialized.contains("hash"));
        assert!(!serialized.contains("bearer"));
    }
}
