use anyhow::{bail, Context, Result};
use audetic_core::sync::RecordId;
use rusqlite::{params, Connection};

use crate::sync::protocol::{
    ChangeCursor, ChangeOperation, ChangePage, ChangeRecord, ChangeTarget, RecordKind, Snapshot,
    MAX_CHANGE_PAGE,
};

use super::library_codec::{decode_change, encode_change, STORED_CODEC_V1};

/// Authoritative, replay-safe change feed backed only by self-contained rows.
pub struct LibraryChangeFeedRepository;

impl LibraryChangeFeedRepository {
    pub(crate) fn append(conn: &Connection, record: &ChangeRecord) -> Result<ChangeCursor> {
        if record.cursor != ChangeCursor::ZERO {
            bail!("a new authoritative change must not supply its own cursor");
        }
        validate_self_contained(record)?;
        let body = encode_change(record)?;
        conn.execute(
            "INSERT INTO shared_library_change_feed_v1
                (codec_version,operation,kind,record_id,authoritative_revision,body_json)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                STORED_CODEC_V1,
                operation_name(record.operation),
                kind_name(record.kind),
                record.record_id.to_string(),
                to_i64(record.authoritative_revision, "authoritative revision")?,
                body,
            ],
        )
        .context("appending replay-safe Shared Library change")?;
        let cursor = u64::try_from(conn.last_insert_rowid())
            .context("negative replay-safe Shared Library cursor")?;
        Ok(ChangeCursor::new(cursor))
    }

    pub fn latest_cursor(conn: &Connection) -> Result<ChangeCursor> {
        let value = conn
            .query_row(
                "SELECT COALESCE(MAX(cursor),0) FROM shared_library_change_feed_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .context("reading replay-safe Shared Library target")?;
        Ok(ChangeCursor::new(
            u64::try_from(value).context("negative replay-safe Shared Library cursor")?,
        ))
    }

    pub fn page(
        conn: &Connection,
        after: ChangeCursor,
        requested_target: Option<ChangeTarget>,
        limit: usize,
    ) -> Result<ChangePage> {
        if limit == 0 || limit > MAX_CHANGE_PAGE {
            bail!("change page limit must be between 1 and {MAX_CHANGE_PAGE}");
        }
        let latest = Self::latest_cursor(conn)?;
        require_known_cursor(conn, after, latest, "after")?;
        let target = requested_target.unwrap_or_else(|| ChangeTarget::new(latest));
        let target_cursor = target.cursor();
        require_known_cursor(conn, target_cursor, latest, "target")?;
        if target_cursor > latest {
            bail!("change target is newer than the committed feed");
        }
        if after > target_cursor {
            bail!("change cursor is past the immutable target");
        }

        let after_sql = to_i64(after.value(), "after cursor")?;
        let target_sql = to_i64(target_cursor.value(), "target cursor")?;
        let mut statement = conn
            .prepare(
                "SELECT cursor,codec_version,operation,kind,record_id,
                        authoritative_revision,body_json
                 FROM shared_library_change_feed_v1
                 WHERE cursor>?1 AND cursor<=?2
                 ORDER BY cursor ASC LIMIT ?3",
            )
            .context("preparing replay-safe Shared Library page")?;
        let rows = statement
            .query_map(params![after_sql, target_sql, limit], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, u16>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .context("reading replay-safe Shared Library page")?;
        let mut changes = Vec::new();
        for row in rows {
            let (cursor, codec, operation, kind, record_id, revision, body) = row?;
            let cursor =
                ChangeCursor::new(u64::try_from(cursor).context("negative change row cursor")?);
            let change = decode_change(codec, cursor, &body)?;
            validate_self_contained(&change)?;
            if operation != operation_name(change.operation)
                || kind != kind_name(change.kind)
                || record_id != change.record_id.to_string()
                || u64::try_from(revision).ok() != Some(change.authoritative_revision)
            {
                bail!("stored change body disagrees with its indexed columns");
            }
            changes.push(change);
        }

        let through = changes.last().map_or(after, |change| change.cursor);
        let eligible_remain: bool = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM shared_library_change_feed_v1
                    WHERE cursor>?1 AND cursor<=?2
                 )",
                params![to_i64(through.value(), "through cursor")?, target_sql],
                |row| row.get(0),
            )
            .context("checking replay-safe Shared Library page completion")?;
        let complete = through == target_cursor && !eligible_remain;
        if !complete && through == after {
            bail!("change request cannot advance to its immutable target");
        }
        Ok(ChangePage {
            target_cursor: target,
            after_cursor: after,
            through_cursor: through,
            complete,
            changes,
        })
    }

    /// Seed one exact baseline from current projections and retained tombstones.
    /// Called by migration 9 inside the migration's exclusive transaction.
    pub(crate) fn seed_current(conn: &Connection) -> Result<()> {
        let existing: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM shared_library_change_feed_v1)",
            [],
            |row| row.get(0),
        )?;
        if existing {
            bail!("replay-safe Shared Library feed must be empty before seeding");
        }

        let live = {
            let mut statement = conn.prepare(
                "SELECT record_id,kind,origin_device_id,authoritative_revision
                 FROM shared_record_index
                 WHERE deleted_at IS NULL
                   AND NOT EXISTS (
                     SELECT 1 FROM sync_outbox_items o
                     WHERE o.record_id=shared_record_index.record_id
                       AND o.kind=shared_record_index.kind
                       AND json_type(o.snapshot_json,'$.deleted_at') IS NOT NULL
                   )
                   AND NOT EXISTS (
                     SELECT 1
                     FROM shared_artifacts a
                     INNER JOIN sync_outbox_items o
                       ON o.record_id=a.parent_record_id AND o.kind='meeting'
                     WHERE shared_record_index.kind='artifact'
                       AND a.record_id=shared_record_index.record_id
                       AND json_type(o.snapshot_json,'$.deleted_at') IS NOT NULL
                   )
                 ORDER BY CASE kind WHEN 'dictation' THEN 0 WHEN 'meeting' THEN 1 ELSE 2 END,
                          record_id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        for (record_id, kind, origin, revision) in live {
            let record_id = parse_id::<RecordId>(&record_id, "authoritative record ID")?;
            let kind = parse_kind(&kind)?;
            let Some(snapshot) =
                super::shared_library::authoritative_snapshot(conn, kind, record_id)?
            else {
                // Projection reads retain their own visibility guards.
                continue;
            };
            let changed_at = snapshot_changed_at(&snapshot).to_owned();
            Self::append(
                conn,
                &ChangeRecord {
                    cursor: ChangeCursor::ZERO,
                    operation: ChangeOperation::Upsert,
                    kind,
                    record_id,
                    origin_device_id: origin
                        .as_deref()
                        .map(|value| parse_id(value, "authoritative origin device ID"))
                        .transpose()?,
                    authoritative_revision: u64::try_from(revision)
                        .context("negative authoritative revision")?,
                    snapshot: Some(snapshot),
                    changed_at,
                },
            )?;
        }

        let tombstones = {
            let mut statement = conn.prepare(
                "SELECT t.record_id,t.kind,t.deleted_version,t.deleted_at,i.origin_device_id
                 FROM sync_tombstones t
                 LEFT JOIN shared_record_index i ON i.record_id=t.record_id
                 WHERE i.record_id IS NULL OR i.deleted_at IS NOT NULL
                 ORDER BY CASE t.kind WHEN 'dictation' THEN 0 WHEN 'meeting' THEN 1 ELSE 2 END,
                          t.record_id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        for (record_id, kind, revision, deleted_at, origin) in tombstones {
            let record_id = parse_id::<RecordId>(&record_id, "tombstone record ID")?;
            Self::append(
                conn,
                &ChangeRecord {
                    cursor: ChangeCursor::ZERO,
                    operation: ChangeOperation::Delete,
                    kind: parse_kind(&kind)?,
                    record_id,
                    origin_device_id: origin
                        .as_deref()
                        .map(|value| parse_id(value, "tombstone origin device ID"))
                        .transpose()?,
                    authoritative_revision: u64::try_from(revision)
                        .context("negative tombstone revision")?,
                    snapshot: None,
                    changed_at: deleted_at,
                },
            )?;
        }
        Ok(())
    }
}

fn validate_self_contained(change: &ChangeRecord) -> Result<()> {
    if change.authoritative_revision == 0 {
        bail!("authoritative change revision must be positive");
    }
    match change.operation {
        ChangeOperation::Delete => {
            if change.snapshot.is_some() {
                bail!("delete change must not contain a live snapshot");
            }
        }
        ChangeOperation::Upsert | ChangeOperation::PayloadAvailability => {
            let snapshot = change
                .snapshot
                .as_ref()
                .context("non-delete change requires a self-contained snapshot")?;
            if matches!(snapshot, Snapshot::Delete(_))
                || snapshot.record_id() != change.record_id
                || snapshot.kind() != change.kind
            {
                bail!("change identity disagrees with its self-contained snapshot");
            }
            if change.origin_device_id != Some(snapshot.origin_device_id()) {
                bail!("change origin disagrees with its self-contained snapshot");
            }
        }
    }
    Ok(())
}

fn require_known_cursor(
    conn: &Connection,
    cursor: ChangeCursor,
    latest: ChangeCursor,
    name: &str,
) -> Result<()> {
    if cursor == ChangeCursor::ZERO {
        return Ok(());
    }
    if cursor > latest {
        bail!("{name} change cursor is newer than the committed feed");
    }
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM shared_library_change_feed_v1 WHERE cursor=?1)",
        [to_i64(cursor.value(), "change cursor")?],
        |row| row.get(0),
    )?;
    if !exists {
        bail!("{name} change cursor does not identify a committed feed record");
    }
    Ok(())
}

fn snapshot_changed_at(snapshot: &Snapshot) -> &str {
    match snapshot {
        Snapshot::Dictation(value) => &value.updated_at,
        Snapshot::Meeting(value) => &value.updated_at,
        Snapshot::Artifact(value) => &value.updated_at,
        Snapshot::Delete(value) => &value.deleted_at,
    }
}

fn operation_name(value: ChangeOperation) -> &'static str {
    match value {
        ChangeOperation::Upsert => "upsert",
        ChangeOperation::Delete => "delete",
        ChangeOperation::PayloadAvailability => "payload_availability",
    }
}

pub(super) fn kind_name(value: RecordKind) -> &'static str {
    match value {
        RecordKind::Dictation => "dictation",
        RecordKind::Meeting => "meeting",
        RecordKind::Artifact => "artifact",
    }
}

pub(super) fn parse_kind(value: &str) -> Result<RecordKind> {
    match value {
        "dictation" => Ok(RecordKind::Dictation),
        "meeting" => Ok(RecordKind::Meeting),
        "artifact" => Ok(RecordKind::Artifact),
        _ => bail!("invalid stored record kind {value:?}"),
    }
}

fn parse_id<T>(value: &str, field: &str) -> Result<T>
where
    T: std::str::FromStr<Err = String>,
{
    value
        .parse()
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("invalid {field}"))
}

fn to_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).with_context(|| format!("{field} exceeds SQLite integer range"))
}

#[cfg(test)]
mod tests {
    use audetic_core::sync::DeviceId;

    use crate::sync::protocol::{DictationPayload, DictationSnapshot};

    use super::*;

    fn snapshot(record_id: RecordId, origin: DeviceId, text: &str) -> Snapshot {
        Snapshot::Dictation(DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id,
            origin_device_id: origin,
            local_version: 1,
            created_at: "2026-09-05T10:00:00Z".into(),
            updated_at: "2026-09-05T10:00:00Z".into(),
            payload: DictationPayload {
                text: text.into(),
                recording_payload: Default::default(),
            },
        })
    }

    fn append_dictation(
        conn: &Connection,
        record_id: RecordId,
        origin: DeviceId,
        text: &str,
    ) -> ChangeCursor {
        LibraryChangeFeedRepository::append(
            conn,
            &ChangeRecord {
                cursor: ChangeCursor::ZERO,
                operation: ChangeOperation::Upsert,
                kind: RecordKind::Dictation,
                record_id,
                origin_device_id: Some(origin),
                authoritative_revision: 1,
                snapshot: Some(snapshot(record_id, origin, text)),
                changed_at: "2026-09-05T10:00:00Z".into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn paging_holds_an_immutable_target_and_replays_identically() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let origin = DeviceId::new();
        let first_id = RecordId::new();
        let second_id = RecordId::new();
        let third_id = RecordId::new();
        assert_eq!(append_dictation(&conn, first_id, origin, "one").value(), 1);
        assert_eq!(append_dictation(&conn, second_id, origin, "two").value(), 2);

        let first = LibraryChangeFeedRepository::page(&conn, ChangeCursor::ZERO, None, 1).unwrap();
        assert_eq!(first.target_cursor.cursor().value(), 2);
        assert_eq!(first.through_cursor.value(), 1);
        assert!(!first.complete);
        assert_eq!(first.changes[0].record_id, first_id);

        assert_eq!(
            append_dictation(&conn, third_id, origin, "three").value(),
            3
        );
        let replay = LibraryChangeFeedRepository::page(
            &conn,
            ChangeCursor::ZERO,
            Some(first.target_cursor),
            1,
        )
        .unwrap();
        assert_eq!(replay.target_cursor, first.target_cursor);
        assert_eq!(replay.through_cursor, first.through_cursor);
        assert_eq!(replay.changes[0].record_id, first.changes[0].record_id);

        let continuation = LibraryChangeFeedRepository::page(
            &conn,
            first.through_cursor,
            Some(first.target_cursor),
            10,
        )
        .unwrap();
        assert!(continuation.complete);
        assert_eq!(continuation.through_cursor.value(), 2);
        assert_eq!(continuation.changes.len(), 1);
        assert_eq!(continuation.changes[0].record_id, second_id);

        for _ in 0..2 {
            let repeated_completion = LibraryChangeFeedRepository::page(
                &conn,
                continuation.through_cursor,
                Some(continuation.target_cursor),
                10,
            )
            .unwrap();
            assert_eq!(
                repeated_completion.target_cursor,
                continuation.target_cursor
            );
            assert_eq!(repeated_completion.after_cursor.value(), 2);
            assert_eq!(repeated_completion.through_cursor.value(), 2);
            assert!(repeated_completion.complete);
            assert!(repeated_completion.changes.is_empty());
        }

        let next = LibraryChangeFeedRepository::page(&conn, continuation.through_cursor, None, 10)
            .unwrap();
        assert!(next.complete);
        assert_eq!(next.target_cursor.cursor().value(), 3);
        assert_eq!(next.changes[0].record_id, third_id);

        assert!(LibraryChangeFeedRepository::page(
            &conn,
            ChangeCursor::new(3),
            Some(ChangeTarget::new(ChangeCursor::new(2))),
            10,
        )
        .is_err());
    }

    #[test]
    fn empty_feed_has_an_explicit_zero_completion() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();

        let page =
            LibraryChangeFeedRepository::page(&conn, ChangeCursor::ZERO, None, MAX_CHANGE_PAGE)
                .unwrap();
        assert_eq!(page.target_cursor.cursor(), ChangeCursor::ZERO);
        assert_eq!(page.through_cursor, ChangeCursor::ZERO);
        assert!(page.complete);
        assert!(page.changes.is_empty());

        let repeated = LibraryChangeFeedRepository::page(
            &conn,
            ChangeCursor::ZERO,
            Some(ChangeTarget::new(ChangeCursor::ZERO)),
            MAX_CHANGE_PAGE,
        )
        .unwrap();
        assert_eq!(repeated.target_cursor.cursor(), ChangeCursor::ZERO);
        assert_eq!(repeated.after_cursor, ChangeCursor::ZERO);
        assert_eq!(repeated.through_cursor, ChangeCursor::ZERO);
        assert!(repeated.complete);
        assert!(repeated.changes.is_empty());
    }

    #[test]
    fn migration_seed_reconstructs_live_rows_and_retained_tombstones() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let origin = DeviceId::new();
        let live_id = RecordId::new();
        let deleted_id = RecordId::new();
        crate::db::shared_library::SharedLibraryRepository::apply(
            &mut conn,
            &snapshot(live_id, origin, "live"),
        )
        .unwrap();
        crate::db::shared_library::SharedLibraryRepository::apply_delete(
            &mut conn,
            deleted_id,
            RecordKind::Meeting,
            "2026-09-05T11:00:00Z",
        )
        .unwrap();
        conn.execute("DELETE FROM shared_library_change_feed_v1", [])
            .unwrap();
        conn.execute(
            "DELETE FROM sqlite_sequence WHERE name='shared_library_change_feed_v1'",
            [],
        )
        .unwrap();

        LibraryChangeFeedRepository::seed_current(&conn).unwrap();
        let page =
            LibraryChangeFeedRepository::page(&conn, ChangeCursor::ZERO, None, MAX_CHANGE_PAGE)
                .unwrap();
        assert!(page.complete);
        assert_eq!(page.changes.len(), 2);
        assert_eq!(page.changes[0].record_id, live_id);
        assert_eq!(page.changes[0].operation, ChangeOperation::Upsert);
        assert!(page.changes[0].snapshot.is_some());
        assert_eq!(page.changes[1].record_id, deleted_id);
        assert_eq!(page.changes[1].operation, ChangeOperation::Delete);
        assert!(page.changes[1].snapshot.is_none());
    }
}
