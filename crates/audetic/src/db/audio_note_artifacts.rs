//! SQLite persistence for durable Audio Note processor outputs.

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
            other => anyhow::bail!("unknown artifact status `{other}`"),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AudioNoteArtifact {
    pub id: i64,
    pub note_id: i64,
    pub kind: String,
    pub title: String,
    pub template_id: Option<String>,
    pub agent_profile_id: Option<i64>,
    pub status: ArtifactStatus,
    pub content_markdown: Option<String>,
    pub content_json: Option<serde_json::Value>,
    pub error: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

const COLUMNS: &str = "a.id,a.note_id,a.kind,a.title,a.template_id,a.agent_profile_id,a.status, \
    a.content_markdown,a.error,a.stdout,a.stderr,a.created_at,a.updated_at,a.completed_at,a.content_json";

pub struct AudioNoteArtifactRepository;

impl AudioNoteArtifactRepository {
    pub fn insert_pending(
        conn: &Connection,
        note_id: i64,
        kind: &str,
        title: &str,
        template_id: Option<&str>,
        agent_profile_id: Option<i64>,
    ) -> Result<i64> {
        conn.execute("INSERT INTO audio_note_artifacts(note_id,kind,title,template_id,agent_profile_id,status) VALUES(?1,?2,?3,?4,?5,'pending')", params![note_id,kind,title,template_id,agent_profile_id]).context("Failed to insert Audio Note artifact")?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_running(conn: &Connection, id: i64) -> Result<()> {
        conn.execute("UPDATE audio_note_artifacts SET status='running',updated_at=CURRENT_TIMESTAMP WHERE id=?1", [id])?;
        Ok(())
    }

    /// Call once before accepting work at daemon startup. Pending/running
    /// artifacts belong to the previous process and cannot finish themselves.
    /// Preserve partial output and diagnostic streams, including on hidden
    /// notes, but give every interrupted run a durable, actionable terminal state.
    pub fn sweep_interrupted(conn: &Connection) -> Result<usize> {
        conn.execute(
            "UPDATE audio_note_artifacts SET \
             error='Interrupted: the Audetic daemon stopped while this artifact was ' || status || \
                   '; generate the artifact again to retry', \
             status='error', updated_at=CURRENT_TIMESTAMP, completed_at=CURRENT_TIMESTAMP \
             WHERE status IN ('pending','running')",
            [],
        )
        .context("Failed to recover interrupted Audio Note artifacts")
    }

    pub fn complete(
        conn: &Connection,
        id: i64,
        content_markdown: &str,
        stdout: &str,
        stderr: &str,
    ) -> Result<()> {
        Self::complete_with_json(conn, id, content_markdown, None, stdout, stderr)
    }

    pub fn complete_with_json(
        conn: &Connection,
        id: i64,
        content_markdown: &str,
        content_json: Option<&serde_json::Value>,
        stdout: &str,
        stderr: &str,
    ) -> Result<()> {
        let json = content_json.map(serde_json::to_string).transpose()?;
        conn.execute("UPDATE audio_note_artifacts SET status='completed',content_markdown=?1,content_json=?2,stdout=?3,stderr=?4,error=NULL,updated_at=CURRENT_TIMESTAMP,completed_at=CURRENT_TIMESTAMP WHERE id=?5", params![content_markdown,json,stdout,stderr,id])?;
        Ok(())
    }

    pub fn set_content_json(conn: &Connection, id: i64, content: &serde_json::Value) -> Result<()> {
        conn.execute("UPDATE audio_note_artifacts SET content_json=?1,updated_at=CURRENT_TIMESTAMP WHERE id=?2", params![serde_json::to_string(content)?,id])?;
        Ok(())
    }

    pub fn fail(conn: &Connection, id: i64, error: &str, stdout: &str, stderr: &str) -> Result<()> {
        conn.execute("UPDATE audio_note_artifacts SET status='error',error=?1,stdout=?2,stderr=?3,updated_at=CURRENT_TIMESTAMP,completed_at=CURRENT_TIMESTAMP WHERE id=?4", params![error,stdout,stderr,id])?;
        Ok(())
    }

    pub fn list_for_note(conn: &Connection, note_id: i64) -> Result<Vec<AudioNoteArtifact>> {
        Self::list(conn, note_id, false)
    }

    pub fn list_for_live_note(conn: &Connection, note_id: i64) -> Result<Vec<AudioNoteArtifact>> {
        Self::list(conn, note_id, true)
    }

    fn list(conn: &Connection, note_id: i64, live: bool) -> Result<Vec<AudioNoteArtifact>> {
        let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM audio_note_artifacts a \
            WHERE a.note_id=?1 AND (?2=0 OR EXISTS(SELECT 1 FROM audio_notes n WHERE n.id=a.note_id AND n.deleted_at IS NULL)) \
            ORDER BY a.created_at DESC,a.id DESC"))?;
        let rows = stmt.query_map(params![note_id, live], row_to_artifact)?;
        rows.map(|r| r.map_err(anyhow::Error::from).and_then(|r| r))
            .collect()
    }

    pub fn get(conn: &Connection, id: i64) -> Result<Option<AudioNoteArtifact>> {
        conn.query_row(
            &format!("SELECT {COLUMNS} FROM audio_note_artifacts a WHERE a.id=?1"),
            [id],
            row_to_artifact,
        )
        .optional()?
        .transpose()
    }

    pub fn get_for_live_note(
        conn: &Connection,
        note_id: i64,
        id: i64,
    ) -> Result<Option<AudioNoteArtifact>> {
        conn.query_row(&format!("SELECT {COLUMNS} FROM audio_note_artifacts a INNER JOIN audio_notes n ON n.id=a.note_id AND n.deleted_at IS NULL WHERE a.id=?1 AND a.note_id=?2"), params![id,note_id],row_to_artifact).optional()?.transpose()
    }

    pub fn delete_for_note(conn: &Connection, note_id: i64, id: i64) -> Result<bool> {
        Ok(conn.execute(
            "DELETE FROM audio_note_artifacts WHERE id=?1 AND note_id=?2",
            params![id, note_id],
        )? > 0)
    }

    pub fn delete_for_live_note(conn: &Connection, note_id: i64, id: i64) -> Result<bool> {
        Ok(conn.execute("DELETE FROM audio_note_artifacts WHERE id=?1 AND note_id=?2 AND EXISTS(SELECT 1 FROM audio_notes WHERE id=?2 AND deleted_at IS NULL)",params![id,note_id])? > 0)
    }
}

fn row_to_artifact(row: &Row<'_>) -> rusqlite::Result<Result<AudioNoteArtifact>> {
    Ok((|| {
        let json: Option<String> = row.get(14)?;
        Ok(AudioNoteArtifact {
            id: row.get(0)?,
            note_id: row.get(1)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            template_id: row.get(4)?,
            agent_profile_id: row.get(5)?,
            status: ArtifactStatus::parse(&row.get::<_, String>(6)?)?,
            content_markdown: row.get(7)?,
            error: row.get(8)?,
            stdout: row.get(9)?,
            stderr: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
            completed_at: row.get(13)?,
            content_json: json.map(|s| serde_json::from_str(&s)).transpose()?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::audio_notes::AudioNoteRepository;

    #[test]
    fn structured_outputs_and_hidden_parent() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        crate::db::migrate(&conn)?;
        let note = AudioNoteRepository::insert(&conn, None, "a.wav")?;
        AudioNoteRepository::complete(&conn, note, "t", "buy milk", None, 1)?;
        let artifact = AudioNoteArtifactRepository::insert_pending(
            &conn,
            note,
            "shopping-list",
            "Items",
            None,
            None,
        )?;
        let json = serde_json::json!({"items":[{"name":"milk"}]});
        AudioNoteArtifactRepository::complete_with_json(
            &conn,
            artifact,
            "- milk",
            Some(&json),
            "",
            "",
        )?;
        assert_eq!(
            AudioNoteArtifactRepository::get(&conn, artifact)?
                .unwrap()
                .content_json,
            Some(json)
        );
        assert_eq!(
            AudioNoteArtifactRepository::list_for_live_note(&conn, note)?.len(),
            1
        );
        AudioNoteRepository::soft_delete(&conn, note)?;
        assert!(AudioNoteArtifactRepository::get_for_live_note(&conn, note, artifact)?.is_none());
        assert!(AudioNoteArtifactRepository::list_for_live_note(&conn, note)?.is_empty());
        assert!(!AudioNoteArtifactRepository::delete_for_live_note(
            &conn, note, artifact
        )?);
        assert!(AudioNoteArtifactRepository::get(&conn, artifact)?.is_some());
        assert!(
            AudioNoteArtifactRepository::insert_pending(&conn, 999, "x", "X", None, None).is_err()
        );
        Ok(())
    }

    #[test]
    fn startup_sweep_only_finishes_interrupted_artifacts_and_preserves_outputs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("notes.sqlite3");
        let conn = crate::db::init_db_at(&path)?;
        let note = AudioNoteRepository::insert(&conn, None, "a.wav")?;
        AudioNoteRepository::complete(&conn, note, "t", "transcript", None, 1)?;
        let mut ids = Vec::new();
        for status in ["pending", "running", "completed", "error"] {
            let id = AudioNoteArtifactRepository::insert_pending(
                &conn, note, "summary", status, None, None,
            )?;
            conn.execute(
                "UPDATE audio_note_artifacts SET status=?1,content_markdown='partial', \
                 content_json='{}',stdout='stdout',stderr='stderr',error='original error', \
                 updated_at='old',completed_at=NULL WHERE id=?2",
                params![status, id],
            )?;
            ids.push(id);
        }
        // Connection initialization must not perform startup recovery.
        drop(conn);
        let conn = crate::db::init_db_at(&path)?;
        assert_eq!(
            AudioNoteArtifactRepository::get(&conn, ids[0])?
                .unwrap()
                .status,
            ArtifactStatus::Pending
        );
        AudioNoteRepository::soft_delete(&conn, note)?;
        assert_eq!(AudioNoteArtifactRepository::sweep_interrupted(&conn)?, 2);
        assert_eq!(AudioNoteArtifactRepository::sweep_interrupted(&conn)?, 0);
        drop(conn);
        let conn = crate::db::init_db_at(&path)?;
        for (i, prior_status) in ["pending", "running"].iter().enumerate() {
            let artifact = AudioNoteArtifactRepository::get(&conn, ids[i])?.unwrap();
            assert_eq!(artifact.status, ArtifactStatus::Error);
            assert!(artifact.error.as_deref().unwrap().contains(prior_status));
            assert!(artifact
                .error
                .as_deref()
                .unwrap()
                .contains("again to retry"));
            assert!(artifact.completed_at.is_some());
            assert_ne!(artifact.updated_at, "old");
            assert_eq!(artifact.content_markdown.as_deref(), Some("partial"));
            assert_eq!(artifact.content_json, Some(serde_json::json!({})));
            assert_eq!(artifact.stdout.as_deref(), Some("stdout"));
            assert_eq!(artifact.stderr.as_deref(), Some("stderr"));
        }
        for (i, status) in [(2, ArtifactStatus::Completed), (3, ArtifactStatus::Error)] {
            let artifact = AudioNoteArtifactRepository::get(&conn, ids[i])?.unwrap();
            assert_eq!(artifact.status, status);
            assert_eq!(artifact.error.as_deref(), Some("original error"));
            assert_eq!(artifact.updated_at, "old");
            assert!(artifact.completed_at.is_none());
        }
        Ok(())
    }
}
