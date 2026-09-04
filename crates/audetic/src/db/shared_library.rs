use anyhow::{Context, Result};
use audetic_core::sync::{DeviceId, RecordId};
use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::sync::protocol::{
    ChangeEnvelope, ChangeOperation, CompletedArtifactSnapshot, DictationSnapshot, MeetingSnapshot,
    MeetingTitlePatch, RecordKind, SharedArtifact, SharedDictation, SharedMeeting, Snapshot,
};

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
    #[error("artifact parent meeting is absent or tombstoned")]
    ParentUnavailable,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
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
    pub fn apply(
        conn: &mut Connection,
        snapshot: &Snapshot,
    ) -> std::result::Result<ApplyResult, ApplySnapshotError> {
        match snapshot {
            Snapshot::Dictation(value) => Self::apply_snapshot(conn, value),
            Snapshot::Meeting(value) => Self::apply_meeting_snapshot(conn, value),
            Snapshot::Artifact(value) => Self::apply_artifact_snapshot(conn, value),
        }
    }
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
            kind: RecordKind::Dictation,
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

    pub fn apply_meeting_snapshot(
        conn: &mut Connection,
        snapshot: &MeetingSnapshot,
    ) -> std::result::Result<ApplyResult, ApplySnapshotError> {
        let tx = conn
            .transaction()
            .context("starting authoritative meeting transaction")?;
        let existing: Option<(String, Option<String>, u64, Option<String>)> = tx
            .query_row(
                "SELECT kind, origin_device_id, authoritative_revision, deleted_at FROM shared_record_index WHERE record_id = ?1",
                [snapshot.record_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        if let Some((kind, origin, revision, deleted_at)) = &existing {
            if kind != "meeting" {
                return Err(ApplySnapshotError::KindChanged);
            }
            if deleted_at.is_some() {
                return Err(ApplySnapshotError::Tombstoned);
            }
            if origin
                .as_deref()
                .is_some_and(|value| value != snapshot.origin_device_id.to_string())
            {
                return Err(ApplySnapshotError::OriginChanged);
            }
            if let Some(current) = get_meeting_from(&tx, snapshot.record_id)? {
                if snapshot.local_version < current.local_version {
                    return Err(ApplySnapshotError::VersionConflict);
                }
                if snapshot.local_version == current.local_version {
                    if meeting_matches_snapshot(&current, snapshot)
                        || (hub_owns_meeting_title(&tx, snapshot.record_id)?
                            && meeting_origin_content_matches_snapshot(&current, snapshot))
                    {
                        tx.commit()?;
                        return Ok(ApplyResult {
                            revision: *revision,
                            changed: false,
                        });
                    }
                    return Err(ApplySnapshotError::VersionConflict);
                }
                if !meeting_origin_content_matches_snapshot(&current, snapshot) {
                    return Err(ApplySnapshotError::VersionConflict);
                }
            }
        }
        let revision = existing.as_ref().map_or(1, |value| value.2 + 1);
        tx.execute(
            "INSERT INTO shared_record_index(record_id,kind,origin_device_id,authoritative_revision)
             VALUES(?1,'meeting',?2,?3)
             ON CONFLICT(record_id) DO UPDATE SET origin_device_id=COALESCE(shared_record_index.origin_device_id,excluded.origin_device_id), authoritative_revision=excluded.authoritative_revision, updated_at=CURRENT_TIMESTAMP",
            params![snapshot.record_id.to_string(), snapshot.origin_device_id.to_string(), revision],
        )?;
        let current = get_meeting_from(&tx, snapshot.record_id)?;
        let preserve_hub_title = hub_owns_meeting_title(&tx, snapshot.record_id)?;
        let (title, title_source, title_version) = if preserve_hub_title {
            let current = current.as_ref().unwrap();
            (
                current.title.clone(),
                current.title_source.clone(),
                current.title_version,
            )
        } else {
            (
                snapshot.payload.title.clone(),
                snapshot.payload.title_source.clone(),
                snapshot.payload.title_version,
            )
        };
        tx.execute(
            "INSERT INTO shared_meetings(record_id,origin_device_id,title,title_source,title_version,source_filename,transcript_text,transcript_segments,duration_seconds,status,source_created_at,source_updated_at,source_completed_at,local_version,authoritative_revision)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'completed',?10,?11,?12,?13,?14)
             ON CONFLICT(record_id) DO UPDATE SET title=excluded.title,title_source=excluded.title_source,title_version=excluded.title_version,source_updated_at=excluded.source_updated_at,local_version=excluded.local_version,authoritative_revision=excluded.authoritative_revision",
            params![snapshot.record_id.to_string(),snapshot.origin_device_id.to_string(),title,title_source,title_version,
                snapshot.payload.source_filename,snapshot.payload.transcript_text,
                snapshot.payload.transcript_segments.as_ref().map(serde_json::to_string).transpose()?,
                snapshot.payload.duration_seconds,snapshot.created_at,snapshot.updated_at,snapshot.payload.completed_at,
                snapshot.local_version,revision],
        )?;
        append_change(
            &tx,
            PendingChange {
                kind: RecordKind::Meeting,
                record_id: snapshot.record_id,
                origin: Some(snapshot.origin_device_id),
                revision,
                snapshot: Some(Snapshot::Meeting(snapshot.clone())),
                operation: ChangeOperation::Upsert,
                changed_at: &snapshot.updated_at,
            },
        )?;
        tx.commit()?;
        Ok(ApplyResult {
            revision,
            changed: true,
        })
    }

    pub fn apply_artifact_snapshot(
        conn: &mut Connection,
        snapshot: &CompletedArtifactSnapshot,
    ) -> std::result::Result<ApplyResult, ApplySnapshotError> {
        let tx = conn
            .transaction()
            .context("starting authoritative artifact transaction")?;
        let parent: Option<(String, Option<String>)> = tx.query_row(
            "SELECT i.kind,i.deleted_at FROM shared_record_index i INNER JOIN shared_meetings m ON m.record_id=i.record_id WHERE i.record_id=?1",
            [snapshot.parent_record_id.to_string()], |row| Ok((row.get(0)?,row.get(1)?))
        ).optional()?;
        if !matches!(parent, Some((ref kind, None)) if kind == "meeting") {
            return Err(ApplySnapshotError::ParentUnavailable);
        }
        let existing: Option<(String, Option<String>, u64, Option<String>)> = tx.query_row(
            "SELECT kind,origin_device_id,authoritative_revision,deleted_at FROM shared_record_index WHERE record_id=?1",
            [snapshot.record_id.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
        ).optional()?;
        if let Some((kind, origin, revision, deleted_at)) = &existing {
            if kind != "artifact" {
                return Err(ApplySnapshotError::KindChanged);
            }
            if deleted_at.is_some() {
                return Err(ApplySnapshotError::Tombstoned);
            }
            if origin
                .as_deref()
                .is_some_and(|value| value != snapshot.origin_device_id.to_string())
            {
                return Err(ApplySnapshotError::OriginChanged);
            }
            let current = get_artifact_from(&tx, snapshot.record_id)?
                .ok_or(ApplySnapshotError::VersionConflict)?;
            if artifact_matches_snapshot(&current, snapshot) {
                tx.commit()?;
                return Ok(ApplyResult {
                    revision: *revision,
                    changed: false,
                });
            }
            return Err(ApplySnapshotError::VersionConflict);
        }
        let revision = 1;
        tx.execute("INSERT INTO shared_record_index(record_id,kind,origin_device_id,authoritative_revision) VALUES(?1,'artifact',?2,?3)", params![snapshot.record_id.to_string(),snapshot.origin_device_id.to_string(),revision])?;
        tx.execute(
            "INSERT INTO shared_artifacts(record_id,parent_record_id,origin_device_id,artifact_kind,title,template_id,agent_profile_name,content_markdown,source_created_at,source_updated_at,source_completed_at,local_version,authoritative_revision)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![snapshot.record_id.to_string(),snapshot.parent_record_id.to_string(),snapshot.origin_device_id.to_string(),
                snapshot.payload.artifact_kind,snapshot.payload.title,snapshot.payload.template_id,snapshot.payload.agent_profile_name,
                snapshot.payload.content_markdown,snapshot.created_at,snapshot.updated_at,snapshot.payload.completed_at,snapshot.local_version,revision],
        )?;
        append_change(
            &tx,
            PendingChange {
                kind: RecordKind::Artifact,
                record_id: snapshot.record_id,
                origin: Some(snapshot.origin_device_id),
                revision,
                snapshot: Some(Snapshot::Artifact(snapshot.clone())),
                operation: ChangeOperation::Upsert,
                changed_at: &snapshot.updated_at,
            },
        )?;
        tx.commit()?;
        Ok(ApplyResult {
            revision,
            changed: true,
        })
    }

    pub fn get_meeting(conn: &Connection, record_id: RecordId) -> Result<Option<SharedMeeting>> {
        get_meeting_from(conn, record_id)
    }

    pub fn page_meetings(
        conn: &Connection,
        query: Option<&str>,
        after: Option<(&str, RecordId)>,
        limit: usize,
    ) -> Result<Vec<SharedMeeting>> {
        let mut sql = "SELECT record_id FROM shared_meetings WHERE deleted_at IS NULL".to_owned();
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(query) = query {
            sql.push_str(" AND (COALESCE(title,'') LIKE ? OR transcript_text LIKE ?)");
            let pattern = format!("%{query}%");
            values.push(Box::new(pattern.clone()));
            values.push(Box::new(pattern));
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
        let ids = statement
            .query_map(refs.as_slice(), |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                let id = id.parse().map_err(anyhow::Error::msg)?;
                get_meeting_from(conn, id)?.context("shared meeting disappeared")
            })
            .collect()
    }

    pub fn compare_and_set_meeting_title(
        conn: &mut Connection,
        record_id: RecordId,
        patch: &MeetingTitlePatch,
    ) -> Result<Option<SharedMeeting>> {
        let title = patch.title.trim();
        if title.is_empty() {
            anyhow::bail!("Meeting Title cannot be blank");
        }
        let title_source = patch.title_source.as_deref().unwrap_or("manual");
        if !matches!(title_source, "manual" | "generated") {
            anyhow::bail!("invalid title source");
        }
        let tx = conn.transaction()?;
        let current = get_meeting_from(&tx, record_id)?;
        let Some(current) = current else {
            return Ok(None);
        };
        if current.title_version != patch.expected_title_version {
            anyhow::bail!("title_version_conflict");
        }
        let revision = current.authoritative_revision + 1;
        tx.execute(
            "UPDATE shared_meetings SET title=?2,title_source=?3,title_authority='hub',title_version=title_version+1,authoritative_revision=?4,source_updated_at=?5 WHERE record_id=?1 AND deleted_at IS NULL AND title_version=?6",
            params![record_id.to_string(),title,title_source,revision,chrono::Utc::now().to_rfc3339(),patch.expected_title_version],
        )?;
        tx.execute("UPDATE shared_record_index SET authoritative_revision=?2,updated_at=CURRENT_TIMESTAMP WHERE record_id=?1",params![record_id.to_string(),revision])?;
        let updated =
            get_meeting_from(&tx, record_id)?.context("meeting disappeared after title update")?;
        append_change(
            &tx,
            PendingChange {
                kind: RecordKind::Meeting,
                record_id,
                origin: Some(updated.origin_device_id),
                revision,
                snapshot: None,
                operation: ChangeOperation::Upsert,
                changed_at: &updated.updated_at,
            },
        )?;
        tx.commit()?;
        Ok(Some(updated))
    }

    pub fn apply_delete(
        conn: &mut Connection,
        record_id: RecordId,
        kind: RecordKind,
        deleted_at: &str,
    ) -> std::result::Result<ApplyResult, ApplySnapshotError> {
        let tx = conn.transaction()?;
        let kind_name = super::sync_outbox::kind_name(kind);
        let existing: Option<(String,u64,Option<String>,Option<String>)> = tx.query_row(
            "SELECT kind,authoritative_revision,deleted_at,origin_device_id FROM shared_record_index WHERE record_id=?1",
            [record_id.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))
        ).optional()?;
        if let Some((existing_kind, revision, existing_deleted, _)) = &existing {
            if existing_kind != kind_name {
                return Err(ApplySnapshotError::KindChanged);
            }
            if existing_deleted.is_some() {
                tx.commit()?;
                return Ok(ApplyResult {
                    revision: *revision,
                    changed: false,
                });
            }
        }
        let revision = existing.as_ref().map_or(1, |value| value.1 + 1);
        tx.execute(
            "INSERT INTO shared_record_index(record_id,kind,authoritative_revision,deleted_at) VALUES(?1,?2,?3,?4)
             ON CONFLICT(record_id) DO UPDATE SET authoritative_revision=excluded.authoritative_revision,deleted_at=excluded.deleted_at,updated_at=CURRENT_TIMESTAMP",
            params![record_id.to_string(),kind_name,revision,deleted_at],
        )?;
        match kind {
            RecordKind::Dictation => {
                tx.execute("UPDATE shared_dictations SET deleted_at=?2,authoritative_revision=?3 WHERE record_id=?1",params![record_id.to_string(),deleted_at,revision])?;
            }
            RecordKind::Artifact => {
                tx.execute("UPDATE shared_artifacts SET deleted_at=?2,authoritative_revision=?3 WHERE record_id=?1",params![record_id.to_string(),deleted_at,revision])?;
            }
            RecordKind::Meeting => {
                tx.execute("UPDATE shared_meetings SET deleted_at=?2,authoritative_revision=?3 WHERE record_id=?1",params![record_id.to_string(),deleted_at,revision])?;
                let children = {
                    let mut statement = tx.prepare("SELECT record_id FROM shared_artifacts WHERE parent_record_id=?1 AND deleted_at IS NULL")?;
                    let rows = statement
                        .query_map([record_id.to_string()], |row| row.get::<_, String>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    rows
                };
                for child in children {
                    let child_id: RecordId = child.parse().map_err(anyhow::Error::msg)?;
                    let child_revision: u64 = tx.query_row("SELECT authoritative_revision+1 FROM shared_record_index WHERE record_id=?1",[&child],|row| row.get(0))?;
                    tx.execute("UPDATE shared_record_index SET deleted_at=?2,authoritative_revision=?3,updated_at=CURRENT_TIMESTAMP WHERE record_id=?1",params![child,deleted_at,child_revision])?;
                    tx.execute("UPDATE shared_artifacts SET deleted_at=?2,authoritative_revision=?3 WHERE record_id=?1",params![child,deleted_at,child_revision])?;
                    tx.execute("INSERT OR IGNORE INTO sync_tombstones(record_id,kind,deleted_version,deleted_at) VALUES(?1,'artifact',?2,?3)",params![child,child_revision,deleted_at])?;
                    append_change(
                        &tx,
                        PendingChange {
                            kind: RecordKind::Artifact,
                            record_id: child_id,
                            origin: None,
                            revision: child_revision,
                            snapshot: None,
                            operation: ChangeOperation::Delete,
                            changed_at: deleted_at,
                        },
                    )?;
                }
            }
        }
        tx.execute("INSERT OR IGNORE INTO sync_tombstones(record_id,kind,deleted_version,deleted_at) VALUES(?1,?2,?3,?4)",params![record_id.to_string(),kind_name,revision,deleted_at])?;
        append_change(
            &tx,
            PendingChange {
                kind,
                record_id,
                origin: existing
                    .and_then(|value| value.3)
                    .map(|value| value.parse())
                    .transpose()
                    .map_err(anyhow::Error::msg)?,
                revision,
                snapshot: None,
                operation: ChangeOperation::Delete,
                changed_at: deleted_at,
            },
        )?;
        tx.commit()?;
        Ok(ApplyResult {
            revision,
            changed: true,
        })
    }
}

struct PendingChange<'a> {
    kind: RecordKind,
    record_id: RecordId,
    origin: Option<DeviceId>,
    revision: u64,
    snapshot: Option<Snapshot>,
    operation: ChangeOperation,
    changed_at: &'a str,
}

fn append_change(conn: &Connection, pending: PendingChange<'_>) -> Result<()> {
    let change = ChangeEnvelope {
        cursor: None,
        operation: pending.operation,
        kind: pending.kind,
        record_id: pending.record_id,
        origin_device_id: pending.origin,
        authoritative_revision: pending.revision,
        snapshot: pending.snapshot,
        changed_at: pending.changed_at.to_owned(),
    };
    conn.execute("INSERT INTO shared_library_changes(operation,kind,record_id,authoritative_revision,change_json) VALUES(?1,?2,?3,?4,?5)",params![match pending.operation { ChangeOperation::Upsert=>"upsert",ChangeOperation::Delete=>"delete"},crate::db::sync_outbox::kind_name(pending.kind),pending.record_id.to_string(),pending.revision,serde_json::to_string(&change)?])?;
    Ok(())
}

struct StoredMeetingRow {
    record_id: String,
    origin_device_id: String,
    title: Option<String>,
    title_source: Option<String>,
    title_version: u64,
    source_filename: Option<String>,
    transcript_text: String,
    transcript_segments: Option<String>,
    duration_seconds: u64,
    status: String,
    created_at: String,
    updated_at: String,
    completed_at: String,
    local_version: u64,
    authoritative_revision: u64,
}

fn get_meeting_from(conn: &Connection, record_id: RecordId) -> Result<Option<SharedMeeting>> {
    let row: Option<StoredMeetingRow> = conn.query_row(
        "SELECT record_id,origin_device_id,title,title_source,title_version,source_filename,transcript_text,transcript_segments,duration_seconds,status,source_created_at,source_updated_at,source_completed_at,local_version,authoritative_revision FROM shared_meetings WHERE record_id=?1 AND deleted_at IS NULL",
        [record_id.to_string()], |row| Ok(StoredMeetingRow { record_id:row.get(0)?,origin_device_id:row.get(1)?,title:row.get(2)?,title_source:row.get(3)?,title_version:row.get(4)?,source_filename:row.get(5)?,transcript_text:row.get(6)?,transcript_segments:row.get(7)?,duration_seconds:row.get(8)?,status:row.get(9)?,created_at:row.get(10)?,updated_at:row.get(11)?,completed_at:row.get(12)?,local_version:row.get(13)?,authoritative_revision:row.get(14)? })
    ).optional()?;
    row.map(|row| {
        let id = row.record_id.parse().map_err(anyhow::Error::msg)?;
        let artifacts = list_artifacts_for_meeting(conn, id)?;
        Ok(SharedMeeting {
            record_id: id,
            origin_device_id: row.origin_device_id.parse().map_err(anyhow::Error::msg)?,
            title: row.title,
            title_source: row.title_source,
            title_version: row.title_version,
            source_filename: row.source_filename,
            transcript_text: row.transcript_text,
            transcript_segments: row
                .transcript_segments
                .as_deref()
                .and_then(|value| serde_json::from_str(value).ok()),
            duration_seconds: row.duration_seconds,
            status: row.status,
            created_at: row.created_at,
            updated_at: row.updated_at,
            completed_at: row.completed_at,
            local_version: row.local_version,
            authoritative_revision: row.authoritative_revision,
            artifacts,
        })
    })
    .transpose()
}

fn hub_owns_meeting_title(conn: &Connection, record_id: RecordId) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT title_authority = 'hub' FROM shared_meetings WHERE record_id = ?1 AND deleted_at IS NULL",
            [record_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

fn list_artifacts_for_meeting(conn: &Connection, parent: RecordId) -> Result<Vec<SharedArtifact>> {
    let mut statement=conn.prepare("SELECT record_id,parent_record_id,origin_device_id,artifact_kind,title,template_id,agent_profile_name,content_markdown,source_created_at,source_updated_at,source_completed_at,local_version,authoritative_revision FROM shared_artifacts WHERE parent_record_id=?1 AND deleted_at IS NULL ORDER BY source_created_at DESC,record_id DESC")?;
    let rows = statement.query_map([parent.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
            row.get(10)?,
            row.get(11)?,
            row.get(12)?,
        ))
    })?;
    rows.map(|row| {
        let row = row?;
        Ok(SharedArtifact {
            record_id: row.0.parse().map_err(anyhow::Error::msg)?,
            parent_record_id: row.1.parse().map_err(anyhow::Error::msg)?,
            origin_device_id: row.2.parse().map_err(anyhow::Error::msg)?,
            artifact_kind: row.3,
            title: row.4,
            template_id: row.5,
            agent_profile_name: row.6,
            content_markdown: row.7,
            created_at: row.8,
            updated_at: row.9,
            completed_at: row.10,
            local_version: row.11,
            authoritative_revision: row.12,
        })
    })
    .collect()
}

fn get_artifact_from(conn: &Connection, id: RecordId) -> Result<Option<SharedArtifact>> {
    Ok(list_artifacts_for_meeting_by_id(conn, id)?
        .into_iter()
        .next())
}
fn list_artifacts_for_meeting_by_id(
    conn: &Connection,
    id: RecordId,
) -> Result<Vec<SharedArtifact>> {
    let parent:Option<String>=conn.query_row("SELECT parent_record_id FROM shared_artifacts WHERE record_id=?1 AND deleted_at IS NULL",[id.to_string()],|row|row.get(0)).optional()?;
    match parent {
        Some(parent) => {
            list_artifacts_for_meeting(conn, parent.parse().map_err(anyhow::Error::msg)?).map(
                |values| {
                    values
                        .into_iter()
                        .filter(|value| value.record_id == id)
                        .collect()
                },
            )
        }
        None => Ok(vec![]),
    }
}
fn meeting_matches_snapshot(current: &SharedMeeting, snapshot: &MeetingSnapshot) -> bool {
    meeting_origin_content_matches_snapshot(current, snapshot)
        && current.title == snapshot.payload.title
        && current.title_source == snapshot.payload.title_source
        && current.title_version == snapshot.payload.title_version
        && current.updated_at == snapshot.updated_at
}

fn meeting_origin_content_matches_snapshot(
    current: &SharedMeeting,
    snapshot: &MeetingSnapshot,
) -> bool {
    current.origin_device_id == snapshot.origin_device_id
        && current.source_filename == snapshot.payload.source_filename
        && current.transcript_text == snapshot.payload.transcript_text
        && serde_json::to_value(&current.transcript_segments).ok()
            == serde_json::to_value(&snapshot.payload.transcript_segments).ok()
        && current.duration_seconds == snapshot.payload.duration_seconds
        && current.status == snapshot.payload.status
        && current.created_at == snapshot.created_at
        && current.completed_at == snapshot.payload.completed_at
}
fn artifact_matches_snapshot(
    current: &SharedArtifact,
    snapshot: &CompletedArtifactSnapshot,
) -> bool {
    current.local_version == snapshot.local_version
        && current.parent_record_id == snapshot.parent_record_id
        && current.origin_device_id == snapshot.origin_device_id
        && current.artifact_kind == snapshot.payload.artifact_kind
        && current.title == snapshot.payload.title
        && current.template_id == snapshot.payload.template_id
        && current.agent_profile_name == snapshot.payload.agent_profile_name
        && current.content_markdown == snapshot.payload.content_markdown
        && current.created_at == snapshot.created_at
        && current.updated_at == snapshot.updated_at
        && current.completed_at == snapshot.payload.completed_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::protocol::{
        CompletedArtifactPayload, DictationPayload, MeetingPayload, RecordKind,
    };

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

    fn meeting_snapshot(record_id: RecordId, origin: DeviceId) -> MeetingSnapshot {
        MeetingSnapshot {
            kind: RecordKind::Meeting,
            schema_version: 1,
            record_id,
            origin_device_id: origin,
            local_version: 1,
            created_at: "2026-09-04T10:00:00Z".into(),
            updated_at: "2026-09-04T10:01:00Z".into(),
            payload: MeetingPayload {
                title: Some("Origin title".into()),
                title_source: Some("generated".into()),
                title_version: 1,
                source_filename: Some("meeting.wav".into()),
                transcript_text: "A portable meeting transcript".into(),
                transcript_segments: None,
                duration_seconds: 60,
                status: "completed".into(),
                completed_at: "2026-09-04T10:01:00Z".into(),
            },
        }
    }

    fn artifact_snapshot(
        record_id: RecordId,
        parent_record_id: RecordId,
        origin: DeviceId,
    ) -> CompletedArtifactSnapshot {
        CompletedArtifactSnapshot {
            kind: RecordKind::Artifact,
            schema_version: 1,
            record_id,
            parent_record_id,
            origin_device_id: origin,
            local_version: 1,
            created_at: "2026-09-04T10:02:00Z".into(),
            updated_at: "2026-09-04T10:02:00Z".into(),
            payload: CompletedArtifactPayload {
                artifact_kind: "summary".into(),
                title: "Summary".into(),
                template_id: Some("standard_meeting".into()),
                agent_profile_name: Some("Local agent".into()),
                content_markdown: "# Summary".into(),
                completed_at: "2026-09-04T10:02:00Z".into(),
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

    #[test]
    fn hub_title_edit_survives_idempotent_and_newer_origin_snapshots() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let record_id = RecordId::new();
        let origin = DeviceId::new();
        let first = meeting_snapshot(record_id, origin);
        SharedLibraryRepository::apply_meeting_snapshot(&mut conn, &first).unwrap();

        let edited = SharedLibraryRepository::compare_and_set_meeting_title(
            &mut conn,
            record_id,
            &MeetingTitlePatch {
                title: "Hub-owned title".into(),
                expected_title_version: 1,
                title_source: None,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(edited.title.as_deref(), Some("Hub-owned title"));

        let duplicate = SharedLibraryRepository::apply_meeting_snapshot(&mut conn, &first).unwrap();
        assert!(!duplicate.changed);
        let mut newer = first;
        newer.local_version = 2;
        newer.updated_at = "2026-09-04T10:03:00Z".into();
        newer.payload.title = Some("Later generated title".into());
        newer.payload.title_version = 2;
        SharedLibraryRepository::apply_meeting_snapshot(&mut conn, &newer).unwrap();
        let current = SharedLibraryRepository::get_meeting(&conn, record_id)
            .unwrap()
            .unwrap();
        assert_eq!(current.title.as_deref(), Some("Hub-owned title"));
        assert_eq!(current.title_source.as_deref(), Some("manual"));
    }

    #[test]
    fn newer_meeting_snapshot_cannot_change_authoritative_transcript_metadata() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let record_id = RecordId::new();
        let origin = DeviceId::new();
        let first = meeting_snapshot(record_id, origin);
        SharedLibraryRepository::apply_meeting_snapshot(&mut conn, &first).unwrap();

        let mut changed = first;
        changed.local_version = 2;
        changed.payload.transcript_segments = Some(vec![audetic_core::jobs_client::Segment {
            start: 0.0,
            end: 1.0,
            text: "changed after acceptance".into(),
        }]);

        assert!(matches!(
            SharedLibraryRepository::apply_meeting_snapshot(&mut conn, &changed),
            Err(ApplySnapshotError::VersionConflict)
        ));
        let stored = SharedLibraryRepository::get_meeting(&conn, record_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, "completed");
        assert!(stored.transcript_segments.is_none());
        assert_eq!(stored.authoritative_revision, 1);
    }

    #[test]
    fn meeting_delete_tombstones_children_once_and_blocks_delayed_records() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let meeting_id = RecordId::new();
        let artifact_id = RecordId::new();
        let origin = DeviceId::new();
        let meeting = meeting_snapshot(meeting_id, origin);
        let artifact = artifact_snapshot(artifact_id, meeting_id, origin);
        SharedLibraryRepository::apply_meeting_snapshot(&mut conn, &meeting).unwrap();
        SharedLibraryRepository::apply_artifact_snapshot(&mut conn, &artifact).unwrap();

        let first = SharedLibraryRepository::apply_delete(
            &mut conn,
            meeting_id,
            RecordKind::Meeting,
            "2026-09-04T11:00:00Z",
        )
        .unwrap();
        assert!(first.changed);
        let duplicate = SharedLibraryRepository::apply_delete(
            &mut conn,
            meeting_id,
            RecordKind::Meeting,
            "2026-09-04T11:00:00Z",
        )
        .unwrap();
        assert!(!duplicate.changed);
        assert!(SharedLibraryRepository::get_meeting(&conn, meeting_id)
            .unwrap()
            .is_none());
        assert!(matches!(
            SharedLibraryRepository::apply_artifact_snapshot(&mut conn, &artifact),
            Err(ApplySnapshotError::ParentUnavailable)
        ));
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM sync_tombstones", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_library_changes", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            4
        );
    }

    #[test]
    fn artifact_idempotence_requires_the_same_local_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let meeting_id = RecordId::new();
        let artifact_id = RecordId::new();
        let origin = DeviceId::new();
        SharedLibraryRepository::apply_meeting_snapshot(
            &mut conn,
            &meeting_snapshot(meeting_id, origin),
        )
        .unwrap();
        let first = artifact_snapshot(artifact_id, meeting_id, origin);
        assert!(
            SharedLibraryRepository::apply_artifact_snapshot(&mut conn, &first)
                .unwrap()
                .changed
        );
        assert!(
            !SharedLibraryRepository::apply_artifact_snapshot(&mut conn, &first)
                .unwrap()
                .changed
        );

        let mut conflicting_version = first;
        conflicting_version.local_version = 2;
        assert!(matches!(
            SharedLibraryRepository::apply_artifact_snapshot(&mut conn, &conflicting_version),
            Err(ApplySnapshotError::VersionConflict)
        ));
        let stored = get_artifact_from(&conn, artifact_id).unwrap().unwrap();
        assert_eq!(stored.local_version, 1);
        assert_eq!(stored.authoritative_revision, 1);
    }

    #[test]
    fn kind_specific_prearrival_delete_rejects_later_snapshot_and_kind_change() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let delayed_id = RecordId::new();
        SharedLibraryRepository::apply_delete(
            &mut conn,
            delayed_id,
            RecordKind::Meeting,
            "2026-09-04T11:00:00Z",
        )
        .unwrap();
        assert!(matches!(
            SharedLibraryRepository::apply_meeting_snapshot(
                &mut conn,
                &meeting_snapshot(delayed_id, DeviceId::new())
            ),
            Err(ApplySnapshotError::Tombstoned)
        ));
        assert!(matches!(
            SharedLibraryRepository::apply_snapshot(
                &mut conn,
                &snapshot(delayed_id, DeviceId::new())
            ),
            Err(ApplySnapshotError::KindChanged)
        ));
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_library_changes", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
