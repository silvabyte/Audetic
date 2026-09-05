use audetic_core::sync::{CacheLevel, HubId, PayloadAvailability, SyncRole, SyncSetupRequest};
use axum::http::Method;
use futures_util::TryStreamExt;
use sha2::{Digest, Sha256};
use std::time::Duration;

use crate::db::meeting_artifacts::MeetingArtifactRepository;
use crate::db::meetings::MeetingRepository;
use crate::history::SearchParams;
use crate::sync::client::HandshakeExpectation;
use crate::sync::observer::WorkerEvent;
use crate::sync::protocol::{MeetingTitlePatch, SnapshotBatch, SnapshotDisposition};
use crate::sync::tailscale::MappingState;

use super::{HomeHubTopology, OperationFault};

#[tokio::test]
async fn slice_1_activation_discovery_fail_closed_and_restart_reconstructs_one_runtime() {
    let topology = HomeHubTopology::new();
    let mut hub = topology.daemon("hub", "Alice@Example.com");
    let device = topology.daemon("laptop", "Alice@Example.com");
    hub.start().await;
    device.start().await;

    let connection = hub.activate_hub(false).await;
    assert_eq!(topology.tailnet.published_router_count("hub"), 1);
    let discovery = device.discover().await;
    assert_eq!(discovery.discovered_hubs.len(), 1);
    assert_eq!(discovery.discovered_hubs[0].connection, connection);
    device.connect(connection.clone(), false).await;

    let outsider = topology.daemon("outsider", "Bob@Example.com");
    outsider.start().await;
    let wrong_login = outsider.discover().await;
    assert!(wrong_login.discovered_hubs.is_empty());
    assert!(!wrong_login.discovery_failures.is_empty());

    let spoofing_client = crate::sync::client::HubClient::with_transport(
        &connection.base_url,
        topology
            .tailnet
            .spoofing_transport("outsider", "Alice@Example.com"),
    )
    .unwrap();
    assert!(spoofing_client
        .handshake(HandshakeExpectation::default())
        .await
        .unwrap_err()
        .to_string()
        .contains("403"));
    // This proves the application-side trust model only. Real Tailscale must
    // still be smoke-tested to ensure its trusted proxy overwrites identity.

    topology.tailnet.add_node("tagged", "Alice@Example.com");
    topology.tailnet.set_tagged("tagged", true);
    topology.tailnet.add_node("offline", "Alice@Example.com");
    topology.tailnet.set_online("offline", false);
    let filtered = device.discover().await;
    assert_eq!(filtered.discovered_hubs.len(), 1);
    assert!(filtered
        .discovery_failures
        .iter()
        .all(|failure| !failure.candidate.contains("tagged")
            && !failure.candidate.contains("offline")));

    let client = device.direct_client("hub");
    let wrong_hub = client
        .handshake(HandshakeExpectation {
            hub_id: Some(HubId::new()),
            owner_login: Some("Alice@Example.com"),
        })
        .await
        .unwrap_err();
    assert!(wrong_hub.to_string().contains("expected Home Hub"));

    topology.tailnet.fault(
        "laptop",
        "hub",
        Method::GET,
        "/audetic/v1/info",
        OperationFault::OverrideProtocol(Some("99".into())),
    );
    assert!(client
        .handshake(HandshakeExpectation::default())
        .await
        .unwrap_err()
        .to_string()
        .contains("426"));
    topology.tailnet.fault(
        "laptop",
        "hub",
        Method::GET,
        "/audetic/v1/info",
        OperationFault::OverrideIdentity(None),
    );
    assert!(client
        .handshake(HandshakeExpectation::default())
        .await
        .unwrap_err()
        .to_string()
        .contains("403"));
    topology.tailnet.fault(
        "laptop",
        "hub",
        Method::GET,
        "/audetic/v1/info",
        OperationFault::FunnelRequest,
    );
    assert!(client
        .handshake(HandshakeExpectation::default())
        .await
        .unwrap_err()
        .to_string()
        .contains("403"));

    let blocked = topology.daemon("blocked-hub", "Alice@Example.com");
    blocked.start().await;
    topology.tailnet.set_funnel("blocked-hub", true);
    let error = blocked
        .service()
        .configure(SyncSetupRequest {
            role: SyncRole::HomeHub,
            device_name: None,
            hub: None,
            upload_recording_payloads: false,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: true,
            confirm_serve_change: true,
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Funnel"));
    assert_eq!(topology.tailnet.published_router_count("blocked-hub"), 0);
    topology.tailnet.set_funnel("blocked-hub", false);
    topology
        .tailnet
        .set_mapping("blocked-hub", MappingState::Collision);
    assert!(blocked
        .service()
        .configure(SyncSetupRequest {
            role: SyncRole::HomeHub,
            device_name: None,
            hub: None,
            upload_recording_payloads: false,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: true,
            confirm_serve_change: true,
        })
        .await
        .unwrap_err()
        .to_string()
        .contains("mapping"));

    hub.restart().await;
    assert_eq!(topology.tailnet.published_router_count("hub"), 1);
    assert!(hub.probe.events().iter().any(|event| matches!(
        event,
        WorkerEvent::ListenerStopped { role_epoch } if *role_epoch == hub.role_epoch()
    )));
    assert!(hub
        .probe
        .events()
        .iter()
        .any(|event| matches!(event, WorkerEvent::OutboxStopped { .. })));
}

#[tokio::test]
async fn slice_4_payload_staging_integrity_ranges_retries_and_cancellation() {
    let topology = HomeHubTopology::new();
    let hub = topology.daemon("hub", "owner@example.com");
    let mut device = topology.daemon("device", "owner@example.com");
    hub.start().await;
    device.start().await;
    let hub_connection = hub.activate_hub(true).await;
    device.connect(hub_connection.clone(), true).await;
    hub.wait_for_cycle().await;
    device.wait_for_cycle().await;

    let payload = b"0123456789-payload";
    let (_local_id, record_id) = device.insert_dictation("payload", Some(payload));
    let (staged_path, checksum): (String, String) = device
        .connection()
        .query_row(
            "SELECT staged_path,checksum FROM sync_outbox_blobs WHERE record_id=?1",
            [record_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(std::path::Path::new(&staged_path).is_file());
    std::fs::remove_file(device.root.join("payload.wav")).unwrap();
    device.drive_cycle().await;

    let client = device.direct_client("hub");
    assert!(client
        .head_blob(hub_connection.hub_id, &checksum)
        .await
        .unwrap());
    let page = client
        .page_dictations(hub_connection.hub_id, None, None, None, None, 10)
        .await
        .unwrap();
    let uploaded = page
        .items
        .iter()
        .find(|item| item.record_id == record_id)
        .unwrap();
    assert_eq!(
        uploaded.recording_payload.availability,
        PayloadAvailability::Available
    );

    let full = client
        .stream_payload(
            hub_connection.hub_id,
            crate::sync::protocol::RecordKind::Dictation,
            record_id,
            None,
        )
        .await
        .unwrap();
    assert_eq!(full.status, 200);
    let full_chunks: Vec<_> = full.body.try_collect().await.unwrap();
    assert_eq!(full_chunks.concat(), payload);

    let partial = client
        .stream_payload(
            hub_connection.hub_id,
            crate::sync::protocol::RecordKind::Dictation,
            record_id,
            Some("bytes=2-6"),
        )
        .await
        .unwrap();
    assert_eq!(partial.status, 206);
    assert_eq!(partial.metadata.content_length, Some(5));
    let partial_chunks: Vec<_> = partial.body.try_collect().await.unwrap();
    assert_eq!(partial_chunks.concat(), b"23456");

    let unsatisfied = client
        .stream_payload(
            hub_connection.hub_id,
            crate::sync::protocol::RecordKind::Dictation,
            record_id,
            Some("bytes=999-1000"),
        )
        .await
        .unwrap();
    assert_eq!(unsatisfied.status, 416);

    let payload_path = crate::sync::protocol::hub_payload_path(
        crate::sync::protocol::RecordKind::Dictation,
        record_id,
    );
    let routed_payload_path = format!("/audetic/{payload_path}");
    topology.tailnet.fault(
        "device",
        "hub",
        Method::GET,
        &routed_payload_path,
        OperationFault::TruncateResponseBody(4),
    );
    let truncated = client
        .stream_payload(
            hub_connection.hub_id,
            crate::sync::protocol::RecordKind::Dictation,
            record_id,
            Some("bytes=0-8"),
        )
        .await
        .unwrap();
    assert!(truncated.body.try_collect::<Vec<_>>().await.is_err());

    topology.tailnet.fault(
        "device",
        "hub",
        Method::GET,
        &routed_payload_path,
        OperationFault::CorruptResponseBody,
    );
    // Playback does not perform an end-to-end digest check. Same-length
    // download integrity is the HTTPS/Tailscale transport boundary; the Hub
    // still authoritatively verifies checksums on upload.
    let transport_mutated_without_digest_guarantee = client
        .stream_payload(
            hub_connection.hub_id,
            crate::sync::protocol::RecordKind::Dictation,
            record_id,
            None,
        )
        .await
        .unwrap();
    let corrupt_chunks: Vec<_> = transport_mutated_without_digest_guarantee
        .body
        .try_collect()
        .await
        .unwrap();
    assert_ne!(corrupt_chunks.concat(), payload);

    let request_file = device.root.join("integrity.wav");
    std::fs::write(&request_file, b"integrity").unwrap();
    let request_checksum = format!("{:x}", Sha256::digest(b"integrity"));
    let blob_path = format!("/audetic/v1/blobs/{request_checksum}");
    for fault in [
        OperationFault::TruncateRequestBody(3),
        OperationFault::CorruptRequestBody,
    ] {
        topology
            .tailnet
            .fault("device", "hub", Method::PUT, &blob_path, fault);
        assert!(client
            .upload_blob(
                hub_connection.hub_id,
                &request_checksum,
                &request_file,
                9,
                "audio/wav",
            )
            .await
            .is_err());
        assert!(!client
            .head_blob(hub_connection.hub_id, &request_checksum)
            .await
            .unwrap());
    }

    let (_local_id, retry_id) = device.insert_dictation("payload retry", Some(b"retryable"));
    let retry_checksum: String = device
        .connection()
        .query_row(
            "SELECT checksum FROM sync_outbox_blobs WHERE record_id=?1",
            [retry_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    topology.tailnet.fault(
        "device",
        "hub",
        Method::PUT,
        &format!("/audetic/v1/blobs/{retry_checksum}"),
        OperationFault::FailBeforeDispatch("upload interrupted"),
    );
    device.drive_failed_cycle().await;
    assert_eq!(
        device
            .connection()
            .query_row(
                "SELECT state FROM sync_outbox_blobs WHERE record_id=?1",
                [retry_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "pending"
    );
    device.drive_cycle_by(Duration::from_secs(301)).await;
    assert!(client
        .head_blob(hub_connection.hub_id, &retry_checksum)
        .await
        .unwrap());

    let large_payload = vec![b'x'; 32 * 1024];
    let (_local_id, cancelled_upload_id) =
        device.insert_dictation("cancelled upload", Some(&large_payload));
    let cancelled_checksum: String = device
        .connection()
        .query_row(
            "SELECT checksum FROM sync_outbox_blobs WHERE record_id=?1",
            [cancelled_upload_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let upload_gate = super::FaultGate::new();
    topology.tailnet.fault(
        "device",
        "hub",
        Method::PUT,
        &format!("/audetic/v1/blobs/{cancelled_checksum}"),
        OperationFault::HoldRequestBodyAfterFirstChunk(upload_gate.clone()),
    );
    let role_epoch = device.role_epoch();
    let successful_before_restart = device.probe.successful_cycles(role_epoch);
    device.begin_cycle_by(Duration::from_secs(2)).await;
    upload_gate.wait_entered().await;
    let temp_root = hub.root.join("sync/blobs/.tmp");
    assert!(std::fs::read_dir(&temp_root).unwrap().next().is_some());
    device.shutdown_for_restart().await;
    upload_gate.wait_cancelled().await;
    assert!(std::fs::read_dir(&temp_root).unwrap().next().is_none());
    assert_eq!(
        device
            .connection()
            .query_row(
                "SELECT state,lease_owner FROM sync_outbox_blobs WHERE record_id=?1",
                [cancelled_upload_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .unwrap(),
        ("pending".into(), None)
    );
    device.reconstruct().await;
    device
        .probe
        .wait_for(|events| {
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        WorkerEvent::OutboxCycleSucceeded {
                            role_epoch: event_epoch
                        } if *event_epoch == role_epoch
                    )
                })
                .count()
                > successful_before_restart
        })
        .await;
    let lifecycle = device.probe.events();
    assert_eq!(
        lifecycle
            .iter()
            .filter(|event| matches!(
                event,
                WorkerEvent::OutboxStarted {
                    role_epoch: event_epoch
                } if *event_epoch == role_epoch
            ))
            .count(),
        2
    );
    assert_eq!(
        lifecycle
            .iter()
            .filter(|event| matches!(
                event,
                WorkerEvent::OutboxStopped {
                    role_epoch: event_epoch
                } if *event_epoch == role_epoch
            ))
            .count(),
        1
    );
    assert!(client
        .head_blob(hub_connection.hub_id, &cancelled_checksum)
        .await
        .unwrap());

    let response_gate = super::FaultGate::new();
    topology.tailnet.fault(
        "device",
        "hub",
        Method::GET,
        &routed_payload_path,
        OperationFault::HoldResponseBodyAfterFirstChunk(response_gate.clone()),
    );
    let held_response = client
        .stream_payload(
            hub_connection.hub_id,
            crate::sync::protocol::RecordKind::Dictation,
            record_id,
            None,
        )
        .await
        .unwrap();
    let response_consumer =
        tokio::spawn(async move { held_response.body.try_collect::<Vec<_>>().await });
    response_gate.wait_entered().await;
    response_consumer.abort();
    assert!(matches!(
        super::watchdog("joining cancelled payload consumer", response_consumer).await,
        Err(error) if error.is_cancelled()
    ));
    response_gate.wait_cancelled().await;

    let gate = super::FaultGate::new();
    topology.tailnet.fault(
        "device",
        "hub",
        Method::GET,
        &routed_payload_path,
        OperationFault::HoldBeforeDispatch(gate.clone()),
    );
    let cancellation_client = device.direct_client("hub");
    let hub_id = hub_connection.hub_id;
    let cancelled = tokio::spawn(async move {
        cancellation_client
            .stream_payload(
                hub_id,
                crate::sync::protocol::RecordKind::Dictation,
                record_id,
                None,
            )
            .await
    });
    gate.wait_entered().await;
    cancelled.abort();
    assert!(matches!(
        super::watchdog("joining pre-dispatch cancellation", cancelled).await,
        Err(error) if error.is_cancelled()
    ));
    gate.release();
}

#[tokio::test]
async fn slice_2_partitioned_creation_and_lost_response_converge_idempotently() {
    let topology = HomeHubTopology::new();
    let hub = topology.daemon("hub", "owner@example.com");
    let mut device = topology.daemon("device", "owner@example.com");
    hub.start().await;
    device.start().await;
    let hub_connection = hub.activate_hub(false).await;
    device.connect(hub_connection, false).await;
    hub.wait_for_cycle().await;
    device.wait_for_cycle().await;

    topology.tailnet.partition("device", "hub");
    let (_, record_id) = device.insert_dictation("offline dictation", None);
    device.drive_failed_cycle().await;

    let local = device
        .service()
        .library()
        .dictations(&SearchParams::new())
        .await
        .unwrap();
    let local = local.iter().find(|entry| entry.id == record_id).unwrap();
    assert!(local.offline);
    assert_eq!(
        local.upload_state,
        Some(audetic_core::sync::UploadState::Pending)
    );
    assert_eq!(
        hub.connection()
            .query_row("SELECT COUNT(*) FROM shared_dictations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );

    topology.tailnet.heal("device", "hub");
    topology.tailnet.fault(
        "device",
        "hub",
        Method::POST,
        "/audetic/v1/snapshots",
        OperationFault::LoseResponseAfterDispatch("response lost after commit"),
    );
    device.drive_failed_cycle_by(Duration::from_secs(301)).await;
    let hub_db = hub.connection();
    assert_eq!(
        hub_db
            .query_row("SELECT COUNT(*) FROM shared_dictations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        hub_db
            .query_row("SELECT COUNT(*) FROM shared_library_changes", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(hub_db);

    assert_eq!(
        device
            .connection()
            .query_row(
                "SELECT state,lease_owner FROM sync_outbox_items WHERE record_id=?1",
                [record_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .unwrap(),
        ("pending".into(), None)
    );
    device.restart().await;

    device.drive_cycle_by(Duration::from_secs(301)).await;
    let hub_db = hub.connection();
    assert_eq!(
        hub_db
            .query_row("SELECT COUNT(*) FROM shared_dictations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        hub_db
            .query_row("SELECT COUNT(*) FROM shared_library_changes", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let device_db = device.connection();
    assert_eq!(
        device_db
            .query_row(
                "SELECT state FROM sync_outbox_items WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "synced"
    );
    assert_eq!(
        topology
            .tailnet
            .dispatch_count(Method::POST, "/audetic/v1/snapshots"),
        2
    );
}

#[tokio::test]
async fn slice_3_uuid_identity_parent_mapping_cas_and_tombstones_win() {
    let topology = HomeHubTopology::new();
    let hub = topology.daemon("hub", "owner@example.com");
    let first = topology.daemon("first", "owner@example.com");
    let second = topology.daemon("second", "owner@example.com");
    hub.start().await;
    first.start().await;
    second.start().await;
    let hub_connection = hub.activate_hub(false).await;
    first.connect(hub_connection.clone(), false).await;
    second.connect(hub_connection.clone(), false).await;
    hub.wait_for_cycle().await;
    first.wait_for_cycle().await;
    second.wait_for_cycle().await;

    let create_meeting = |daemon: &super::TestDaemon, title: &str| {
        let db = daemon.connection();
        let local_id = MeetingRepository::insert(&db, Some(title), "/missing/audio.wav").unwrap();
        MeetingRepository::complete(
            &db,
            local_id,
            "/missing/transcript.txt",
            &format!("transcript from {title}"),
            None,
            10,
        )
        .unwrap();
        let artifact_id = MeetingArtifactRepository::insert_pending(
            &db,
            local_id,
            "summary",
            &format!("{title} summary"),
            Some("standard_meeting"),
            None,
        )
        .unwrap();
        MeetingArtifactRepository::complete(&db, artifact_id, "# Summary", "", "").unwrap();
        let meeting = MeetingRepository::get(&db, local_id).unwrap().unwrap();
        let artifact = MeetingArtifactRepository::get(&db, artifact_id)
            .unwrap()
            .unwrap();
        (local_id, meeting, artifact)
    };
    let (first_local, first_meeting, first_artifact) = create_meeting(&first, "first");
    let (second_local, second_meeting, second_artifact) = create_meeting(&second, "second");
    assert_eq!(first_local, second_local);
    assert_ne!(first_meeting.sync_id, second_meeting.sync_id);
    assert_ne!(first_artifact.id, second_artifact.id);
    assert_eq!(first_artifact.meeting_id, first_meeting.sync_id);
    assert_eq!(second_artifact.meeting_id, second_meeting.sync_id);

    first.drive_cycle().await;
    second.drive_cycle().await;
    let hub_library = crate::sync::library::HubLibrary::new(hub.db_path.clone());
    let meetings = hub_library.page_meetings(None, None, 10).unwrap();
    assert_eq!(meetings.items.len(), 2);
    assert!(meetings.items.iter().any(|meeting| {
        meeting.record_id == first_meeting.sync_id
            && meeting.artifacts[0].record_id == first_artifact.id
            && meeting.artifacts[0].parent_record_id == first_meeting.sync_id
    }));
    assert!(meetings.items.iter().any(|meeting| {
        meeting.record_id == second_meeting.sync_id
            && meeting.artifacts[0].record_id == second_artifact.id
            && meeting.artifacts[0].parent_record_id == second_meeting.sync_id
    }));

    let client = first.direct_client("hub");
    let current = client
        .meeting(hub_connection.hub_id, first_meeting.sync_id)
        .await
        .unwrap()
        .unwrap();
    let expected = current.title_version;
    let updated = client
        .update_meeting_title(
            hub_connection.hub_id,
            first_meeting.sync_id,
            &MeetingTitlePatch {
                title: "authoritative".into(),
                expected_title_version: expected,
                title_source: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.title.as_deref(), Some("authoritative"));
    assert!(client
        .update_meeting_title(
            hub_connection.hub_id,
            first_meeting.sync_id,
            &MeetingTitlePatch {
                title: "stale".into(),
                expected_title_version: expected,
                title_source: None,
            },
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("409"));

    let delayed = first_meeting.snapshot().unwrap();
    first
        .service()
        .library()
        .delete_meeting(first_meeting.sync_id)
        .await
        .unwrap();
    let delayed_result = client
        .upload_snapshots(
            hub_connection.hub_id,
            &SnapshotBatch {
                snapshots: vec![delayed.into()],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        delayed_result.results[0].disposition,
        SnapshotDisposition::Rejected
    );
    assert_eq!(
        delayed_result.results[0].error_code.as_deref(),
        Some("tombstoned")
    );
    assert!(hub_library
        .meeting(first_meeting.sync_id)
        .unwrap()
        .is_none());
    assert_eq!(
        hub_library
            .page_meetings(None, None, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        hub.connection()
            .query_row(
                "SELECT COUNT(*) FROM shared_artifacts WHERE parent_record_id=?1 AND deleted_at IS NULL",
                [first_meeting.sync_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn cycle_driver_ignores_a_restarts_initial_cycle_before_advancing_clock() {
    let topology = HomeHubTopology::new();
    let hub = topology.daemon("hub", "owner@example.com");
    let mut device = topology.daemon("device", "owner@example.com");
    hub.start().await;
    device.start().await;
    let hub_connection = hub.activate_hub(false).await;
    device.connect(hub_connection, false).await;
    device.wait_for_cycle().await;

    let (_, record_id) = device.insert_dictation("restart ordering", None);
    let initial_cycle = super::FaultGate::new();
    let advanced_cycle = super::FaultGate::new();
    for gate in [&initial_cycle, &advanced_cycle] {
        topology.tailnet.fault(
            "device",
            "hub",
            Method::POST,
            "/audetic/v1/snapshots",
            OperationFault::HoldBeforeDispatchThenFail(gate.clone(), "ordered failure"),
        );
    }

    device.shutdown_for_restart().await;
    device.reconstruct().await;
    initial_cycle.wait_entered().await;

    let mut driven_cycle = Box::pin(device.drive_failed_cycle_by(Duration::from_secs(301)));
    tokio::select! {
        () = &mut driven_cycle => panic!("cycle driver returned while the initial cycle was held"),
        () = tokio::task::yield_now() => {}
    }
    initial_cycle.release();
    tokio::select! {
        () = advanced_cycle.wait_entered() => {}
        () = &mut driven_cycle => panic!("restart's initial cycle satisfied the requested driven cycle"),
    }
    advanced_cycle.release();
    super::watchdog("waiting for explicitly ordered driven cycle", driven_cycle).await;

    assert_eq!(
        device
            .connection()
            .query_row(
                "SELECT state,lease_owner FROM sync_outbox_items WHERE record_id=?1",
                [record_id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .unwrap(),
        ("pending".into(), None)
    );
}
