use async_trait::async_trait;
use audetic_core::sync::{HubConnection, HubId, RecordId, SyncRole};

use std::sync::Arc;

use crate::db::sync_outbox::SyncOutboxRepository;
use crate::sync::protocol::{
    ChangeCursor, ChangePage, ChangeTarget, DeleteSnapshot, DictationPage, DictationPayload,
    DictationSnapshot, MeetingPage, RecordKind, RecordingPayloadDescriptor, SharedDictation,
    SharedMeeting,
};
use crate::sync::transport::{
    HubCapabilities, HubChangeSource, HubTransferError, RemoteDictationLibrary,
    RemoteLibraryMutations, RemoteMeetingLibrary,
};

use super::*;

fn hub() -> HubConnection {
    HubConnection {
        base_url: "https://hub.example.ts.net:8443/audetic/".into(),
        hub_id: HubId::new(),
        owner_login: "owner@example.com".into(),
    }
}

#[derive(Default)]
struct PagedHub {
    dictations: Vec<SharedDictation>,
    meetings: Vec<SharedMeeting>,
}

fn page_start(cursor: Option<&str>) -> usize {
    cursor.and_then(|value| value.parse().ok()).unwrap_or(0)
}

#[async_trait]
impl RemoteDictationLibrary for PagedHub {
    async fn page_dictations(
        &self,
        _hub: &HubConnection,
        _query: Option<&str>,
        _from: Option<&str>,
        _to: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DictationPage, HubTransferError> {
        let start = page_start(cursor);
        let end = start.saturating_add(limit).min(self.dictations.len());
        Ok(DictationPage {
            items: self.dictations[start..end].to_vec(),
            next_cursor: (end < self.dictations.len()).then(|| end.to_string()),
        })
    }
}

#[async_trait]
impl RemoteMeetingLibrary for PagedHub {
    async fn page_meetings(
        &self,
        _hub: &HubConnection,
        _query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeetingPage, HubTransferError> {
        let start = page_start(cursor);
        let end = start.saturating_add(limit).min(self.meetings.len());
        Ok(MeetingPage {
            items: self.meetings[start..end].to_vec(),
            next_cursor: (end < self.meetings.len()).then(|| end.to_string()),
        })
    }

    async fn meeting(
        &self,
        _hub: &HubConnection,
        id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError> {
        Ok(self
            .meetings
            .iter()
            .find(|item| item.record_id == id)
            .cloned())
    }
}

#[async_trait]
impl RemoteLibraryMutations for PagedHub {
    async fn update_meeting_title(
        &self,
        _hub: &HubConnection,
        id: RecordId,
        patch: crate::sync::protocol::MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError> {
        let mut meeting = self
            .meetings
            .iter()
            .find(|item| item.record_id == id)
            .cloned()
            .ok_or_else(|| HubTransferError::Http {
                status: 404,
                message: "meeting not found".into(),
                retry_after: None,
            })?;
        if meeting.title_version != patch.expected_title_version {
            return Err(HubTransferError::Http {
                status: 409,
                message: "meeting title changed; reload and retry".into(),
                retry_after: None,
            });
        }
        meeting.title = Some(patch.title);
        meeting.title_source = Some(patch.title_source.unwrap_or_else(|| "manual".into()));
        meeting.title_version += 1;
        Ok(meeting)
    }

    async fn delete_record(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _kind: RecordKind,
    ) -> Result<(), HubTransferError> {
        Ok(())
    }
}

struct UnusedChangeSource;

#[async_trait]
impl HubChangeSource for UnusedChangeSource {
    async fn page_changes(
        &self,
        _hub: &HubConnection,
        _after: ChangeCursor,
        _target: Option<ChangeTarget>,
        _limit: usize,
    ) -> Result<ChangePage, HubTransferError> {
        Err(HubTransferError::Retryable("unused".to_owned()))
    }
}

fn connected_library<T>(path: std::path::PathBuf, remote: Arc<T>) -> SharedLibrary
where
    T: RemoteDictationLibrary + RemoteMeetingLibrary + RemoteLibraryMutations + 'static,
{
    let network = Arc::new(crate::sync::client::NetworkHubAdapter::default());
    let capabilities = HubCapabilities::for_test(
        network.clone(),
        network.clone(),
        remote.clone(),
        remote.clone(),
        remote,
        network,
        Arc::new(UnusedChangeSource),
    );
    crate::sync::SyncService::with_dependencies(
        path,
        Arc::new(crate::sync::tailscale::Tailscale::new(
            crate::sync::tailscale::SystemCommandRunner,
        )),
        capabilities,
        "127.0.0.1:0".parse().unwrap(),
    )
    .library()
}

struct ScriptedPagingHub {
    dictation_pages: Vec<DictationPage>,
    meeting_pages: Vec<MeetingPage>,
}

struct PausedTitleHub {
    meeting: SharedMeeting,
    mutation_started: Arc<tokio::sync::Notify>,
    mutation_release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl RemoteDictationLibrary for PausedTitleHub {
    async fn page_dictations(
        &self,
        _hub: &HubConnection,
        _query: Option<&str>,
        _from: Option<&str>,
        _to: Option<&str>,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<DictationPage, HubTransferError> {
        Ok(DictationPage {
            items: vec![],
            next_cursor: None,
        })
    }
}

#[async_trait]
impl RemoteMeetingLibrary for PausedTitleHub {
    async fn page_meetings(
        &self,
        _hub: &HubConnection,
        _query: Option<&str>,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<MeetingPage, HubTransferError> {
        Ok(MeetingPage {
            items: vec![self.meeting.clone()],
            next_cursor: None,
        })
    }

    async fn meeting(
        &self,
        _hub: &HubConnection,
        id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError> {
        Ok((self.meeting.record_id == id).then(|| self.meeting.clone()))
    }
}

#[async_trait]
impl RemoteLibraryMutations for PausedTitleHub {
    async fn update_meeting_title(
        &self,
        _hub: &HubConnection,
        id: RecordId,
        patch: crate::sync::protocol::MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError> {
        self.mutation_started.notify_one();
        self.mutation_release.notified().await;
        let mut meeting = self.meeting.clone();
        meeting.record_id = id;
        meeting.title = Some(patch.title);
        meeting.title_source = Some(patch.title_source.unwrap_or_else(|| "manual".into()));
        meeting.title_version = patch.expected_title_version + 1;
        meeting.updated_at = "2026-09-05T12:30:00Z".into();
        Ok(meeting)
    }

    async fn delete_record(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _kind: RecordKind,
    ) -> Result<(), HubTransferError> {
        Ok(())
    }
}

#[async_trait]
impl RemoteDictationLibrary for ScriptedPagingHub {
    async fn page_dictations(
        &self,
        _hub: &HubConnection,
        _query: Option<&str>,
        _from: Option<&str>,
        _to: Option<&str>,
        cursor: Option<&str>,
        _limit: usize,
    ) -> Result<DictationPage, HubTransferError> {
        let index = page_start(cursor);
        Ok(self.dictation_pages[index].clone())
    }
}

#[async_trait]
impl RemoteMeetingLibrary for ScriptedPagingHub {
    async fn page_meetings(
        &self,
        _hub: &HubConnection,
        _query: Option<&str>,
        cursor: Option<&str>,
        _limit: usize,
    ) -> Result<MeetingPage, HubTransferError> {
        let index = page_start(cursor);
        Ok(self.meeting_pages[index].clone())
    }

    async fn meeting(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError> {
        Ok(None)
    }
}

#[async_trait]
impl RemoteLibraryMutations for ScriptedPagingHub {
    async fn update_meeting_title(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _patch: crate::sync::protocol::MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError> {
        Err(HubTransferError::Transport("not implemented".into()))
    }

    async fn delete_record(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _kind: RecordKind,
    ) -> Result<(), HubTransferError> {
        Err(HubTransferError::Transport("not implemented".into()))
    }
}

fn set_connected(conn: &rusqlite::Connection) {
    let settings = crate::db::sync_settings::SyncSettings {
        role: SyncRole::ConnectedDevice,
        hub: Some(hub()),
        ..Default::default()
    };
    crate::db::sync_settings::SyncSettingsRepository::save(conn, &settings).unwrap();
}

fn mask(conn: &rusqlite::Connection, id: RecordId, kind: RecordKind) {
    SyncOutboxRepository::enqueue_snapshot(
        conn,
        &DeleteSnapshot {
            kind,
            schema_version: 1,
            record_id: id,
            origin_device_id: audetic_core::sync::DeviceId::new(),
            local_version: 2,
            deleted_at: "2026-09-05T12:00:00Z".into(),
        }
        .into(),
    )
    .unwrap();
}

fn shared_dictation(index: usize) -> SharedDictation {
    let order = 999 - index;
    SharedDictation {
        record_id: RecordId::new(),
        origin_device_id: audetic_core::sync::DeviceId::new(),
        text: format!("dictation-{index:03}"),
        created_at: format!("2026-09-05T{order:03}:00:00Z"),
        updated_at: format!("2026-09-05T{order:03}:00:00Z"),
        local_version: 1,
        authoritative_revision: 1,
        recording_payload: RecordingPayloadDescriptor::unavailable(),
    }
}

fn shared_meeting(index: usize) -> SharedMeeting {
    let order = 999 - index;
    SharedMeeting {
        record_id: RecordId::new(),
        origin_device_id: audetic_core::sync::DeviceId::new(),
        title: Some(format!("Meeting {index:03}")),
        title_source: Some("manual".into()),
        title_version: 1,
        source_filename: None,
        transcript_text: "transcript".into(),
        transcript_segments: None,
        duration_seconds: 1,
        status: "completed".into(),
        created_at: format!("2026-09-05T{order:03}:00:00Z"),
        updated_at: format!("2026-09-05T{order:03}:00:00Z"),
        completed_at: format!("2026-09-05T{order:03}:00:00Z"),
        local_version: 1,
        authoritative_revision: 1,
        recording_payload: RecordingPayloadDescriptor::unavailable(),
        artifacts: vec![],
    }
}

#[tokio::test]
async fn remote_dictation_paging_skips_more_than_two_full_pages_of_masks() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("dictations.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    set_connected(&conn);
    let items = (0..340).map(shared_dictation).collect::<Vec<_>>();
    for item in &items[..220] {
        mask(&conn, item.record_id, RecordKind::Dictation);
    }
    drop(conn);
    let library = connected_library(
        path,
        Arc::new(PagedHub {
            dictations: items.clone(),
            meetings: vec![],
        }),
    );

    let page = library
        .dictations(&crate::history::SearchParams::new().with_limit(100))
        .await
        .unwrap();

    assert_eq!(page.len(), 100);
    assert_eq!(page[0].id, items[220].record_id);
    assert_eq!(page[99].id, items[319].record_id);
}

#[test]
fn home_hub_sql_applies_deletion_masks_before_its_page_limit() {
    let temp = tempfile::tempdir().unwrap();
    let mut conn = crate::db::migrate_db_at(&temp.path().join("home-hub.sqlite")).unwrap();
    let origin = audetic_core::sync::DeviceId::new();
    let snapshots = (0..340)
        .map(|index| {
            let timestamp =
                (chrono::Utc::now() - chrono::Duration::seconds(index as i64)).to_rfc3339();
            DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: RecordId::new(),
                origin_device_id: origin,
                local_version: 1,
                created_at: timestamp.clone(),
                updated_at: timestamp,
                payload: DictationPayload {
                    text: format!("authoritative-{index:03}"),
                    recording_payload: RecordingPayloadDescriptor::unavailable(),
                },
            }
        })
        .collect::<Vec<_>>();
    for snapshot in &snapshots {
        crate::db::shared_library::SharedLibraryRepository::apply_snapshot(&mut conn, snapshot)
            .unwrap();
    }
    for snapshot in &snapshots[..220] {
        mask(&conn, snapshot.record_id, RecordKind::Dictation);
    }

    let page = crate::db::shared_library::SharedLibraryRepository::page_dictations(
        &conn, None, None, None, None, 100,
    )
    .unwrap();

    assert_eq!(page.len(), 100);
    assert_eq!(page[0].record_id, snapshots[220].record_id);
    assert_eq!(page[99].record_id, snapshots[319].record_id);
}

#[tokio::test]
async fn remote_meeting_paging_skips_more_than_two_full_pages_of_masks() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("meetings.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    set_connected(&conn);
    let items = (0..340).map(shared_meeting).collect::<Vec<_>>();
    for item in &items[..220] {
        mask(&conn, item.record_id, RecordKind::Meeting);
    }
    drop(conn);
    let library = connected_library(
        path,
        Arc::new(PagedHub {
            dictations: vec![],
            meetings: items.clone(),
        }),
    );

    let page = library
        .meetings(MeetingPageRequest {
            query: None,
            offset: 0,
            limit: 100,
        })
        .await
        .unwrap();

    assert_eq!(page.len(), 100);
    assert_eq!(page[0].id, items[220].record_id);
    assert_eq!(page[99].id, items[319].record_id);
}

#[tokio::test]
async fn direct_history_uuid_lookup_walks_beyond_masked_remote_pages() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("lookup.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    set_connected(&conn);
    let items = (0..340).map(shared_dictation).collect::<Vec<_>>();
    for item in &items[..220] {
        mask(&conn, item.record_id, RecordKind::Dictation);
    }
    let target = items[339].record_id;
    drop(conn);
    let library = connected_library(
        path,
        Arc::new(PagedHub {
            dictations: items,
            meetings: vec![],
        }),
    );

    let entry = library.dictation(target).await.unwrap();

    assert_eq!(entry.id, target);
    assert_eq!(entry.text, "dictation-339");
}

#[tokio::test]
async fn filtered_dictation_paging_counts_distinct_records_across_overlapping_pages() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("overlapping-dictations.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    set_connected(&conn);
    drop(conn);
    let items = (0..100).map(shared_dictation).collect::<Vec<_>>();
    let library = connected_library(
        path,
        Arc::new(ScriptedPagingHub {
            dictation_pages: vec![
                DictationPage {
                    items: items[..60].to_vec(),
                    next_cursor: Some("1".into()),
                },
                DictationPage {
                    items: items[..60].to_vec(),
                    next_cursor: Some("2".into()),
                },
                DictationPage {
                    items: items[60..].to_vec(),
                    next_cursor: None,
                },
            ],
            meeting_pages: vec![],
        }),
    );

    let page = library
        .dictations(&crate::history::SearchParams::new().with_limit(100))
        .await
        .unwrap();

    assert_eq!(page.len(), 100);
}

#[tokio::test]
async fn filtered_meeting_paging_counts_distinct_records_across_overlapping_pages() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("overlapping-meetings.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    set_connected(&conn);
    drop(conn);
    let items = (0..100).map(shared_meeting).collect::<Vec<_>>();
    let library = connected_library(
        path,
        Arc::new(ScriptedPagingHub {
            dictation_pages: vec![],
            meeting_pages: vec![
                MeetingPage {
                    items: items[..60].to_vec(),
                    next_cursor: Some("1".into()),
                },
                MeetingPage {
                    items: items[..60].to_vec(),
                    next_cursor: Some("2".into()),
                },
                MeetingPage {
                    items: items[60..].to_vec(),
                    next_cursor: None,
                },
            ],
        }),
    );

    let page = library
        .meetings(MeetingPageRequest {
            query: Some("transcript".into()),
            offset: 0,
            limit: 100,
        })
        .await
        .unwrap();

    assert_eq!(page.len(), 100);
}

#[tokio::test]
async fn filtered_remote_paging_rejects_repeated_cursors() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("repeated-cursor.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    set_connected(&conn);
    drop(conn);
    let item = shared_dictation(0);
    let library = connected_library(
        path,
        Arc::new(ScriptedPagingHub {
            dictation_pages: vec![
                DictationPage {
                    items: vec![item.clone()],
                    next_cursor: Some("1".into()),
                },
                DictationPage {
                    items: vec![item],
                    next_cursor: Some("1".into()),
                },
            ],
            meeting_pages: vec![],
        }),
    );

    let error = library
        .dictations(&crate::history::SearchParams::new().with_limit(100))
        .await
        .unwrap_err();

    assert!(matches!(error, LibraryError::Unavailable(_)));
}

#[tokio::test]
async fn filtered_meeting_paging_rejects_repeated_cursors() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("repeated-meeting-cursor.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    set_connected(&conn);
    drop(conn);
    let item = shared_meeting(0);
    let library = connected_library(
        path,
        Arc::new(ScriptedPagingHub {
            dictation_pages: vec![],
            meeting_pages: vec![
                MeetingPage {
                    items: vec![item.clone()],
                    next_cursor: Some("1".into()),
                },
                MeetingPage {
                    items: vec![item],
                    next_cursor: Some("1".into()),
                },
            ],
        }),
    );

    let error = library
        .meetings(MeetingPageRequest {
            query: Some("transcript".into()),
            offset: 0,
            limit: 100,
        })
        .await
        .unwrap_err();

    assert!(matches!(error, LibraryError::Unavailable(_)));
}

#[tokio::test]
async fn direct_history_lookup_rejects_cyclic_cursors() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("direct-cursor.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    set_connected(&conn);
    drop(conn);
    let item = shared_dictation(0);
    let library = connected_library(
        path,
        Arc::new(ScriptedPagingHub {
            dictation_pages: vec![
                DictationPage {
                    items: vec![item.clone()],
                    next_cursor: Some("1".into()),
                },
                DictationPage {
                    items: vec![item],
                    next_cursor: Some("1".into()),
                },
            ],
            meeting_pages: vec![],
        }),
    );

    let error = library.dictation(RecordId::new()).await.unwrap_err();

    assert!(matches!(error, LibraryError::Unavailable(_)));
}

#[tokio::test]
async fn authoritative_title_is_mirrored_without_origin_publication() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("title.sqlite");
    let mut conn = crate::db::migrate_db_at(&path).unwrap();
    let local_id =
        crate::db::meetings::MeetingRepository::insert(&conn, Some("Origin title"), "/missing.wav")
            .unwrap();
    crate::db::meetings::MeetingRepository::complete(
        &conn,
        local_id,
        "/missing.txt",
        "transcript",
        None,
        10,
    )
    .unwrap();
    let original = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    crate::db::shared_library::SharedLibraryRepository::apply_meeting_snapshot(
        &mut conn,
        &original.snapshot().unwrap(),
    )
    .unwrap();
    crate::db::sync_settings::SyncSettingsRepository::save(
        &conn,
        &crate::db::sync_settings::SyncSettings {
            role: SyncRole::HomeHub,
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);
    let library = crate::sync::SyncService::production(path.clone()).library();

    let result = library
        .update_meeting_title(original.sync_id, "Authoritative title".into())
        .await
        .unwrap();

    assert_eq!(result.local_id, Some(local_id));
    let conn = crate::db::open_db_at(&path).unwrap();
    let mirrored = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    assert_eq!(mirrored.title.as_deref(), Some("Authoritative title"));
    assert_eq!(mirrored.title_source.as_deref(), Some("manual"));
    let authoritative =
        crate::db::shared_library::SharedLibraryRepository::get_meeting(&conn, original.sync_id)
            .unwrap()
            .unwrap();
    assert_eq!(mirrored.title_version as u64, authoritative.title_version);
    assert_eq!(mirrored.sync_version, original.sync_version);
    let competing: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_outbox_items WHERE record_id=?1 AND kind='meeting'",
            [original.sync_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(competing, 0);
    assert_eq!(
        library.recent_meeting_titles(10).unwrap(),
        vec!["Authoritative title"]
    );
    let seeded = mirrored.snapshot().unwrap();
    assert_eq!(seeded.payload.title.as_deref(), Some("Authoritative title"));
    assert_eq!(seeded.payload.title_version, authoritative.title_version);

    crate::db::sync_settings::SyncSettingsRepository::save(
        &conn,
        &crate::db::sync_settings::SyncSettings::default(),
    )
    .unwrap();
    drop(conn);
    let standalone = library.meeting(original.sync_id).await.unwrap();
    assert_eq!(standalone.title.as_deref(), Some("Authoritative title"));
    assert_eq!(standalone.title_version, authoritative.title_version);
}

#[test]
fn authoritative_title_and_local_mirror_roll_back_together() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("title-rollback.sqlite");
    let mut conn = crate::db::migrate_db_at(&path).unwrap();
    let local_id = crate::db::meetings::MeetingRepository::insert(
        &conn,
        Some("Original title"),
        "/missing.wav",
    )
    .unwrap();
    crate::db::meetings::MeetingRepository::complete(
        &conn,
        local_id,
        "/missing.txt",
        "transcript",
        None,
        10,
    )
    .unwrap();
    let original = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    crate::db::shared_library::SharedLibraryRepository::apply_meeting_snapshot(
        &mut conn,
        &original.snapshot().unwrap(),
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_title_change
         BEFORE INSERT ON shared_library_changes
         BEGIN SELECT RAISE(FAIL, 'reject title publication'); END;",
    )
    .unwrap();

    let result = crate::db::shared_library::SharedLibraryRepository::compare_and_set_meeting_title(
        &mut conn,
        original.sync_id,
        &crate::sync::protocol::MeetingTitlePatch {
            title: "Must roll back".into(),
            expected_title_version: original.title_version as u64,
            title_source: None,
        },
    );

    assert!(result.is_err());
    let local = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    let shared =
        crate::db::shared_library::SharedLibraryRepository::get_meeting(&conn, original.sync_id)
            .unwrap()
            .unwrap();
    assert_eq!(local.title.as_deref(), Some("Original title"));
    assert_eq!(shared.title.as_deref(), Some("Original title"));
    assert_eq!(local.title_version, original.title_version);
    assert_eq!(shared.title_version, original.title_version as u64);
}

#[tokio::test]
async fn connected_authoritative_title_response_updates_the_local_mirror() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("connected-title.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    let local_id = crate::db::meetings::MeetingRepository::insert(
        &conn,
        Some("Old local title"),
        "/missing.wav",
    )
    .unwrap();
    crate::db::meetings::MeetingRepository::complete(
        &conn,
        local_id,
        "/missing.txt",
        "transcript",
        None,
        10,
    )
    .unwrap();
    let original = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    conn.execute(
        "UPDATE meetings SET title_version=5 WHERE id=?1",
        [local_id],
    )
    .unwrap();
    set_connected(&conn);
    drop(conn);
    let mut authoritative = shared_meeting(0);
    authoritative.record_id = original.sync_id;
    authoritative.origin_device_id = original.origin_device_id;
    authoritative.title = Some("Old authoritative title".into());
    authoritative.title_version = 5;
    let library = connected_library(
        path.clone(),
        Arc::new(PagedHub {
            dictations: vec![],
            meetings: vec![authoritative],
        }),
    );

    let result = library
        .update_meeting_title(original.sync_id, "Connected title".into())
        .await
        .unwrap();

    assert_eq!(result.local_id, Some(local_id));
    let conn = crate::db::open_db_at(&path).unwrap();
    let mirrored = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    assert_eq!(mirrored.title.as_deref(), Some("Connected title"));
    assert_eq!(mirrored.title_source.as_deref(), Some("manual"));
    assert_eq!(mirrored.title_version, 6);
    assert_eq!(mirrored.sync_version, original.sync_version);
    let outbox_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_outbox_items WHERE record_id=?1",
            [original.sync_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outbox_count, 0);
}

#[tokio::test]
async fn connected_title_commit_survives_local_mirror_failure_and_records_repair_health() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("connected-title-mirror-failure.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    let local_id = crate::db::meetings::MeetingRepository::insert(
        &conn,
        Some("Old local title"),
        "/missing.wav",
    )
    .unwrap();
    let original = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    conn.execute(
        "UPDATE meetings SET title_version=5 WHERE id=?1",
        [local_id],
    )
    .unwrap();
    set_connected(&conn);
    drop(conn);
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut authoritative = shared_meeting(0);
    authoritative.record_id = original.sync_id;
    authoritative.title_version = 5;
    let library = connected_library(
        path.clone(),
        Arc::new(PausedTitleHub {
            meeting: authoritative,
            mutation_started: started.clone(),
            mutation_release: release.clone(),
        }),
    );
    let task = tokio::spawn(async move {
        library
            .update_meeting_title(original.sync_id, "Committed remotely".into())
            .await
    });
    started.notified().await;
    let conn = crate::db::open_db_at(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_connected_title_mirror
         BEFORE UPDATE OF title ON meetings
         BEGIN SELECT RAISE(FAIL, 'mirror unavailable'); END;",
    )
    .unwrap();
    drop(conn);
    release.notify_one();

    let result = task.await.unwrap().unwrap();

    assert_eq!(result.title.as_deref(), Some("Committed remotely"));
    assert_eq!(result.local_id, None);
    let conn = crate::db::open_db_at(&path).unwrap();
    let local = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    assert_eq!(local.title.as_deref(), Some("Old local title"));
    let health = crate::db::sync_settings::SyncSettingsRepository::get(&conn)
        .unwrap()
        .last_error
        .unwrap();
    assert!(health.contains("local mirror repair is required"));
}

#[tokio::test]
async fn old_hub_title_response_cannot_overwrite_newer_local_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("stale-connected-title.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    let local_id = crate::db::meetings::MeetingRepository::insert(
        &conn,
        Some("Old local title"),
        "/missing.wav",
    )
    .unwrap();
    let original = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    conn.execute(
        "UPDATE meetings SET title_version=5 WHERE id=?1",
        [local_id],
    )
    .unwrap();
    set_connected(&conn);
    drop(conn);
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut authoritative = shared_meeting(0);
    authoritative.record_id = original.sync_id;
    authoritative.title_version = 5;
    let library = connected_library(
        path.clone(),
        Arc::new(PausedTitleHub {
            meeting: authoritative,
            mutation_started: started.clone(),
            mutation_release: release.clone(),
        }),
    );
    let task = tokio::spawn(async move {
        library
            .update_meeting_title(original.sync_id, "Old hub receipt".into())
            .await
    });
    started.notified().await;
    let conn = crate::db::open_db_at(&path).unwrap();
    conn.execute(
        "UPDATE meetings SET title='Newer local title',title_version=6 WHERE id=?1",
        [local_id],
    )
    .unwrap();
    conn.execute(
        "UPDATE sync_settings SET hub_id=?1 WHERE singleton=1",
        [audetic_core::sync::HubId::new().to_string()],
    )
    .unwrap();
    drop(conn);
    release.notify_one();

    let result = task.await.unwrap().unwrap();

    assert_eq!(result.title.as_deref(), Some("Old hub receipt"));
    assert_eq!(result.local_id, None);
    let conn = crate::db::open_db_at(&path).unwrap();
    let local = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap();
    assert_eq!(local.title.as_deref(), Some("Newer local title"));
    assert_eq!(local.title_version, 6);
}

#[tokio::test]
async fn title_regeneration_validation_is_typed_as_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("title-validation.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    let local_id =
        crate::db::meetings::MeetingRepository::insert(&conn, None, "/missing.wav").unwrap();
    let record_id = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap()
        .sync_id;
    drop(conn);
    let library = crate::sync::SyncService::local_library(path).library();

    let error = library
        .regenerate_meeting_title(record_id)
        .await
        .unwrap_err();

    assert!(matches!(error, LibraryError::Conflict(_)));
}

#[tokio::test]
async fn unavailable_title_profile_is_typed_as_conflict() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("title-profile-validation.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    let local_id =
        crate::db::meetings::MeetingRepository::insert(&conn, None, "/missing.wav").unwrap();
    crate::db::meetings::MeetingRepository::complete(
        &conn,
        local_id,
        "/missing.txt",
        "transcript",
        None,
        1,
    )
    .unwrap();
    crate::db::agent_profiles::AgentProfileRepository::ensure_builtin_profiles(&conn).unwrap();
    conn.execute("UPDATE agent_profiles SET enabled=0", [])
        .unwrap();
    conn.execute(
        "INSERT INTO agent_profiles
         (name,kind,executable,args_json,prompt_mode,default_profile,enabled)
         VALUES('Missing','missing','definitely-not-an-audetic-agent','[]','stdin',1,1)",
        [],
    )
    .unwrap();
    let record_id = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap()
        .sync_id;
    drop(conn);
    let library = crate::sync::SyncService::local_library(path).library();

    let error = library
        .regenerate_meeting_title(record_id)
        .await
        .unwrap_err();

    assert!(matches!(error, LibraryError::Conflict(_)));
}

#[tokio::test]
async fn artifact_workflow_validation_is_typed_as_invalid() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("artifact-validation.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    let local_id =
        crate::db::meetings::MeetingRepository::insert(&conn, None, "/missing.wav").unwrap();
    crate::db::meetings::MeetingRepository::complete(
        &conn,
        local_id,
        "/missing.txt",
        "transcript",
        None,
        1,
    )
    .unwrap();
    let record_id = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap()
        .sync_id;
    drop(conn);
    let library = crate::sync::SyncService::local_library(path).library();

    let error = library
        .generate_artifact(
            record_id,
            crate::meeting_artifacts::GenerateArtifactRequest {
                kind: "summary".into(),
                template_id: "unknown-template".into(),
                agent_profile_id: None,
                custom_context: None,
            },
        )
        .await
        .unwrap_err();

    assert!(matches!(error, LibraryError::Invalid(_)));
}

#[tokio::test]
async fn unavailable_artifact_profile_is_typed_as_invalid() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("artifact-profile-validation.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    let local_id =
        crate::db::meetings::MeetingRepository::insert(&conn, None, "/missing.wav").unwrap();
    crate::db::meetings::MeetingRepository::complete(
        &conn,
        local_id,
        "/missing.txt",
        "transcript",
        None,
        1,
    )
    .unwrap();
    conn.execute(
        "INSERT INTO agent_profiles
         (name,kind,executable,args_json,prompt_mode,default_profile,enabled)
         VALUES('Missing','missing','definitely-not-an-audetic-agent','[]','stdin',0,1)",
        [],
    )
    .unwrap();
    let profile_id = conn.last_insert_rowid();
    let record_id = crate::db::meetings::MeetingRepository::get(&conn, local_id)
        .unwrap()
        .unwrap()
        .sync_id;
    drop(conn);
    let library = crate::sync::SyncService::local_library(path).library();

    let error = library
        .generate_artifact(
            record_id,
            crate::meeting_artifacts::GenerateArtifactRequest {
                kind: "summary".into(),
                template_id: "standard_meeting".into(),
                agent_profile_id: Some(profile_id),
                custom_context: None,
            },
        )
        .await
        .unwrap_err();

    assert!(matches!(error, LibraryError::Invalid(_)));
}

#[tokio::test]
async fn operational_payload_stream_bounds_ranges() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("payload.wav");
    std::fs::write(&path, b"0123456789").unwrap();

    let payload = super::payload::open_operational_payload_for_test(path, Some("bytes=2-5"))
        .await
        .unwrap();

    assert_eq!(payload.status, 206);
    assert_eq!(payload.metadata.content_length, Some(4));
}

#[tokio::test]
async fn operational_payload_stream_reports_unsatisfied_ranges() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("payload.wav");
    std::fs::write(&path, b"short").unwrap();

    let payload = super::payload::open_operational_payload_for_test(path, Some("bytes=20-30"))
        .await
        .unwrap();

    assert_eq!(payload.status, 416);
    assert_eq!(
        payload.metadata.content_range,
        Some(crate::sync::transport::PayloadContentRange::Unsatisfied { complete_length: 5 })
    );
}

#[tokio::test]
async fn direct_local_meeting_lookup_is_not_limited_by_list_page_size() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("direct-meeting.sqlite");
    let conn = crate::db::migrate_db_at(&path).unwrap();
    let mut target = None;
    for index in 0..120 {
        let local_id = crate::db::meetings::MeetingRepository::insert(
            &conn,
            Some(&format!("Meeting {index}")),
            "/missing.wav",
        )
        .unwrap();
        crate::db::meetings::MeetingRepository::complete(
            &conn,
            local_id,
            "/missing.txt",
            "transcript",
            None,
            1,
        )
        .unwrap();
        if index == 0 {
            target = Some(
                crate::db::meetings::MeetingRepository::get(&conn, local_id)
                    .unwrap()
                    .unwrap()
                    .sync_id,
            );
        }
    }
    drop(conn);
    let library = crate::sync::SyncService::local_library(path).library();

    let meeting = library.meeting(target.unwrap()).await.unwrap();

    assert_eq!(meeting.id, target.unwrap());
}
