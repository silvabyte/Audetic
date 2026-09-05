use anyhow::{Context, Result};
use audetic_core::sync::{RecordId, SyncRole};
use rusqlite::{Connection, OptionalExtension};

use super::schemas::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use super::sync_identity::SyncIdentityRepository;
use super::sync_outbox::SyncOutboxRepository;
use super::sync_settings::SyncSettingsRepository;
use crate::sync::protocol::{
    DictationPayload, DictationSnapshot, RecordKind, RecordingPayloadDescriptor,
};

pub fn insert_workflow(conn: &Connection, workflow: &Workflow) -> Result<i64> {
    Ok(insert_workflow_record(conn, workflow)?.0)
}

pub fn insert_workflow_record(conn: &Connection, workflow: &Workflow) -> Result<(i64, RecordId)> {
    let (workflow_type_str, _json_data) = workflow.to_row()?;

    // Extract text and audio_path from the workflow data
    let (text, audio_path) = match &workflow.data {
        WorkflowData::VoiceToText(data) => (&data.text, &data.audio_path),
    };

    let sync_id = workflow.sync_id.unwrap_or_default();
    let initial_settings = SyncSettingsRepository::get(conn)?;
    let staging =
        if sync_active(initial_settings.role) && initial_settings.upload_recording_payloads {
            attempt_recording_staging(conn, std::path::Path::new(audio_path))
        } else {
            RecordingStaging::default()
        };
    let staged_path = staging.staged.as_ref().map(|payload| payload.path.clone());
    let result = (|| -> Result<(i64, RecordId, bool)> {
        let transaction = conn
            .unchecked_transaction()
            .context("Failed to start workflow transaction")?;
        let identity = SyncIdentityRepository::get_or_create_device(&transaction)?;
        let settings = SyncSettingsRepository::get(&transaction)?;
        let staging_applies = settings.upload_recording_payloads
            && initial_settings.upload_recording_payloads
            && sync_active(settings.role);
        let recording_payload = staging_applies
            .then(|| {
                staging
                    .staged
                    .as_ref()
                    .map(|payload| payload.descriptor.clone())
            })
            .flatten()
            .unwrap_or_else(RecordingPayloadDescriptor::unavailable);
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
        if sync_active(settings.role) {
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
                    payload: DictationPayload {
                        text: text.clone(),
                        recording_payload: recording_payload.clone(),
                    },
                }
                .into(),
            )?;
            if staging_applies {
                enqueue_missing_payload(
                    &transaction,
                    sync_id,
                    RecordKind::Dictation,
                    &recording_payload,
                    &staging,
                )?;
            } else if !settings.upload_recording_payloads {
                SyncOutboxRepository::enqueue_blob(
                    &transaction,
                    sync_id,
                    RecordKind::Dictation,
                    &recording_payload,
                    None,
                )?;
            }
        }
        transaction.commit().context("Failed to commit workflow")?;
        Ok((row_id, sync_id, staging_applies && staging.staged.is_some()))
    })();
    let keep_staged = result.as_ref().is_ok_and(|value| value.2);
    drop(staging);
    if !keep_staged {
        reclaim_after_failed_association(conn, staged_path);
    }
    result.map(|(row_id, record_id, _)| (row_id, record_id))
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

    let transaction = conn.unchecked_transaction()?;
    let records = {
        let mut statement = transaction.prepare(
            "SELECT sync_id FROM workflows WHERE deleted_at IS NULL
             ORDER BY created_at ASC, id ASC LIMIT ?1",
        )?;
        let values = statement
            .query_map([to_delete], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        values
    };
    let mut staged_paths = Vec::new();
    for record_id in &records {
        staged_paths.extend(SyncOutboxRepository::remove_record_state(
            &transaction,
            record_id.parse().map_err(anyhow::Error::msg)?,
            RecordKind::Dictation,
        )?);
    }
    let deleted = transaction.execute(
        "DELETE FROM workflows WHERE sync_id IN (
            SELECT sync_id FROM workflows WHERE deleted_at IS NULL
            ORDER BY created_at ASC, id ASC LIMIT ?1
        )",
        [to_delete],
    )?;
    transaction
        .commit()
        .context("Failed to prune old workflows")?;
    if let Err(error) = SyncOutboxRepository::reclaim_staged_paths(conn, &staged_paths) {
        tracing::warn!(%error, "failed to reclaim pruned dictation staging files");
    }
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

pub fn backfill_visible_dictations(
    conn: &Connection,
    target_role: SyncRole,
    upload_recording_payloads: bool,
) -> Result<usize> {
    let mut cursor = BackfillCursor::dictations();
    backfill_visible_records_batch_cancellable(
        conn,
        target_role,
        upload_recording_payloads,
        i64::MAX as usize,
        &mut cursor,
        &tokio_util::sync::CancellationToken::new(),
    )
}

pub fn backfill_visible_records_batch(
    conn: &Connection,
    target_role: SyncRole,
    upload_recording_payloads: bool,
    limit: usize,
) -> Result<usize> {
    backfill_visible_records_batch_cancellable(
        conn,
        target_role,
        upload_recording_payloads,
        limit,
        &mut BackfillCursor::default(),
        &tokio_util::sync::CancellationToken::new(),
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum BackfillPhase {
    #[default]
    Dictations,
    Meetings,
    Artifacts,
    Complete,
}

#[derive(Clone, Debug)]
pub(crate) struct BackfillCursor {
    phase: BackfillPhase,
    after_id: i64,
    stop_after: BackfillPhase,
}

impl Default for BackfillCursor {
    fn default() -> Self {
        Self {
            phase: BackfillPhase::Dictations,
            after_id: 0,
            stop_after: BackfillPhase::Artifacts,
        }
    }
}

impl BackfillCursor {
    fn dictations() -> Self {
        Self {
            stop_after: BackfillPhase::Dictations,
            ..Self::default()
        }
    }

    fn meetings() -> Self {
        Self {
            phase: BackfillPhase::Meetings,
            after_id: 0,
            stop_after: BackfillPhase::Meetings,
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.phase == BackfillPhase::Complete
    }

    fn advance_phase(&mut self) {
        if self.phase == self.stop_after {
            self.phase = BackfillPhase::Complete;
            self.after_id = 0;
            return;
        }
        self.phase = match self.phase {
            BackfillPhase::Dictations => BackfillPhase::Meetings,
            BackfillPhase::Meetings => BackfillPhase::Artifacts,
            BackfillPhase::Artifacts | BackfillPhase::Complete => BackfillPhase::Complete,
        };
        self.after_id = 0;
    }
}

pub(crate) fn backfill_visible_records_batch_cancellable(
    conn: &Connection,
    target_role: SyncRole,
    upload_recording_payloads: bool,
    limit: usize,
    cursor: &mut BackfillCursor,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<usize> {
    if limit == 0 || !sync_active(target_role) || cursor.is_complete() {
        return Ok(0);
    }
    let identity = SyncIdentityRepository::get_or_create_device(conn)?;
    let mut attempted = 0;
    while attempted < limit && !cursor.is_complete() && !cancellation.is_cancelled() {
        let remaining = limit - attempted;
        let ids = match cursor.phase {
            BackfillPhase::Dictations => select_dictation_ids(
                conn,
                &identity.device_id.to_string(),
                upload_recording_payloads,
                cursor.after_id,
                remaining,
            )?,
            BackfillPhase::Meetings => select_meeting_ids(
                conn,
                &identity.device_id.to_string(),
                upload_recording_payloads,
                cursor.after_id,
                remaining,
            )?,
            BackfillPhase::Artifacts => select_artifact_ids(
                conn,
                &identity.device_id.to_string(),
                cursor.after_id,
                remaining,
            )?,
            BackfillPhase::Complete => Vec::new(),
        };
        if ids.is_empty() {
            cursor.advance_phase();
            continue;
        }
        for id in ids {
            if cancellation.is_cancelled() {
                break;
            }
            cursor.after_id = id;
            attempted += 1;
            let result = match cursor.phase {
                BackfillPhase::Dictations => backfill_dictation(
                    conn,
                    id,
                    target_role,
                    upload_recording_payloads,
                    cancellation,
                ),
                BackfillPhase::Meetings => backfill_meeting(
                    conn,
                    id,
                    target_role,
                    upload_recording_payloads,
                    cancellation,
                ),
                BackfillPhase::Artifacts => backfill_artifact(conn, id, target_role),
                BackfillPhase::Complete => Ok(()),
            };
            if let Err(error) = result {
                tracing::warn!(record_id = id, %error, "failed to backfill Shared Library record");
            }
        }
    }
    Ok(attempted)
}

pub fn backfill_visible_meetings(
    conn: &Connection,
    target_role: SyncRole,
    upload_recording_payloads: bool,
) -> Result<usize> {
    let mut cursor = BackfillCursor::meetings();
    backfill_visible_records_batch_cancellable(
        conn,
        target_role,
        upload_recording_payloads,
        i64::MAX as usize,
        &mut cursor,
        &tokio_util::sync::CancellationToken::new(),
    )
}

fn select_dictation_ids(
    conn: &Connection,
    device_id: &str,
    upload_recording_payloads: bool,
    after_id: i64,
    limit: usize,
) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT id FROM workflows w WHERE id>?1 AND deleted_at IS NULL AND origin_device_id=?2
           AND (NOT EXISTS(SELECT 1 FROM sync_outbox_items o
                           WHERE o.record_id=w.sync_id AND o.kind='dictation')
                OR (?3 AND NOT EXISTS(SELECT 1 FROM sync_outbox_blobs b
                                      WHERE b.record_id=w.sync_id AND b.payload_role='recording')))
         ORDER BY id LIMIT ?4",
    )?;
    let ids = statement
        .query_map(
            rusqlite::params![
                after_id,
                device_id,
                upload_recording_payloads,
                sql_limit(limit)
            ],
            |row| row.get(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn select_meeting_ids(
    conn: &Connection,
    device_id: &str,
    upload_recording_payloads: bool,
    after_id: i64,
    limit: usize,
) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT id FROM meetings m WHERE id>?1 AND deleted_at IS NULL AND status='completed'
           AND origin_device_id=?2
           AND (NOT EXISTS(SELECT 1 FROM sync_outbox_items o
                           WHERE o.record_id=m.sync_id AND o.kind='meeting')
                OR (?3 AND NOT EXISTS(SELECT 1 FROM sync_outbox_blobs b
                                      WHERE b.record_id=m.sync_id AND b.payload_role='recording')))
         ORDER BY id LIMIT ?4",
    )?;
    let ids = statement
        .query_map(
            rusqlite::params![
                after_id,
                device_id,
                upload_recording_payloads,
                sql_limit(limit)
            ],
            |row| row.get(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn select_artifact_ids(
    conn: &Connection,
    device_id: &str,
    after_id: i64,
    limit: usize,
) -> Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT a.id FROM meeting_artifacts a
         INNER JOIN meetings m ON m.id=a.meeting_id AND m.deleted_at IS NULL
         WHERE a.id>?1 AND a.status='completed' AND a.origin_device_id=?2
           AND NOT EXISTS(SELECT 1 FROM sync_outbox_items o
                          WHERE o.record_id=a.sync_id AND o.kind='artifact')
         ORDER BY a.id LIMIT ?3",
    )?;
    let ids = statement
        .query_map(
            rusqlite::params![after_id, device_id, sql_limit(limit)],
            |row| row.get(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(ids)
}

fn backfill_dictation(
    conn: &Connection,
    workflow_id: i64,
    target_role: SyncRole,
    upload_recording_payloads: bool,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let row = conn.query_row(
        "SELECT sync_id,origin_device_id,sync_version,text,created_at,audio_path
         FROM workflows WHERE id=?1 AND deleted_at IS NULL",
        [workflow_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        },
    )?;
    let record_id = row.0.parse().map_err(anyhow::Error::msg)?;
    let existing = SyncOutboxRepository::payload_descriptor(conn, record_id)?;
    let staging = if existing.is_none() && upload_recording_payloads {
        attempt_recording_staging_cancellable(conn, std::path::Path::new(&row.5), cancellation)
    } else {
        RecordingStaging::default()
    };
    if cancellation.is_cancelled() {
        let staged_path = staging.staged.as_ref().map(|value| value.path.clone());
        drop(staging);
        reclaim_after_failed_association(conn, staged_path);
        return Ok(());
    }
    let staged_path = staging.staged.as_ref().map(|value| value.path.clone());
    let result = (|| -> Result<bool> {
        let transaction = conn.unchecked_transaction()?;
        if !backfill_policy_matches(&transaction, target_role, upload_recording_payloads)? {
            transaction.commit()?;
            return Ok(false);
        }
        let current = transaction
            .query_row(
                "SELECT sync_id,origin_device_id,sync_version,text,created_at,audio_path
             FROM workflows WHERE id=?1 AND deleted_at IS NULL",
                [workflow_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        if current.as_ref() != Some(&row) {
            transaction.commit()?;
            return Ok(false);
        }
        let current_existing = SyncOutboxRepository::payload_descriptor(&transaction, record_id)?;
        let recording_payload = current_existing
            .clone()
            .or_else(|| {
                staging
                    .staged
                    .as_ref()
                    .map(|value| value.descriptor.clone())
            })
            .unwrap_or_else(RecordingPayloadDescriptor::unavailable);
        SyncOutboxRepository::enqueue_snapshot(
            &transaction,
            &DictationSnapshot {
                kind: RecordKind::Dictation,
                schema_version: 1,
                record_id,
                origin_device_id: row.1.parse().map_err(anyhow::Error::msg)?,
                local_version: row.2,
                created_at: portable_timestamp(&row.4)?,
                updated_at: portable_timestamp(&row.4)?,
                payload: DictationPayload {
                    text: row.3.clone(),
                    recording_payload: recording_payload.clone(),
                },
            }
            .into(),
        )?;
        if current_existing.is_none() {
            enqueue_missing_payload(
                &transaction,
                record_id,
                RecordKind::Dictation,
                &recording_payload,
                &staging,
            )?;
        }
        transaction.commit()?;
        Ok(current_existing.is_none() && staging.staged.is_some())
    })();
    let keep_staged = result.as_ref().is_ok_and(|keep| *keep);
    drop(staging);
    if !keep_staged {
        reclaim_after_failed_association(conn, staged_path);
    }
    result.map(|_| ())
}

fn backfill_meeting(
    conn: &Connection,
    meeting_id: i64,
    target_role: SyncRole,
    upload_recording_payloads: bool,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    let meeting = crate::db::meetings::MeetingRepository::get(conn, meeting_id)?
        .context("visible meeting disappeared during backfill")?;
    let existing = SyncOutboxRepository::payload_descriptor(conn, meeting.sync_id)?;
    let staging = if existing.is_none() && upload_recording_payloads {
        attempt_recording_staging_cancellable(
            conn,
            std::path::Path::new(&meeting.audio_path),
            cancellation,
        )
    } else {
        RecordingStaging::default()
    };
    if cancellation.is_cancelled() {
        let staged_path = staging.staged.as_ref().map(|value| value.path.clone());
        drop(staging);
        reclaim_after_failed_association(conn, staged_path);
        return Ok(());
    }
    let staged_path = staging.staged.as_ref().map(|value| value.path.clone());
    let result = (|| -> Result<bool> {
        let transaction = conn.unchecked_transaction()?;
        if !backfill_policy_matches(&transaction, target_role, upload_recording_payloads)? {
            transaction.commit()?;
            return Ok(false);
        }
        let current = crate::db::meetings::MeetingRepository::get(&transaction, meeting_id)?;
        if !current.as_ref().is_some_and(|value| {
            value.sync_id == meeting.sync_id
                && value.sync_version == meeting.sync_version
                && value.audio_path == meeting.audio_path
                && value.status == crate::meeting::status::MeetingPhase::Completed.as_str()
        }) {
            transaction.commit()?;
            return Ok(false);
        }
        let current = current.expect("checked meeting");
        let current_existing =
            SyncOutboxRepository::payload_descriptor(&transaction, current.sync_id)?;
        let recording_payload = current_existing
            .clone()
            .or_else(|| {
                staging
                    .staged
                    .as_ref()
                    .map(|value| value.descriptor.clone())
            })
            .unwrap_or_else(RecordingPayloadDescriptor::unavailable);
        SyncOutboxRepository::enqueue_snapshot(
            &transaction,
            &current
                .snapshot_with_payload(recording_payload.clone())?
                .into(),
        )?;
        if current_existing.is_none() {
            enqueue_missing_payload(
                &transaction,
                current.sync_id,
                RecordKind::Meeting,
                &recording_payload,
                &staging,
            )?;
        }
        transaction.commit()?;
        Ok(current_existing.is_none() && staging.staged.is_some())
    })();
    let keep_staged = result.as_ref().is_ok_and(|keep| *keep);
    drop(staging);
    if !keep_staged {
        reclaim_after_failed_association(conn, staged_path);
    }
    result.map(|_| ())
}

fn backfill_artifact(conn: &Connection, artifact_id: i64, target_role: SyncRole) -> Result<()> {
    let transaction = conn.unchecked_transaction()?;
    let settings = SyncSettingsRepository::get(&transaction)?;
    if settings.role != target_role {
        transaction.commit()?;
        return Ok(());
    }
    let artifact =
        crate::db::meeting_artifacts::MeetingArtifactRepository::get(&transaction, artifact_id)?
            .context("visible artifact disappeared during backfill")?;
    SyncOutboxRepository::enqueue_snapshot(&transaction, &artifact.snapshot(&transaction)?.into())?;
    transaction.commit()?;
    Ok(())
}

fn enqueue_missing_payload(
    transaction: &Connection,
    record_id: RecordId,
    kind: RecordKind,
    descriptor: &RecordingPayloadDescriptor,
    staging: &RecordingStaging,
) -> Result<()> {
    if let Some(error) = staging.error.as_deref() {
        SyncOutboxRepository::enqueue_blob_staging_failure(transaction, record_id, kind, error)
    } else {
        SyncOutboxRepository::enqueue_blob(
            transaction,
            record_id,
            kind,
            descriptor,
            staging
                .staged
                .as_ref()
                .map(|payload| payload.path.as_path()),
        )
        .map(|_| ())
    }
}

fn sync_active(role: SyncRole) -> bool {
    matches!(role, SyncRole::HomeHub | SyncRole::ConnectedDevice)
}

fn backfill_policy_matches(
    conn: &Connection,
    target_role: SyncRole,
    upload_recording_payloads: bool,
) -> Result<bool> {
    let settings = SyncSettingsRepository::get(conn)?;
    Ok(settings.role == target_role
        && settings.upload_recording_payloads == upload_recording_payloads)
}

fn sql_limit(limit: usize) -> i64 {
    limit.min(i64::MAX as usize) as i64
}

fn portable_timestamp(value: &str) -> Result<String> {
    if chrono::DateTime::parse_from_rfc3339(value).is_ok() {
        return Ok(value.to_owned());
    }
    let naive = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .with_context(|| format!("Invalid workflow timestamp {value:?}"))?;
    Ok(naive.and_utc().to_rfc3339())
}

pub(crate) fn database_path(conn: &Connection) -> Result<std::path::PathBuf> {
    let path: String = conn
        .query_row(
            "SELECT file FROM pragma_database_list WHERE name='main'",
            [],
            |row| row.get(0),
        )
        .context("resolving database path for Recording Payload staging")?;
    if path.is_empty() {
        anyhow::bail!("Recording Payload staging requires a file-backed database");
    }
    Ok(path.into())
}

#[derive(Default)]
pub(crate) struct RecordingStaging {
    pub staged: Option<crate::sync::payload::StagedPayload>,
    pub error: Option<String>,
}

pub(crate) fn attempt_recording_staging(
    conn: &Connection,
    source: &std::path::Path,
) -> RecordingStaging {
    attempt_recording_staging_cancellable(conn, source, &tokio_util::sync::CancellationToken::new())
}

pub(crate) fn attempt_recording_staging_cancellable(
    conn: &Connection,
    source: &std::path::Path,
    cancellation: &tokio_util::sync::CancellationToken,
) -> RecordingStaging {
    match crate::sync::payload::resolve_operational_audio(source) {
        Ok(None) => return RecordingStaging::default(),
        Ok(Some(_)) => {}
        Err(error) => {
            let error = format!("Recording Payload staging failed: {error:#}");
            tracing::warn!(%error, source = %source.display());
            return RecordingStaging {
                staged: None,
                error: Some(error),
            };
        }
    }
    match database_path(conn).and_then(|db_path| {
        crate::sync::payload::stage_recording_cancellable(&db_path, source, cancellation)
    }) {
        Ok(staged) => RecordingStaging {
            staged,
            error: None,
        },
        Err(error) => {
            let error = format!("Recording Payload staging failed: {error:#}");
            tracing::warn!(%error, source = %source.display());
            RecordingStaging {
                staged: None,
                error: Some(error),
            }
        }
    }
}

fn reclaim_after_failed_association(conn: &Connection, path: Option<std::path::PathBuf>) {
    let Some(path) = path else {
        return;
    };
    if let Err(error) = SyncOutboxRepository::reclaim_staged_paths(conn, &[path]) {
        tracing::warn!(%error, "failed to reclaim unowned Recording Payload staging file");
    }
}
