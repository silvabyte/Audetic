use anyhow::{bail, Context, Result};
use audetic_core::sync::{RecordId, UploadState};
use rusqlite::{params, Connection, Transaction};

use crate::sync::protocol::DictationSnapshot;

#[derive(Clone, Debug)]
pub struct OutboxItem {
    pub record_id: RecordId,
    pub local_version: u64,
    pub snapshot: DictationSnapshot,
    pub attempts: u32,
    pub lease_owner: String,
}

pub struct SyncOutboxRepository;

impl SyncOutboxRepository {
    pub fn enqueue_snapshot(tx: &Transaction<'_>, snapshot: &DictationSnapshot) -> Result<()> {
        let json = serde_json::to_string(snapshot).context("serializing dictation snapshot")?;
        let changed = tx
            .execute(
                "INSERT INTO sync_outbox_items
                 (record_id, kind, local_version, snapshot_json, state)
              VALUES (?1, 'dictation', ?2, ?3, 'pending')
              ON CONFLICT(record_id, kind) DO UPDATE SET
                 local_version = excluded.local_version,
                 snapshot_json = excluded.snapshot_json,
                 state = 'pending', accepted_hub_revision = NULL, attempts = 0,
                 lease_owner = NULL, lease_expires_at = NULL, next_attempt_at = NULL,
                 last_error = NULL, updated_at = CURRENT_TIMESTAMP
              WHERE excluded.local_version > sync_outbox_items.local_version",
                params![
                    snapshot.record_id.to_string(),
                    snapshot.local_version,
                    &json
                ],
            )
            .context("enqueueing dictation snapshot")?;
        if changed == 0 {
            let (version, existing_json) = tx
                .query_row(
                    "SELECT local_version, snapshot_json FROM sync_outbox_items
                     WHERE record_id = ?1 AND kind = 'dictation'",
                    [snapshot.record_id.to_string()],
                    |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
                )
                .context("reading unchanged dictation outbox snapshot")?;
            if snapshot.local_version == version && json != existing_json {
                bail!(
                    "dictation {} local version {} conflicts with its durable outbox snapshot",
                    snapshot.record_id,
                    snapshot.local_version
                );
            }
        }
        Ok(())
    }

    pub fn claim_items(
        conn: &mut Connection,
        lease_owner: &str,
        now: &str,
        lease_expires_at: &str,
        limit: usize,
    ) -> Result<Vec<OutboxItem>> {
        let tx = conn.transaction().context("starting outbox claim")?;
        tx.execute(
            "UPDATE sync_outbox_items
             SET state = 'pending', lease_owner = NULL, lease_expires_at = NULL,
                 last_error = COALESCE(last_error, 'upload interrupted; retrying'),
                 updated_at = CURRENT_TIMESTAMP
             WHERE state = 'uploading' AND lease_expires_at <= ?1",
            [now],
        )
        .context("releasing expired outbox leases")?;
        let candidates = {
            let mut statement = tx.prepare(
                "SELECT record_id FROM sync_outbox_items
                 WHERE state = 'pending'
                   AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
                 ORDER BY created_at, record_id LIMIT ?2",
            )?;
            let rows = statement
                .query_map(params![now, limit], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let mut claimed = Vec::with_capacity(candidates.len());
        for record_id in candidates {
            tx.execute(
                "UPDATE sync_outbox_items SET state = 'uploading', attempts = attempts + 1,
                    lease_owner = ?2, lease_expires_at = ?3, updated_at = CURRENT_TIMESTAMP
                 WHERE record_id = ?1 AND kind = 'dictation' AND state = 'pending'",
                params![record_id, lease_owner, lease_expires_at],
            )?;
            let item = tx.query_row(
                "SELECT record_id, local_version, snapshot_json, attempts, lease_owner
                 FROM sync_outbox_items WHERE record_id = ?1 AND kind = 'dictation'",
                [&record_id],
                |row| {
                    let id: String = row.get(0)?;
                    let json: String = row.get(2)?;
                    Ok((
                        id,
                        row.get::<_, u64>(1)?,
                        json,
                        row.get::<_, u32>(3)?,
                        row.get(4)?,
                    ))
                },
            )?;
            claimed.push(OutboxItem {
                record_id: item.0.parse().map_err(anyhow::Error::msg)?,
                local_version: item.1,
                snapshot: serde_json::from_str(&item.2)?,
                attempts: item.3,
                lease_owner: item.4,
            });
        }
        tx.commit().context("committing outbox claims")?;
        Ok(claimed)
    }

    pub fn mark_snapshot_accepted(
        conn: &Connection,
        item: &OutboxItem,
        revision: u64,
    ) -> Result<()> {
        conn.execute(
            "UPDATE sync_outbox_items SET state = 'synced', accepted_hub_revision = ?3,
                lease_owner = NULL, lease_expires_at = NULL, next_attempt_at = NULL,
                last_error = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE record_id = ?1 AND kind = 'dictation' AND local_version = ?2
                AND lease_owner = ?4",
            params![
                item.record_id.to_string(),
                item.local_version,
                revision,
                item.lease_owner
            ],
        )?;
        Ok(())
    }

    pub fn mark_retry(conn: &Connection, item: &OutboxItem, next: &str, error: &str) -> Result<()> {
        conn.execute(
            "UPDATE sync_outbox_items SET state = 'pending', lease_owner = NULL,
                lease_expires_at = NULL, next_attempt_at = ?4, last_error = ?5,
                updated_at = CURRENT_TIMESTAMP
             WHERE record_id = ?1 AND kind = 'dictation' AND local_version = ?2
                AND lease_owner = ?3",
            params![
                item.record_id.to_string(),
                item.local_version,
                item.lease_owner,
                next,
                error
            ],
        )?;
        Ok(())
    }

    pub fn mark_needs_attention(conn: &Connection, item: &OutboxItem, error: &str) -> Result<()> {
        conn.execute(
            "UPDATE sync_outbox_items SET state = 'needs_attention', lease_owner = NULL,
                 lease_expires_at = NULL, next_attempt_at = NULL, last_error = ?4,
                 updated_at = CURRENT_TIMESTAMP
             WHERE record_id = ?1 AND kind = 'dictation' AND local_version = ?2
                AND lease_owner = ?3",
            params![
                item.record_id.to_string(),
                item.local_version,
                item.lease_owner,
                error
            ],
        )?;
        Ok(())
    }

    pub fn retry_all(conn: &Connection) -> Result<usize> {
        conn.execute(
            "UPDATE sync_outbox_items SET state = 'pending', next_attempt_at = NULL,
                lease_owner = NULL, lease_expires_at = NULL, last_error = NULL,
                updated_at = CURRENT_TIMESTAMP
             WHERE state IN ('pending', 'uploading', 'needs_attention')",
            [],
        )
        .context("resetting outbox items")
    }

    pub fn counts(conn: &Connection) -> Result<(u64, Option<String>)> {
        let pending = conn.query_row(
            "SELECT COUNT(*) FROM sync_outbox_items WHERE state != 'synced'",
            [],
            |row| row.get(0),
        )?;
        let error = conn
            .query_row(
                "SELECT last_error FROM sync_outbox_items WHERE last_error IS NOT NULL
                 ORDER BY updated_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();
        Ok((pending, error))
    }

    pub fn state_for(conn: &Connection, record_id: RecordId) -> Result<Option<UploadState>> {
        let value = conn
            .query_row(
                "SELECT state FROM sync_outbox_items WHERE record_id = ?1 AND kind = 'dictation'",
                [record_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .ok();
        Ok(value.and_then(|state| match state.as_str() {
            "pending" => Some(UploadState::Pending),
            "uploading" => Some(UploadState::Uploading),
            "synced" => Some(UploadState::Synced),
            "needs_attention" => Some(UploadState::NeedsAttention),
            _ => None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::protocol::{DictationPayload, RecordKind};
    use audetic_core::sync::DeviceId;

    fn snapshot(record_id: RecordId, version: u64, text: &str) -> DictationSnapshot {
        DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id,
            origin_device_id: DeviceId::new(),
            local_version: version,
            created_at: "2026-09-04T10:00:00Z".into(),
            updated_at: "2026-09-04T10:00:00Z".into(),
            payload: DictationPayload { text: text.into() },
        }
    }

    #[test]
    fn expired_uploading_lease_is_recovered_after_restart() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let tx = conn.transaction().unwrap();
        let snapshot = snapshot(RecordId::new(), 1, "recover me");
        SyncOutboxRepository::enqueue_snapshot(&tx, &snapshot).unwrap();
        tx.commit().unwrap();
        let first = SyncOutboxRepository::claim_items(
            &mut conn,
            "worker-one",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:01Z",
            25,
        )
        .unwrap();
        assert_eq!(first.len(), 1);
        let recovered = SyncOutboxRepository::claim_items(
            &mut conn,
            "worker-two",
            "2026-09-04T10:00:02Z",
            "2026-09-04T10:00:32Z",
            25,
        )
        .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].attempts, 2);
        assert_eq!(recovered[0].lease_owner, "worker-two");
    }

    #[test]
    fn stale_and_conflicting_same_version_enqueues_preserve_newer_durable_work() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let record_id = RecordId::new();
        let origin = DeviceId::new();
        let mut newer = snapshot(record_id, 2, "newer");
        newer.origin_device_id = origin;
        let mut stale = snapshot(record_id, 1, "stale");
        stale.origin_device_id = origin;
        let mut conflicting = snapshot(record_id, 2, "conflicting");
        conflicting.origin_device_id = origin;

        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &newer).unwrap();
        tx.commit().unwrap();
        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &stale).unwrap();
        tx.commit().unwrap();

        let stored: (u64, String, String) = conn
            .query_row(
                "SELECT local_version, snapshot_json, state FROM sync_outbox_items",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored.0, 2);
        assert_eq!(
            serde_json::from_str::<DictationSnapshot>(&stored.1).unwrap(),
            newer
        );
        assert_eq!(stored.2, "pending");

        let tx = conn.transaction().unwrap();
        assert!(SyncOutboxRepository::enqueue_snapshot(&tx, &conflicting).is_err());
        drop(tx);
        let json: String = conn
            .query_row("SELECT snapshot_json FROM sync_outbox_items", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<DictationSnapshot>(&json).unwrap(),
            newer
        );
    }

    #[test]
    fn older_in_flight_results_cannot_change_a_newer_enqueued_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let record_id = RecordId::new();
        let origin = DeviceId::new();
        let mut first = snapshot(record_id, 1, "first");
        first.origin_device_id = origin;
        let mut second = snapshot(record_id, 2, "second");
        second.origin_device_id = origin;
        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &first).unwrap();
        tx.commit().unwrap();
        let claimed = SyncOutboxRepository::claim_items(
            &mut conn,
            "old-worker",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:30Z",
            1,
        )
        .unwrap()
        .pop()
        .unwrap();

        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &second).unwrap();
        tx.commit().unwrap();
        SyncOutboxRepository::mark_snapshot_accepted(&conn, &claimed, 1).unwrap();
        SyncOutboxRepository::mark_retry(&conn, &claimed, "2026-09-04T11:00:00Z", "stale retry")
            .unwrap();
        SyncOutboxRepository::mark_needs_attention(&conn, &claimed, "stale rejection").unwrap();

        let stored: (u64, String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT local_version, state, lease_owner, last_error FROM sync_outbox_items",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(stored, (2, "pending".into(), None, None));
    }

    #[test]
    fn identical_same_version_enqueue_preserves_an_active_lease() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let item = snapshot(RecordId::new(), 1, "same");
        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &item).unwrap();
        tx.commit().unwrap();
        SyncOutboxRepository::claim_items(
            &mut conn,
            "worker",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:30Z",
            1,
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &item).unwrap();
        tx.commit().unwrap();

        let state: (String, Option<String>) = conn
            .query_row(
                "SELECT state, lease_owner FROM sync_outbox_items",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, ("uploading".into(), Some("worker".into())));
    }
}
