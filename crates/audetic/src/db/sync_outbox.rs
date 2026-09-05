use anyhow::{bail, Context, Result};
use audetic_core::sync::{DeviceId, RecordId, UploadState};
use rusqlite::{params, Connection, OptionalExtension};

use crate::sync::protocol::{RecordKind, RecordingPayloadDescriptor, Snapshot};

#[derive(Clone, Debug)]
pub struct OutboxItem {
    pub record_id: RecordId,
    pub kind: RecordKind,
    pub local_version: u64,
    pub snapshot: Snapshot,
    pub attempts: u32,
    pub lease_owner: String,
}

#[derive(Clone, Debug)]
pub struct OutboxBlob {
    pub record_id: RecordId,
    pub kind: RecordKind,
    pub checksum: String,
    pub staged_path: std::path::PathBuf,
    pub byte_size: u64,
    pub media_type: String,
    pub attempts: u32,
    pub lease_owner: String,
}

pub struct SyncOutboxRepository;

impl SyncOutboxRepository {
    pub fn enqueue_snapshot(tx: &Connection, snapshot: &Snapshot) -> Result<()> {
        let json = serde_json::to_string(snapshot).context("serializing snapshot")?;
        let kind = kind_name(snapshot.kind());
        let changed = tx
            .execute(
                "INSERT INTO sync_outbox_items
                 (record_id, kind, local_version, snapshot_json, state)
               VALUES (?1, ?2, ?3, ?4, 'pending')
              ON CONFLICT(record_id, kind) DO UPDATE SET
                 local_version = excluded.local_version,
                 snapshot_json = excluded.snapshot_json,
                 state = 'pending', accepted_hub_revision = NULL, attempts = 0,
                 lease_owner = NULL, lease_expires_at = NULL, next_attempt_at = NULL,
                 last_error = NULL, updated_at = CURRENT_TIMESTAMP
              WHERE excluded.local_version > sync_outbox_items.local_version",
                params![
                    snapshot.record_id().to_string(),
                    kind,
                    snapshot.local_version(),
                    &json
                ],
            )
            .context("enqueueing sync snapshot")?;
        if changed == 0 {
            let (version, existing_json) = tx
                .query_row(
                    "SELECT local_version, snapshot_json FROM sync_outbox_items
                     WHERE record_id = ?1 AND kind = ?2",
                    params![snapshot.record_id().to_string(), kind],
                    |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
                )
                .context("reading unchanged sync outbox snapshot")?;
            if snapshot.local_version() == version && json != existing_json {
                let existing: Snapshot = serde_json::from_str(&existing_json)?;
                if same_snapshot_except_recording_payload(&existing, snapshot)? {
                    tx.execute(
                        "UPDATE sync_outbox_items SET snapshot_json=?3,state='pending',
                         accepted_hub_revision=NULL,attempts=0,lease_owner=NULL,lease_expires_at=NULL,
                         next_attempt_at=NULL,last_error=NULL,updated_at=CURRENT_TIMESTAMP
                         WHERE record_id=?1 AND kind=?2 AND local_version=?4",
                        params![snapshot.record_id().to_string(),kind,json,snapshot.local_version()],
                    )?;
                } else {
                    bail!(
                        "{} {} local version {} conflicts with its durable outbox snapshot",
                        kind,
                        snapshot.record_id(),
                        snapshot.local_version()
                    );
                }
            }
        }
        Ok(())
    }

    pub fn enqueue_blob(
        tx: &Connection,
        record_id: RecordId,
        kind: RecordKind,
        descriptor: &RecordingPayloadDescriptor,
        staged_path: Option<&std::path::Path>,
    ) -> Result<Option<std::path::PathBuf>> {
        if kind == RecordKind::Artifact {
            bail!("artifacts do not have Recording Payloads");
        }
        let previous = tx
            .query_row(
                "SELECT staged_path FROM sync_outbox_blobs WHERE record_id=?1 AND payload_role='recording'",
                [record_id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        if previous.is_some() {
            return Ok(None);
        }
        match descriptor.availability {
            audetic_core::sync::PayloadAvailability::Unavailable => {
                tx.execute(
                    "INSERT INTO sync_outbox_blobs(record_id,kind,availability,state)
                     VALUES(?1,?2,'unavailable','synced')
                     ON CONFLICT(record_id,payload_role) DO UPDATE SET
                        kind=excluded.kind,checksum=NULL,staged_path=NULL,byte_size=NULL,media_type=NULL,
                        availability='unavailable',state='synced',attempts=0,lease_owner=NULL,
                        lease_expires_at=NULL,next_attempt_at=NULL,last_error=NULL,updated_at=CURRENT_TIMESTAMP",
                    params![record_id.to_string(), kind_name(kind)],
                )?;
            }
            _ => {
                let checksum = descriptor
                    .checksum
                    .as_deref()
                    .context("payload checksum is missing")?;
                let byte_size = descriptor
                    .byte_size
                    .context("payload byte size is missing")?;
                let media_type = descriptor
                    .media_type
                    .as_deref()
                    .context("payload media type is missing")?;
                let staged_path = staged_path.context("staged payload path is missing")?;
                tx.execute(
                    "INSERT INTO sync_outbox_blobs(record_id,kind,checksum,staged_path,byte_size,media_type,availability,state)
                     VALUES(?1,?2,?3,?4,?5,?6,'pending','pending')
                     ON CONFLICT(record_id,payload_role) DO UPDATE SET
                        kind=excluded.kind,checksum=excluded.checksum,staged_path=excluded.staged_path,
                        byte_size=excluded.byte_size,media_type=excluded.media_type,availability='pending',
                        state=CASE WHEN sync_outbox_blobs.checksum=excluded.checksum
                                   AND sync_outbox_blobs.state='uploading'
                                   THEN sync_outbox_blobs.state ELSE 'pending' END,
                        attempts=CASE WHEN sync_outbox_blobs.checksum=excluded.checksum
                                      THEN sync_outbox_blobs.attempts ELSE 0 END,
                        lease_owner=CASE WHEN sync_outbox_blobs.checksum=excluded.checksum
                                         AND sync_outbox_blobs.state='uploading'
                                         THEN sync_outbox_blobs.lease_owner ELSE NULL END,
                        lease_expires_at=CASE WHEN sync_outbox_blobs.checksum=excluded.checksum
                                              AND sync_outbox_blobs.state='uploading'
                                              THEN sync_outbox_blobs.lease_expires_at ELSE NULL END,
                        next_attempt_at=NULL,last_error=NULL,updated_at=CURRENT_TIMESTAMP",
                    params![record_id.to_string(),kind_name(kind),checksum,staged_path.to_string_lossy(),byte_size,media_type],
                )?;
            }
        }
        Ok(None)
    }

    pub fn enqueue_blob_staging_failure(
        tx: &Connection,
        record_id: RecordId,
        kind: RecordKind,
        error: &str,
    ) -> Result<()> {
        if kind == RecordKind::Artifact {
            bail!("artifacts do not have Recording Payloads");
        }
        tx.execute(
            "INSERT OR IGNORE INTO sync_outbox_blobs
             (record_id,kind,availability,state,last_error)
             VALUES(?1,?2,'needs_attention','needs_attention',?3)",
            params![record_id.to_string(), kind_name(kind), error],
        )?;
        Ok(())
    }

    pub fn pause_blob_uploads(conn: &Connection) -> Result<usize> {
        conn.execute(
            "UPDATE sync_outbox_blobs SET state='pending',lease_owner=NULL,lease_expires_at=NULL,
                 next_attempt_at=NULL,updated_at=CURRENT_TIMESTAMP
             WHERE state IN ('pending','uploading')",
            [],
        )
        .context("pausing Recording Payload uploads")
    }

    pub fn reset_restageable_for_backfill(conn: &Connection) -> Result<usize> {
        conn.execute(
            "DELETE FROM sync_outbox_blobs
             WHERE (availability='unavailable' AND state='synced')
                OR (availability='needs_attention' AND state='needs_attention'
                    AND checksum IS NULL AND staged_path IS NULL)",
            [],
        )
        .context("resetting restageable payload markers for policy backfill")
    }

    /// Reset authority-scoped acceptance before activating a new destination.
    /// Only snapshots originated by this device are touched.
    /// Durable staged payloads remain referenced and become pending; payloads
    /// without a usable stage are removed with their metadata so the normal
    /// local-record backfill can restage the source or publish it unavailable.
    pub fn reset_for_new_destination(
        tx: &Connection,
        local_device_id: DeviceId,
    ) -> Result<Vec<std::path::PathBuf>> {
        let local_items = {
            let mut statement = tx.prepare(
                "SELECT snapshot_json FROM sync_outbox_items ORDER BY created_at,record_id",
            )?;
            let snapshots = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            snapshots
                .into_iter()
                .map(|json| {
                    serde_json::from_str::<Snapshot>(&json)
                        .context("reading outbox snapshot for destination activation")
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .filter(|snapshot| snapshot.origin_device_id() == local_device_id)
                .collect::<Vec<_>>()
        };

        let mut obsolete_staged_paths = Vec::new();
        for mut snapshot in local_items {
            let record_id = snapshot.record_id();
            let kind = snapshot.kind();
            if kind == RecordKind::Artifact {
                reset_snapshot_for_new_destination(tx, &snapshot)?;
                continue;
            }

            let blob = tx
                .query_row(
                    "SELECT checksum,staged_path,byte_size,media_type FROM sync_outbox_blobs
                     WHERE record_id=?1 AND kind=?2",
                    params![record_id.to_string(), kind_name(kind)],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<u64>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                        ))
                    },
                )
                .optional()?;
            let usable_stage =
                blob.as_ref()
                    .and_then(|(checksum, staged_path, byte_size, media_type)| {
                        let (Some(checksum), Some(staged_path), Some(byte_size), Some(media_type)) =
                            (checksum, staged_path, byte_size, media_type)
                        else {
                            return None;
                        };
                        std::fs::metadata(staged_path)
                            .ok()
                            .filter(std::fs::Metadata::is_file)
                            .map(|_| {
                                (
                                    checksum.clone(),
                                    std::path::PathBuf::from(staged_path),
                                    *byte_size,
                                    media_type.clone(),
                                )
                            })
                    });

            if let Some((checksum, _staged_path, byte_size, media_type)) = usable_stage {
                set_recording_payload(
                    &mut snapshot,
                    RecordingPayloadDescriptor::pending(checksum, byte_size, media_type),
                );
                reset_snapshot_for_new_destination(tx, &snapshot)?;
                tx.execute(
                    "UPDATE sync_outbox_blobs SET state='pending',availability='pending',attempts=0,
                         lease_owner=NULL,lease_expires_at=NULL,next_attempt_at=NULL,last_error=NULL,
                         updated_at=CURRENT_TIMESTAMP WHERE record_id=?1 AND kind=?2",
                    params![record_id.to_string(), kind_name(kind)],
                )?;
            } else {
                if let Some((_, Some(path), _, _)) = blob {
                    obsolete_staged_paths.push(path.into());
                }
                tx.execute(
                    "DELETE FROM sync_outbox_blobs WHERE record_id=?1 AND kind=?2",
                    params![record_id.to_string(), kind_name(kind)],
                )?;
                tx.execute(
                    "DELETE FROM sync_outbox_items WHERE record_id=?1 AND kind=?2",
                    params![record_id.to_string(), kind_name(kind)],
                )?;
            }
        }
        Ok(obsolete_staged_paths)
    }

    pub fn claim_items(
        conn: &mut Connection,
        role_epoch: u64,
        lease_owner: &str,
        now: &str,
        lease_expires_at: &str,
        limit: usize,
    ) -> Result<Vec<OutboxItem>> {
        let tx = conn.transaction().context("starting outbox claim")?;
        if !epoch_is_current(&tx, role_epoch)? {
            tx.commit()?;
            return Ok(Vec::new());
        }
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
                "SELECT record_id, kind FROM sync_outbox_items
                 WHERE state = 'pending'
                   AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
                  ORDER BY CASE kind WHEN 'dictation' THEN 0 WHEN 'meeting' THEN 1 ELSE 2 END,
                           created_at, record_id LIMIT ?2",
            )?;
            let rows = statement
                .query_map(params![now, limit], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let mut claimed = Vec::with_capacity(candidates.len());
        for (record_id, kind) in candidates {
            tx.execute(
                "UPDATE sync_outbox_items SET state = 'uploading', attempts = attempts + 1,
                    lease_owner = ?3, lease_expires_at = ?4, updated_at = CURRENT_TIMESTAMP
                 WHERE record_id = ?1 AND kind = ?2 AND state = 'pending'",
                params![record_id, kind, lease_owner, lease_expires_at],
            )?;
            let item = tx.query_row(
                "SELECT record_id, local_version, snapshot_json, attempts, lease_owner
                 FROM sync_outbox_items WHERE record_id = ?1 AND kind = ?2",
                params![&record_id, &kind],
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
                kind: parse_kind(&kind)?,
                local_version: item.1,
                snapshot: serde_json::from_str(&item.2)?,
                attempts: item.3,
                lease_owner: item.4,
            });
        }
        tx.commit().context("committing outbox claims")?;
        Ok(claimed)
    }

    pub fn claim_blobs(
        conn: &mut Connection,
        role_epoch: u64,
        lease_owner: &str,
        now: &str,
        lease_expires_at: &str,
        limit: usize,
    ) -> Result<Vec<OutboxBlob>> {
        let tx = conn.transaction().context("starting blob outbox claim")?;
        if !epoch_is_current(&tx, role_epoch)? {
            tx.commit()?;
            return Ok(Vec::new());
        }
        tx.execute(
            "UPDATE sync_outbox_blobs SET state='pending',lease_owner=NULL,lease_expires_at=NULL,
                 last_error=COALESCE(last_error,'upload interrupted; retrying'),updated_at=CURRENT_TIMESTAMP
             WHERE state='uploading' AND lease_expires_at <= ?1",
            [now],
        )?;
        let candidates = {
            let mut statement = tx.prepare(
                "SELECT b.record_id,b.kind FROM sync_outbox_blobs b
                 WHERE b.state='pending' AND (b.next_attempt_at IS NULL OR b.next_attempt_at <= ?1)
                   AND EXISTS(SELECT 1 FROM sync_outbox_items i WHERE i.record_id=b.record_id
                              AND i.kind=b.kind AND i.state='synced')
                 ORDER BY b.created_at,b.record_id LIMIT ?2",
            )?;
            let rows = statement
                .query_map(params![now, limit], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        let mut claimed = Vec::with_capacity(candidates.len());
        for (record_id, kind) in candidates {
            tx.execute(
                "UPDATE sync_outbox_blobs SET state='uploading',attempts=attempts+1,
                    lease_owner=?3,lease_expires_at=?4,updated_at=CURRENT_TIMESTAMP
                 WHERE record_id=?1 AND kind=?2 AND state='pending'",
                params![record_id, kind, lease_owner, lease_expires_at],
            )?;
            let row = tx.query_row(
                "SELECT checksum,staged_path,byte_size,media_type,attempts,lease_owner
                 FROM sync_outbox_blobs WHERE record_id=?1 AND kind=?2",
                params![record_id, kind],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, u32>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )?;
            claimed.push(OutboxBlob {
                record_id: record_id.parse().map_err(anyhow::Error::msg)?,
                kind: parse_kind(&kind)?,
                checksum: row.0,
                staged_path: row.1.into(),
                byte_size: row.2,
                media_type: row.3,
                attempts: row.4,
                lease_owner: row.5,
            });
        }
        tx.commit()?;
        Ok(claimed)
    }

    pub fn mark_snapshot_accepted(
        conn: &Connection,
        role_epoch: u64,
        item: &OutboxItem,
        revision: u64,
    ) -> Result<()> {
        conn.execute(
            "UPDATE sync_outbox_items SET state = 'synced', accepted_hub_revision = ?3,
                lease_owner = NULL, lease_expires_at = NULL, next_attempt_at = NULL,
                last_error = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE record_id = ?1 AND kind = ?5 AND local_version = ?2
                 AND lease_owner = ?4
                 AND EXISTS(SELECT 1 FROM sync_settings WHERE singleton=1 AND role_epoch=?6)",
            params![
                item.record_id.to_string(),
                item.local_version,
                revision,
                item.lease_owner,
                kind_name(item.kind),
                role_epoch,
            ],
        )?;
        Ok(())
    }

    pub fn mark_retry(
        conn: &Connection,
        role_epoch: u64,
        item: &OutboxItem,
        next: &str,
        error: &str,
    ) -> Result<()> {
        conn.execute(
            "UPDATE sync_outbox_items SET state = 'pending', lease_owner = NULL,
                lease_expires_at = NULL, next_attempt_at = ?4, last_error = ?5,
                updated_at = CURRENT_TIMESTAMP
             WHERE record_id = ?1 AND kind = ?6 AND local_version = ?2
                 AND lease_owner = ?3
                 AND EXISTS(SELECT 1 FROM sync_settings WHERE singleton=1 AND role_epoch=?7)",
            params![
                item.record_id.to_string(),
                item.local_version,
                item.lease_owner,
                next,
                error,
                kind_name(item.kind),
                role_epoch,
            ],
        )?;
        Ok(())
    }

    pub fn mark_needs_attention(
        conn: &Connection,
        role_epoch: u64,
        item: &OutboxItem,
        error: &str,
    ) -> Result<()> {
        conn.execute(
            "UPDATE sync_outbox_items SET state = 'needs_attention', lease_owner = NULL,
                 lease_expires_at = NULL, next_attempt_at = NULL, last_error = ?4,
                 updated_at = CURRENT_TIMESTAMP
             WHERE record_id = ?1 AND kind = ?5 AND local_version = ?2
                 AND lease_owner = ?3
                 AND EXISTS(SELECT 1 FROM sync_settings WHERE singleton=1 AND role_epoch=?6)",
            params![
                item.record_id.to_string(),
                item.local_version,
                item.lease_owner,
                error,
                kind_name(item.kind),
                role_epoch,
            ],
        )?;
        Ok(())
    }

    pub fn mark_blob_accepted(conn: &Connection, role_epoch: u64, blob: &OutboxBlob) -> Result<()> {
        let changed = conn.execute(
            "UPDATE sync_outbox_blobs SET state='synced',availability='available',lease_owner=NULL,
                 lease_expires_at=NULL,next_attempt_at=NULL,last_error=NULL,staged_path=NULL,
                 updated_at=CURRENT_TIMESTAMP
              WHERE record_id=?1 AND kind=?2 AND checksum=?3 AND lease_owner=?4
                AND EXISTS(SELECT 1 FROM sync_settings WHERE singleton=1 AND role_epoch=?5)",
            params![
                blob.record_id.to_string(),
                kind_name(blob.kind),
                blob.checksum,
                blob.lease_owner,
                role_epoch,
            ],
        )?;
        if changed == 1 {
            Self::reclaim_staged_paths(conn, std::slice::from_ref(&blob.staged_path))?;
        }
        Ok(())
    }

    pub fn mark_blob_retry(
        conn: &Connection,
        role_epoch: u64,
        blob: &OutboxBlob,
        next: &str,
        error: &str,
    ) -> Result<()> {
        conn.execute(
            "UPDATE sync_outbox_blobs SET state='pending',lease_owner=NULL,lease_expires_at=NULL,
                 next_attempt_at=?5,last_error=?6,updated_at=CURRENT_TIMESTAMP
              WHERE record_id=?1 AND kind=?2 AND checksum=?3 AND lease_owner=?4
                AND EXISTS(SELECT 1 FROM sync_settings WHERE singleton=1 AND role_epoch=?7)",
            params![
                blob.record_id.to_string(),
                kind_name(blob.kind),
                blob.checksum,
                blob.lease_owner,
                next,
                error,
                role_epoch,
            ],
        )?;
        Ok(())
    }

    pub fn mark_blob_needs_attention(
        conn: &Connection,
        role_epoch: u64,
        blob: &OutboxBlob,
        error: &str,
    ) -> Result<()> {
        conn.execute(
            "UPDATE sync_outbox_blobs SET state='needs_attention',availability='needs_attention',
                 lease_owner=NULL,lease_expires_at=NULL,next_attempt_at=NULL,last_error=?5,
                 updated_at=CURRENT_TIMESTAMP
              WHERE record_id=?1 AND kind=?2 AND checksum=?3 AND lease_owner=?4
                AND EXISTS(SELECT 1 FROM sync_settings WHERE singleton=1 AND role_epoch=?6)",
            params![
                blob.record_id.to_string(),
                kind_name(blob.kind),
                blob.checksum,
                blob.lease_owner,
                error,
                role_epoch,
            ],
        )?;
        Ok(())
    }

    pub fn retry_all(conn: &Connection) -> Result<usize> {
        let transaction = conn
            .unchecked_transaction()
            .context("starting outbox retry transaction")?;
        let restageable = Self::reset_restageable_for_backfill(&transaction)?;
        let items = transaction
            .execute(
                "UPDATE sync_outbox_items SET state = 'pending', next_attempt_at = NULL,
                lease_owner = NULL, lease_expires_at = NULL, last_error = NULL,
                updated_at = CURRENT_TIMESTAMP
             WHERE state IN ('pending', 'uploading', 'needs_attention')",
                [],
            )
            .context("resetting outbox items")?;
        let blobs = transaction.execute(
            "UPDATE sync_outbox_blobs SET state='pending',availability='pending',next_attempt_at=NULL,
                 lease_owner=NULL,lease_expires_at=NULL,last_error=NULL,updated_at=CURRENT_TIMESTAMP
             WHERE state IN ('pending','uploading','needs_attention') AND staged_path IS NOT NULL",
            [],
        ).context("resetting outbox blobs")?;
        transaction
            .commit()
            .context("committing outbox retry transaction")?;
        Ok(restageable + items + blobs)
    }

    /// Remove both metadata and Recording Payload work for one local record.
    /// Callers may include this in a wider local-delete transaction, then pass
    /// the returned paths to `reclaim_staged_paths` after commit.
    pub fn remove_record_state(
        tx: &Connection,
        record_id: RecordId,
        kind: RecordKind,
    ) -> Result<Vec<std::path::PathBuf>> {
        let paths = {
            let mut statement = tx.prepare(
                "SELECT staged_path FROM sync_outbox_blobs
                 WHERE record_id=?1 AND kind=?2 AND staged_path IS NOT NULL",
            )?;
            let values = statement
                .query_map(params![record_id.to_string(), kind_name(kind)], |row| {
                    row.get::<_, String>(0).map(Into::into)
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            values
        };
        tx.execute(
            "DELETE FROM sync_outbox_blobs WHERE record_id=?1 AND kind=?2",
            params![record_id.to_string(), kind_name(kind)],
        )?;
        tx.execute(
            "DELETE FROM sync_outbox_items WHERE record_id=?1 AND kind=?2",
            params![record_id.to_string(), kind_name(kind)],
        )?;
        Ok(paths)
    }

    /// Reclaim finalized staging files while holding the same namespace lock
    /// used by staging. The reference query is deliberately the final action
    /// before unlink so a checksum path cannot be reused in the gap.
    pub fn reclaim_staged_paths(conn: &Connection, paths: &[std::path::PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let db_path = crate::db::operations::database_path(conn)?;
        let _staging_lock = crate::sync::payload::lock_staging_for_db(&db_path)?;
        Self::reclaim_staged_paths_locked(conn, paths)
    }

    pub(crate) fn reclaim_staged_paths_for_epoch(
        conn: &Connection,
        role_epoch: u64,
        paths: &[std::path::PathBuf],
    ) -> Result<bool> {
        if paths.is_empty() {
            return Ok(true);
        }
        let db_path = crate::db::operations::database_path(conn)?;
        let _staging_lock = crate::sync::payload::lock_staging_for_db(&db_path)?;
        let transaction = conn.unchecked_transaction()?;
        if !epoch_is_current(&transaction, role_epoch)? {
            transaction.commit()?;
            return Ok(false);
        }
        Self::reclaim_staged_paths_locked(&transaction, paths)?;
        transaction.commit()?;
        Ok(true)
    }

    fn reclaim_staged_paths_locked(conn: &Connection, paths: &[std::path::PathBuf]) -> Result<()> {
        let mut unique = std::collections::BTreeSet::new();
        for path in paths {
            if !unique.insert(path) {
                continue;
            }
            let referenced: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sync_outbox_blobs WHERE staged_path=?1)",
                [path.to_string_lossy().as_ref()],
                |row| row.get(0),
            )?;
            if referenced {
                continue;
            }
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "failed to reclaim Recording Payload staging file");
                }
            }
        }
        Ok(())
    }

    pub fn release_worker_leases(
        conn: &Connection,
        role_epoch: u64,
        worker_id: &str,
    ) -> Result<()> {
        let transaction = conn.unchecked_transaction()?;
        transaction.execute(
            "UPDATE sync_outbox_items SET state='pending',lease_owner=NULL,lease_expires_at=NULL,
                 next_attempt_at=NULL,last_error=COALESCE(last_error,'upload cancelled'),
                  updated_at=CURRENT_TIMESTAMP WHERE state='uploading' AND lease_owner=?1
                  AND EXISTS(SELECT 1 FROM sync_settings WHERE singleton=1 AND role_epoch=?2)",
            rusqlite::params![worker_id, role_epoch],
        )?;
        transaction.execute(
            "UPDATE sync_outbox_blobs SET state='pending',lease_owner=NULL,lease_expires_at=NULL,
                 next_attempt_at=NULL,last_error=COALESCE(last_error,'upload cancelled'),
                  updated_at=CURRENT_TIMESTAMP WHERE state='uploading' AND lease_owner=?1
                  AND EXISTS(SELECT 1 FROM sync_settings WHERE singleton=1 AND role_epoch=?2)",
            rusqlite::params![worker_id, role_epoch],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn counts(conn: &Connection) -> Result<(u64, Option<String>)> {
        let pending = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM sync_outbox_items WHERE state != 'synced') +
                    (SELECT COUNT(*) FROM sync_outbox_blobs WHERE state != 'synced')",
            [],
            |row| row.get(0),
        )?;
        let error = conn
            .query_row(
                "SELECT last_error FROM (
                    SELECT last_error,updated_at FROM sync_outbox_items WHERE last_error IS NOT NULL
                    UNION ALL SELECT last_error,updated_at FROM sync_outbox_blobs WHERE last_error IS NOT NULL
                 ) ORDER BY updated_at DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .ok();
        Ok((pending, error))
    }

    pub fn pending_bytes(conn: &Connection) -> Result<u64> {
        conn.query_row(
            "SELECT COALESCE(SUM(byte_size),0) FROM sync_outbox_blobs WHERE state!='synced'",
            [],
            |row| row.get(0),
        )
        .context("counting pending Recording Payload bytes")
    }

    pub fn payload_availability(
        conn: &Connection,
        record_id: RecordId,
    ) -> Result<Option<audetic_core::sync::PayloadAvailability>> {
        let value = conn
            .query_row(
                "SELECT availability FROM sync_outbox_blobs WHERE record_id=?1 AND payload_role='recording'",
                [record_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.and_then(|value| match value.as_str() {
            "pending" => Some(audetic_core::sync::PayloadAvailability::Pending),
            "available" => Some(audetic_core::sync::PayloadAvailability::Available),
            "unavailable" => Some(audetic_core::sync::PayloadAvailability::Unavailable),
            "needs_attention" => Some(audetic_core::sync::PayloadAvailability::NeedsAttention),
            _ => None,
        }))
    }

    pub fn payload_descriptor(
        conn: &Connection,
        record_id: RecordId,
    ) -> Result<Option<RecordingPayloadDescriptor>> {
        let value = conn
            .query_row(
                "SELECT checksum,byte_size,media_type,availability FROM sync_outbox_blobs
                 WHERE record_id=?1 AND payload_role='recording'",
                [record_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<u64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        value
            .map(|(checksum, byte_size, media_type, availability)| {
                let availability = match availability.as_str() {
                    "pending" => audetic_core::sync::PayloadAvailability::Pending,
                    "available" => audetic_core::sync::PayloadAvailability::Available,
                    "needs_attention" if checksum.is_some() => {
                        audetic_core::sync::PayloadAvailability::Pending
                    }
                    "needs_attention" => audetic_core::sync::PayloadAvailability::Unavailable,
                    "unavailable" => audetic_core::sync::PayloadAvailability::Unavailable,
                    _ => bail!("invalid outbox payload availability {availability}"),
                };
                Ok(RecordingPayloadDescriptor {
                    checksum,
                    byte_size,
                    media_type,
                    availability,
                })
            })
            .transpose()
    }

    pub fn state_for(conn: &Connection, record_id: RecordId) -> Result<Option<UploadState>> {
        Self::state_for_kind(conn, record_id, RecordKind::Dictation)
    }

    pub fn state_for_kind(
        conn: &Connection,
        record_id: RecordId,
        kind: RecordKind,
    ) -> Result<Option<UploadState>> {
        let value = conn
            .query_row(
                "SELECT state FROM sync_outbox_items WHERE record_id = ?1 AND kind = ?2",
                params![record_id.to_string(), kind_name(kind)],
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

    pub fn deletion_masks(
        conn: &Connection,
        record_id: RecordId,
        kind: RecordKind,
    ) -> Result<bool> {
        let snapshot = conn
            .query_row(
                "SELECT snapshot_json FROM sync_outbox_items WHERE record_id=?1 AND kind=?2",
                params![record_id.to_string(), kind_name(kind)],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        snapshot
            .map(|json| serde_json::from_str::<Snapshot>(&json))
            .transpose()
            .map(|snapshot| matches!(snapshot, Some(Snapshot::Delete(_))))
            .context("reading deletion mask from sync outbox")
    }

    /// Whether an upload attempt could have committed on the Home Hub even if
    /// this device did not receive the acceptance response. Deleting such a
    /// record locally is unsafe: an in-flight or previously interrupted upload
    /// could otherwise make the supposedly deleted record reappear remotely.
    pub fn may_have_reached_hub(
        conn: &Connection,
        record_id: RecordId,
        kind: RecordKind,
    ) -> Result<bool> {
        conn.query_row(
            "SELECT attempts > 0 OR accepted_hub_revision IS NOT NULL OR state = 'synced' \
             FROM sync_outbox_items WHERE record_id = ?1 AND kind = ?2",
            params![record_id.to_string(), kind_name(kind)],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .context("checking whether a sync record may have reached the Home Hub")
    }
}

fn epoch_is_current(conn: &Connection, role_epoch: u64) -> Result<bool> {
    conn.execute(
        "INSERT OR IGNORE INTO sync_settings(singleton) VALUES(1)",
        [],
    )
    .context("materializing sync state for outbox epoch check")?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_settings WHERE singleton=1 AND role_epoch=?1)",
        [role_epoch],
        |row| row.get(0),
    )
    .context("checking outbox worker role epoch")
}

fn same_snapshot_except_recording_payload(left: &Snapshot, right: &Snapshot) -> Result<bool> {
    let mut left = left.clone();
    let mut right = right.clone();
    for snapshot in [&mut left, &mut right] {
        match snapshot {
            Snapshot::Dictation(value) => value.payload.recording_payload = Default::default(),
            Snapshot::Meeting(value) => value.payload.recording_payload = Default::default(),
            Snapshot::Artifact(_) => {}
            Snapshot::Delete(_) => {}
        }
    }
    Ok(serde_json::to_value(left)? == serde_json::to_value(right)?)
}

fn reset_snapshot_for_new_destination(conn: &Connection, snapshot: &Snapshot) -> Result<()> {
    conn.execute(
        "UPDATE sync_outbox_items SET snapshot_json=?3,state='pending',accepted_hub_revision=NULL,
             attempts=0,lease_owner=NULL,lease_expires_at=NULL,next_attempt_at=NULL,last_error=NULL,
             updated_at=CURRENT_TIMESTAMP WHERE record_id=?1 AND kind=?2",
        params![
            snapshot.record_id().to_string(),
            kind_name(snapshot.kind()),
            serde_json::to_string(snapshot)?
        ],
    )?;
    Ok(())
}

fn set_recording_payload(snapshot: &mut Snapshot, descriptor: RecordingPayloadDescriptor) {
    match snapshot {
        Snapshot::Dictation(value) => value.payload.recording_payload = descriptor,
        Snapshot::Meeting(value) => value.payload.recording_payload = descriptor,
        Snapshot::Artifact(_) => {}
        Snapshot::Delete(_) => {}
    }
}

pub const fn kind_name(kind: RecordKind) -> &'static str {
    match kind {
        RecordKind::Dictation => "dictation",
        RecordKind::Meeting => "meeting",
        RecordKind::Artifact => "artifact",
    }
}

pub(crate) fn parse_kind(value: &str) -> Result<RecordKind> {
    match value {
        "dictation" => Ok(RecordKind::Dictation),
        "meeting" => Ok(RecordKind::Meeting),
        "artifact" => Ok(RecordKind::Artifact),
        _ => bail!("unknown outbox record kind {value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::protocol::{DictationPayload, DictationSnapshot, RecordKind};
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
            payload: DictationPayload {
                text: text.into(),
                recording_payload: Default::default(),
            },
        }
    }

    #[test]
    fn expired_uploading_lease_is_recovered_after_restart() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let tx = conn.transaction().unwrap();
        let snapshot = snapshot(RecordId::new(), 1, "recover me");
        SyncOutboxRepository::enqueue_snapshot(&tx, &snapshot.into()).unwrap();
        tx.commit().unwrap();
        let first = SyncOutboxRepository::claim_items(
            &mut conn,
            0,
            "worker-one",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:01Z",
            25,
        )
        .unwrap();
        assert_eq!(first.len(), 1);
        let recovered = SyncOutboxRepository::claim_items(
            &mut conn,
            0,
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
        SyncOutboxRepository::enqueue_snapshot(&tx, &newer.clone().into()).unwrap();
        tx.commit().unwrap();
        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &stale.into()).unwrap();
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
            match serde_json::from_str::<Snapshot>(&stored.1).unwrap() {
                Snapshot::Dictation(value) => value,
                _ => panic!("wrong kind"),
            },
            newer
        );
        assert_eq!(stored.2, "pending");

        let tx = conn.transaction().unwrap();
        assert!(SyncOutboxRepository::enqueue_snapshot(&tx, &conflicting.into()).is_err());
        drop(tx);
        let json: String = conn
            .query_row("SELECT snapshot_json FROM sync_outbox_items", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            match serde_json::from_str::<Snapshot>(&json).unwrap() {
                Snapshot::Dictation(value) => value,
                _ => panic!("wrong kind"),
            },
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
        SyncOutboxRepository::enqueue_snapshot(&tx, &first.into()).unwrap();
        tx.commit().unwrap();
        let claimed = SyncOutboxRepository::claim_items(
            &mut conn,
            0,
            "old-worker",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:30Z",
            1,
        )
        .unwrap()
        .pop()
        .unwrap();

        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &second.into()).unwrap();
        tx.commit().unwrap();
        SyncOutboxRepository::mark_snapshot_accepted(&conn, 0, &claimed, 1).unwrap();
        SyncOutboxRepository::mark_retry(&conn, 0, &claimed, "2026-09-04T11:00:00Z", "stale retry")
            .unwrap();
        SyncOutboxRepository::mark_needs_attention(&conn, 0, &claimed, "stale rejection").unwrap();

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
    fn stale_role_epoch_cannot_claim_or_commit_worker_progress() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let item = snapshot(RecordId::new(), 1, "old authority");
        SyncOutboxRepository::enqueue_snapshot(&conn, &item.into()).unwrap();
        let claimed = SyncOutboxRepository::claim_items(
            &mut conn,
            0,
            "old-worker",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:30Z",
            1,
        )
        .unwrap()
        .pop()
        .unwrap();
        conn.execute("UPDATE sync_settings SET role_epoch=1", [])
            .unwrap();

        SyncOutboxRepository::mark_snapshot_accepted(&conn, 0, &claimed, 99).unwrap();
        assert!(SyncOutboxRepository::claim_items(
            &mut conn,
            0,
            "another-old-worker",
            "2026-09-04T11:00:00Z",
            "2026-09-04T11:00:30Z",
            1,
        )
        .unwrap()
        .is_empty());
        let stored: (String, Option<u64>, Option<String>) = conn
            .query_row(
                "SELECT state,accepted_hub_revision,lease_owner FROM sync_outbox_items",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            stored,
            ("uploading".into(), None, Some("old-worker".into()))
        );
    }

    #[test]
    fn identical_same_version_enqueue_preserves_an_active_lease() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let item = snapshot(RecordId::new(), 1, "same");
        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &item.clone().into()).unwrap();
        tx.commit().unwrap();
        SyncOutboxRepository::claim_items(
            &mut conn,
            0,
            "worker",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:30Z",
            1,
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        SyncOutboxRepository::enqueue_snapshot(&tx, &item.into()).unwrap();
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

    #[test]
    fn deletion_requires_hub_after_an_upload_attempt_may_have_reached_it() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let item = snapshot(RecordId::new(), 1, "delete race");
        SyncOutboxRepository::enqueue_snapshot(&conn, &item.clone().into()).unwrap();
        assert!(!SyncOutboxRepository::may_have_reached_hub(
            &conn,
            item.record_id,
            RecordKind::Dictation,
        )
        .unwrap());

        let claimed = SyncOutboxRepository::claim_items(
            &mut conn,
            0,
            "worker",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:30Z",
            1,
        )
        .unwrap()
        .pop()
        .unwrap();
        assert!(SyncOutboxRepository::may_have_reached_hub(
            &conn,
            item.record_id,
            RecordKind::Dictation,
        )
        .unwrap());

        SyncOutboxRepository::mark_retry(
            &conn,
            0,
            &claimed,
            "2026-09-04T11:00:00Z",
            "response lost",
        )
        .unwrap();
        assert!(SyncOutboxRepository::may_have_reached_hub(
            &conn,
            item.record_id,
            RecordKind::Dictation,
        )
        .unwrap());
    }

    #[test]
    fn expired_blob_claim_is_recovered_independently_from_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.bin");
        std::fs::write(&path, b"payload").unwrap();
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let item = snapshot(RecordId::new(), 1, "blob");
        SyncOutboxRepository::enqueue_snapshot(&conn, &item.clone().into()).unwrap();
        conn.execute("UPDATE sync_outbox_items SET state='synced'", [])
            .unwrap();
        let descriptor = RecordingPayloadDescriptor::pending(
            "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".into(),
            7,
            "application/octet-stream".into(),
        );
        SyncOutboxRepository::enqueue_blob(
            &conn,
            item.record_id,
            RecordKind::Dictation,
            &descriptor,
            Some(&path),
        )
        .unwrap();

        let first = SyncOutboxRepository::claim_blobs(
            &mut conn,
            0,
            "one",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:01Z",
            1,
        )
        .unwrap();
        assert_eq!(first.len(), 1);
        let recovered = SyncOutboxRepository::claim_blobs(
            &mut conn,
            0,
            "two",
            "2026-09-04T10:00:02Z",
            "2026-09-04T10:00:32Z",
            1,
        )
        .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].attempts, 2);
        assert_eq!(recovered[0].lease_owner, "two");
    }

    #[test]
    fn accepted_deduplicated_staging_is_released_after_its_last_reference() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("audetic.db");
        let path = crate::sync::payload::staging_root_for_db(&db_path).join("payload.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"payload").unwrap();
        let mut conn = crate::db::migrate_db_at(&db_path).unwrap();
        let descriptor = RecordingPayloadDescriptor::pending(
            "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".into(),
            7,
            "application/octet-stream".into(),
        );
        for text in ["one", "two"] {
            let item = snapshot(RecordId::new(), 1, text);
            SyncOutboxRepository::enqueue_snapshot(&conn, &item.clone().into()).unwrap();
            SyncOutboxRepository::enqueue_blob(
                &conn,
                item.record_id,
                RecordKind::Dictation,
                &descriptor,
                Some(&path),
            )
            .unwrap();
        }
        conn.execute("UPDATE sync_outbox_items SET state='synced'", [])
            .unwrap();
        let claimed = SyncOutboxRepository::claim_blobs(
            &mut conn,
            0,
            "worker",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:30Z",
            2,
        )
        .unwrap();
        assert_eq!(claimed.len(), 2);
        SyncOutboxRepository::mark_blob_accepted(&conn, 0, &claimed[0]).unwrap();
        assert!(path.exists());
        SyncOutboxRepository::mark_blob_accepted(&conn, 0, &claimed[1]).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn record_removal_never_unlinks_staging_still_referenced_by_another_record() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("audetic.db");
        let path = crate::sync::payload::staging_root_for_db(&db_path).join("payload.bin");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"payload").unwrap();
        let conn = crate::db::migrate_db_at(&db_path).unwrap();
        let descriptor = RecordingPayloadDescriptor::pending(
            "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".into(),
            7,
            "application/octet-stream".into(),
        );
        let ids = [RecordId::new(), RecordId::new()];
        for id in ids {
            SyncOutboxRepository::enqueue_blob(
                &conn,
                id,
                RecordKind::Dictation,
                &descriptor,
                Some(&path),
            )
            .unwrap();
        }

        let transaction = conn.unchecked_transaction().unwrap();
        let first =
            SyncOutboxRepository::remove_record_state(&transaction, ids[0], RecordKind::Dictation)
                .unwrap();
        transaction.commit().unwrap();
        SyncOutboxRepository::reclaim_staged_paths(&conn, &first).unwrap();
        assert!(path.exists());

        let transaction = conn.unchecked_transaction().unwrap();
        let second =
            SyncOutboxRepository::remove_record_state(&transaction, ids[1], RecordKind::Dictation)
                .unwrap();
        transaction.commit().unwrap();
        SyncOutboxRepository::reclaim_staged_paths(&conn, &second).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn backfill_enqueue_preserves_existing_blob_transfer_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("payload.bin");
        std::fs::write(&path, b"payload").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let record_id = RecordId::new();
        let descriptor = RecordingPayloadDescriptor::pending(
            "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".into(),
            7,
            "application/octet-stream".into(),
        );
        SyncOutboxRepository::enqueue_blob(
            &conn,
            record_id,
            RecordKind::Dictation,
            &descriptor,
            Some(&path),
        )
        .unwrap();
        conn.execute(
            "UPDATE sync_outbox_blobs SET state='synced',availability='available'",
            [],
        )
        .unwrap();

        SyncOutboxRepository::enqueue_blob(
            &conn,
            record_id,
            RecordKind::Dictation,
            &RecordingPayloadDescriptor::unavailable(),
            None,
        )
        .unwrap();

        let stored: (String, String, Option<String>) = conn
            .query_row(
                "SELECT state,availability,staged_path FROM sync_outbox_blobs",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            stored,
            (
                "synced".into(),
                "available".into(),
                Some(path.to_string_lossy().into_owned())
            )
        );
    }

    #[test]
    fn portable_descriptor_does_not_publish_transfer_needs_attention() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        let record_id = RecordId::new();
        conn.execute(
            "INSERT INTO sync_outbox_blobs
             (record_id,kind,checksum,staged_path,byte_size,media_type,availability,state,last_error)
             VALUES(?1,'meeting',?2,'/missing',7,'audio/wav','needs_attention','needs_attention','lost')",
            params![
                record_id.to_string(),
                "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"
            ],
        )
        .unwrap();

        let descriptor = SyncOutboxRepository::payload_descriptor(&conn, record_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            descriptor.availability,
            audetic_core::sync::PayloadAvailability::Pending
        );
    }
}
