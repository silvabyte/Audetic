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
    assert_eq!(applied, 5);

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
fn backfill_uses_the_intended_target_role_before_role_persistence() {
    let conn = setup_test_db().unwrap();
    insert_workflow(&conn, &create_test_workflow("pre-activation")).unwrap();
    assert_eq!(
        super::sync_settings::SyncSettingsRepository::get(&conn)
            .unwrap()
            .role,
        audetic_core::sync::SyncRole::Standalone
    );

    assert_eq!(
        backfill_visible_dictations(&conn, audetic_core::sync::SyncRole::ConnectedDevice).unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM sync_outbox_items", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        super::sync_settings::SyncSettingsRepository::get(&conn)
            .unwrap()
            .role,
        audetic_core::sync::SyncRole::Standalone
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
