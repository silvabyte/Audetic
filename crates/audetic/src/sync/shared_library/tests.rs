use async_trait::async_trait;
use audetic_core::sync::{HubConnection, HubId, RecordId, SyncRole};

use std::sync::Arc;

use crate::db::sync_outbox::SyncOutboxRepository;
use crate::sync::protocol::{
    DeleteSnapshot, DictationPage, DictationPayload, DictationSnapshot, MeetingPage, RecordKind,
    RecordingPayloadDescriptor, SharedDictation, SharedMeeting,
};
use crate::sync::transport::{
    HubCapabilities, HubTransferError, RemoteDictationLibrary, RemoteLibraryMutations,
    RemoteMeetingLibrary,
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

fn connected_library(path: std::path::PathBuf, remote: Arc<PagedHub>) -> SharedLibrary {
    let network = Arc::new(crate::sync::client::NetworkHubAdapter::default());
    let capabilities = HubCapabilities::for_test(
        network.clone(),
        network.clone(),
        remote.clone(),
        remote.clone(),
        remote,
        network,
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
