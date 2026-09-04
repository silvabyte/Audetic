use anyhow::Result;
use audetic_core::sync::{HubConnection, SyncRole};
use tokio::sync::oneshot;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::db::sync_outbox::{OutboxItem, SyncOutboxRepository};

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
        }
    }

    #[cfg(test)]
    fn with_retry_jitter(mut self, retry_jitter: RetryJitter) -> Self {
        self.retry_jitter = retry_jitter;
        self
    }

    pub async fn run(self, mut shutdown: oneshot::Receiver<()>) {
        loop {
            let _ = self.process_once().await;
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = &mut shutdown => break,
            }
        }
    }

    pub async fn process_once(&self) -> Result<usize> {
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
        if items.is_empty() {
            return Ok(0);
        }
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
                self.remote.upload_snapshots(hub, batch).await
            }
            SyncRole::Standalone => return Ok(0),
        };
        match response {
            Ok(response) => {
                for item in &items {
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
                    mark_retry(&connection, item, &error, (self.retry_jitter)(item))?;
                }
            }
            Err(HubTransferError::NeedsAttention(error)) => {
                for item in &items {
                    SyncOutboxRepository::mark_needs_attention(&connection, item, &error)?;
                }
            }
        }
        Ok(items.len())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
    use crate::sync::client::DiscoveryOutcome;
    use crate::sync::protocol::{DictationPage, SnapshotBatchResponse};
    use async_trait::async_trait;
    use audetic_core::sync::{HubCandidate, HubConnection};

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
                payload: crate::sync::protocol::DictationPayload { text: "x".into() },
            }
            .into(),
            attempts: 1,
            lease_owner: "worker".into(),
        };
        assert_eq!((worker.retry_jitter)(&item), 7);
    }
}
