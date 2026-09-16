//! Durable identity for this Audetic installation.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

pub struct SyncRepository;

impl SyncRepository {
    pub(crate) const ENTITY_IDENTITY_VERSION: i64 = 1;

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
        Uuid::parse_str(&node_id).context("Stored sync node identity is not a valid UUID")?;
        Ok(node_id)
    }
}
