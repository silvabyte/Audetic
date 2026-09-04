use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncServeOwnership {
    pub https_port: u16,
    pub mount_path: String,
    pub proxy_url: String,
}

pub struct SyncServeRepository;

impl SyncServeRepository {
    /// Read ownership only when the sync migration is present.
    ///
    /// Uninstall uses this against databases from any Audetic version. An old
    /// database without this table proves no Audetic Serve ownership and must
    /// not be migrated or modified merely to inspect it.
    pub fn get_if_available(conn: &Connection) -> Result<Option<SyncServeOwnership>> {
        let table_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sync_serve_ownership')",
                [],
                |row| row.get(0),
            )
            .context("Failed to inspect Audetic Serve ownership schema")?;
        if !table_exists {
            return Ok(None);
        }
        Self::get(conn)
    }

    pub fn get(conn: &Connection) -> Result<Option<SyncServeOwnership>> {
        conn.query_row(
            "SELECT https_port, mount_path, proxy_url
             FROM sync_serve_ownership WHERE singleton = 1",
            [],
            |row| {
                let port: i64 = row.get(0)?;
                let https_port = u16::try_from(port).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?;
                Ok(SyncServeOwnership {
                    https_port,
                    mount_path: row.get(1)?,
                    proxy_url: row.get(2)?,
                })
            },
        )
        .optional()
        .context("Failed to read Audetic Serve ownership")
    }

    pub fn save(conn: &Connection, ownership: &SyncServeOwnership) -> Result<()> {
        conn.execute(
            "INSERT INTO sync_serve_ownership
                (singleton, https_port, mount_path, proxy_url, configured_at)
             VALUES (1, ?1, ?2, ?3, CURRENT_TIMESTAMP)
             ON CONFLICT(singleton) DO UPDATE SET
                https_port = excluded.https_port,
                mount_path = excluded.mount_path,
                proxy_url = excluded.proxy_url,
                configured_at = excluded.configured_at",
            rusqlite::params![
                i64::from(ownership.https_port),
                ownership.mount_path,
                ownership.proxy_url,
            ],
        )
        .context("Failed to persist Audetic Serve ownership")?;
        Ok(())
    }

    pub fn clear(conn: &Connection) -> Result<()> {
        conn.execute("DELETE FROM sync_serve_ownership WHERE singleton = 1", [])
            .context("Failed to clear Audetic Serve ownership")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_round_trips_and_clears() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let ownership = SyncServeOwnership {
            https_port: 8443,
            mount_path: "/audetic".into(),
            proxy_url: "http://127.0.0.1:3738".into(),
        };

        SyncServeRepository::save(&conn, &ownership).unwrap();
        assert_eq!(SyncServeRepository::get(&conn).unwrap(), Some(ownership));
        SyncServeRepository::clear(&conn).unwrap();
        assert_eq!(SyncServeRepository::get(&conn).unwrap(), None);
    }

    #[test]
    fn compatibility_read_treats_pre_sync_database_as_unowned() {
        let conn = Connection::open_in_memory().unwrap();

        assert_eq!(SyncServeRepository::get_if_available(&conn).unwrap(), None);
    }
}
