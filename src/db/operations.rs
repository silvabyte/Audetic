use anyhow::{Context, Result};
use rusqlite::Connection;

use super::schemas::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};

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
            let data = WorkflowData::VoiceToText(VoiceToTextData { text, audio_path });

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
    let mut sql = "SELECT id, workflow_type, text, audio_path, created_at FROM workflows WHERE 1=1"
        .to_string();
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

    let mut stmt = conn
        .prepare(&sql)
        .context("Failed to prepare search query")?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let workflows = stmt
        .query_map(param_refs.as_slice(), |row| {
            let id: i64 = row.get(0)?;
            let workflow_type: String = row.get(1)?;
            let text: String = row.get(2)?;
            let audio_path: String = row.get(3)?;
            let created_at: String = row.get(4)?;

            let data = WorkflowData::VoiceToText(VoiceToTextData { text, audio_path });

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
