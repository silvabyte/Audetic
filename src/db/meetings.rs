//! Meeting record persistence.
//!
//! CRUD operations for the `meetings` table. Follows the same pattern as
//! `operations.rs` — raw SQL with rusqlite, no ORM.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::meeting::status::MeetingPhase;

/// A meeting record from the database.
#[derive(Debug, Clone)]
pub struct MeetingRecord {
    pub id: i64,
    pub title: Option<String>,
    pub status: String,
    pub audio_path: String,
    pub transcript_path: Option<String>,
    pub transcript_text: Option<String>,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
}

/// Repository for meeting records.
pub struct MeetingRepository;

impl MeetingRepository {
    /// Insert a new meeting record (status = recording).
    /// Returns the new meeting ID.
    pub fn insert(conn: &Connection, title: Option<&str>, audio_path: &str) -> Result<i64> {
        conn.execute(
            "INSERT INTO meetings (title, status, audio_path) VALUES (?1, ?2, ?3)",
            params![title, MeetingPhase::Recording.as_str(), audio_path],
        )
        .context("Failed to insert meeting")?;

        Ok(conn.last_insert_rowid())
    }

    /// Update the meeting status.
    pub fn update_status(conn: &Connection, id: i64, phase: MeetingPhase) -> Result<()> {
        conn.execute(
            "UPDATE meetings SET status = ?1 WHERE id = ?2",
            params![phase.as_str(), id],
        )
        .context("Failed to update meeting status")?;
        Ok(())
    }

    /// Mark meeting as completed with transcript and duration.
    pub fn complete(
        conn: &Connection,
        id: i64,
        transcript_path: &str,
        transcript_text: &str,
        duration_seconds: i64,
    ) -> Result<()> {
        conn.execute(
            "UPDATE meetings SET status = ?1, transcript_path = ?2, transcript_text = ?3, \
             duration_seconds = ?4, completed_at = CURRENT_TIMESTAMP WHERE id = ?5",
            params![
                MeetingPhase::Completed.as_str(),
                transcript_path,
                transcript_text,
                duration_seconds,
                id,
            ],
        )
        .context("Failed to complete meeting")?;
        Ok(())
    }

    /// Mark meeting as failed with error.
    pub fn fail(conn: &Connection, id: i64, error: &str) -> Result<()> {
        conn.execute(
            "UPDATE meetings SET status = ?1, error = ?2, completed_at = CURRENT_TIMESTAMP WHERE id = ?3",
            params![MeetingPhase::Error.as_str(), error, id],
        )
        .context("Failed to mark meeting as failed")?;
        Ok(())
    }

    /// Get a meeting by ID.
    pub fn get(conn: &Connection, id: i64) -> Result<Option<MeetingRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, title, status, audio_path, transcript_path, transcript_text, \
                 duration_seconds, started_at, completed_at, error, created_at \
                 FROM meetings WHERE id = ?1",
            )
            .context("Failed to prepare meeting query")?;

        let mut rows = stmt
            .query_map(params![id], |row| {
                Ok(MeetingRecord {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    status: row.get(2)?,
                    audio_path: row.get(3)?,
                    transcript_path: row.get(4)?,
                    transcript_text: row.get(5)?,
                    duration_seconds: row.get(6)?,
                    started_at: row.get(7)?,
                    completed_at: row.get(8)?,
                    error: row.get(9)?,
                    created_at: row.get(10)?,
                })
            })
            .context("Failed to query meeting")?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// List meetings, newest first.
    pub fn list(conn: &Connection, limit: usize) -> Result<Vec<MeetingRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, title, status, audio_path, transcript_path, transcript_text, \
                 duration_seconds, started_at, completed_at, error, created_at \
                 FROM meetings ORDER BY started_at DESC, id DESC LIMIT ?1",
            )
            .context("Failed to prepare meetings list query")?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(MeetingRecord {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    status: row.get(2)?,
                    audio_path: row.get(3)?,
                    transcript_path: row.get(4)?,
                    transcript_text: row.get(5)?,
                    duration_seconds: row.get(6)?,
                    started_at: row.get(7)?,
                    completed_at: row.get(8)?,
                    error: row.get(9)?,
                    created_at: row.get(10)?,
                })
            })
            .context("Failed to list meetings")?;

        let mut meetings = Vec::new();
        for row in rows {
            meetings.push(row?);
        }

        Ok(meetings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrate;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn test_insert_meeting() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, Some("Standup"), "/tmp/meeting.wav").unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_get_meeting() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, Some("Test"), "/tmp/test.wav").unwrap();

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(meeting.id, id);
        assert_eq!(meeting.title, Some("Test".to_string()));
        assert_eq!(meeting.status, "recording");
        assert_eq!(meeting.audio_path, "/tmp/test.wav");
    }

    #[test]
    fn test_get_nonexistent_meeting() {
        let conn = setup_db();
        let result = MeetingRepository::get(&conn, 9999).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_update_status() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, None, "/tmp/test.wav").unwrap();

        MeetingRepository::update_status(&conn, id, MeetingPhase::Transcribing).unwrap();

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(meeting.status, "transcribing");
    }

    #[test]
    fn test_complete_meeting() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, Some("Meeting"), "/tmp/test.wav").unwrap();

        MeetingRepository::complete(
            &conn,
            id,
            "/tmp/test.txt",
            "Hello world transcript",
            3600,
        )
        .unwrap();

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(meeting.status, "completed");
        assert_eq!(meeting.transcript_path, Some("/tmp/test.txt".to_string()));
        assert_eq!(
            meeting.transcript_text,
            Some("Hello world transcript".to_string())
        );
        assert_eq!(meeting.duration_seconds, Some(3600));
        assert!(meeting.completed_at.is_some());
    }

    #[test]
    fn test_fail_meeting() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, None, "/tmp/test.wav").unwrap();

        MeetingRepository::fail(&conn, id, "Transcription timeout").unwrap();

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(meeting.status, "error");
        assert_eq!(meeting.error, Some("Transcription timeout".to_string()));
    }

    #[test]
    fn test_list_meetings() {
        let conn = setup_db();

        MeetingRepository::insert(&conn, Some("Meeting 1"), "/tmp/m1.wav").unwrap();
        MeetingRepository::insert(&conn, Some("Meeting 2"), "/tmp/m2.wav").unwrap();
        MeetingRepository::insert(&conn, Some("Meeting 3"), "/tmp/m3.wav").unwrap();

        let meetings = MeetingRepository::list(&conn, 2).unwrap();
        assert_eq!(meetings.len(), 2);
        // Newest first
        assert_eq!(meetings[0].title, Some("Meeting 3".to_string()));
    }

    #[test]
    fn test_list_empty() {
        let conn = setup_db();
        let meetings = MeetingRepository::list(&conn, 10).unwrap();
        assert!(meetings.is_empty());
    }
}
