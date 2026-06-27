//! SQLite persistence for generated meeting artifacts.

use anyhow::{Context, Result};
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
    pub id: i64,
    pub meeting_id: i64,
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

impl MeetingArtifactRepository {
    pub fn insert_pending(
        conn: &Connection,
        meeting_id: i64,
        kind: &str,
        title: &str,
        template_id: Option<&str>,
        agent_profile_id: Option<i64>,
    ) -> Result<i64> {
        conn.execute(
            "INSERT INTO meeting_artifacts \
             (meeting_id, kind, title, template_id, agent_profile_id, status) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                meeting_id,
                kind,
                title,
                template_id,
                agent_profile_id,
                ArtifactStatus::Pending.as_str(),
            ],
        )
        .context("Failed to insert meeting artifact")?;
        Ok(conn.last_insert_rowid())
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
        conn.execute(
            "UPDATE meeting_artifacts SET status = ?1, content_markdown = ?2, \
             stdout = ?3, stderr = ?4, error = NULL, updated_at = CURRENT_TIMESTAMP, \
             completed_at = CURRENT_TIMESTAMP WHERE id = ?5",
            params![
                ArtifactStatus::Completed.as_str(),
                content_markdown,
                stdout,
                stderr,
                id,
            ],
        )
        .context("Failed to complete meeting artifact")?;
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
                "SELECT id, meeting_id, kind, title, template_id, agent_profile_id, status, \
                 content_markdown, error, stdout, stderr, created_at, updated_at, completed_at \
                 FROM meeting_artifacts WHERE meeting_id = ?1 ORDER BY created_at DESC, id DESC",
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
                 a.content_markdown, a.error, a.stdout, a.stderr, a.created_at, a.updated_at, a.completed_at \
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
        conn.query_row(
            "SELECT id, meeting_id, kind, title, template_id, agent_profile_id, status, \
             content_markdown, error, stdout, stderr, created_at, updated_at, completed_at \
             FROM meeting_artifacts WHERE id = ?1",
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
             a.content_markdown, a.error, a.stdout, a.stderr, a.created_at, a.updated_at, a.completed_at \
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
        let n = conn
            .execute(
                "DELETE FROM meeting_artifacts \
                 WHERE id = ?1 AND meeting_id = ?2 \
                 AND EXISTS (SELECT 1 FROM meetings WHERE id = ?2 AND deleted_at IS NULL)",
                params![id, meeting_id],
            )
            .context("Failed to delete live meeting artifact")?;
        Ok(n > 0)
    }
}

fn row_to_artifact(row: &Row) -> rusqlite::Result<Result<MeetingArtifact>> {
    let status: String = row.get(6)?;
    Ok((|| {
        Ok(MeetingArtifact {
            id: row.get(0)?,
            meeting_id: row.get(1)?,
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

#[cfg(test)]
mod tests {
    use super::MeetingArtifactRepository;
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
}
