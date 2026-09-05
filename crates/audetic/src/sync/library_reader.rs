use anyhow::Result;
use audetic_core::sync::{DeviceId, PayloadAvailability, RecordId, SyncRole, UploadState};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::db::shared_library::SharedLibraryRepository;
use crate::db::sync_outbox::SyncOutboxRepository;
use crate::history::{HistoryEntry, HistorySource, SearchParams};

use super::transport::RemoteLibrary;
use crate::db::sync_settings::SyncSettings;

pub struct LibraryReadResult {
    pub entries: Vec<HistoryEntry>,
    pub hub_reachable: bool,
    pub error: Option<String>,
}

pub struct LibraryReader {
    db_path: PathBuf,
    remote: Arc<dyn RemoteLibrary>,
}

#[derive(Clone, Debug)]
pub struct LibraryMeeting {
    pub id: RecordId,
    pub local_id: Option<i64>,
    pub origin_device_id: DeviceId,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub title_version: u64,
    pub source_filename: Option<String>,
    pub status: String,
    pub transcript_text: Option<String>,
    pub transcript_segments: Option<Vec<audetic_core::jobs_client::Segment>>,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub upload_state: Option<UploadState>,
    pub payload_availability: PayloadAvailability,
    pub source: &'static str,
    pub offline: bool,
    pub read_only: bool,
    pub artifacts: Vec<super::protocol::SharedArtifact>,
}

pub struct MeetingLibraryReader {
    db_path: PathBuf,
    remote: Arc<dyn RemoteLibrary>,
}
impl MeetingLibraryReader {
    pub fn new(db_path: PathBuf, remote: Arc<dyn RemoteLibrary>) -> Self {
        Self { db_path, remote }
    }
    pub async fn read(
        &self,
        settings: &SyncSettings,
        query: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<LibraryReadResultMeetings> {
        let connection = crate::db::open_db_at(&self.db_path)?;
        let limit = limit.clamp(1, super::protocol::MAX_MEETING_PAGE);
        let fetch = offset.saturating_add(limit);
        let mut entries = BTreeMap::new();
        let local_fetch = if query.is_some() { usize::MAX } else { fetch };
        for meeting in crate::db::meetings::MeetingRepository::list(&connection, local_fetch)? {
            if query.is_some_and(|query| {
                !contains_case_insensitive(meeting.title.as_deref().unwrap_or(""), query)
                    && !contains_case_insensitive(
                        meeting.transcript_text.as_deref().unwrap_or(""),
                        query,
                    )
            }) {
                continue;
            }
            let upload = SyncOutboxRepository::state_for_kind(
                &connection,
                meeting.sync_id,
                super::protocol::RecordKind::Meeting,
            )?;
            let payload = SyncOutboxRepository::payload_availability(&connection, meeting.sync_id)?;
            entries.insert(meeting.sync_id, local_meeting(meeting, upload, payload));
        }
        let (reachable, error) = match settings.role {
            SyncRole::Standalone => (false, None),
            SyncRole::HomeHub => {
                for meeting in
                    SharedLibraryRepository::page_meetings(&connection, query, None, fetch)?
                {
                    let id = meeting.record_id;
                    let shared = shared_meeting(meeting, false, false);
                    let shared = overlay_local_payload(entries.get(&id), shared);
                    entries.insert(id, shared);
                }
                (true, None)
            }
            SyncRole::ConnectedDevice => {
                let hub = settings
                    .hub
                    .as_ref()
                    .expect("connected settings contain a hub");
                let mut cursor = None;
                let mut fetched = 0;
                let mut failure = None;
                loop {
                    match self
                        .remote
                        .page_meetings(
                            hub,
                            query,
                            cursor.as_deref(),
                            fetch
                                .saturating_sub(fetched)
                                .clamp(1, super::protocol::MAX_MEETING_PAGE),
                        )
                        .await
                    {
                        Ok(page) => {
                            let len = page.items.len();
                            fetched += len;
                            for meeting in page.items {
                                let id = meeting.record_id;
                                let shared = shared_meeting(meeting, false, false);
                                let shared = overlay_local_payload(entries.get(&id), shared);
                                entries.insert(id, shared);
                            }
                            cursor = page.next_cursor;
                            if fetched >= fetch || cursor.is_none() || len == 0 {
                                break;
                            }
                        }
                        Err(err) => {
                            failure = Some(err.to_string());
                            break;
                        }
                    }
                }
                if failure.is_some() {
                    entries.retain(|_, value| value.source == "local");
                    for value in entries.values_mut() {
                        value.offline = true;
                        value.read_only = SyncOutboxRepository::may_have_reached_hub(
                            &connection,
                            value.id,
                            super::protocol::RecordKind::Meeting,
                        )?;
                    }
                    (false, failure)
                } else {
                    (true, None)
                }
            }
        };
        let mut meetings: Vec<_> = entries.into_values().collect();
        meetings.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        let meetings = meetings.into_iter().skip(offset).take(limit).collect();
        Ok(LibraryReadResultMeetings {
            meetings,
            hub_reachable: reachable,
            error,
        })
    }
}
pub struct LibraryReadResultMeetings {
    pub meetings: Vec<LibraryMeeting>,
    pub hub_reachable: bool,
    pub error: Option<String>,
}
fn local_meeting(
    value: crate::db::meetings::MeetingRecord,
    upload_state: Option<UploadState>,
    outbox_payload: Option<PayloadAvailability>,
) -> LibraryMeeting {
    let operational_payload =
        crate::sync::payload::resolve_operational_audio(std::path::Path::new(&value.audio_path))
            .ok()
            .flatten()
            .is_some();
    LibraryMeeting {
        id: value.sync_id,
        local_id: Some(value.id),
        origin_device_id: value.origin_device_id,
        title: value.title,
        title_source: value.title_source,
        title_version: value.title_version.try_into().unwrap_or_default(),
        source_filename: value.source_filename,
        status: value.status,
        transcript_text: value.transcript_text,
        transcript_segments: value.transcript_segments,
        duration_seconds: value.duration_seconds,
        started_at: value.started_at,
        completed_at: value.completed_at,
        error: value.error,
        created_at: value.created_at,
        upload_state,
        payload_availability: if operational_payload {
            PayloadAvailability::Available
        } else {
            outbox_payload.unwrap_or(PayloadAvailability::Unavailable)
        },
        source: "local",
        offline: false,
        read_only: false,
        artifacts: vec![],
    }
}
fn shared_meeting(
    value: super::protocol::SharedMeeting,
    offline: bool,
    read_only: bool,
) -> LibraryMeeting {
    LibraryMeeting {
        id: value.record_id,
        local_id: None,
        origin_device_id: value.origin_device_id,
        title: value.title,
        title_source: value.title_source,
        title_version: value.title_version,
        source_filename: value.source_filename,
        status: value.status,
        transcript_text: Some(value.transcript_text),
        transcript_segments: value.transcript_segments,
        duration_seconds: value.duration_seconds.try_into().ok(),
        started_at: value.created_at.clone(),
        completed_at: Some(value.completed_at),
        error: None,
        created_at: value.created_at,
        upload_state: Some(UploadState::Synced),
        payload_availability: value.recording_payload.availability,
        source: "shared",
        offline,
        read_only,
        artifacts: value.artifacts,
    }
}

fn overlay_local_payload(
    local: Option<&LibraryMeeting>,
    mut shared: LibraryMeeting,
) -> LibraryMeeting {
    if let Some(local) = local {
        shared.local_id = local.local_id;
        shared.payload_availability =
            merge_payload_availability(local.payload_availability, shared.payload_availability);
        shared.upload_state = local.upload_state;
    }
    shared
}

fn contains_case_insensitive(value: &str, query: &str) -> bool {
    value.to_lowercase().contains(&query.to_lowercase())
}

impl LibraryReader {
    pub fn new(db_path: PathBuf, remote: Arc<dyn RemoteLibrary>) -> Self {
        Self { db_path, remote }
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
            if entry.payload_availability == PayloadAvailability::Unavailable {
                entry.payload_availability =
                    SyncOutboxRepository::payload_availability(&connection, entry.id)?
                        .unwrap_or(PayloadAvailability::Unavailable);
            }
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
                    let id = shared.record_id;
                    let mut entry = shared_entry(shared, false, false);
                    if let Some(local) = entries.get(&id) {
                        entry.payload_availability = merge_payload_availability(
                            local.payload_availability,
                            entry.payload_availability,
                        );
                    }
                    entries.insert(id, entry);
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
                        .remote
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
                                let id = shared.record_id;
                                let mut entry = shared_entry(shared, false, true);
                                if let Some(local) = entries.get(&id) {
                                    entry.payload_availability = merge_payload_availability(
                                        local.payload_availability,
                                        entry.payload_availability,
                                    );
                                }
                                entries.insert(id, entry);
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

fn merge_payload_availability(
    local: PayloadAvailability,
    shared: PayloadAvailability,
) -> PayloadAvailability {
    if shared == PayloadAvailability::Available || local == PayloadAvailability::Available {
        PayloadAvailability::Available
    } else if local == PayloadAvailability::NeedsAttention {
        PayloadAvailability::NeedsAttention
    } else if local == PayloadAvailability::Pending {
        PayloadAvailability::Pending
    } else {
        shared
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
        payload_availability: shared.recording_payload.availability,
        offline,
        read_only,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
    use crate::sync::protocol::{DictationPayload, DictationSnapshot, RecordKind};
    use crate::sync::transport::HubTransferError;
    use async_trait::async_trait;
    use audetic_core::sync::{CacheLevel, HubConnection, HubId, SyncRole};

    #[test]
    fn local_payload_failure_is_visible_until_the_hub_has_an_available_blob() {
        assert_eq!(
            merge_payload_availability(
                PayloadAvailability::NeedsAttention,
                PayloadAvailability::Pending,
            ),
            PayloadAvailability::NeedsAttention
        );
        assert_eq!(
            merge_payload_availability(
                PayloadAvailability::NeedsAttention,
                PayloadAvailability::Available,
            ),
            PayloadAvailability::Available
        );
    }

    struct OfflineHub;
    impl RemoteLibrary for OfflineHub {}

    struct LocalHub {
        library: super::super::library::HubLibrary,
    }

    #[async_trait]
    impl RemoteLibrary for LocalHub {
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
                    recording_payload: Default::default(),
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
                    recording_payload: Default::default(),
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
    async fn shared_meeting_overlay_keeps_origin_audio_available_without_duplicates() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("meetings.sqlite");
        let audio_path = temp.path().join("meeting.wav");
        std::fs::write(&audio_path, b"audio").unwrap();
        let mut conn = crate::db::migrate_db_at(&path).unwrap();
        let local_id = crate::db::meetings::MeetingRepository::insert(
            &conn,
            Some("Local title"),
            audio_path.to_str().unwrap(),
        )
        .unwrap();
        crate::db::meetings::MeetingRepository::complete(
            &conn,
            local_id,
            "/tmp/transcript.txt",
            "portable transcript",
            None,
            30,
        )
        .unwrap();
        let local = crate::db::meetings::MeetingRepository::get(&conn, local_id)
            .unwrap()
            .unwrap();
        SharedLibraryRepository::apply_meeting_snapshot(&mut conn, &local.snapshot().unwrap())
            .unwrap();
        drop(conn);

        let result = MeetingLibraryReader::new(path, Arc::new(OfflineHub))
            .read(&settings(SyncRole::HomeHub), None, 0, 10)
            .await
            .unwrap();
        assert_eq!(result.meetings.len(), 1);
        assert_eq!(result.meetings[0].id, local.sync_id);
        assert_eq!(result.meetings[0].source, "shared");
        assert_eq!(result.meetings[0].local_id, Some(local_id));
        assert_eq!(
            result.meetings[0].payload_availability,
            PayloadAvailability::Available
        );
    }

    #[tokio::test]
    async fn meeting_pages_are_capped_at_one_hundred_without_losing_deep_offsets() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("meeting-pages.sqlite");
        let conn = crate::db::migrate_db_at(&path).unwrap();
        for index in 0..120 {
            let id = crate::db::meetings::MeetingRepository::insert(
                &conn,
                Some(&format!("Meeting {index:03}")),
                "/missing.mp3",
            )
            .unwrap();
            crate::db::meetings::MeetingRepository::complete(
                &conn,
                id,
                "/missing.txt",
                "transcript",
                None,
                10,
            )
            .unwrap();
        }
        drop(conn);
        let reader = MeetingLibraryReader::new(path, Arc::new(OfflineHub));

        let first = reader
            .read(&settings(SyncRole::Standalone), None, 0, 500)
            .await
            .unwrap();
        assert_eq!(first.meetings.len(), 100);
        let deep = reader
            .read(&settings(SyncRole::Standalone), None, 100, 500)
            .await
            .unwrap();
        assert_eq!(deep.meetings.len(), 20);
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
    async fn accepted_local_meeting_is_read_only_while_connected_hub_is_offline() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("meeting-offline.db");
        let conn = crate::db::migrate_db_at(&path).unwrap();
        let local_id = crate::db::meetings::MeetingRepository::insert(
            &conn,
            Some("Accepted meeting"),
            "/missing.wav",
        )
        .unwrap();
        crate::db::meetings::MeetingRepository::complete(
            &conn,
            local_id,
            "/missing.txt",
            "already shared transcript",
            None,
            30,
        )
        .unwrap();
        let meeting = crate::db::meetings::MeetingRepository::get(&conn, local_id)
            .unwrap()
            .unwrap();
        crate::db::sync_outbox::SyncOutboxRepository::enqueue_snapshot(
            &conn,
            &meeting.snapshot().unwrap().into(),
        )
        .unwrap();
        conn.execute(
            "UPDATE sync_outbox_items SET state = 'synced', accepted_hub_revision = 1 \
             WHERE record_id = ?1 AND kind = 'meeting'",
            [meeting.sync_id.to_string()],
        )
        .unwrap();
        drop(conn);

        let result = MeetingLibraryReader::new(path, Arc::new(OfflineHub))
            .read(&settings(SyncRole::ConnectedDevice), None, 0, 10)
            .await
            .unwrap();

        assert_eq!(result.meetings.len(), 1);
        assert!(result.meetings[0].offline);
        assert!(result.meetings[0].read_only);
        assert_eq!(result.meetings[0].upload_state, Some(UploadState::Synced));
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
                    payload: DictationPayload {
                        text: data.text,
                        recording_payload: Default::default(),
                    },
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
                    recording_payload: Default::default(),
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
