use anyhow::{Context, Result};
use audetic_core::sync::{DeviceId, HubId};
use rusqlite::{Connection, OptionalExtension};

use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncIdentity {
    pub device_id: DeviceId,
    pub hub_id: Option<HubId>,
    pub owner_login: Option<String>,
}

pub struct SyncIdentityRepository;

impl SyncIdentityRepository {
    /// Return this installation's durable Device ID, creating it exactly once.
    pub fn get_or_create_device(conn: &Connection) -> Result<SyncIdentity> {
        if let Some(identity) = Self::get(conn)? {
            return Ok(identity);
        }

        let device_id = DeviceId::new();
        conn.execute(
            "INSERT OR IGNORE INTO sync_identity (singleton, device_id) VALUES (1, ?1)",
            [device_id.to_string()],
        )
        .context("Failed to persist sync Device ID")?;

        Self::get(conn)?.context("Sync identity was not present after creation")
    }

    pub fn get(conn: &Connection) -> Result<Option<SyncIdentity>> {
        let row = conn
            .query_row(
                "SELECT device_id, hub_id, owner_login FROM sync_identity WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .context("Failed to read sync identity")?;

        row.map(|(device_id, hub_id, owner_login)| {
            Ok(SyncIdentity {
                device_id: parse_id(&device_id, "device_id")?,
                hub_id: hub_id.map(|value| parse_id(&value, "hub_id")).transpose()?,
                owner_login,
            })
        })
        .transpose()
    }

    /// Configure this installation as a Home Hub.
    ///
    /// A previously assigned Hub ID is retained so disabling and re-enabling
    /// the same Home Hub does not create a new shared-library authority.
    pub fn configure_hub(conn: &Connection, owner_login: &str) -> Result<SyncIdentity> {
        let identity = Self::get_or_create_device(conn)?;
        let proposed_hub_id = identity.hub_id.unwrap_or_else(HubId::new);
        Self::save_hub(conn, proposed_hub_id, owner_login)?;

        Self::get(conn)?.context("Sync identity disappeared after Home Hub configuration")
    }

    /// Persist a prepared Hub ID and owner as part of a caller-owned transaction.
    pub fn save_hub(conn: &Connection, hub_id: HubId, owner_login: &str) -> Result<()> {
        Self::get_or_create_device(conn)?;
        conn.execute(
            "UPDATE sync_identity
             SET hub_id = ?1, owner_login = ?2,
                  updated_at = CURRENT_TIMESTAMP
             WHERE singleton = 1",
            rusqlite::params![hub_id.to_string(), owner_login],
        )
        .context("Failed to configure Home Hub identity")?;
        Ok(())
    }

    /// Ensure a Connected Device has its stable local identity.
    ///
    /// Hub connection details belong to `sync_settings`; this intentionally
    /// does not replace a dormant local Hub ID retained for future reactivation.
    pub fn connect_device(conn: &Connection) -> Result<SyncIdentity> {
        Self::get_or_create_device(conn)
    }
}

fn parse_id<T>(value: &str, column: &str) -> Result<T>
where
    T: FromStr<Err = String>,
{
    value
        .parse()
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("Invalid {column} in sync_identity"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrate_db_at, open_db_at};

    #[test]
    fn device_and_hub_ids_survive_reopen_and_reconfiguration() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        let conn = migrate_db_at(&path).unwrap();

        let device = SyncIdentityRepository::get_or_create_device(&conn).unwrap();
        let hub = SyncIdentityRepository::configure_hub(&conn, "owner@example.com").unwrap();
        assert_eq!(hub.device_id, device.device_id);
        assert_eq!(hub.owner_login.as_deref(), Some("owner@example.com"));
        drop(conn);

        let reopened = open_db_at(&path).unwrap();
        let persisted = SyncIdentityRepository::get_or_create_device(&reopened).unwrap();
        let reconfigured =
            SyncIdentityRepository::configure_hub(&reopened, "owner@example.com").unwrap();
        assert_eq!(persisted, hub);
        assert_eq!(reconfigured.hub_id, hub.hub_id);
    }
}
