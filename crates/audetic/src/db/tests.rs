use super::init::migrate;
use super::operations::*;
use super::schemas::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use anyhow::Result;
use rusqlite::Connection;

fn setup_test_db() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;
    Ok(conn)
}

fn create_test_workflow(text: &str) -> Workflow {
    Workflow::new(
        WorkflowType::VoiceToText,
        WorkflowData::VoiceToText(VoiceToTextData {
            text: text.to_string(),
            audio_path: "/tmp/test.wav".to_string(),
        }),
    )
}

#[test]
fn test_migrate_creates_table() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();

    // Verify table exists by querying it
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='workflows'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn numbered_migrations_are_idempotent_and_preserve_legacy_rows() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE workflows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_type TEXT NOT NULL,
            text TEXT NOT NULL,
            audio_path TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
         );
         INSERT INTO workflows (workflow_type, text, audio_path)
         VALUES ('VoiceToText', 'legacy text', '/tmp/legacy.wav');
         CREATE TABLE meetings (
            id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT, title_source TEXT,
            title_updated_at TIMESTAMP, title_version INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT 'recording', audio_path TEXT NOT NULL,
            source_filename TEXT, transcript_path TEXT, transcript_text TEXT,
            transcript_segments TEXT, duration_seconds INTEGER,
            started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP, error TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP, deleted_at TIMESTAMP
         );
         INSERT INTO meetings (id,title,status,audio_path,transcript_text,duration_seconds,completed_at)
         VALUES (41,'Legacy meeting','completed','/tmp/meeting.wav','legacy transcript',30,CURRENT_TIMESTAMP);
         CREATE TABLE meeting_artifacts (
            id INTEGER PRIMARY KEY AUTOINCREMENT, meeting_id INTEGER NOT NULL,
            kind TEXT NOT NULL, title TEXT NOT NULL, template_id TEXT,
            agent_profile_id INTEGER, status TEXT NOT NULL, content_markdown TEXT,
            error TEXT, stdout TEXT, stderr TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, completed_at TIMESTAMP
         );
         INSERT INTO meeting_artifacts (id,meeting_id,kind,title,status,content_markdown,completed_at)
         VALUES (73,41,'summary','Legacy summary','completed','# Legacy',CURRENT_TIMESTAMP);",
    )
    .unwrap();

    migrate(&conn).unwrap();
    let first_identity: (String, String, i64) = conn
        .query_row(
            "SELECT sync_id, origin_device_id, sync_version FROM workflows WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let first_meeting_identity: (i64, String, String) = conn
        .query_row(
            "SELECT id, sync_id, origin_device_id FROM meetings WHERE id = 41",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let first_artifact_identity: (i64, i64, String, String) = conn
        .query_row(
            "SELECT id, meeting_id, sync_id, origin_device_id FROM meeting_artifacts WHERE id = 73",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    migrate(&conn).unwrap();

    let applied: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(applied, 8);

    let legacy: (String, String) = conn
        .query_row(
            "SELECT text, audio_path FROM workflows WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(legacy, ("legacy text".into(), "/tmp/legacy.wav".into()));
    let second_identity: (String, String, i64) = conn
        .query_row(
            "SELECT sync_id, origin_device_id, sync_version FROM workflows WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(first_identity, second_identity);
    let second_meeting_identity: (i64, String, String) = conn
        .query_row(
            "SELECT id, sync_id, origin_device_id FROM meetings WHERE id = 41",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let second_artifact_identity: (i64, i64, String, String) = conn
        .query_row(
            "SELECT id, meeting_id, sync_id, origin_device_id FROM meeting_artifacts WHERE id = 73",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(first_meeting_identity, second_meeting_identity);
    assert_eq!(first_artifact_identity, second_artifact_identity);
    assert_eq!(first_artifact_identity.1, first_meeting_identity.0);
    assert!(first_meeting_identity.1.parse::<uuid::Uuid>().is_ok());
    assert!(first_artifact_identity.2.parse::<uuid::Uuid>().is_ok());
    assert!(first_identity.0.parse::<uuid::Uuid>().is_ok());
    assert!(first_identity.1.parse::<uuid::Uuid>().is_ok());
    assert_eq!(first_identity.2, 1);
}

#[test]
fn populated_v7_database_migrates_to_zero_role_epoch_without_data_loss() {
    let conn = Connection::open_in_memory().unwrap();
    migrate(&conn).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO sync_settings(singleton) VALUES(1)",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE sync_settings
         SET role='home_hub',device_name='Kitchen Hub',upload_recording_payloads=1
         WHERE singleton=1",
        [],
    )
    .unwrap();
    let (_, record_id) = insert_workflow_record(
        &conn,
        &Workflow::new(
            WorkflowType::VoiceToText,
            WorkflowData::VoiceToText(VoiceToTextData {
                text: "persisted before role epochs".into(),
                audio_path: "/tmp/v7.wav".into(),
            }),
        ),
    )
    .unwrap();
    conn.execute("DELETE FROM schema_migrations WHERE version=8", [])
        .unwrap();
    conn.execute("ALTER TABLE sync_settings DROP COLUMN role_epoch", [])
        .unwrap();
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, 7);

    migrate(&conn).unwrap();

    let settings: (String, Option<String>, bool, i64) = conn
        .query_row(
            "SELECT role,device_name,upload_recording_payloads,role_epoch
             FROM sync_settings WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        settings,
        ("home_hub".into(), Some("Kitchen Hub".into()), true, 0)
    );
    let workflow_text: String = conn
        .query_row(
            "SELECT text FROM workflows WHERE sync_id=?1",
            [record_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(workflow_text, "persisted before role epochs");
    let outbox_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sync_outbox_items WHERE record_id=?1",
            [record_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outbox_count, 1);
}

#[test]
fn test_insert_workflow() {
    let conn = setup_test_db().unwrap();
    let workflow = create_test_workflow("Test transcription");

    let id = insert_workflow(&conn, &workflow).unwrap();
    assert!(id > 0);
}

#[test]
fn home_hub_workflow_write_and_outbox_enqueue_are_atomic() {
    let conn = setup_test_db().unwrap();
    super::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
    conn.execute(
        "UPDATE sync_settings SET role = 'home_hub' WHERE singleton = 1",
        [],
    )
    .unwrap();
    insert_workflow(&conn, &create_test_workflow("queued")).unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM workflows", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sync_outbox_items", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );

    conn.execute_batch(
        "CREATE TRIGGER reject_outbox BEFORE INSERT ON sync_outbox_items
         BEGIN SELECT RAISE(ABORT, 'simulated outbox failure'); END;",
    )
    .unwrap();
    assert!(insert_workflow(&conn, &create_test_workflow("must roll back")).is_err());
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM workflows", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn home_hub_meeting_completion_and_outbox_enqueue_are_atomic() {
    let conn = setup_test_db().unwrap();
    super::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
    conn.execute(
        "UPDATE sync_settings SET role = 'home_hub' WHERE singleton = 1",
        [],
    )
    .unwrap();
    let meeting_id =
        super::meetings::MeetingRepository::insert(&conn, Some("Review"), "/tmp/review.wav")
            .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_meeting_outbox BEFORE INSERT ON sync_outbox_items
         WHEN NEW.kind = 'meeting'
         BEGIN SELECT RAISE(ABORT, 'simulated meeting outbox failure'); END;",
    )
    .unwrap();

    assert!(super::meetings::MeetingRepository::complete(
        &conn,
        meeting_id,
        "/tmp/review.txt",
        "portable transcript",
        None,
        30,
    )
    .is_err());
    let meeting = super::meetings::MeetingRepository::get(&conn, meeting_id)
        .unwrap()
        .unwrap();
    assert_eq!(meeting.status, "recording");
    assert!(meeting.transcript_text.is_none());
}

#[test]
fn home_hub_artifact_completion_and_outbox_enqueue_are_atomic() {
    let conn = setup_test_db().unwrap();
    super::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
    conn.execute(
        "UPDATE sync_settings SET role = 'home_hub' WHERE singleton = 1",
        [],
    )
    .unwrap();
    let meeting_id =
        super::meetings::MeetingRepository::insert(&conn, Some("Review"), "/tmp/review.wav")
            .unwrap();
    super::meetings::MeetingRepository::complete(
        &conn,
        meeting_id,
        "/tmp/review.txt",
        "portable transcript",
        None,
        30,
    )
    .unwrap();
    let artifact_id = super::meeting_artifacts::MeetingArtifactRepository::insert_pending(
        &conn,
        meeting_id,
        "summary",
        "Summary",
        Some("standard_meeting"),
        None,
    )
    .unwrap();
    super::meeting_artifacts::MeetingArtifactRepository::set_running(&conn, artifact_id).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_artifact_outbox BEFORE INSERT ON sync_outbox_items
         WHEN NEW.kind = 'artifact'
         BEGIN SELECT RAISE(ABORT, 'simulated artifact outbox failure'); END;",
    )
    .unwrap();

    assert!(
        super::meeting_artifacts::MeetingArtifactRepository::complete(
            &conn,
            artifact_id,
            "# Summary",
            "# Summary",
            "",
        )
        .is_err()
    );
    let artifact = super::meeting_artifacts::MeetingArtifactRepository::get(&conn, artifact_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        artifact.status,
        super::meeting_artifacts::ArtifactStatus::Running
    );
    assert!(artifact.content_markdown.is_none());
}

#[test]
fn backfill_revalidates_the_persisted_role_before_enqueueing() {
    let conn = setup_test_db().unwrap();
    insert_workflow(&conn, &create_test_workflow("pre-activation")).unwrap();
    assert_eq!(
        super::sync_settings::SyncSettingsRepository::get(&conn)
            .unwrap()
            .role,
        audetic_core::sync::SyncRole::Standalone
    );

    assert_eq!(
        backfill_visible_dictations(&conn, audetic_core::sync::SyncRole::HomeHub, false,).unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sync_outbox_items", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        super::sync_settings::SyncSettingsRepository::get(&conn)
            .unwrap()
            .role,
        audetic_core::sync::SyncRole::Standalone
    );
    conn.execute(
        "UPDATE sync_settings SET role='home_hub' WHERE singleton=1",
        [],
    )
    .unwrap();
    assert_eq!(
        backfill_visible_dictations(&conn, audetic_core::sync::SyncRole::HomeHub, false,).unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sync_outbox_items", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn bounded_backfill_preserves_accepted_payloads_and_only_creates_missing_rows() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("audetic.db");
    let conn = super::migrate_db_at(&db_path).unwrap();
    let mut record_ids = Vec::new();
    for index in 0..2 {
        let source = temp.path().join(format!("recording-{index}.wav"));
        std::fs::write(&source, format!("payload-{index}")).unwrap();
        let (_, record_id) = insert_workflow_record(
            &conn,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: format!("record-{index}"),
                    audio_path: source.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap();
        record_ids.push((record_id, source));
    }
    conn.execute(
        "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
        [],
    )
    .unwrap();

    assert_eq!(
        backfill_visible_records_batch(&conn, audetic_core::sync::SyncRole::HomeHub, true, 1,)
            .unwrap(),
        1
    );
    let accepted_id = record_ids[0].0;
    let accepted_path: String = conn
        .query_row(
            "SELECT staged_path FROM sync_outbox_blobs WHERE record_id=?1",
            [accepted_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "UPDATE sync_outbox_items SET state='synced',accepted_hub_revision=1
         WHERE record_id=?1",
        [accepted_id.to_string()],
    )
    .unwrap();
    conn.execute(
        "UPDATE sync_outbox_blobs SET state='synced',availability='available'
         WHERE record_id=?1",
        [accepted_id.to_string()],
    )
    .unwrap();
    std::fs::remove_file(&record_ids[0].1).unwrap();

    assert_eq!(
        backfill_visible_records_batch(&conn, audetic_core::sync::SyncRole::HomeHub, true, 25,)
            .unwrap(),
        1
    );
    assert_eq!(
        backfill_visible_records_batch(&conn, audetic_core::sync::SyncRole::HomeHub, true, 25,)
            .unwrap(),
        0
    );
    let accepted: (String, String, String, u64) = conn
        .query_row(
            "SELECT b.state,b.availability,b.staged_path,i.accepted_hub_revision
             FROM sync_outbox_blobs b JOIN sync_outbox_items i USING(record_id,kind)
             WHERE record_id=?1",
            [accepted_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        accepted,
        ("synced".into(), "available".into(), accepted_path, 1)
    );
}

#[test]
fn bounded_backfill_continues_after_an_individual_record_failure() {
    let conn = setup_test_db().unwrap();
    let (invalid_id, invalid_record_id) =
        insert_workflow_record(&conn, &create_test_workflow("invalid timestamp")).unwrap();
    let (_, valid_record_id) =
        insert_workflow_record(&conn, &create_test_workflow("still backfilled")).unwrap();
    conn.execute(
        "UPDATE sync_settings SET role='home_hub' WHERE singleton=1",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE workflows SET created_at='not-a-timestamp' WHERE id=?1",
        [invalid_id],
    )
    .unwrap();

    assert_eq!(
        backfill_visible_records_batch(&conn, audetic_core::sync::SyncRole::HomeHub, false, 2,)
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sync_outbox_items WHERE record_id=?1",
            [invalid_record_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sync_outbox_items WHERE record_id=?1",
            [valid_record_id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
}

#[test]
fn keyset_backfill_advances_past_a_full_batch_of_failures() {
    let conn = setup_test_db().unwrap();
    let mut records = Vec::new();
    for text in ["bad one", "bad two", "reachable"] {
        records.push(insert_workflow_record(&conn, &create_test_workflow(text)).unwrap());
    }
    for (id, _) in &records[..2] {
        conn.execute(
            "UPDATE workflows SET created_at='not-a-timestamp' WHERE id=?1",
            [id],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE sync_settings SET role='home_hub' WHERE singleton=1",
        [],
    )
    .unwrap();

    let mut cursor = super::BackfillCursor::default();
    let cancellation = tokio_util::sync::CancellationToken::new();
    for _ in 0..3 {
        assert_eq!(
            super::backfill_visible_records_batch_cancellable(
                &conn,
                audetic_core::sync::SyncRole::HomeHub,
                false,
                1,
                &mut cursor,
                &cancellation,
            )
            .unwrap(),
            1
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sync_outbox_items WHERE record_id=?1",
            [records[2].1.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
}

#[test]
fn retry_removes_metadata_less_staging_failure_for_a_new_backfill_pass() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("audetic.db");
    let conn = super::migrate_db_at(&db_path).unwrap();
    super::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
    conn.execute(
        "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
        [],
    )
    .unwrap();
    std::fs::write(temp.path().join("sync"), b"block staging").unwrap();
    let source = temp.path().join("retry.wav");
    std::fs::write(&source, b"retry payload").unwrap();
    let (_, record_id) = insert_workflow_record(
        &conn,
        &Workflow::new(
            WorkflowType::VoiceToText,
            WorkflowData::VoiceToText(VoiceToTextData {
                text: "retry staging".into(),
                audio_path: source.to_string_lossy().into_owned(),
            }),
        ),
    )
    .unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT state FROM sync_outbox_blobs WHERE record_id=?1",
            [record_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "needs_attention"
    );

    std::fs::remove_file(temp.path().join("sync")).unwrap();
    super::sync_outbox::SyncOutboxRepository::retry_all(&conn).unwrap();
    assert_eq!(
        backfill_visible_records_batch(&conn, audetic_core::sync::SyncRole::HomeHub, true, 25,)
            .unwrap(),
        1
    );
    let staged: String = conn
        .query_row(
            "SELECT staged_path FROM sync_outbox_blobs WHERE record_id=?1",
            [record_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(std::path::Path::new(&staged).is_file());
}

#[test]
fn failed_dictation_commit_reclaims_unreferenced_finalized_staging() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("audetic.db");
    let conn = super::migrate_db_at(&db_path).unwrap();
    super::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
    conn.execute(
        "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_workflow BEFORE INSERT ON workflows
         BEGIN SELECT RAISE(ABORT, 'simulated commit failure'); END;",
    )
    .unwrap();
    let source = temp.path().join("orphan.wav");
    std::fs::write(&source, b"must not be orphaned").unwrap();

    assert!(insert_workflow_record(
        &conn,
        &Workflow::new(
            WorkflowType::VoiceToText,
            WorkflowData::VoiceToText(VoiceToTextData {
                text: "fails".into(),
                audio_path: source.to_string_lossy().into_owned(),
            }),
        ),
    )
    .is_err());
    let staged_files = std::fs::read_dir(crate::sync::payload::staging_root_for_db(&db_path))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != ".lock")
        .count();
    assert_eq!(staged_files, 0);
}

#[test]
fn pruning_dictations_removes_blob_state_and_reclaims_staging() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("audetic.db");
    let conn = super::migrate_db_at(&db_path).unwrap();
    super::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
    conn.execute(
        "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
        [],
    )
    .unwrap();
    let source = temp.path().join("prune.wav");
    std::fs::write(&source, b"prune payload").unwrap();
    insert_workflow_record(
        &conn,
        &Workflow::new(
            WorkflowType::VoiceToText,
            WorkflowData::VoiceToText(VoiceToTextData {
                text: "prune".into(),
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

    assert_eq!(prune_old_workflows(&conn, 0).unwrap(), 1);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sync_outbox_items", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sync_outbox_blobs", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(!std::path::Path::new(&staged).exists());
}

#[test]
fn missing_historical_audio_is_normal_unavailable_metadata_not_an_upload_error() {
    let conn = setup_test_db().unwrap();
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
    let workflow = Workflow::new(
        WorkflowType::VoiceToText,
        WorkflowData::VoiceToText(VoiceToTextData {
            text: "metadata survives".into(),
            audio_path: "/definitely/missing/audetic-recording.wav".into(),
        }),
    );
    insert_workflow(&conn, &workflow).unwrap();

    assert_eq!(
        conn.query_row("SELECT state FROM sync_outbox_items", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "pending"
    );
    assert_eq!(
        conn.query_row(
            "SELECT availability || ':' || state FROM sync_outbox_blobs",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "unavailable:synced"
    );
    let snapshot: String = conn
        .query_row("SELECT snapshot_json FROM sync_outbox_items", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&snapshot).unwrap()["payload"]
            ["recording_payload"]["availability"],
        "unavailable"
    );
}

#[test]
fn dictation_persistence_survives_payload_staging_failure() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("audetic.db");
    let conn = super::migrate_db_at(&db_path).unwrap();
    super::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
    conn.execute(
        "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
        [],
    )
    .unwrap();
    std::fs::write(temp.path().join("sync"), b"blocks staging directory").unwrap();
    let source = temp.path().join("recording.wav");
    std::fs::write(&source, b"recording bytes").unwrap();

    let (_, record_id) = insert_workflow_record(
        &conn,
        &Workflow::new(
            WorkflowType::VoiceToText,
            WorkflowData::VoiceToText(VoiceToTextData {
                text: "local success".into(),
                audio_path: source.to_string_lossy().into_owned(),
            }),
        ),
    )
    .unwrap();

    assert!(get_workflow_by_sync_id(&conn, record_id).unwrap().is_some());
    assert_eq!(
        conn.query_row("SELECT state FROM sync_outbox_items", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "pending"
    );
    let failure: (String, String, Option<String>) = conn
        .query_row(
            "SELECT availability,state,last_error FROM sync_outbox_blobs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(failure.0, "needs_attention");
    assert_eq!(failure.1, "needs_attention");
    assert!(failure
        .2
        .is_some_and(|error| error.contains("staging failed")));
}

#[test]
fn meeting_completion_survives_payload_staging_failure() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("audetic.db");
    let conn = super::migrate_db_at(&db_path).unwrap();
    super::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
    conn.execute(
        "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
        [],
    )
    .unwrap();
    std::fs::write(temp.path().join("sync"), b"blocks staging directory").unwrap();
    let source = temp.path().join("meeting.wav");
    std::fs::write(&source, b"meeting bytes").unwrap();
    let id = super::meetings::MeetingRepository::insert(
        &conn,
        Some("Meeting"),
        source.to_string_lossy().as_ref(),
    )
    .unwrap();

    super::meetings::MeetingRepository::complete(
        &conn,
        id,
        "/tmp/meeting.txt",
        "completed locally",
        None,
        30,
    )
    .unwrap();

    let meeting = super::meetings::MeetingRepository::get(&conn, id)
        .unwrap()
        .unwrap();
    assert_eq!(meeting.status, "completed");
    assert_eq!(
        meeting.transcript_text.as_deref(),
        Some("completed locally")
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
}

#[test]
fn test_get_recent_workflows() {
    let conn = setup_test_db().unwrap();

    // Insert multiple workflows
    let workflow1 = create_test_workflow("First transcription");
    let workflow2 = create_test_workflow("Second transcription");
    let workflow3 = create_test_workflow("Third transcription");

    insert_workflow(&conn, &workflow1).unwrap();
    insert_workflow(&conn, &workflow2).unwrap();
    insert_workflow(&conn, &workflow3).unwrap();

    // Get recent workflows
    let workflows = get_recent_workflows(&conn, 2).unwrap();

    // Verify we got exactly 2 workflows
    assert_eq!(workflows.len(), 2);

    // Verify both workflows are from our test data
    for workflow in &workflows {
        let WorkflowData::VoiceToText(data) = &workflow.data;
        assert!(
            data.text == "First transcription"
                || data.text == "Second transcription"
                || data.text == "Third transcription"
        );
    }
}

#[test]
fn test_count_workflows() {
    let conn = setup_test_db().unwrap();

    // Initially empty
    assert_eq!(count_workflows(&conn).unwrap(), 0);

    // Insert some workflows
    let workflow1 = create_test_workflow("First");
    let workflow2 = create_test_workflow("Second");

    insert_workflow(&conn, &workflow1).unwrap();
    assert_eq!(count_workflows(&conn).unwrap(), 1);

    insert_workflow(&conn, &workflow2).unwrap();
    assert_eq!(count_workflows(&conn).unwrap(), 2);
}

#[test]
fn test_prune_old_workflows() {
    let conn = setup_test_db().unwrap();

    // Insert 15 workflows
    for i in 1..=15 {
        let workflow = create_test_workflow(&format!("Transcription {}", i));
        insert_workflow(&conn, &workflow).unwrap();
    }

    assert_eq!(count_workflows(&conn).unwrap(), 15);

    // Prune to keep only 10
    let pruned = prune_old_workflows(&conn, 10).unwrap();
    assert_eq!(pruned, 5);
    assert_eq!(count_workflows(&conn).unwrap(), 10);

    // Pruning again should do nothing
    let pruned_again = prune_old_workflows(&conn, 10).unwrap();
    assert_eq!(pruned_again, 0);
}

#[test]
fn test_search_workflows_by_text() {
    let conn = setup_test_db().unwrap();

    let workflow1 = create_test_workflow("Hello world");
    let workflow2 = create_test_workflow("Goodbye world");
    let workflow3 = create_test_workflow("Hello there");

    insert_workflow(&conn, &workflow1).unwrap();
    insert_workflow(&conn, &workflow2).unwrap();
    insert_workflow(&conn, &workflow3).unwrap();

    // Search for "Hello"
    let results = search_workflows(&conn, Some("Hello"), None, None, 10).unwrap();
    assert_eq!(results.len(), 2);

    // Search for "Goodbye"
    let results = search_workflows(&conn, Some("Goodbye"), None, None, 10).unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn test_search_workflows_limit() {
    let conn = setup_test_db().unwrap();

    for i in 1..=10 {
        let workflow = create_test_workflow(&format!("Test {}", i));
        insert_workflow(&conn, &workflow).unwrap();
    }

    // Search with limit
    let results = search_workflows(&conn, None, None, None, 5).unwrap();
    assert_eq!(results.len(), 5);
}

#[test]
fn test_workflow_serialization() {
    let workflow = create_test_workflow("Test text");
    let (workflow_type, json) = workflow.to_row().unwrap();

    assert_eq!(workflow_type, "VoiceToText");
    assert!(json.contains("Test text"));
    assert!(json.contains("/tmp/test.wav"));
}

#[test]
fn test_workflow_from_row() {
    let workflow = Workflow::from_row(
        1,
        "VoiceToText".to_string(),
        r#"{"type":"VoiceToText","payload":{"text":"Test","audio_path":"/tmp/test.wav"}}"#
            .to_string(),
        "2025-01-01 00:00:00".to_string(),
    )
    .unwrap();

    assert_eq!(workflow.id, Some(1));
    assert_eq!(workflow.created_at, Some("2025-01-01 00:00:00".to_string()));

    let WorkflowData::VoiceToText(data) = workflow.data;
    assert_eq!(data.text, "Test");
    assert_eq!(data.audio_path, "/tmp/test.wav");
}
