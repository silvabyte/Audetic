use audetic::db::meeting_artifacts::MeetingArtifactRepository;
use audetic::db::meetings::MeetingRepository;
use audetic::db::shared_library::SharedLibraryRepository;
use audetic::db::{self, VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use audetic::sync::protocol::{DictationPayload, DictationSnapshot, RecordKind, Snapshot};

fn workflow(text: &str) -> Workflow {
    Workflow::new(
        WorkflowType::VoiceToText,
        WorkflowData::VoiceToText(VoiceToTextData {
            text: text.to_owned(),
            audio_path: "/tmp/local-only.wav".to_owned(),
        }),
    )
}

#[test]
fn isolated_meetings_and_artifacts_keep_uuid_parentage_across_colliding_local_ids() {
    let temp = tempfile::tempdir().unwrap();
    let first = db::migrate_db_at(&temp.path().join("first-meetings.db")).unwrap();
    let second = db::migrate_db_at(&temp.path().join("second-meetings.db")).unwrap();
    let mut hub = db::migrate_db_at(&temp.path().join("meeting-hub.db")).unwrap();

    let mut expected = Vec::new();
    for (connection, title) in [(&first, "First meeting"), (&second, "Second meeting")] {
        let local_meeting =
            MeetingRepository::insert(connection, Some(title), "/tmp/local-only.wav").unwrap();
        assert_eq!(local_meeting, 1);
        MeetingRepository::complete(
            connection,
            local_meeting,
            "/tmp/local-only.txt",
            &format!("Transcript for {title}"),
            None,
            30,
        )
        .unwrap();
        let meeting = MeetingRepository::get(connection, local_meeting)
            .unwrap()
            .unwrap();

        let local_artifact = MeetingArtifactRepository::insert_pending(
            connection,
            local_meeting,
            "summary",
            "Summary",
            Some("standard_meeting"),
            None,
        )
        .unwrap();
        assert_eq!(local_artifact, 1);
        MeetingArtifactRepository::complete(
            connection,
            local_artifact,
            &format!("# {title}"),
            "",
            "",
        )
        .unwrap();
        let artifact = MeetingArtifactRepository::get(connection, local_artifact)
            .unwrap()
            .unwrap();
        assert_ne!(meeting.sync_id, artifact.id);
        assert_eq!(artifact.meeting_id, meeting.sync_id);

        let meeting_snapshot = meeting.snapshot().unwrap();
        let artifact_snapshot = artifact.snapshot(connection).unwrap();
        assert!(
            SharedLibraryRepository::apply(&mut hub, &Snapshot::Meeting(meeting_snapshot.clone()))
                .unwrap()
                .changed
        );
        assert!(
            !SharedLibraryRepository::apply(&mut hub, &Snapshot::Meeting(meeting_snapshot))
                .unwrap()
                .changed
        );
        assert!(
            SharedLibraryRepository::apply(
                &mut hub,
                &Snapshot::Artifact(artifact_snapshot.clone())
            )
            .unwrap()
            .changed
        );
        assert!(
            !SharedLibraryRepository::apply(&mut hub, &Snapshot::Artifact(artifact_snapshot))
                .unwrap()
                .changed
        );
        expected.push((meeting.sync_id, artifact.id));
    }

    assert_ne!(expected[0].0, expected[1].0);
    assert_ne!(expected[0].1, expected[1].1);
    let meetings = SharedLibraryRepository::page_meetings(&hub, None, None, 10).unwrap();
    assert_eq!(meetings.len(), 2);
    for (meeting_id, artifact_id) in expected {
        let meeting = meetings
            .iter()
            .find(|meeting| meeting.record_id == meeting_id)
            .unwrap();
        assert_eq!(meeting.artifacts.len(), 1);
        assert_eq!(meeting.artifacts[0].record_id, artifact_id);
        assert_eq!(meeting.artifacts[0].parent_record_id, meeting_id);
    }
}

#[test]
fn isolated_origins_with_colliding_integer_ids_remain_distinct_by_uuid() {
    let temp = tempfile::tempdir().unwrap();
    let first_path = temp.path().join("first.db");
    let second_path = temp.path().join("second.db");
    let hub_path = temp.path().join("hub.db");
    let first = db::migrate_db_at(&first_path).unwrap();
    let second = db::migrate_db_at(&second_path).unwrap();
    let mut hub = db::migrate_db_at(&hub_path).unwrap();

    let (first_row, first_id) =
        db::insert_workflow_record(&first, &workflow("first device")).unwrap();
    let (second_row, second_id) =
        db::insert_workflow_record(&second, &workflow("second device")).unwrap();
    assert_eq!(first_row, 1);
    assert_eq!(second_row, 1);
    assert_ne!(first_id, second_id);

    for (connection, id) in [(&first, first_id), (&second, second_id)] {
        let stored = db::get_workflow_by_sync_id(connection, id)
            .unwrap()
            .unwrap();
        let WorkflowData::VoiceToText(data) = stored.data;
        let created_at = stored.created_at.unwrap();
        SharedLibraryRepository::apply_snapshot(
            &mut hub,
            &DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: stored.sync_id.unwrap(),
                origin_device_id: stored.origin_device_id.unwrap(),
                local_version: stored.sync_version,
                created_at: created_at.clone(),
                updated_at: created_at,
                payload: DictationPayload { text: data.text },
            },
        )
        .unwrap();
    }

    let shared =
        SharedLibraryRepository::page_dictations(&hub, None, None, None, None, 10).unwrap();
    assert_eq!(shared.len(), 2);
    assert_ne!(shared[0].record_id, shared[1].record_id);
}
