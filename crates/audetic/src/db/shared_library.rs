use anyhow::{Context, Result};
use audetic_core::sync::{DeviceId, RecordId};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::sync::protocol::{ChangeEnvelope, ChangeOperation, DictationSnapshot, SharedDictation};

#[derive(Debug, Error)]
pub enum ApplySnapshotError {
    #[error("record was deleted and cannot be restored by a delayed snapshot")]
    Tombstoned,
    #[error("record kind is immutable")]
    KindChanged,
    #[error("record origin is immutable")]
    OriginChanged,
    #[error("snapshot conflicts with an accepted local version")]
    VersionConflict,
    #[error(transparent)]
    Database(#[from] anyhow::Error),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyResult {
    pub revision: u64,
    pub changed: bool,
}

pub struct SharedLibraryRepository;

impl SharedLibraryRepository {
    pub fn apply_snapshot(
        conn: &mut Connection,
        snapshot: &DictationSnapshot,
    ) -> std::result::Result<ApplyResult, ApplySnapshotError> {
        let tx = conn
            .transaction()
            .context("starting authoritative dictation transaction")?;
        let index = tx
            .query_row(
                "SELECT kind, origin_device_id, authoritative_revision, deleted_at
                 FROM shared_record_index WHERE record_id = ?1",
                [snapshot.record_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, u64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()
            .context("reading shared record provenance")?;
        if let Some((kind, origin, revision, deleted_at)) = &index {
            if kind != "dictation" {
                return Err(ApplySnapshotError::KindChanged);
            }
            if deleted_at.is_some() {
                return Err(ApplySnapshotError::Tombstoned);
            }
            if origin
                .as_deref()
                .is_some_and(|origin| origin != snapshot.origin_device_id.to_string())
            {
                return Err(ApplySnapshotError::OriginChanged);
            }
            if let Some(existing) = Self::get_from(&tx, snapshot.record_id)? {
                if existing.created_at != snapshot.created_at {
                    return Err(ApplySnapshotError::VersionConflict);
                }
                if snapshot.local_version < existing.local_version {
                    return Err(ApplySnapshotError::VersionConflict);
                }
                if snapshot.local_version == existing.local_version {
                    if existing.text == snapshot.payload.text
                        && existing.origin_device_id == snapshot.origin_device_id
                        && existing.updated_at == snapshot.updated_at
                    {
                        tx.commit().context("committing idempotent snapshot")?;
                        return Ok(ApplyResult {
                            revision: *revision,
                            changed: false,
                        });
                    }
                    return Err(ApplySnapshotError::VersionConflict);
                }
            }
        }

        let revision = index.map_or(1, |(_, _, revision, _)| revision + 1);
        tx.execute(
            "INSERT INTO shared_record_index
                (record_id, kind, origin_device_id, authoritative_revision)
             VALUES (?1, 'dictation', ?2, ?3)
             ON CONFLICT(record_id) DO UPDATE SET
                origin_device_id = COALESCE(shared_record_index.origin_device_id, excluded.origin_device_id),
                authoritative_revision = excluded.authoritative_revision,
                updated_at = CURRENT_TIMESTAMP",
            params![snapshot.record_id.to_string(), snapshot.origin_device_id.to_string(), revision],
        )
        .context("upserting shared record index")?;
        tx.execute(
            "INSERT INTO shared_dictations
                (record_id, origin_device_id, text, source_created_at, source_updated_at,
                 local_version, authoritative_revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(record_id) DO UPDATE SET text = excluded.text,
                source_updated_at = excluded.source_updated_at,
                local_version = excluded.local_version,
                authoritative_revision = excluded.authoritative_revision",
            params![
                snapshot.record_id.to_string(),
                snapshot.origin_device_id.to_string(),
                snapshot.payload.text,
                snapshot.created_at,
                snapshot.updated_at,
                snapshot.local_version,
                revision,
            ],
        )
        .context("upserting shared dictation")?;
        let change = ChangeEnvelope::upsert(snapshot.clone(), revision);
        let change_json = serde_json::to_string(&change).context("serializing library change")?;
        tx.execute(
            "INSERT INTO shared_library_changes
                (operation, kind, record_id, authoritative_revision, change_json)
             VALUES ('upsert', 'dictation', ?1, ?2, ?3)",
            params![snapshot.record_id.to_string(), revision, change_json],
        )
        .context("appending library change")?;
        tx.commit().context("committing authoritative dictation")?;
        Ok(ApplyResult {
            revision,
            changed: true,
        })
    }

    pub fn apply_tombstone(
        conn: &mut Connection,
        record_id: RecordId,
        origin: Option<DeviceId>,
        deleted_at: &str,
    ) -> Result<ApplyResult> {
        let tx = conn.transaction()?;
        let existing: Option<(String, Option<String>, u64, Option<String>)> = tx
            .query_row(
                "SELECT kind, origin_device_id, authoritative_revision, deleted_at
                 FROM shared_record_index WHERE record_id = ?1",
                [record_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if existing.as_ref().is_some_and(|row| row.3.is_some()) {
            return Ok(ApplyResult {
                revision: existing.unwrap().2,
                changed: false,
            });
        }
        let revision = existing.as_ref().map_or(1, |row| row.2 + 1);
        tx.execute(
            "INSERT INTO shared_record_index
                (record_id, kind, origin_device_id, authoritative_revision, deleted_at)
             VALUES (?1, 'dictation', ?2, ?3, ?4)
             ON CONFLICT(record_id) DO UPDATE SET authoritative_revision = excluded.authoritative_revision,
                deleted_at = excluded.deleted_at, updated_at = CURRENT_TIMESTAMP",
            params![record_id.to_string(), origin.map(|id| id.to_string()), revision, deleted_at],
        )?;
        tx.execute(
            "UPDATE shared_dictations SET deleted_at = ?2, authoritative_revision = ?3
             WHERE record_id = ?1",
            params![record_id.to_string(), deleted_at, revision],
        )?;
        tx.execute(
            "INSERT INTO sync_tombstones (record_id, kind, deleted_version, deleted_at)
             VALUES (?1, 'dictation', ?2, ?3)
             ON CONFLICT(record_id) DO NOTHING",
            params![record_id.to_string(), revision, deleted_at],
        )?;
        let change = ChangeEnvelope {
            cursor: None,
            operation: ChangeOperation::Delete,
            record_id,
            origin_device_id: origin,
            authoritative_revision: revision,
            snapshot: None,
            changed_at: deleted_at.to_owned(),
        };
        tx.execute(
            "INSERT INTO shared_library_changes
                (operation, kind, record_id, authoritative_revision, change_json)
             VALUES ('delete', 'dictation', ?1, ?2, ?3)",
            params![
                record_id.to_string(),
                revision,
                serde_json::to_string(&change)?
            ],
        )?;
        tx.commit()?;
        Ok(ApplyResult {
            revision,
            changed: true,
        })
    }

    pub fn get(conn: &Connection, record_id: RecordId) -> Result<Option<SharedDictation>> {
        Self::get_from(conn, record_id)
    }

    fn get_from(conn: &Connection, record_id: RecordId) -> Result<Option<SharedDictation>> {
        conn.query_row(
            "SELECT record_id, origin_device_id, text, source_created_at, source_updated_at,
                    local_version, authoritative_revision
             FROM shared_dictations WHERE record_id = ?1 AND deleted_at IS NULL",
            [record_id.to_string()],
            |row| {
                let record: String = row.get(0)?;
                let origin: String = row.get(1)?;
                Ok((
                    record,
                    origin,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?
        .map(|row| {
            Ok(SharedDictation {
                record_id: row.0.parse().map_err(anyhow::Error::msg)?,
                origin_device_id: row.1.parse().map_err(anyhow::Error::msg)?,
                text: row.2,
                created_at: row.3,
                updated_at: row.4,
                local_version: row.5,
                authoritative_revision: row.6,
            })
        })
        .transpose()
    }

    pub fn page_dictations(
        conn: &Connection,
        query: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        after: Option<(&str, RecordId)>,
        limit: usize,
    ) -> Result<Vec<SharedDictation>> {
        let mut sql = "SELECT record_id, origin_device_id, text, source_created_at,
            source_updated_at, local_version, authoritative_revision FROM shared_dictations
            WHERE deleted_at IS NULL"
            .to_owned();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(query) = query {
            sql.push_str(" AND text LIKE ?");
            values.push(Box::new(format!("%{query}%")));
        }
        if let Some(from) = from {
            sql.push_str(" AND source_created_at >= ?");
            values.push(Box::new(from.to_owned()));
        }
        if let Some(to) = to {
            sql.push_str(" AND source_created_at <= ?");
            values.push(Box::new(to.to_owned()));
        }
        if let Some((created_at, record_id)) = after {
            sql.push_str(
                " AND (source_created_at < ? OR (source_created_at = ? AND record_id < ?))",
            );
            values.push(Box::new(created_at.to_owned()));
            values.push(Box::new(created_at.to_owned()));
            values.push(Box::new(record_id.to_string()));
        }
        sql.push_str(" ORDER BY source_created_at DESC, record_id DESC LIMIT ?");
        values.push(Box::new(limit));
        let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(AsRef::as_ref).collect();
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(refs.as_slice(), |row| {
            let record: String = row.get(0)?;
            let origin: String = row.get(1)?;
            Ok((
                record,
                origin,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?;
        rows.map(|row| {
            let row = row?;
            Ok(SharedDictation {
                record_id: row.0.parse().map_err(anyhow::Error::msg)?,
                origin_device_id: row.1.parse().map_err(anyhow::Error::msg)?,
                text: row.2,
                created_at: row.3,
                updated_at: row.4,
                local_version: row.5,
                authoritative_revision: row.6,
            })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::protocol::{DictationPayload, RecordKind};

    fn snapshot(record_id: RecordId, origin: DeviceId) -> DictationSnapshot {
        DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id,
            origin_device_id: origin,
            local_version: 1,
            created_at: "2026-09-04T10:00:00Z".into(),
            updated_at: "2026-09-04T10:00:00Z".into(),
            payload: DictationPayload {
                text: "portable text".into(),
            },
        }
    }

    #[test]
    fn duplicate_upload_is_one_item_and_one_change_and_provenance_is_immutable() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let record_id = RecordId::new();
        let origin = DeviceId::new();
        let item = snapshot(record_id, origin);
        assert!(
            SharedLibraryRepository::apply_snapshot(&mut conn, &item)
                .unwrap()
                .changed
        );
        assert!(
            !SharedLibraryRepository::apply_snapshot(&mut conn, &item)
                .unwrap()
                .changed
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_dictations", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_library_changes", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );

        let changed_origin = snapshot(record_id, DeviceId::new());
        assert!(matches!(
            SharedLibraryRepository::apply_snapshot(&mut conn, &changed_origin),
            Err(ApplySnapshotError::OriginChanged)
        ));

        let mut changed_content = item;
        changed_content.updated_at = "2026-09-04T10:00:01Z".into();
        assert!(matches!(
            SharedLibraryRepository::apply_snapshot(&mut conn, &changed_content),
            Err(ApplySnapshotError::VersionConflict)
        ));

        let mut changed_creation = snapshot(record_id, origin);
        changed_creation.local_version = 2;
        changed_creation.created_at = "2026-09-04T09:00:00Z".into();
        assert!(matches!(
            SharedLibraryRepository::apply_snapshot(&mut conn, &changed_creation),
            Err(ApplySnapshotError::VersionConflict)
        ));
    }

    #[test]
    fn prearrival_tombstone_beats_delayed_snapshot() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let record_id = RecordId::new();
        SharedLibraryRepository::apply_tombstone(
            &mut conn,
            record_id,
            None,
            "2026-09-04T11:00:00Z",
        )
        .unwrap();
        assert!(matches!(
            SharedLibraryRepository::apply_snapshot(
                &mut conn,
                &snapshot(record_id, DeviceId::new())
            ),
            Err(ApplySnapshotError::Tombstoned)
        ));
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_dictations", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_library_changes", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
