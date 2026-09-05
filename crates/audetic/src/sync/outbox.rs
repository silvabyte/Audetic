use anyhow::Result;
use audetic_core::sync::{HubConnection, SyncRole};
use tokio_util::sync::CancellationToken;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::db::sync_outbox::{OutboxBlob, OutboxItem, SyncOutboxRepository};

use super::library::HubLibrary;
use super::protocol::{SnapshotBatch, SnapshotDisposition};
use super::service::{HubAccess, HubTransferError};

pub const OUTBOX_BATCH_SIZE: usize = 25;
const MAX_RETRY_DELAY_SECONDS: i64 = 300;

type RetryJitter = fn(&OutboxItem) -> u64;

pub struct OutboxWorker {
    db_path: PathBuf,
    role: SyncRole,
    hub: Option<HubConnection>,
    remote: Arc<dyn HubAccess>,
    local_hub: HubLibrary,
    worker_id: String,
    retry_jitter: RetryJitter,
    upload_recording_payloads: bool,
}

impl OutboxWorker {
    pub fn new(
        db_path: PathBuf,
        role: SyncRole,
        hub: Option<HubConnection>,
        remote: Arc<dyn HubAccess>,
    ) -> Self {
        Self {
            local_hub: HubLibrary::new(db_path.clone()),
            db_path,
            role,
            hub,
            remote,
            worker_id: format!("outbox-{}", uuid::Uuid::new_v4()),
            retry_jitter: record_retry_jitter,
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

    pub async fn run(self, cancellation: CancellationToken) {
        let mut backfill_cursor = crate::db::BackfillCursor::default();
        loop {
            if cancellation.is_cancelled() {
                break;
            }
            let db_path = self.db_path.clone();
            let role = self.role;
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
        let now = chrono::Utc::now();
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
            let response = match self.role {
                SyncRole::HomeHub => self
                    .local_hub
                    .apply_snapshots(batch.snapshots)
                    .map_err(|error| HubTransferError::Retryable(error.to_string())),
                SyncRole::ConnectedDevice => {
                    let hub = self
                        .hub
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("missing Home Hub"))?;
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Ok(None),
                        response = self.remote.upload_snapshots(hub, batch) => response,
                    }
                }
                SyncRole::Standalone => return Ok(Some(0)),
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
                            None => mark_retry(
                                &connection,
                                item,
                                "Home Hub omitted the snapshot result",
                                (self.retry_jitter)(item),
                            )?,
                        }
                    }
                }
                Err(HubTransferError::Retryable(error)) => {
                    for item in &items {
                        if cancellation.is_cancelled() {
                            return Ok(None);
                        }
                        mark_retry(&connection, item, &error, (self.retry_jitter)(item))?;
                    }
                }
                Err(HubTransferError::NeedsAttention(error)) => {
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
            let uploaded = match self.role {
                SyncRole::HomeHub => tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Ok(None),
                    uploaded = self.local_hub.accept_blob_file(
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
                SyncRole::ConnectedDevice => {
                    let hub = self
                        .hub
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("missing Home Hub"))?;
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Ok(None),
                        uploaded = self.remote.upload_blob(hub, blob) => uploaded,
                    }
                }
                SyncRole::Standalone => return Ok(Some(items.len())),
            };
            match uploaded {
                Ok(()) => {
                    SyncOutboxRepository::mark_blob_accepted(&connection, blob)?;
                }
                Err(HubTransferError::Retryable(error)) => {
                    mark_blob_retry(&connection, blob, &error, blob_retry_jitter(blob))?;
                }
                Err(HubTransferError::NeedsAttention(error)) => {
                    SyncOutboxRepository::mark_blob_needs_attention(&connection, blob, &error)?;
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
) -> Result<()> {
    let delay = retry_delay_seconds(item.attempts, jitter);
    let next = chrono::Utc::now() + chrono::Duration::seconds(delay);
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
) -> Result<()> {
    let delay = retry_delay_seconds(blob.attempts, jitter);
    let next = chrono::Utc::now() + chrono::Duration::seconds(delay);
    SyncOutboxRepository::mark_blob_retry(connection, blob, &next.to_rfc3339(), error)
}

fn blob_retry_jitter(blob: &OutboxBlob) -> u64 {
    let value = blob.record_id.as_uuid().as_u128();
    (value as u64) ^ ((value >> 64) as u64) ^ u64::from(blob.attempts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
    use crate::sync::client::DiscoveryOutcome;
    use crate::sync::protocol::{DictationPage, SnapshotBatchResponse};
    use async_trait::async_trait;
    use audetic_core::sync::{HubCandidate, HubConnection};
    use tokio::sync::Notify;

    struct UnusedRemote;
    #[async_trait]
    impl HubAccess for UnusedRemote {
        async fn handshake(
            &self,
            _hub: &HubConnection,
        ) -> std::result::Result<HubCandidate, String> {
            unreachable!()
        }
        async fn discover(&self, _candidates: Vec<String>, _owner: &str) -> DiscoveryOutcome {
            unreachable!()
        }
        async fn upload_snapshots(
            &self,
            _hub: &HubConnection,
            _batch: SnapshotBatch,
        ) -> std::result::Result<SnapshotBatchResponse, HubTransferError> {
            unreachable!()
        }
        async fn page_dictations(
            &self,
            _hub: &HubConnection,
            _query: Option<&str>,
            _from: Option<&str>,
            _to: Option<&str>,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> std::result::Result<DictationPage, HubTransferError> {
            unreachable!()
        }
    }

    struct LoopbackHub {
        library: HubLibrary,
    }

    struct BlockingHub {
        started: Arc<Notify>,
    }

    #[async_trait]
    impl HubAccess for BlockingHub {
        async fn handshake(
            &self,
            _hub: &HubConnection,
        ) -> std::result::Result<HubCandidate, String> {
            unreachable!()
        }

        async fn discover(&self, _candidates: Vec<String>, _owner: &str) -> DiscoveryOutcome {
            unreachable!()
        }

        async fn upload_snapshots(
            &self,
            _hub: &HubConnection,
            _batch: SnapshotBatch,
        ) -> std::result::Result<SnapshotBatchResponse, HubTransferError> {
            self.started.notify_one();
            std::future::pending().await
        }

        async fn page_dictations(
            &self,
            _hub: &HubConnection,
            _query: Option<&str>,
            _from: Option<&str>,
            _to: Option<&str>,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> std::result::Result<DictationPage, HubTransferError> {
            unreachable!()
        }
    }

    #[async_trait]
    impl HubAccess for LoopbackHub {
        async fn handshake(
            &self,
            _hub: &HubConnection,
        ) -> std::result::Result<HubCandidate, String> {
            unreachable!()
        }

        async fn discover(&self, _candidates: Vec<String>, _owner: &str) -> DiscoveryOutcome {
            unreachable!()
        }

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
            blob: &OutboxBlob,
        ) -> std::result::Result<(), HubTransferError> {
            self.library
                .accept_blob_file(
                    &blob.staged_path,
                    &blob.checksum,
                    blob.byte_size,
                    &blob.media_type,
                )
                .await
                .map(|_| ())
                .map_err(|error| HubTransferError::Retryable(error.to_string()))
        }

        async fn page_dictations(
            &self,
            _hub: &HubConnection,
            _query: Option<&str>,
            _from: Option<&str>,
            _to: Option<&str>,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> std::result::Result<DictationPage, HubTransferError> {
            unreachable!()
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
            SyncRole::HomeHub,
            None,
            Arc::new(UnusedRemote),
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
            SyncRole::HomeHub,
            None,
            Arc::new(UnusedRemote),
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
            SyncRole::HomeHub,
            None,
            Arc::new(UnusedRemote),
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
            SyncRole::ConnectedDevice,
            Some(hub),
            Arc::new(LoopbackHub {
                library: HubLibrary::new(hub_db.clone()),
            }),
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
            SyncRole::ConnectedDevice,
            Some(hub),
            Arc::new(BlockingHub {
                started: Arc::clone(&started),
            }),
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
            SyncRole::HomeHub,
            None,
            Arc::new(UnusedRemote),
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
