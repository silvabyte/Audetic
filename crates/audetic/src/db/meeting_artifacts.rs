//! SQLite persistence for generated meeting artifacts.

use anyhow::{Context, Result};
use audetic_core::sync::{DeviceId, RecordId, SyncRole};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStatus {
    Pending,
    Running,
    Completed,
    Error,
}

impl ArtifactStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Error => "error",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "error" => Ok(Self::Error),
            other => Err(anyhow::anyhow!("unknown artifact status `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MeetingArtifact {
    pub id: RecordId,
    pub meeting_id: RecordId,
    #[serde(skip)]
    #[schema(ignore)]
    pub local_id: i64,
    #[serde(skip)]
    #[schema(ignore)]
    pub local_meeting_id: i64,
    pub origin_device_id: DeviceId,
    pub sync_version: u64,
    pub kind: String,
    pub title: String,
    pub template_id: Option<String>,
    pub agent_profile_id: Option<i64>,
    pub status: ArtifactStatus,
    pub content_markdown: Option<String>,
    pub error: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

pub struct MeetingArtifactRepository;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactDeleteOutcome {
    Deleted,
    NotFound,
    InFlight,
    RequiresHub,
}

impl MeetingArtifactRepository {
    pub fn insert_pending(
        conn: &Connection,
        meeting_id: i64,
        kind: &str,
        title: &str,
        template_id: Option<&str>,
        agent_profile_id: Option<i64>,
    ) -> Result<i64> {
        let transaction = conn.unchecked_transaction()?;
        let identity =
            crate::db::sync_identity::SyncIdentityRepository::get_or_create_device(&transaction)?;
        let sync_id = RecordId::new();
        transaction.execute(
            "INSERT INTO meeting_artifacts \
             (meeting_id, kind, title, template_id, agent_profile_id, status, sync_id, origin_device_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                meeting_id,
                kind,
                title,
                template_id,
                agent_profile_id,
                ArtifactStatus::Pending.as_str(),
                sync_id.to_string(),
                identity.device_id.to_string(),
            ],
        )
        .context("Failed to insert meeting artifact")?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(id)
    }

    pub fn set_running(conn: &Connection, id: i64) -> Result<()> {
        conn.execute(
            "UPDATE meeting_artifacts SET status = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
            params![ArtifactStatus::Running.as_str(), id],
        )
        .context("Failed to mark artifact running")?;
        Ok(())
    }

    pub fn complete(
        conn: &Connection,
        id: i64,
        content_markdown: &str,
        stdout: &str,
        stderr: &str,
    ) -> Result<()> {
        let transaction = conn.unchecked_transaction()?;
        let affected = transaction
            .execute(
                "UPDATE meeting_artifacts SET status = ?1, content_markdown = ?2, \
             stdout = ?3, stderr = ?4, error = NULL, updated_at = CURRENT_TIMESTAMP, \
             completed_at = CURRENT_TIMESTAMP WHERE id = ?5 \
             AND EXISTS (SELECT 1 FROM meetings m \
                         WHERE m.id = meeting_artifacts.meeting_id AND m.deleted_at IS NULL)",
                params![
                    ArtifactStatus::Completed.as_str(),
                    content_markdown,
                    stdout,
                    stderr,
                    id,
                ],
            )
            .context("Failed to complete meeting artifact")?;
        if affected == 0 {
            anyhow::bail!("artifact or its live meeting disappeared before completion");
        }
        let artifact =
            Self::get_from(&transaction, id)?.context("completed artifact disappeared")?;
        let role = crate::db::sync_settings::SyncSettingsRepository::get(&transaction)?.role;
        if matches!(role, SyncRole::HomeHub | SyncRole::ConnectedDevice) {
            crate::db::sync_outbox::SyncOutboxRepository::enqueue_snapshot(
                &transaction,
                &artifact.snapshot(&transaction)?.into(),
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn fail(conn: &Connection, id: i64, error: &str, stdout: &str, stderr: &str) -> Result<()> {
        conn.execute(
            "UPDATE meeting_artifacts SET status = ?1, error = ?2, stdout = ?3, stderr = ?4, \
             updated_at = CURRENT_TIMESTAMP, completed_at = CURRENT_TIMESTAMP WHERE id = ?5",
            params![ArtifactStatus::Error.as_str(), error, stdout, stderr, id],
        )
        .context("Failed to fail meeting artifact")?;
        Ok(())
    }

    pub fn list_for_meeting(conn: &Connection, meeting_id: i64) -> Result<Vec<MeetingArtifact>> {
        let mut stmt = conn
            .prepare(
                "SELECT a.id, a.meeting_id, a.kind, a.title, a.template_id, a.agent_profile_id, a.status, \
                 a.content_markdown, a.error, a.stdout, a.stderr, a.created_at, a.updated_at, a.completed_at, \
                 a.sync_id, m.sync_id, a.origin_device_id, a.sync_version \
                 FROM meeting_artifacts a INNER JOIN meetings m ON m.id = a.meeting_id \
                 WHERE a.meeting_id = ?1 ORDER BY a.created_at DESC, a.id DESC",
            )
            .context("Failed to prepare meeting artifact list")?;
        let rows = stmt
            .query_map(params![meeting_id], row_to_artifact)
            .context("Failed to query meeting artifacts")?;
        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(row??);
        }
        Ok(artifacts)
    }

    pub fn list_for_live_meeting(
        conn: &Connection,
        meeting_id: i64,
    ) -> Result<Vec<MeetingArtifact>> {
        let mut stmt = conn
            .prepare(
                "SELECT a.id, a.meeting_id, a.kind, a.title, a.template_id, a.agent_profile_id, a.status, \
                 a.content_markdown, a.error, a.stdout, a.stderr, a.created_at, a.updated_at, a.completed_at, \
                 a.sync_id, m.sync_id, a.origin_device_id, a.sync_version \
                 FROM meeting_artifacts a \
                 INNER JOIN meetings m ON m.id = a.meeting_id AND m.deleted_at IS NULL \
                 WHERE a.meeting_id = ?1 ORDER BY a.created_at DESC, a.id DESC",
            )
            .context("Failed to prepare live meeting artifact list")?;
        let rows = stmt
            .query_map(params![meeting_id], row_to_artifact)
            .context("Failed to query live meeting artifacts")?;
        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(row??);
        }
        Ok(artifacts)
    }

    pub fn get(conn: &Connection, id: i64) -> Result<Option<MeetingArtifact>> {
        Self::get_from(conn, id)
    }

    fn get_from(conn: &Connection, id: i64) -> Result<Option<MeetingArtifact>> {
        conn.query_row(
            "SELECT a.id, a.meeting_id, a.kind, a.title, a.template_id, a.agent_profile_id, a.status, \
              a.content_markdown, a.error, a.stdout, a.stderr, a.created_at, a.updated_at, a.completed_at, \
              a.sync_id, m.sync_id, a.origin_device_id, a.sync_version \
              FROM meeting_artifacts a INNER JOIN meetings m ON m.id = a.meeting_id WHERE a.id = ?1",
            params![id],
            row_to_artifact,
        )
        .optional()
        .context("Failed to query meeting artifact")?
        .transpose()
    }

    pub fn get_for_live_meeting(
        conn: &Connection,
        meeting_id: i64,
        id: i64,
    ) -> Result<Option<MeetingArtifact>> {
        conn.query_row(
            "SELECT a.id, a.meeting_id, a.kind, a.title, a.template_id, a.agent_profile_id, a.status, \
              a.content_markdown, a.error, a.stdout, a.stderr, a.created_at, a.updated_at, a.completed_at, \
              a.sync_id, m.sync_id, a.origin_device_id, a.sync_version \
             FROM meeting_artifacts a \
             INNER JOIN meetings m ON m.id = a.meeting_id AND m.deleted_at IS NULL \
             WHERE a.id = ?1 AND a.meeting_id = ?2",
            params![id, meeting_id],
            row_to_artifact,
        )
        .optional()
        .context("Failed to query live meeting artifact")?
        .transpose()
    }

    pub fn delete_for_meeting(conn: &Connection, meeting_id: i64, id: i64) -> Result<bool> {
        let n = conn
            .execute(
                "DELETE FROM meeting_artifacts WHERE id = ?1 AND meeting_id = ?2",
                params![id, meeting_id],
            )
            .context("Failed to delete meeting artifact")?;
        Ok(n > 0)
    }

    pub fn delete_for_live_meeting(conn: &Connection, meeting_id: i64, id: i64) -> Result<bool> {
        Ok(matches!(
            Self::delete_for_live_meeting_impl(conn, meeting_id, id, false)?,
            ArtifactDeleteOutcome::Deleted
        ))
    }

    pub fn delete_for_live_meeting_guarded(
        conn: &Connection,
        meeting_id: i64,
        id: i64,
    ) -> Result<ArtifactDeleteOutcome> {
        Self::delete_for_live_meeting_impl(conn, meeting_id, id, true)
    }

    fn delete_for_live_meeting_impl(
        conn: &Connection,
        meeting_id: i64,
        id: i64,
        guard_possible_upload: bool,
    ) -> Result<ArtifactDeleteOutcome> {
        let transaction = conn.unchecked_transaction()?;
        let artifact: Option<(String, String)> = transaction
            .query_row(
                "SELECT sync_id, status FROM meeting_artifacts WHERE id = ?1 AND meeting_id = ?2",
                params![id, meeting_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((sync_id, status)) = artifact else {
            transaction.commit()?;
            return Ok(ArtifactDeleteOutcome::NotFound);
        };
        if matches!(status.as_str(), "pending" | "running") {
            transaction.commit()?;
            return Ok(ArtifactDeleteOutcome::InFlight);
        }
        if guard_possible_upload {
            let active_sync = transaction
                .query_row(
                    "SELECT role != 'standalone' FROM sync_settings WHERE singleton = 1",
                    [],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false);
            let may_have_uploaded =
                sync_id
                    .parse()
                    .map_err(anyhow::Error::msg)
                    .and_then(|record_id| {
                        crate::db::sync_outbox::SyncOutboxRepository::may_have_reached_hub(
                            &transaction,
                            record_id,
                            crate::sync::protocol::RecordKind::Artifact,
                        )
                    })?;
            if active_sync && may_have_uploaded {
                transaction.commit()?;
                return Ok(ArtifactDeleteOutcome::RequiresHub);
            }
        }
        let n = transaction
            .execute(
                "DELETE FROM meeting_artifacts \
                 WHERE id = ?1 AND meeting_id = ?2 \
                 AND EXISTS (SELECT 1 FROM meetings WHERE id = ?2 AND deleted_at IS NULL)",
                params![id, meeting_id],
            )
            .context("Failed to delete live meeting artifact")?;
        if n > 0 {
            transaction.execute(
                "DELETE FROM sync_outbox_items WHERE record_id = ?1 AND kind = 'artifact'",
                params![sync_id],
            )?;
        }
        transaction.commit()?;
        Ok(if n > 0 {
            ArtifactDeleteOutcome::Deleted
        } else {
            ArtifactDeleteOutcome::NotFound
        })
    }

    pub fn internal_id(conn: &Connection, sync_id: RecordId) -> Result<Option<i64>> {
        conn.query_row(
            "SELECT id FROM meeting_artifacts WHERE sync_id = ?1",
            [sync_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .context("Failed to resolve artifact UUID")
    }

    pub fn get_by_sync_id(conn: &Connection, sync_id: RecordId) -> Result<Option<MeetingArtifact>> {
        Self::internal_id(conn, sync_id)?
            .map(|id| Self::get(conn, id))
            .transpose()
            .map(Option::flatten)
    }

    /// Remove durable local state after the Home Hub has accepted an artifact
    /// tombstone. Covers both operational artifacts and transient shared-run
    /// artifacts without exposing sync-table SQL to the HTTP route.
    pub fn cleanup_after_shared_delete(
        conn: &Connection,
        parent_record_id: RecordId,
        artifact_record_id: RecordId,
    ) -> Result<()> {
        let transaction = conn.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM sync_outbox_items WHERE record_id = ?1 AND kind = 'artifact'",
            [artifact_record_id.to_string()],
        )?;
        transaction.execute(
            "DELETE FROM sync_artifact_runs \
             WHERE artifact_record_id = ?1 AND parent_record_id = ?2",
            params![artifact_record_id.to_string(), parent_record_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list_shared_runs(
        conn: &Connection,
        parent_record_id: RecordId,
    ) -> Result<Vec<MeetingArtifact>> {
        let mut statement = conn.prepare(
            "SELECT artifact_record_id, origin_device_id, kind, title, template_id, \
                    agent_profile_id, status, content_markdown, error, created_at, updated_at, \
                    completed_at FROM sync_artifact_runs WHERE parent_record_id = ?1 \
             ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([parent_record_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, Option<String>>(11)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                origin,
                status,
                kind,
                title,
                template_id,
                agent_profile_id,
                content_markdown,
                error,
                created_at,
                updated_at,
                completed_at,
            ) = row?;
            Ok(MeetingArtifact {
                id: id.parse().map_err(anyhow::Error::msg)?,
                meeting_id: parent_record_id,
                local_id: 0,
                local_meeting_id: 0,
                origin_device_id: origin.parse().map_err(anyhow::Error::msg)?,
                sync_version: 1,
                kind,
                title,
                template_id,
                agent_profile_id,
                status: ArtifactStatus::parse(&status)?,
                content_markdown,
                error,
                stdout: None,
                stderr: None,
                created_at,
                updated_at,
                completed_at,
            })
        })
        .collect()
    }
}

fn row_to_artifact(row: &Row) -> rusqlite::Result<Result<MeetingArtifact>> {
    let status: String = row.get(6)?;
    Ok((|| {
        Ok(MeetingArtifact {
            local_id: row.get(0)?,
            local_meeting_id: row.get(1)?,
            id: row
                .get::<_, String>(14)?
                .parse()
                .map_err(anyhow::Error::msg)?,
            meeting_id: row
                .get::<_, String>(15)?
                .parse()
                .map_err(anyhow::Error::msg)?,
            origin_device_id: row
                .get::<_, String>(16)?
                .parse()
                .map_err(anyhow::Error::msg)?,
            sync_version: row.get(17)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            template_id: row.get(4)?,
            agent_profile_id: row.get(5)?,
            status: ArtifactStatus::parse(&status)?,
            content_markdown: row.get(7)?,
            error: row.get(8)?,
            stdout: row.get(9)?,
            stderr: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
            completed_at: row.get(13)?,
        })
    })())
}

impl MeetingArtifact {
    pub fn snapshot(
        &self,
        conn: &Connection,
    ) -> Result<crate::sync::protocol::CompletedArtifactSnapshot> {
        if self.status != ArtifactStatus::Completed {
            anyhow::bail!("only completed artifacts can be snapshotted");
        }
        let content_markdown = self
            .content_markdown
            .clone()
            .filter(|value| !value.trim().is_empty())
            .context("completed artifact has no content")?;
        let completed_at = self
            .completed_at
            .clone()
            .context("completed artifact has no timestamp")?;
        let profile_name = self
            .agent_profile_id
            .map(|id| {
                conn.query_row(
                    "SELECT name FROM agent_profiles WHERE id = ?1",
                    [id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })
            .transpose()?
            .flatten();
        Ok(crate::sync::protocol::CompletedArtifactSnapshot {
            kind: crate::sync::protocol::RecordKind::Artifact,
            schema_version: 1,
            record_id: self.id,
            parent_record_id: self.meeting_id,
            origin_device_id: self.origin_device_id,
            local_version: self.sync_version,
            created_at: portable_timestamp(&self.created_at)?,
            updated_at: portable_timestamp(&self.updated_at)?,
            payload: crate::sync::protocol::CompletedArtifactPayload {
                artifact_kind: self.kind.clone(),
                title: self.title.clone(),
                template_id: self.template_id.clone(),
                agent_profile_name: profile_name,
                content_markdown,
                completed_at: portable_timestamp(&completed_at)?,
            },
        })
    }
}

fn portable_timestamp(value: &str) -> Result<String> {
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.with_timezone(&chrono::Utc).to_rfc3339());
    }
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .map(|timestamp| timestamp.and_utc().to_rfc3339())
        .with_context(|| format!("invalid artifact timestamp {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::{ArtifactDeleteOutcome, MeetingArtifactRepository};
    use crate::db::{meetings::MeetingRepository, migrate};
    use anyhow::Result;
    use rusqlite::Connection;

    fn setup_db() -> Result<Connection> {
        let conn = Connection::open_in_memory()?;
        migrate(&conn)?;
        Ok(conn)
    }

    #[test]
    fn live_meeting_queries_hide_artifacts_after_soft_delete() -> Result<()> {
        let conn = setup_db()?;
        let meeting_id = MeetingRepository::insert(&conn, Some("Standup"), "/tmp/meeting.wav")?;
        MeetingRepository::complete(
            &conn,
            meeting_id,
            "/tmp/meeting.txt",
            "we made a decision",
            None,
            30,
        )?;

        let artifact_id = MeetingArtifactRepository::insert_pending(
            &conn,
            meeting_id,
            "summary",
            "Summary",
            Some("standard_meeting"),
            None,
        )?;
        MeetingArtifactRepository::complete(&conn, artifact_id, "# Summary", "# Summary", "")?;

        assert_eq!(
            MeetingArtifactRepository::list_for_live_meeting(&conn, meeting_id)?.len(),
            1
        );
        assert!(
            MeetingArtifactRepository::get_for_live_meeting(&conn, meeting_id, artifact_id)?
                .is_some()
        );

        MeetingRepository::soft_delete(&conn, meeting_id)?;

        assert!(MeetingArtifactRepository::list_for_live_meeting(&conn, meeting_id)?.is_empty());
        assert!(
            MeetingArtifactRepository::get_for_live_meeting(&conn, meeting_id, artifact_id)?
                .is_none()
        );
        assert!(!MeetingArtifactRepository::delete_for_live_meeting(
            &conn,
            meeting_id,
            artifact_id
        )?);
        assert!(MeetingArtifactRepository::get(&conn, artifact_id)?.is_some());
        Ok(())
    }

    #[test]
    fn guarded_artifact_delete_requires_hub_after_upload_claim() -> Result<()> {
        let mut conn = setup_db()?;
        crate::db::sync_settings::SyncSettingsRepository::get(&conn)?;
        conn.execute(
            "UPDATE sync_settings SET role = 'home_hub' WHERE singleton = 1",
            [],
        )?;
        let meeting_id = MeetingRepository::insert(&conn, Some("Standup"), "/tmp/meeting.wav")?;
        MeetingRepository::complete(
            &conn,
            meeting_id,
            "/tmp/meeting.txt",
            "we made a decision",
            None,
            30,
        )?;
        let artifact_id = MeetingArtifactRepository::insert_pending(
            &conn, meeting_id, "summary", "Summary", None, None,
        )?;
        MeetingArtifactRepository::complete(&conn, artifact_id, "# Summary", "", "")?;
        crate::db::sync_outbox::SyncOutboxRepository::claim_items(
            &mut conn,
            0,
            "worker",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:30Z",
            25,
        )?;

        assert_eq!(
            MeetingArtifactRepository::delete_for_live_meeting_guarded(
                &conn,
                meeting_id,
                artifact_id,
            )?,
            ArtifactDeleteOutcome::RequiresHub
        );
        assert!(MeetingArtifactRepository::get(&conn, artifact_id)?.is_some());
        assert!(MeetingArtifactRepository::delete_for_live_meeting(
            &conn,
            meeting_id,
            artifact_id,
        )?);
        Ok(())
    }

    #[test]
    fn guarded_artifact_delete_rejects_wrong_parent_and_unknown_ids() -> Result<()> {
        let conn = setup_db()?;
        let first = MeetingRepository::insert(&conn, Some("First"), "/tmp/first.wav")?;
        let second = MeetingRepository::insert(&conn, Some("Second"), "/tmp/second.wav")?;
        let artifact = MeetingArtifactRepository::insert_pending(
            &conn, first, "summary", "Summary", None, None,
        )?;
        MeetingArtifactRepository::fail(&conn, artifact, "failed", "", "")?;

        assert_eq!(
            MeetingArtifactRepository::delete_for_live_meeting_guarded(&conn, second, artifact)?,
            ArtifactDeleteOutcome::NotFound
        );
        assert_eq!(
            MeetingArtifactRepository::delete_for_live_meeting_guarded(&conn, first, i64::MAX)?,
            ArtifactDeleteOutcome::NotFound
        );
        assert!(MeetingArtifactRepository::get(&conn, artifact)?.is_some());
        Ok(())
    }

    #[test]
    fn guarded_artifact_delete_rejects_pending_and_running_rows() -> Result<()> {
        let conn = setup_db()?;
        let meeting = MeetingRepository::insert(&conn, Some("First"), "/tmp/first.wav")?;
        let pending = MeetingArtifactRepository::insert_pending(
            &conn, meeting, "summary", "Pending", None, None,
        )?;
        let running = MeetingArtifactRepository::insert_pending(
            &conn, meeting, "summary", "Running", None, None,
        )?;
        MeetingArtifactRepository::set_running(&conn, running)?;

        for artifact in [pending, running] {
            assert_eq!(
                MeetingArtifactRepository::delete_for_live_meeting_guarded(
                    &conn, meeting, artifact,
                )?,
                ArtifactDeleteOutcome::InFlight
            );
            assert!(MeetingArtifactRepository::get(&conn, artifact)?.is_some());
        }
        Ok(())
    }
}
