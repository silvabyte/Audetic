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
fn test_insert_workflow() {
    let conn = setup_test_db().unwrap();
    let workflow = create_test_workflow("Test transcription");

    let id = insert_workflow(&conn, &workflow).unwrap();
    assert!(id > 0);
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
