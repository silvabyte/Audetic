use anyhow::Result;
use audetic_core::sync::{PayloadAvailability, SyncRole, UploadState};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::db::shared_library::SharedLibraryRepository;
use crate::db::sync_outbox::SyncOutboxRepository;
use crate::history::{HistoryEntry, HistorySource, SearchParams};

use super::service::HubAccess;
use crate::db::sync_settings::SyncSettings;

pub struct LibraryReadResult {
    pub entries: Vec<HistoryEntry>,
    pub hub_reachable: bool,
    pub error: Option<String>,
}

pub struct LibraryReader {
    db_path: PathBuf,
    hubs: Arc<dyn HubAccess>,
}

impl LibraryReader {
    pub fn new(db_path: PathBuf, hubs: Arc<dyn HubAccess>) -> Self {
        Self { db_path, hubs }
    }

    pub async fn read(
        &self,
        settings: &SyncSettings,
        params: &SearchParams,
    ) -> Result<LibraryReadResult> {
        let connection = crate::db::open_db_at(&self.db_path)?;
        let offset = params.offset;
        let limit = params.limit.clamp(1, 100);
        let fetch = offset.saturating_add(limit);
        let local = crate::db::list_visible_workflows(
            &connection,
            params.query.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
            0,
            fetch,
        )?;
        let mut entries = BTreeMap::new();
        for workflow in local {
            let mut entry = HistoryEntry::from(workflow);
            entry.upload_state = SyncOutboxRepository::state_for(&connection, entry.id)?;
            entries.insert(entry.id, entry);
        }

        let (reachable, error) = match settings.role {
            SyncRole::Standalone => (false, None),
            SyncRole::HomeHub => {
                for shared in SharedLibraryRepository::page_dictations(
                    &connection,
                    params.query.as_deref(),
                    params.from.as_deref(),
                    params.to.as_deref(),
                    None,
                    fetch,
                )? {
                    entries.insert(shared.record_id, shared_entry(shared, false, false));
                }
                (true, None)
            }
            SyncRole::ConnectedDevice => {
                let hub = settings
                    .hub
                    .as_ref()
                    .expect("connected settings contain a hub");
                let mut cursor = None;
                let mut failure = None;
                let mut fetched_from_hub = 0usize;
                loop {
                    let page_limit = fetch.saturating_sub(fetched_from_hub).clamp(1, 100);
                    match self
                        .hubs
                        .page_dictations(
                            hub,
                            params.query.as_deref(),
                            params.from.as_deref(),
                            params.to.as_deref(),
                            cursor.as_deref(),
                            page_limit,
                        )
                        .await
                    {
                        Ok(page) => {
                            let page_len = page.items.len();
                            fetched_from_hub = fetched_from_hub.saturating_add(page_len);
                            for shared in page.items {
                                entries.insert(shared.record_id, shared_entry(shared, false, true));
                            }
                            cursor = page.next_cursor;
                            if fetched_from_hub >= fetch || cursor.is_none() || page_len == 0 {
                                break;
                            }
                        }
                        Err(error) => {
                            failure = Some(error.to_string());
                            break;
                        }
                    }
                }
                if let Some(error) = failure {
                    entries.retain(|_, entry| entry.source == HistorySource::Local);
                    for entry in entries.values_mut() {
                        entry.offline = true;
                    }
                    (false, Some(error))
                } else {
                    (true, None)
                }
            }
        };
        let mut entries: Vec<_> = entries.into_values().collect();
        entries.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        entries = entries.into_iter().skip(offset).take(limit).collect();
        Ok(LibraryReadResult {
            entries,
            hub_reachable: reachable,
            error,
        })
    }
}

fn shared_entry(
    shared: super::protocol::SharedDictation,
    offline: bool,
    read_only: bool,
) -> HistoryEntry {
    HistoryEntry {
        id: shared.record_id,
        text: shared.text,
        created_at: shared.created_at,
        origin_device_id: shared.origin_device_id,
        source: HistorySource::Shared,
        upload_state: Some(UploadState::Synced),
        payload_availability: PayloadAvailability::Unavailable,
        offline,
        read_only,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
    use crate::sync::client::DiscoveryOutcome;
    use crate::sync::protocol::{
        DictationPayload, DictationSnapshot, RecordKind, SnapshotBatch, SnapshotBatchResponse,
    };
    use crate::sync::service::HubTransferError;
    use async_trait::async_trait;
    use audetic_core::sync::{CacheLevel, HubCandidate, HubConnection, HubId, SyncRole};

    struct OfflineHub;

    #[async_trait]
    impl HubAccess for OfflineHub {
        async fn handshake(
            &self,
            _hub: &HubConnection,
        ) -> std::result::Result<HubCandidate, String> {
            Err("offline".into())
        }
        async fn discover(&self, _candidates: Vec<String>, _owner: &str) -> DiscoveryOutcome {
            DiscoveryOutcome::None { failures: vec![] }
        }
        async fn upload_snapshots(
            &self,
            _hub: &HubConnection,
            _batch: SnapshotBatch,
        ) -> std::result::Result<SnapshotBatchResponse, HubTransferError> {
            Err(HubTransferError::Retryable("offline".into()))
        }
    }

    struct LocalHub {
        library: super::super::library::HubLibrary,
    }

    #[async_trait]
    impl HubAccess for LocalHub {
        async fn handshake(
            &self,
            hub: &HubConnection,
        ) -> std::result::Result<HubCandidate, String> {
            Ok(HubCandidate {
                connection: hub.clone(),
                device_name: Some("Hub".into()),
                protocol_version: 1,
            })
        }

        async fn discover(&self, _candidates: Vec<String>, _owner: &str) -> DiscoveryOutcome {
            unreachable!()
        }

        async fn page_dictations(
            &self,
            _hub: &HubConnection,
            query: Option<&str>,
            from: Option<&str>,
            to: Option<&str>,
            cursor: Option<&str>,
            limit: usize,
        ) -> std::result::Result<super::super::protocol::DictationPage, HubTransferError> {
            self.library
                .page_dictations(query, from, to, cursor, limit)
                .map_err(|error| HubTransferError::Retryable(error.to_string()))
        }
    }

    fn settings(role: SyncRole) -> SyncSettings {
        SyncSettings {
            role,
            hub: (role == SyncRole::ConnectedDevice).then(|| HubConnection {
                base_url: "https://hub.example.ts.net:8443/audetic/".into(),
                hub_id: HubId::new(),
                owner_login: "owner@example.com".into(),
            }),
            cache_level: CacheLevel::LiveOnly,
            ..SyncSettings::default()
        }
    }

    #[tokio::test]
    async fn home_hub_merge_dedupes_sorts_searches_and_paginates_by_uuid() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db.sqlite");
        let mut conn = crate::db::migrate_db_at(&path).unwrap();
        let workflow = Workflow::new(
            WorkflowType::VoiceToText,
            WorkflowData::VoiceToText(VoiceToTextData {
                text: "alpha local".into(),
                audio_path: "/missing".into(),
            }),
        );
        let (_, local_id) = crate::db::insert_workflow_record(&conn, &workflow).unwrap();
        conn.execute(
            "UPDATE workflows SET created_at = '2026-09-04T10:00:00Z' WHERE sync_id = ?1",
            [local_id.to_string()],
        )
        .unwrap();
        let local = crate::db::get_workflow_by_sync_id(&conn, local_id)
            .unwrap()
            .unwrap();
        let local_created = local.created_at.unwrap();
        SharedLibraryRepository::apply_snapshot(
            &mut conn,
            &DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: local_id,
                origin_device_id: local.origin_device_id.unwrap(),
                local_version: 1,
                created_at: local_created.clone(),
                updated_at: local_created,
                payload: DictationPayload {
                    text: "alpha local".into(),
                },
            },
        )
        .unwrap();
        SharedLibraryRepository::apply_snapshot(
            &mut conn,
            &DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: audetic_core::sync::RecordId::new(),
                origin_device_id: audetic_core::sync::DeviceId::new(),
                local_version: 1,
                created_at: "2026-09-04T11:00:00Z".into(),
                updated_at: "2026-09-04T11:00:00Z".into(),
                payload: DictationPayload {
                    text: "alpha remote".into(),
                },
            },
        )
        .unwrap();
        drop(conn);

        let reader = LibraryReader::new(path, Arc::new(OfflineHub));
        let first = reader
            .read(
                &settings(SyncRole::HomeHub),
                &SearchParams::new().with_query("alpha").with_limit(1),
            )
            .await
            .unwrap();
        assert_eq!(first.entries.len(), 1);
        assert_eq!(first.entries[0].text, "alpha remote");
        let mut second_params = SearchParams::new().with_query("alpha").with_limit(1);
        second_params.offset = 1;
        let second = reader
            .read(&settings(SyncRole::HomeHub), &second_params)
            .await
            .unwrap();
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.entries[0].id, local_id);
        assert_eq!(second.entries[0].source, HistorySource::Shared);
    }

    #[tokio::test]
    async fn connected_live_only_falls_back_to_local_rows_with_offline_state() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("db.sqlite");
        let conn = crate::db::migrate_db_at(&path).unwrap();
        crate::db::insert_workflow(
            &conn,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: "offline local".into(),
                    audio_path: "/missing".into(),
                }),
            ),
        )
        .unwrap();
        drop(conn);
        let result = LibraryReader::new(path, Arc::new(OfflineHub))
            .read(&settings(SyncRole::ConnectedDevice), &SearchParams::new())
            .await
            .unwrap();
        assert!(!result.hub_reachable);
        assert_eq!(result.entries.len(), 1);
        assert!(result.entries[0].offline);
    }

    #[tokio::test]
    async fn connected_merge_fetches_each_source_deep_enough_before_offset_and_limit() {
        let temp = tempfile::tempdir().unwrap();
        let local_path = temp.path().join("local.db");
        let hub_path = temp.path().join("hub.db");
        let local = crate::db::migrate_db_at(&local_path).unwrap();
        crate::db::migrate_db_at(&hub_path).unwrap();
        let hub_library = super::super::library::HubLibrary::new(hub_path);
        let mut accepted_duplicates = Vec::new();

        for index in 0..30 {
            let (_, record_id) = crate::db::insert_workflow_record(
                &local,
                &Workflow::new(
                    WorkflowType::VoiceToText,
                    WorkflowData::VoiceToText(VoiceToTextData {
                        text: format!("local-{index:02}"),
                        audio_path: "/missing".into(),
                    }),
                ),
            )
            .unwrap();
            let created_at = format!("2026-09-03T{:02}:00:00Z", index % 24);
            local
                .execute(
                    "UPDATE workflows SET created_at = ?2 WHERE sync_id = ?1",
                    rusqlite::params![record_id.to_string(), created_at],
                )
                .unwrap();
            if index < 15 {
                let stored = crate::db::get_workflow_by_sync_id(&local, record_id)
                    .unwrap()
                    .unwrap();
                let WorkflowData::VoiceToText(data) = stored.data;
                accepted_duplicates.push(DictationSnapshot {
                    kind: RecordKind::Dictation,
                    schema_version: 1,
                    record_id,
                    origin_device_id: stored.origin_device_id.unwrap(),
                    local_version: 1,
                    created_at: created_at.clone(),
                    updated_at: created_at,
                    payload: DictationPayload { text: data.text },
                });
            }
        }
        hub_library.apply_snapshots(accepted_duplicates).unwrap();

        let remote_origin = audetic_core::sync::DeviceId::new();
        let remote: Vec<_> = (0..40)
            .map(|index| DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: audetic_core::sync::RecordId::new(),
                origin_device_id: remote_origin,
                local_version: 1,
                created_at: format!("2026-09-05T{:02}:{:02}:00Z", index / 2, (index % 2) * 30),
                updated_at: format!("2026-09-05T{:02}:{:02}:00Z", index / 2, (index % 2) * 30),
                payload: DictationPayload {
                    text: format!("remote-{index:02}"),
                },
            })
            .collect();
        for batch in remote.chunks(super::super::protocol::MAX_SNAPSHOT_BATCH) {
            hub_library.apply_snapshots(batch.to_vec()).unwrap();
        }
        drop(local);

        let mut params = SearchParams::new().with_limit(20);
        params.offset = 10;
        let result = LibraryReader::new(
            local_path,
            Arc::new(LocalHub {
                library: hub_library,
            }),
        )
        .read(&settings(SyncRole::ConnectedDevice), &params)
        .await
        .unwrap();

        assert_eq!(result.entries.len(), 20);
        assert!(result
            .entries
            .iter()
            .all(|entry| entry.source == HistorySource::Shared));
        assert_eq!(result.entries[0].text, "remote-29");
        assert_eq!(result.entries[19].text, "remote-10");
    }
}
