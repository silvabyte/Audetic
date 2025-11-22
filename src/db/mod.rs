use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct VoiceToTextData {
    pub text: String,
    pub audio_path: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum WorkflowData {
    VoiceToText(VoiceToTextData),
    //will support more types later
}

#[derive(Debug)]
pub enum WorkflowType {
    VoiceToText,
}

impl WorkflowType {
    pub fn from_str(s: &str) -> Result<WorkflowType> {
        match s {
            "VoiceToText" => Ok(WorkflowType::VoiceToText),
            _ => anyhow::bail!("Invalid workflow type: {}", s),
        }
    }
    pub fn to_str(&self) -> &str {
        match self {
            WorkflowType::VoiceToText => "VoiceToText",
        }
    }
}

#[derive(Debug)]
pub struct Workflow {
    pub id: Option<i64>,
    pub workflow_type: WorkflowType,
    pub data: WorkflowData,
    pub created_at: Option<String>,
}

impl Workflow {
    pub fn to_row(&self) -> Result<(String, String)> {
        Ok((
            self.workflow_type.to_str().to_string(),
            serde_json::to_string(&self.data)?,
        ))
    }

    pub fn from_row(id: i64, workflow_type: String, json: String, created_at: String) -> Result<Workflow> {
        Ok(Workflow {
            id: Some(id),
            workflow_type: WorkflowType::from_str(&workflow_type)?,
            data: serde_json::from_str(&json)?,
            created_at: Some(created_at),
        })
    }

    pub fn new(workflow_type: WorkflowType, data: WorkflowData) -> Self {
        Workflow {
            id: None,
            workflow_type,
            data,
            created_at: None,
        }
    }
}

pub fn init_db() -> Result<Connection> {
    let db_path = crate::global::db_file()?;

    // Ensure parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .context("Failed to create database directory")?;
    }

    let conn = Connection::open(&db_path)
        .context("Failed to open database connection")?;

    migrate(&conn)?;

    Ok(conn)
}

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS workflows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            workflow_type TEXT NOT NULL,
            text TEXT NOT NULL,
            audio_path TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )",
        [],
    )
    .context("Failed to create workflows table")?;

    // Create index for faster text searches
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_workflows_created_at ON workflows(created_at DESC)",
        [],
    )
    .context("Failed to create index on created_at")?;

    Ok(())
}

pub fn insert_workflow(conn: &Connection, workflow: &Workflow) -> Result<i64> {
    let (workflow_type_str, _json_data) = workflow.to_row()?;

    // Extract text and audio_path from the workflow data
    let (text, audio_path) = match &workflow.data {
        WorkflowData::VoiceToText(data) => (&data.text, &data.audio_path),
    };

    conn.execute(
        "INSERT INTO workflows (workflow_type, text, audio_path) VALUES (?1, ?2, ?3)",
        rusqlite::params![workflow_type_str, text, audio_path],
    )
    .context("Failed to insert workflow")?;

    Ok(conn.last_insert_rowid())
}

pub fn get_recent_workflows(conn: &Connection, limit: usize) -> Result<Vec<Workflow>> {
    let mut stmt = conn
        .prepare("SELECT id, workflow_type, text, audio_path, created_at FROM workflows ORDER BY created_at DESC LIMIT ?1")
        .context("Failed to prepare query")?;

    let workflows = stmt
        .query_map([limit], |row| {
            let id: i64 = row.get(0)?;
            let workflow_type: String = row.get(1)?;
            let text: String = row.get(2)?;
            let audio_path: String = row.get(3)?;
            let created_at: String = row.get(4)?;

            // Reconstruct the WorkflowData from the database fields
            let data = WorkflowData::VoiceToText(VoiceToTextData {
                text,
                audio_path,
            });

            let workflow_type_enum = WorkflowType::from_str(&workflow_type)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;

            Ok(Workflow {
                id: Some(id),
                workflow_type: workflow_type_enum,
                data,
                created_at: Some(created_at),
            })
        })
        .context("Failed to query workflows")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to map workflows")?;

    Ok(workflows)
}

pub fn count_workflows(conn: &Connection) -> Result<i64> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM workflows", [], |row| row.get(0))
        .context("Failed to count workflows")?;

    Ok(count)
}

pub fn prune_old_workflows(conn: &Connection, max_count: i64) -> Result<usize> {
    let count = count_workflows(conn)?;

    if count <= max_count {
        return Ok(0);
    }

    let to_delete = count - max_count;

    let deleted = conn
        .execute(
            "DELETE FROM workflows WHERE id IN (
                SELECT id FROM workflows ORDER BY created_at ASC LIMIT ?1
            )",
            [to_delete],
        )
        .context("Failed to prune old workflows")?;

    Ok(deleted)
}

pub fn search_workflows(
    conn: &Connection,
    query: Option<&str>,
    date_from: Option<&str>,
    date_to: Option<&str>,
    limit: usize,
) -> Result<Vec<Workflow>> {
    let mut sql = "SELECT id, workflow_type, text, audio_path, created_at FROM workflows WHERE 1=1".to_string();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(q) = query {
        sql.push_str(" AND text LIKE ?");
        params.push(Box::new(format!("%{}%", q)));
    }

    if let Some(from) = date_from {
        sql.push_str(" AND created_at >= ?");
        params.push(Box::new(from.to_string()));
    }

    if let Some(to) = date_to {
        sql.push_str(" AND created_at <= ?");
        params.push(Box::new(to.to_string()));
    }

    sql.push_str(" ORDER BY created_at DESC LIMIT ?");
    params.push(Box::new(limit));

    let mut stmt = conn.prepare(&sql).context("Failed to prepare search query")?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let workflows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let id: i64 = row.get(0)?;
            let workflow_type: String = row.get(1)?;
            let text: String = row.get(2)?;
            let audio_path: String = row.get(3)?;
            let created_at: String = row.get(4)?;

            let data = WorkflowData::VoiceToText(VoiceToTextData {
                text,
                audio_path,
            });

            let workflow_type_enum = WorkflowType::from_str(&workflow_type)
                .map_err(|_| rusqlite::Error::InvalidQuery)?;

            Ok(Workflow {
                id: Some(id),
                workflow_type: workflow_type_enum,
                data,
                created_at: Some(created_at),
            })
        })
        .context("Failed to execute search query")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to map search results")?;

    Ok(workflows)
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
