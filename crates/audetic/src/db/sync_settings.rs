use anyhow::{bail, Context, Result};
use audetic_core::sync::{CacheLevel, HubConnection, HubId, SyncRole};
use rusqlite::Connection;

use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSettings {
    pub role: SyncRole,
    pub device_name: Option<String>,
    pub hub: Option<HubConnection>,
    pub upload_recording_payloads: bool,
    pub cache_level: CacheLevel,
    pub shared_config_enabled: bool,
    pub change_cursor: u64,
    pub shared_config_version: Option<u64>,
    pub last_contact_at: Option<String>,
    pub last_error: Option<String>,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            role: SyncRole::Standalone,
            device_name: None,
            hub: None,
            upload_recording_payloads: false,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: false,
            change_cursor: 0,
            shared_config_version: None,
            last_contact_at: None,
            last_error: None,
        }
    }
}

pub struct SyncSettingsRepository;

impl SyncSettingsRepository {
    /// Read settings, materializing the singleton default on first access.
    pub fn get(conn: &Connection) -> Result<SyncSettings> {
        conn.execute(
            "INSERT OR IGNORE INTO sync_settings (singleton) VALUES (1)",
            [],
        )
        .context("Failed to initialize sync settings")?;

        let row = conn
            .query_row(
                "SELECT role, device_name, hub_url, hub_id, hub_owner_login,
                        upload_recording_payloads, cache_level,
                        shared_config_enabled, change_cursor,
                        shared_config_version, last_contact_at, last_error
                 FROM sync_settings WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, bool>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                    ))
                },
            )
            .context("Failed to read sync settings")?;

        let role = parse_value::<SyncRole>(&row.0, "role")?;
        let hub = match (row.2, row.3, row.4) {
            (Some(base_url), Some(hub_id), Some(owner_login)) => Some(HubConnection {
                base_url,
                hub_id: parse_value::<HubId>(&hub_id, "hub_id")?,
                owner_login,
            }),
            (None, None, None) => None,
            _ => bail!("Incomplete Home Hub connection in sync_settings"),
        };

        Ok(SyncSettings {
            role,
            device_name: row.1,
            hub,
            upload_recording_payloads: row.5,
            cache_level: parse_value::<CacheLevel>(&row.6, "cache_level")?,
            shared_config_enabled: row.7,
            change_cursor: to_u64(row.8, "change_cursor")?,
            shared_config_version: row
                .9
                .map(|version| to_u64(version, "shared_config_version"))
                .transpose()?,
            last_contact_at: row.10,
            last_error: row.11,
        })
    }

    pub fn save(conn: &Connection, settings: &SyncSettings) -> Result<()> {
        validate(settings)?;
        let (hub_url, hub_id, hub_owner_login) = settings
            .hub
            .as_ref()
            .map(|hub| {
                (
                    Some(hub.base_url.as_str()),
                    Some(hub.hub_id.to_string()),
                    Some(hub.owner_login.as_str()),
                )
            })
            .unwrap_or((None, None, None));
        let change_cursor = i64::try_from(settings.change_cursor)
            .context("Sync change cursor exceeds SQLite integer range")?;
        let shared_config_version = settings
            .shared_config_version
            .map(i64::try_from)
            .transpose()
            .context("Shared config version exceeds SQLite integer range")?;

        conn.execute(
            "INSERT INTO sync_settings (
                singleton, role, device_name, hub_url, hub_id, hub_owner_login,
                upload_recording_payloads, cache_level, shared_config_enabled,
                change_cursor, shared_config_version, last_contact_at,
                last_error, updated_at
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                       CURRENT_TIMESTAMP)
             ON CONFLICT(singleton) DO UPDATE SET
                role = excluded.role,
                device_name = excluded.device_name,
                hub_url = excluded.hub_url,
                hub_id = excluded.hub_id,
                hub_owner_login = excluded.hub_owner_login,
                upload_recording_payloads = excluded.upload_recording_payloads,
                cache_level = excluded.cache_level,
                shared_config_enabled = excluded.shared_config_enabled,
                change_cursor = excluded.change_cursor,
                shared_config_version = excluded.shared_config_version,
                last_contact_at = excluded.last_contact_at,
                last_error = excluded.last_error,
                updated_at = excluded.updated_at",
            rusqlite::params![
                settings.role.as_str(),
                settings.device_name.as_deref(),
                hub_url,
                hub_id,
                hub_owner_login,
                settings.upload_recording_payloads,
                settings.cache_level.as_str(),
                settings.shared_config_enabled,
                change_cursor,
                shared_config_version,
                settings.last_contact_at.as_deref(),
                settings.last_error.as_deref(),
            ],
        )
        .context("Failed to persist sync settings")?;
        Ok(())
    }

    pub(crate) fn role_epoch(conn: &Connection) -> Result<u64> {
        Self::get(conn)?;
        let epoch = conn
            .query_row(
                "SELECT role_epoch FROM sync_settings WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .context("Failed to read sync role epoch")?;
        to_u64(epoch, "role_epoch")
    }

    pub(crate) fn compare_and_increment_role_epoch(
        conn: &Connection,
        expected_epoch: u64,
    ) -> Result<Option<u64>> {
        let expected = i64::try_from(expected_epoch)
            .context("Expected sync role epoch exceeds SQLite integer range")?;
        let next = expected
            .checked_add(1)
            .context("Sync role epoch is exhausted")?;
        let changed = conn
            .execute(
                "UPDATE sync_settings
                 SET role_epoch = ?2, updated_at = CURRENT_TIMESTAMP
                 WHERE singleton = 1 AND role_epoch = ?1",
                rusqlite::params![expected, next],
            )
            .context("Failed to compare and increment sync role epoch")?;
        Ok((changed == 1).then_some(next as u64))
    }

    pub(crate) fn record_contact(
        conn: &Connection,
        expected_epoch: u64,
        contacted_at: &str,
    ) -> Result<bool> {
        let epoch = i64::try_from(expected_epoch)
            .context("Expected sync role epoch exceeds SQLite integer range")?;
        conn.execute(
            "UPDATE sync_settings
             SET last_contact_at = ?2, last_error = NULL,
                 updated_at = CURRENT_TIMESTAMP
             WHERE singleton = 1 AND role_epoch = ?1",
            rusqlite::params![epoch, contacted_at],
        )
        .map(|changed| changed == 1)
        .context("Failed to record sync contact")
    }

    pub(crate) fn record_error(
        conn: &Connection,
        expected_epoch: u64,
        error: Option<&str>,
    ) -> Result<bool> {
        let epoch = i64::try_from(expected_epoch)
            .context("Expected sync role epoch exceeds SQLite integer range")?;
        conn.execute(
            "UPDATE sync_settings
             SET last_error = ?2, updated_at = CURRENT_TIMESTAMP
             WHERE singleton = 1 AND role_epoch = ?1",
            rusqlite::params![epoch, error],
        )
        .map(|changed| changed == 1)
        .context("Failed to record sync error")
    }

    pub(crate) fn update_payload_policy(conn: &Connection, enabled: bool) -> Result<()> {
        conn.execute(
            "UPDATE sync_settings
             SET upload_recording_payloads = ?1, updated_at = CURRENT_TIMESTAMP
             WHERE singleton = 1",
            [enabled],
        )
        .context("Failed to update Recording Payload policy")?;
        Ok(())
    }
}

fn validate(settings: &SyncSettings) -> Result<()> {
    match (settings.role, settings.hub.as_ref()) {
        (SyncRole::ConnectedDevice, None) => {
            bail!("Connected Device settings require a Home Hub connection")
        }
        (SyncRole::ConnectedDevice, Some(hub)) if !hub.base_url.ends_with("/audetic/") => {
            bail!("Home Hub base URL must end in /audetic/")
        }
        (SyncRole::Standalone | SyncRole::HomeHub, Some(_)) => {
            bail!("Only Connected Device settings may store a Home Hub connection")
        }
        _ => Ok(()),
    }
}

fn parse_value<T>(value: &str, column: &str) -> Result<T>
where
    T: FromStr<Err = String>,
{
    value
        .parse()
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("Invalid {column} in sync_settings"))
}

fn to_u64(value: i64, column: &str) -> Result<u64> {
    u64::try_from(value).with_context(|| format!("Negative {column} in sync_settings"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrate_db_at, open_db_at};

    #[test]
    fn defaults_and_connected_device_settings_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        let conn = migrate_db_at(&path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap(),
            SyncSettings::default()
        );

        let settings = SyncSettings {
            role: SyncRole::ConnectedDevice,
            device_name: Some("Travel Laptop".into()),
            hub: Some(HubConnection {
                base_url: "https://audetic.example.ts.net:8443/audetic/".into(),
                hub_id: HubId::new(),
                owner_login: "owner@example.com".into(),
            }),
            upload_recording_payloads: true,
            cache_level: CacheLevel::TextForOfflineUse,
            shared_config_enabled: true,
            change_cursor: 42,
            shared_config_version: Some(7),
            last_contact_at: Some("2026-09-04T12:00:00Z".into()),
            last_error: Some("offline".into()),
        };
        SyncSettingsRepository::save(&conn, &settings).unwrap();
        drop(conn);

        let reopened = open_db_at(&path).unwrap();
        assert_eq!(SyncSettingsRepository::get(&reopened).unwrap(), settings);
    }

    #[test]
    fn connected_device_requires_canonical_mounted_hub_url() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let settings = SyncSettings {
            role: SyncRole::ConnectedDevice,
            hub: Some(HubConnection {
                base_url: "https://audetic.example.ts.net:8443/".into(),
                hub_id: HubId::new(),
                owner_login: "owner@example.com".into(),
            }),
            ..SyncSettings::default()
        };

        assert!(SyncSettingsRepository::save(&conn, &settings).is_err());
    }
}
