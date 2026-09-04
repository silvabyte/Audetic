use anyhow::{Context, Result};
use audetic_core::sync::{RecordId, SyncRole};
use rusqlite::Connection;

use super::schemas::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use super::sync_identity::SyncIdentityRepository;
use super::sync_outbox::SyncOutboxRepository;
use super::sync_settings::SyncSettingsRepository;
use crate::sync::protocol::{DictationPayload, DictationSnapshot, RecordKind};

pub fn insert_workflow(conn: &Connection, workflow: &Workflow) -> Result<i64> {
    Ok(insert_workflow_record(conn, workflow)?.0)
}

pub fn insert_workflow_record(conn: &Connection, workflow: &Workflow) -> Result<(i64, RecordId)> {
    let (workflow_type_str, _json_data) = workflow.to_row()?;

    // Extract text and audio_path from the workflow data
    let (text, audio_path) = match &workflow.data {
        WorkflowData::VoiceToText(data) => (&data.text, &data.audio_path),
    };

    let transaction = conn
        .unchecked_transaction()
        .context("Failed to start workflow transaction")?;
    let identity = SyncIdentityRepository::get_or_create_device(&transaction)?;
    let sync_id = workflow.sync_id.unwrap_or_default();
    transaction
        .execute(
            "INSERT INTO workflows
            (workflow_type, text, audio_path, sync_id, origin_device_id, sync_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                workflow_type_str,
                text,
                audio_path,
                sync_id.to_string(),
                identity.device_id.to_string(),
                workflow.sync_version,
            ],
        )
        .context("Failed to insert workflow")?;
    let row_id = transaction.last_insert_rowid();
    let created_at: String = transaction.query_row(
        "SELECT created_at FROM workflows WHERE id = ?1",
        [row_id],
        |row| row.get(0),
    )?;
    let portable_created_at = portable_timestamp(&created_at)?;
    let role = SyncSettingsRepository::get(&transaction)?.role;
    if matches!(role, SyncRole::HomeHub | SyncRole::ConnectedDevice) {
        SyncOutboxRepository::enqueue_snapshot(
            &transaction,
            &DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: sync_id,
                origin_device_id: identity.device_id,
                local_version: workflow.sync_version,
                created_at: portable_created_at.clone(),
                updated_at: portable_created_at,
                payload: DictationPayload { text: text.clone() },
            },
        )?;
    }
    transaction.commit().context("Failed to commit workflow")?;
    Ok((row_id, sync_id))
}

pub fn get_recent_workflows(conn: &Connection, limit: usize) -> Result<Vec<Workflow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, workflow_type, text, audio_path, created_at, sync_id,
                         origin_device_id, sync_version
                  FROM workflows WHERE deleted_at IS NULL
                  ORDER BY created_at DESC, sync_id DESC LIMIT ?1",
        )
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

            let workflow_type_enum =
                WorkflowType::parse(&workflow_type).map_err(|_| rusqlite::Error::InvalidQuery)?;

            let sync_id: String = row.get(5)?;
            let origin_device_id: String = row.get(6)?;
            Ok(Workflow {
                id: Some(id),
                sync_id: Some(sync_id.parse().map_err(|_| rusqlite::Error::InvalidQuery)?),
                origin_device_id: Some(
                    origin_device_id
                        .parse()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                ),
                sync_version: row.get(7)?,
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
        .query_row(
            "SELECT COUNT(*) FROM workflows WHERE deleted_at IS NULL",
            [],
            |row| row.get(0),
        )
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
    let mut sql = "SELECT id, workflow_type, text, audio_path, created_at, sync_id,
                          origin_device_id, sync_version
                   FROM workflows WHERE deleted_at IS NULL"
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

    sql.push_str(" ORDER BY created_at DESC, sync_id DESC LIMIT ?");
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

            let workflow_type_enum =
                WorkflowType::parse(&workflow_type).map_err(|_| rusqlite::Error::InvalidQuery)?;

            let sync_id: String = row.get(5)?;
            let origin_device_id: String = row.get(6)?;
            Ok(Workflow {
                id: Some(id),
                sync_id: Some(sync_id.parse().map_err(|_| rusqlite::Error::InvalidQuery)?),
                origin_device_id: Some(
                    origin_device_id
                        .parse()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                ),
                sync_version: row.get(7)?,
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

pub fn get_workflow_by_sync_id(conn: &Connection, sync_id: RecordId) -> Result<Option<Workflow>> {
    let mut workflows = query_workflows(conn, Some(sync_id), None, None, None, 0, 1)?;
    Ok(workflows.pop())
}

pub fn list_visible_workflows(
    conn: &Connection,
    query: Option<&str>,
    date_from: Option<&str>,
    date_to: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<Vec<Workflow>> {
    query_workflows(conn, None, query, date_from, date_to, offset, limit)
}

fn query_workflows(
    conn: &Connection,
    sync_id: Option<RecordId>,
    query: Option<&str>,
    date_from: Option<&str>,
    date_to: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<Vec<Workflow>> {
    let mut sql = "SELECT id, workflow_type, text, audio_path, created_at, sync_id,
                          origin_device_id, sync_version
                   FROM workflows WHERE deleted_at IS NULL"
        .to_owned();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(sync_id) = sync_id {
        sql.push_str(" AND sync_id = ?");
        params.push(Box::new(sync_id.to_string()));
    }
    if let Some(query) = query {
        sql.push_str(" AND text LIKE ?");
        params.push(Box::new(format!("%{query}%")));
    }
    if let Some(from) = date_from {
        sql.push_str(" AND created_at >= ?");
        params.push(Box::new(from.to_owned()));
    }
    if let Some(to) = date_to {
        sql.push_str(" AND created_at <= ?");
        params.push(Box::new(to.to_owned()));
    }
    sql.push_str(" ORDER BY created_at DESC, sync_id DESC LIMIT ? OFFSET ?");
    params.push(Box::new(limit));
    params.push(Box::new(offset));
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|value| value.as_ref()).collect();
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(refs.as_slice(), map_workflow_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("Failed to map workflows")
}

fn map_workflow_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workflow> {
    let workflow_type: String = row.get(1)?;
    let sync_id: String = row.get(5)?;
    let origin_device_id: String = row.get(6)?;
    Ok(Workflow {
        id: Some(row.get(0)?),
        sync_id: Some(sync_id.parse().map_err(|_| rusqlite::Error::InvalidQuery)?),
        origin_device_id: Some(
            origin_device_id
                .parse()
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
        ),
        sync_version: row.get(7)?,
        workflow_type: WorkflowType::parse(&workflow_type)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        data: WorkflowData::VoiceToText(VoiceToTextData {
            text: row.get(2)?,
            audio_path: row.get(3)?,
        }),
        created_at: Some(row.get(4)?),
    })
}

pub fn backfill_visible_dictations(conn: &Connection, target_role: SyncRole) -> Result<usize> {
    let transaction = conn.unchecked_transaction()?;
    let count = backfill_visible_dictations_in_transaction(&transaction, target_role)?;
    transaction.commit()?;
    Ok(count)
}

pub fn backfill_visible_dictations_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    target_role: SyncRole,
) -> Result<usize> {
    if !matches!(target_role, SyncRole::HomeHub | SyncRole::ConnectedDevice) {
        return Ok(0);
    }
    let identity = SyncIdentityRepository::get_or_create_device(transaction)?;
    let snapshots = {
        let mut statement = transaction.prepare(
            "SELECT sync_id, origin_device_id, sync_version, text, created_at
             FROM workflows WHERE deleted_at IS NULL AND origin_device_id = ?1",
        )?;
        let rows = statement
            .query_map([identity.device_id.to_string()], |row| {
                let record_id: String = row.get(0)?;
                let origin: String = row.get(1)?;
                Ok((
                    record_id,
                    origin,
                    row.get::<_, u64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for row in &snapshots {
        SyncOutboxRepository::enqueue_snapshot(
            transaction,
            &DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id: row.0.parse().map_err(anyhow::Error::msg)?,
                origin_device_id: row.1.parse().map_err(anyhow::Error::msg)?,
                local_version: row.2,
                created_at: portable_timestamp(&row.4)?,
                updated_at: portable_timestamp(&row.4)?,
                payload: DictationPayload {
                    text: row.3.clone(),
                },
            },
        )?;
    }
    Ok(snapshots.len())
}

fn portable_timestamp(value: &str) -> Result<String> {
    if chrono::DateTime::parse_from_rfc3339(value).is_ok() {
        return Ok(value.to_owned());
    }
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .with_context(|| format!("Invalid workflow timestamp {value:?}"))?;
    Ok(naive.and_utc().to_rfc3339())
}
