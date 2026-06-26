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

    pub fn delete_for_meeting(conn: &Connection, meeting_id: i64, id: i64) -> Result<bool> {
        let n = conn
            .execute(
                "DELETE FROM meeting_artifacts WHERE id = ?1 AND meeting_id = ?2",
                params![id, meeting_id],
            )
            .context("Failed to delete meeting artifact")?;
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
