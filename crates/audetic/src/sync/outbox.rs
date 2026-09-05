use anyhow::Result;
use audetic_core::sync::{HubConnection, SyncRole};
use tokio_util::sync::CancellationToken;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::db::sync_outbox::{OutboxBlob, OutboxItem, SyncOutboxRepository};

use super::library::HubLibrary;
use super::protocol::{SnapshotBatch, SnapshotDisposition};
use super::transport::{BlobUpload, HubTransferError, ReplicationTransport};

pub const OUTBOX_BATCH_SIZE: usize = 25;
const MAX_RETRY_DELAY_SECONDS: i64 = 300;
/// Server-requested pauses are honored for at most one hour. This is long
/// enough for normal overload/maintenance windows without allowing a bad Hub
/// response to suppress durable outbox work indefinitely.
const MAX_RETRY_AFTER_SECONDS: i64 = 60 * 60;

type RetryJitter = fn(&OutboxItem) -> u64;
type SchedulingClock = fn() -> chrono::DateTime<chrono::Utc>;

pub struct OutboxWorker {
    db_path: PathBuf,
    destination: OutboxDestination,
    worker_id: String,
    retry_jitter: RetryJitter,
    scheduling_clock: SchedulingClock,
    upload_recording_payloads: bool,
}

pub enum OutboxDestination {
    Local(HubLibrary),
    Remote {
        hub: HubConnection,
        replication: Arc<dyn ReplicationTransport>,
    },
}

impl OutboxDestination {
    const fn role(&self) -> SyncRole {
        match self {
            Self::Local(_) => SyncRole::HomeHub,
            Self::Remote { .. } => SyncRole::ConnectedDevice,
        }
    }
}

impl OutboxWorker {
    pub fn new(db_path: PathBuf, destination: OutboxDestination) -> Self {
        Self {
            db_path,
            destination,
            worker_id: format!("outbox-{}", uuid::Uuid::new_v4()),
            retry_jitter: record_retry_jitter,
            scheduling_clock: chrono::Utc::now,
            upload_recording_payloads: true,
        }
    }

    pub fn with_payload_uploads(mut self, enabled: bool) -> Self {
        self.upload_recording_payloads = enabled;
        self
    }

    #[cfg(test)]
    fn with_retry_jitter(mut self, retry_jitter: RetryJitter) -> Self {
        self.retry_jitter = retry_jitter;
        self
    }

    #[cfg(test)]
    fn with_scheduling_clock(mut self, scheduling_clock: SchedulingClock) -> Self {
        self.scheduling_clock = scheduling_clock;
        self
    }

    pub async fn run(self, cancellation: CancellationToken) {
        let mut backfill_cursor = crate::db::BackfillCursor::default();
        loop {
            if cancellation.is_cancelled() {
                break;
            }
            let db_path = self.db_path.clone();
            let role = self.destination.role();
            let upload_recording_payloads = self.upload_recording_payloads;
            let cancel_backfill = cancellation.clone();
            let mut cursor_for_batch = backfill_cursor.clone();
            let backfill = tokio::task::spawn_blocking(move || {
                let result = crate::db::open_db_at(&db_path).and_then(|connection| {
                    crate::db::backfill_visible_records_batch_cancellable(
                        &connection,
                        role,
                        upload_recording_payloads,
                        OUTBOX_BATCH_SIZE,
                        &mut cursor_for_batch,
                        &cancel_backfill,
                    )
                });
                (cursor_for_batch, result)
            });
            match backfill.await {
                Ok((cursor, Ok(_))) => backfill_cursor = cursor,
                Ok((cursor, Err(error))) => {
                    backfill_cursor = cursor;
                    tracing::warn!(%error, "Shared Library backfill batch failed");
                }
                Err(error) => tracing::warn!(%error, "Shared Library backfill task failed"),
            }
            if cancellation.is_cancelled() {
                break;
            }
            let _ = self.process_once_cancellable(&cancellation).await;
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = cancellation.cancelled() => break,
            }
        }
        match crate::db::open_db_at(&self.db_path) {
            Ok(connection) => {
                if let Err(error) =
                    SyncOutboxRepository::release_worker_leases(&connection, &self.worker_id)
                {
                    tracing::warn!(%error, "failed to release cancelled outbox leases");
                }
            }
            Err(error) => tracing::warn!(%error, "failed to open database while stopping outbox"),
        }
    }

    pub async fn process_once(&self) -> Result<usize> {
        Ok(self
            .process_once_cancellable(&CancellationToken::new())
            .await?
            .unwrap_or(0))
    }

    async fn process_once_cancellable(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Option<usize>> {
        if cancellation.is_cancelled() {
            return Ok(None);
        }
        let now = (self.scheduling_clock)();
        let lease_expiry = now + chrono::Duration::seconds(30);
        let mut connection = crate::db::open_db_at(&self.db_path)?;
        let items = SyncOutboxRepository::claim_items(
            &mut connection,
            &self.worker_id,
            &now.to_rfc3339(),
            &lease_expiry.to_rfc3339(),
            OUTBOX_BATCH_SIZE,
        )?;
        if cancellation.is_cancelled() {
            return Ok(None);
        }
        if !items.is_empty() {
            let batch = SnapshotBatch {
                snapshots: items.iter().map(|item| item.snapshot.clone()).collect(),
            };
            let response = match &self.destination {
                OutboxDestination::Local(library) => library
                    .apply_snapshots(batch.snapshots)
                    .map_err(|error| HubTransferError::Retryable(error.to_string())),
                OutboxDestination::Remote { hub, replication } => {
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Ok(None),
                        response = replication.upload_snapshots(hub, batch) => response,
                    }
                }
            };
            match response {
                Ok(response) => {
                    for item in &items {
                        if cancellation.is_cancelled() {
                            return Ok(None);
                        }
                        let result = response
                            .results
                            .iter()
                            .find(|result| result.record_id == item.record_id);
                        match result {
                            Some(result) if result.disposition == SnapshotDisposition::Accepted => {
                                SyncOutboxRepository::mark_snapshot_accepted(
                                    &connection,
                                    item,
                                    result.authoritative_revision.unwrap_or(0),
                                )?;
                            }
                            Some(result) => SyncOutboxRepository::mark_needs_attention(
                                &connection,
                                item,
                                result.message.as_deref().unwrap_or("snapshot rejected"),
                            )?,
                            None => {
                                let scheduling_now = (self.scheduling_clock)();
                                mark_retry(
                                    &connection,
                                    item,
                                    "Home Hub omitted the snapshot result",
                                    (self.retry_jitter)(item),
                                    &scheduling_now,
                                    None,
                                )?
                            }
                        }
                    }
                }
                Err(error) if error.is_retryable() => {
                    let scheduling_now = (self.scheduling_clock)();
                    let retry_after = error.retry_after();
                    let message = error.to_string();
                    for item in &items {
                        if cancellation.is_cancelled() {
                            return Ok(None);
                        }
                        mark_retry(
                            &connection,
                            item,
                            &message,
                            (self.retry_jitter)(item),
                            &scheduling_now,
                            retry_after,
                        )?;
                    }
                }
                Err(error) => {
                    let error = error.to_string();
                    for item in &items {
                        if cancellation.is_cancelled() {
                            return Ok(None);
                        }
                        SyncOutboxRepository::mark_needs_attention(&connection, item, &error)?;
                    }
                }
            }
        }

        let blobs = if self.upload_recording_payloads {
            SyncOutboxRepository::claim_blobs(
                &mut connection,
                &self.worker_id,
                &now.to_rfc3339(),
                &lease_expiry.to_rfc3339(),
                OUTBOX_BATCH_SIZE,
            )?
        } else {
            Vec::new()
        };
        for blob in &blobs {
            if cancellation.is_cancelled() {
                return Ok(None);
            }
            let metadata = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Ok(None),
                metadata = tokio::fs::metadata(&blob.staged_path) => metadata,
            };
            let staged_size = metadata
                .ok()
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len());
            if staged_size != Some(blob.byte_size) {
                SyncOutboxRepository::mark_blob_needs_attention(
                    &connection,
                    blob,
                    &format!(
                        "staged Recording Payload is missing or has the wrong size; restore it or disable payload upload (expected {}, found {:?})",
                        blob.byte_size, staged_size
                    ),
                )?;
                continue;
            }
            let uploaded = match &self.destination {
                OutboxDestination::Local(library) => tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Ok(None),
                    uploaded = library.accept_blob_file(
                        &blob.staged_path,
                        &blob.checksum,
                        blob.byte_size,
                        &blob.media_type,
                    ) => uploaded
                    .map(|_| ())
                    .map_err(|error| {
                        let message = error.to_string();
                        if message.contains("blob verification failed") {
                            HubTransferError::NeedsAttention(message)
                        } else {
                            HubTransferError::Retryable(message)
                        }
                    }),
                },
                OutboxDestination::Remote { hub, replication } => {
                    let upload = BlobUpload {
                        record_id: blob.record_id,
                        checksum: blob.checksum.clone(),
                        source_path: blob.staged_path.clone(),
                        byte_size: blob.byte_size,
                        media_type: blob.media_type.clone(),
                    };
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Ok(None),
                        uploaded = replication.upload_blob(hub, upload) => uploaded,
                    }
                }
            };
            match uploaded {
                Ok(()) => {
                    SyncOutboxRepository::mark_blob_accepted(&connection, blob)?;
                }
                Err(error) if error.is_retryable() => {
                    let scheduling_now = (self.scheduling_clock)();
                    mark_blob_retry(
                        &connection,
                        blob,
                        &error.to_string(),
                        blob_retry_jitter(blob),
                        &scheduling_now,
                        error.retry_after(),
                    )?;
                }
                Err(error) => {
                    SyncOutboxRepository::mark_blob_needs_attention(
                        &connection,
                        blob,
                        &error.to_string(),
                    )?;
                }
            }
        }
        Ok(Some(items.len() + blobs.len()))
    }
}

fn mark_retry(
    connection: &rusqlite::Connection,
    item: &OutboxItem,
    error: &str,
    jitter: u64,
    now: &chrono::DateTime<chrono::Utc>,
    retry_after: Option<&str>,
) -> Result<()> {
    let next = metadata_retry_at(now, item, jitter, retry_after);
    SyncOutboxRepository::mark_retry(connection, item, &next.to_rfc3339(), error)
}

fn retry_delay_seconds(attempts: u32, jitter: u64) -> i64 {
    let exponent = attempts.saturating_sub(1).min(8);
    let base = 2_i64.pow(exponent);
    let spread = (base / 4).max(1);
    let width = (spread * 2 + 1) as u64;
    let offset = (jitter % width) as i64 - spread;
    (base + offset).clamp(1, MAX_RETRY_DELAY_SECONDS)
}

fn record_retry_jitter(item: &OutboxItem) -> u64 {
    let value = item.record_id.as_uuid().as_u128();
    (value as u64) ^ ((value >> 64) as u64) ^ u64::from(item.attempts)
}

fn mark_blob_retry(
    connection: &rusqlite::Connection,
    blob: &OutboxBlob,
    error: &str,
    jitter: u64,
    now: &chrono::DateTime<chrono::Utc>,
    retry_after: Option<&str>,
) -> Result<()> {
    let next = blob_retry_at(now, blob, jitter, retry_after);
    SyncOutboxRepository::mark_blob_retry(connection, blob, &next.to_rfc3339(), error)
}

fn metadata_retry_at(
    now: &chrono::DateTime<chrono::Utc>,
    item: &OutboxItem,
    jitter: u64,
    retry_after: Option<&str>,
) -> chrono::DateTime<chrono::Utc> {
    next_retry_at(now, item.attempts, jitter, retry_after)
}

fn blob_retry_at(
    now: &chrono::DateTime<chrono::Utc>,
    blob: &OutboxBlob,
    jitter: u64,
    retry_after: Option<&str>,
) -> chrono::DateTime<chrono::Utc> {
    next_retry_at(now, blob.attempts, jitter, retry_after)
}

fn next_retry_at(
    now: &chrono::DateTime<chrono::Utc>,
    attempts: u32,
    jitter: u64,
    retry_after: Option<&str>,
) -> chrono::DateTime<chrono::Utc> {
    let backoff = *now + chrono::Duration::seconds(retry_delay_seconds(attempts, jitter));
    retry_after_deadline(now, retry_after).map_or(backoff, |deadline| backoff.max(deadline))
}

fn retry_after_deadline(
    now: &chrono::DateTime<chrono::Utc>,
    retry_after: Option<&str>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let value = retry_after?.trim();
    let deadline = if let Ok(seconds) = value.parse::<u64>() {
        let seconds = i64::try_from(seconds)
            .unwrap_or(MAX_RETRY_AFTER_SECONDS)
            .min(MAX_RETRY_AFTER_SECONDS);
        *now + chrono::Duration::seconds(seconds)
    } else {
        let parsed = httpdate::parse_http_date(value).ok()?;
        let deadline = chrono::DateTime::<chrono::Utc>::from(parsed);
        let cap = *now + chrono::Duration::seconds(MAX_RETRY_AFTER_SECONDS);
        deadline.min(cap)
    };
    (deadline > *now).then_some(deadline)
}

fn blob_retry_jitter(blob: &OutboxBlob) -> u64 {
    let value = blob.record_id.as_uuid().as_u128();
    (value as u64) ^ ((value >> 64) as u64) ^ u64::from(blob.attempts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
    use crate::sync::protocol::{SnapshotBatchResponse, SnapshotDisposition, SnapshotResult};
    use async_trait::async_trait;
    use std::time::SystemTime;
    use tokio::sync::Notify;

    fn fixed_scheduling_now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2030-01-02T03:04:05Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn retry_test_item(attempts: u32) -> OutboxItem {
        let record_id = audetic_core::sync::RecordId::new();
        OutboxItem {
            record_id,
            kind: crate::sync::protocol::RecordKind::Dictation,
            local_version: 1,
            snapshot: crate::sync::protocol::DictationSnapshot {
                kind: crate::sync::protocol::RecordKind::Dictation,
                schema_version: 1,
                record_id,
                origin_device_id: audetic_core::sync::DeviceId::new(),
                local_version: 1,
                created_at: "2030-01-02T03:00:00Z".into(),
                updated_at: "2030-01-02T03:00:00Z".into(),
                payload: crate::sync::protocol::DictationPayload {
                    text: "retry".into(),
                    recording_payload: Default::default(),
                },
            }
            .into(),
            attempts,
            lease_owner: "worker".into(),
        }
    }

    fn retry_test_blob(attempts: u32) -> OutboxBlob {
        OutboxBlob {
            record_id: audetic_core::sync::RecordId::new(),
            kind: crate::sync::protocol::RecordKind::Dictation,
            checksum: "a".repeat(64),
            staged_path: PathBuf::from("unused"),
            byte_size: 1,
            media_type: "audio/wav".into(),
            attempts,
            lease_owner: "worker".into(),
        }
    }

    fn assert_retry_after_for_metadata_and_blob(
        value: &str,
        expected: chrono::DateTime<chrono::Utc>,
    ) {
        let now = fixed_scheduling_now();
        let item = retry_test_item(1);
        let blob = retry_test_blob(1);
        assert_eq!(metadata_retry_at(&now, &item, 0, Some(value)), expected);
        assert_eq!(blob_retry_at(&now, &blob, 0, Some(value)), expected);
    }

    struct LoopbackHub {
        library: HubLibrary,
    }

    struct BlockingHub {
        started: Arc<Notify>,
    }

    struct RateLimitedHub {
        retry_after: &'static str,
        accept_snapshots: bool,
    }

    #[async_trait]
    impl ReplicationTransport for RateLimitedHub {
        async fn upload_snapshots(
            &self,
            _hub: &HubConnection,
            batch: SnapshotBatch,
        ) -> std::result::Result<SnapshotBatchResponse, HubTransferError> {
            if self.accept_snapshots {
                return Ok(SnapshotBatchResponse {
                    results: batch
                        .snapshots
                        .into_iter()
                        .map(|snapshot| SnapshotResult {
                            record_id: snapshot.record_id(),
                            disposition: SnapshotDisposition::Accepted,
                            authoritative_revision: Some(1),
                            error_code: None,
                            message: None,
                        })
                        .collect(),
                });
            }
            Err(HubTransferError::Http {
                status: 429,
                message: "try later".to_owned(),
                retry_after: Some(self.retry_after.to_owned()),
            })
        }

        async fn upload_blob(
            &self,
            _hub: &HubConnection,
            _blob: BlobUpload,
        ) -> std::result::Result<(), HubTransferError> {
            Err(HubTransferError::Http {
                status: 429,
                message: "try later".to_owned(),
                retry_after: Some(self.retry_after.to_owned()),
            })
        }
    }

    #[async_trait]
    impl ReplicationTransport for BlockingHub {
        async fn upload_snapshots(
            &self,
            _hub: &HubConnection,
            _batch: SnapshotBatch,
        ) -> std::result::Result<SnapshotBatchResponse, HubTransferError> {
            self.started.notify_one();
            std::future::pending().await
        }

        async fn upload_blob(
            &self,
            _hub: &HubConnection,
            _blob: BlobUpload,
        ) -> std::result::Result<(), HubTransferError> {
            std::future::pending().await
        }
    }

    #[async_trait]
    impl ReplicationTransport for LoopbackHub {
        async fn upload_snapshots(
            &self,
            _hub: &HubConnection,
            batch: SnapshotBatch,
        ) -> std::result::Result<SnapshotBatchResponse, HubTransferError> {
            self.library
                .apply_snapshots(batch.snapshots)
                .map_err(|error| HubTransferError::Retryable(error.to_string()))
        }

        async fn upload_blob(
            &self,
            _hub: &HubConnection,
            blob: BlobUpload,
        ) -> std::result::Result<(), HubTransferError> {
            self.library
                .accept_blob_file(
                    &blob.source_path,
                    &blob.checksum,
                    blob.byte_size,
                    &blob.media_type,
                )
                .await
                .map(|_| ())
                .map_err(|error| HubTransferError::Retryable(error.to_string()))
        }
    }

    #[tokio::test]
    async fn home_hub_worker_uses_authoritative_boundary_and_marks_revision() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hub.db");
        let conn = crate::db::migrate_db_at(&path).unwrap();
        crate::db::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
        conn.execute(
            "UPDATE sync_settings SET role = 'home_hub' WHERE singleton = 1",
            [],
        )
        .unwrap();
        crate::db::insert_workflow(
            &conn,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: "hub text".into(),
                    audio_path: "/local/path".into(),
                }),
            ),
        )
        .unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM sync_outbox_items", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(conn);

        let worker = OutboxWorker::new(
            path.clone(),
            OutboxDestination::Local(HubLibrary::new(path.clone())),
        );
        assert_eq!(worker.process_once().await.unwrap(), 1);
        let conn = crate::db::open_db_at(&path).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_dictations", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let state: (String, u64) = conn
            .query_row(
                "SELECT state, accepted_hub_revision FROM sync_outbox_items",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, ("synced".into(), 1));
    }

    #[tokio::test]
    async fn staged_payload_survives_source_cleanup_and_home_hub_accepts_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hub.db");
        let source = temp.path().join("dictation.wav");
        std::fs::write(&source, b"durable payload").unwrap();
        let conn = crate::db::migrate_db_at(&path).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO sync_settings(singleton) VALUES(1)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
            [],
        )
        .unwrap();
        crate::db::insert_workflow(
            &conn,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: "audio too".into(),
                    audio_path: source.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap();
        let staged: String = conn
            .query_row("SELECT staged_path FROM sync_outbox_blobs", [], |row| {
                row.get(0)
            })
            .unwrap();
        std::fs::remove_file(&source).unwrap();
        assert!(std::path::Path::new(&staged).is_file());
        drop(conn);

        let worker = OutboxWorker::new(
            path.clone(),
            OutboxDestination::Local(HubLibrary::new(path.clone())),
        );
        assert_eq!(worker.process_once().await.unwrap(), 2);
        assert!(!std::path::Path::new(&staged).exists());
        let conn = crate::db::open_db_at(&path).unwrap();
        assert_eq!(
            conn.query_row("SELECT state FROM sync_outbox_blobs", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "synced"
        );
        let canonical: String = conn
            .query_row("SELECT canonical_path FROM library_blobs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(std::fs::read(canonical).unwrap(), b"durable payload");
    }

    #[tokio::test]
    async fn payload_disappearance_after_staging_needs_attention_without_blocking_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hub.db");
        let source = temp.path().join("dictation.wav");
        std::fs::write(&source, b"staged then lost").unwrap();
        let conn = crate::db::migrate_db_at(&path).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO sync_settings(singleton) VALUES(1)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
            [],
        )
        .unwrap();
        crate::db::insert_workflow(
            &conn,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: "metadata remains".into(),
                    audio_path: source.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap();
        let staged: String = conn
            .query_row("SELECT staged_path FROM sync_outbox_blobs", [], |row| {
                row.get(0)
            })
            .unwrap();
        std::fs::remove_file(staged).unwrap();
        drop(conn);

        let worker = OutboxWorker::new(
            path.clone(),
            OutboxDestination::Local(HubLibrary::new(path.clone())),
        );
        assert_eq!(worker.process_once().await.unwrap(), 2);
        let conn = crate::db::open_db_at(&path).unwrap();
        assert_eq!(
            conn.query_row("SELECT state FROM sync_outbox_items", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "synced"
        );
        assert_eq!(
            conn.query_row(
                "SELECT availability || ':' || state FROM sync_outbox_blobs",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "needs_attention:needs_attention"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM shared_dictations", [], |row| row
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn connected_device_uploads_metadata_before_payload_to_the_hub_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let source_db = temp.path().join("device.db");
        let hub_db = temp.path().join("hub.db");
        let source_audio = temp.path().join("dictation.wav");
        std::fs::write(&source_audio, b"connected payload").unwrap();
        let source = crate::db::migrate_db_at(&source_db).unwrap();
        crate::db::migrate_db_at(&hub_db).unwrap();
        let hub = HubConnection {
            base_url: "https://hub.example.ts.net:8443/audetic/".into(),
            hub_id: audetic_core::sync::HubId::new(),
            owner_login: "owner@example.com".into(),
        };
        source
            .execute(
                "INSERT OR IGNORE INTO sync_settings(singleton) VALUES(1)",
                [],
            )
            .unwrap();
        source
            .execute(
                "UPDATE sync_settings SET role='connected_device',hub_url=?1,hub_id=?2,
                 hub_owner_login=?3,upload_recording_payloads=1 WHERE singleton=1",
                rusqlite::params![hub.base_url, hub.hub_id.to_string(), hub.owner_login],
            )
            .unwrap();
        let (_, record_id) = crate::db::insert_workflow_record(
            &source,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: "connected text".into(),
                    audio_path: source_audio.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap();
        drop(source);

        let worker = OutboxWorker::new(
            source_db.clone(),
            OutboxDestination::Remote {
                hub,
                replication: Arc::new(LoopbackHub {
                    library: HubLibrary::new(hub_db.clone()),
                }),
            },
        );
        assert_eq!(worker.process_once().await.unwrap(), 2);
        let source = crate::db::open_db_at(&source_db).unwrap();
        assert_eq!(
            source
                .query_row("SELECT state FROM sync_outbox_blobs", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "synced"
        );
        let hub = crate::db::open_db_at(&hub_db).unwrap();
        let payload = crate::db::shared_library::SharedLibraryRepository::payload_blob(
            &hub,
            record_id,
            crate::sync::protocol::RecordKind::Dictation,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read(payload.canonical_path).unwrap(),
            b"connected payload"
        );
    }

    #[tokio::test]
    async fn cancellation_aborts_an_in_flight_upload_and_releases_its_lease() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("device.db");
        let conn = crate::db::migrate_db_at(&db_path).unwrap();
        let hub = HubConnection {
            base_url: "https://hub.example.ts.net:8443/audetic/".into(),
            hub_id: audetic_core::sync::HubId::new(),
            owner_login: "owner@example.com".into(),
        };
        crate::db::sync_settings::SyncSettingsRepository::save(
            &conn,
            &crate::db::sync_settings::SyncSettings {
                role: SyncRole::ConnectedDevice,
                hub: Some(hub.clone()),
                upload_recording_payloads: false,
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::insert_workflow(
            &conn,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: "cancel upload".into(),
                    audio_path: "/missing".into(),
                }),
            ),
        )
        .unwrap();
        drop(conn);
        let started = Arc::new(Notify::new());
        let worker = OutboxWorker::new(
            db_path.clone(),
            OutboxDestination::Remote {
                hub,
                replication: Arc::new(BlockingHub {
                    started: Arc::clone(&started),
                }),
            },
        )
        .with_payload_uploads(false);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(worker.run(task_cancellation));
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .unwrap();

        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("cancelled worker should join promptly")
            .unwrap();

        let conn = crate::db::open_db_at(&db_path).unwrap();
        let state: (String, Option<String>) = conn
            .query_row(
                "SELECT state,lease_owner FROM sync_outbox_items",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, ("pending".into(), None));
    }

    #[tokio::test]
    async fn rate_limited_metadata_uses_the_injected_scheduling_clock() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("device.db");
        let conn = crate::db::migrate_db_at(&db_path).unwrap();
        let hub = HubConnection {
            base_url: "https://hub.example.ts.net:8443/audetic/".into(),
            hub_id: audetic_core::sync::HubId::new(),
            owner_login: "owner@example.com".into(),
        };
        crate::db::sync_settings::SyncSettingsRepository::save(
            &conn,
            &crate::db::sync_settings::SyncSettings {
                role: SyncRole::ConnectedDevice,
                hub: Some(hub.clone()),
                upload_recording_payloads: false,
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::insert_workflow(
            &conn,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: "retry me".into(),
                    audio_path: "/missing".into(),
                }),
            ),
        )
        .unwrap();
        drop(conn);

        let worker = OutboxWorker::new(
            db_path.clone(),
            OutboxDestination::Remote {
                hub,
                replication: Arc::new(RateLimitedHub {
                    retry_after: "30",
                    accept_snapshots: false,
                }),
            },
        )
        .with_payload_uploads(false)
        .with_retry_jitter(|_| 0)
        .with_scheduling_clock(fixed_scheduling_now);
        assert_eq!(worker.process_once().await.unwrap(), 1);

        let conn = crate::db::open_db_at(&db_path).unwrap();
        let expected = (fixed_scheduling_now() + chrono::Duration::seconds(30)).to_rfc3339();
        let metadata: (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT state,next_attempt_at,last_error FROM sync_outbox_items",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(metadata.0, "pending");
        assert_eq!(metadata.1.as_deref(), Some(expected.as_str()));
        assert!(metadata.2.is_some_and(|error| error.contains("HTTP 429")));
    }

    #[tokio::test]
    async fn rate_limited_blob_uses_the_injected_scheduling_clock() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("device.db");
        let source_audio = temp.path().join("dictation.wav");
        std::fs::write(&source_audio, b"retry payload").unwrap();
        let conn = crate::db::migrate_db_at(&db_path).unwrap();
        let hub = HubConnection {
            base_url: "https://hub.example.ts.net:8443/audetic/".into(),
            hub_id: audetic_core::sync::HubId::new(),
            owner_login: "owner@example.com".into(),
        };
        crate::db::sync_settings::SyncSettingsRepository::save(
            &conn,
            &crate::db::sync_settings::SyncSettings {
                role: SyncRole::ConnectedDevice,
                hub: Some(hub.clone()),
                upload_recording_payloads: true,
                ..Default::default()
            },
        )
        .unwrap();
        crate::db::insert_workflow(
            &conn,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: "retry payload".into(),
                    audio_path: source_audio.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap();
        drop(conn);

        let worker = OutboxWorker::new(
            db_path.clone(),
            OutboxDestination::Remote {
                hub,
                replication: Arc::new(RateLimitedHub {
                    retry_after: "30",
                    accept_snapshots: true,
                }),
            },
        )
        .with_retry_jitter(|_| 0)
        .with_scheduling_clock(fixed_scheduling_now);
        assert_eq!(worker.process_once().await.unwrap(), 2);

        let conn = crate::db::open_db_at(&db_path).unwrap();
        let expected = (fixed_scheduling_now() + chrono::Duration::seconds(30)).to_rfc3339();
        assert_eq!(
            conn.query_row("SELECT state FROM sync_outbox_items", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            "synced"
        );
        let blob: (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT state,next_attempt_at,last_error FROM sync_outbox_blobs",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(blob.0, "pending");
        assert_eq!(blob.1.as_deref(), Some(expected.as_str()));
        assert!(blob.2.is_some_and(|error| error.contains("HTTP 429")));
    }

    #[test]
    fn retry_after_delta_seconds_defers_metadata_and_blob_work() {
        let expected = fixed_scheduling_now() + chrono::Duration::seconds(120);
        assert_retry_after_for_metadata_and_blob("120", expected);
    }

    #[test]
    fn retry_after_http_date_defers_metadata_and_blob_work() {
        let expected = fixed_scheduling_now() + chrono::Duration::seconds(90);
        let retry_after = httpdate::fmt_http_date(SystemTime::from(expected));
        assert_retry_after_for_metadata_and_blob(&retry_after, expected);
    }

    #[test]
    fn invalid_or_past_retry_after_uses_backoff_for_metadata_and_blob_work() {
        let now = fixed_scheduling_now();
        let expected = now + chrono::Duration::seconds(retry_delay_seconds(1, 0));
        let past = httpdate::fmt_http_date(SystemTime::from(now - chrono::Duration::seconds(1)));
        for retry_after in ["not a Retry-After value", &past] {
            assert_retry_after_for_metadata_and_blob(retry_after, expected);
        }
    }

    #[test]
    fn retry_after_is_capped_for_metadata_and_blob_work() {
        let now = fixed_scheduling_now();
        let expected = now + chrono::Duration::seconds(MAX_RETRY_AFTER_SECONDS);
        let far_future = httpdate::fmt_http_date(SystemTime::from(
            now + chrono::Duration::seconds(MAX_RETRY_AFTER_SECONDS * 2),
        ));
        for retry_after in [(MAX_RETRY_AFTER_SECONDS * 2).to_string(), far_future] {
            assert_retry_after_for_metadata_and_blob(&retry_after, expected);
        }
    }

    #[test]
    fn retry_backoff_jitter_is_deterministic_and_bounded() {
        assert_eq!(retry_delay_seconds(1, 0), 1);
        assert_eq!(retry_delay_seconds(1, 2), 2);
        assert_eq!(retry_delay_seconds(5, 0), 12);
        assert_eq!(retry_delay_seconds(5, 8), 20);
        assert_eq!(retry_delay_seconds(100, u64::MAX), 300);
        assert_eq!(retry_delay_seconds(5, 7), retry_delay_seconds(5, 7));
    }

    #[test]
    fn worker_accepts_an_injected_retry_jitter_source() {
        fn fixed(_: &OutboxItem) -> u64 {
            7
        }

        let worker = OutboxWorker::new(
            PathBuf::from("unused"),
            OutboxDestination::Local(HubLibrary::new(PathBuf::from("unused"))),
        )
        .with_retry_jitter(fixed);
        let item = OutboxItem {
            record_id: audetic_core::sync::RecordId::new(),
            kind: crate::sync::protocol::RecordKind::Dictation,
            local_version: 1,
            snapshot: crate::sync::protocol::DictationSnapshot {
                kind: crate::sync::protocol::RecordKind::Dictation,
                schema_version: 1,
                record_id: audetic_core::sync::RecordId::new(),
                origin_device_id: audetic_core::sync::DeviceId::new(),
                local_version: 1,
                created_at: "2026-09-04T10:00:00Z".into(),
                updated_at: "2026-09-04T10:00:00Z".into(),
                payload: crate::sync::protocol::DictationPayload {
                    text: "x".into(),
                    recording_payload: Default::default(),
                },
            }
            .into(),
            attempts: 1,
            lease_owner: "worker".into(),
        };
        assert_eq!((worker.retry_jitter)(&item), 7);
    }
}
