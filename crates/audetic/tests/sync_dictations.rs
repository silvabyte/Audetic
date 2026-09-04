use audetic::db::shared_library::SharedLibraryRepository;
use audetic::db::{self, VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use audetic::sync::protocol::{DictationPayload, DictationSnapshot, RecordKind};

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
