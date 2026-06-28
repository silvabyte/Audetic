//! Meeting record persistence.
//!
//! CRUD operations for the `meetings` table. Follows the same pattern as
//! `operations.rs` — raw SQL with rusqlite, no ORM.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::meeting::status::MeetingPhase;

/// Result of a soft-delete attempt, so the API can answer with the right
/// status code (200 / 404 / 409).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftDeleteOutcome {
    /// The row was stamped `deleted_at` and is now hidden from all views.
    Deleted,
    /// No live row with that id — unknown, or already deleted.
    NotFound,
    /// The meeting is still in-flight (recording / review / processing), so
    /// deletion was refused; stop or cancel it first.
    InFlight,
}

/// A meeting record from the database.
#[derive(Debug, Clone)]
pub struct MeetingRecord {
    pub id: i64,
    pub title: Option<String>,
    pub status: String,
    pub audio_path: String,
    pub transcript_path: Option<String>,
    pub transcript_text: Option<String>,
    /// JSON array of `{start,end,text}` segment timestamps, or `None` for
    /// meetings transcribed before timestamps were captured.
    pub transcript_segments: Option<String>,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    /// When set, the meeting has been soft-deleted and is hidden from every
    /// API surface (list, detail, audio, retry). The row and on-disk audio
    /// survive; recovery is a manual DB edit.
    pub deleted_at: Option<String>,
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

    /// Mark a meeting as awaiting review after recording stopped, persisting
    /// the captured duration. The audio WAV is on disk but has not yet been
    /// sent for transcription; the user confirms (and optionally trims) it via
    /// `MeetingMachine::confirm`.
    pub fn set_review(conn: &Connection, id: i64, duration_seconds: i64) -> Result<()> {
        conn.execute(
            "UPDATE meetings SET status = ?1, duration_seconds = ?2 WHERE id = ?3",
            params![MeetingPhase::Review.as_str(), duration_seconds, id],
        )
        .context("Failed to mark meeting for review")?;
        Ok(())
    }

    /// Update the meeting's `audio_path`. The compression pipeline replaces
    /// the original WAV with an MP3 next to it; this keeps the DB row pointing
    /// at the file that actually exists on disk so retries can find it.
    pub fn update_audio_path(conn: &Connection, id: i64, audio_path: &str) -> Result<()> {
        conn.execute(
            "UPDATE meetings SET audio_path = ?1 WHERE id = ?2",
            params![audio_path, id],
        )
        .context("Failed to update meeting audio_path")?;
        Ok(())
    }

    /// Mark meeting as completed with transcript and duration. Clears any
    /// `error` column from a prior failed run so a successful retry leaves
    /// the row in a clean terminal state (otherwise the UI would still show
    /// the old error banner alongside the new transcript).
    pub fn complete(
        conn: &Connection,
        id: i64,
        transcript_path: &str,
        transcript_text: &str,
        transcript_segments: Option<&str>,
        duration_seconds: i64,
    ) -> Result<()> {
        conn.execute(
            "UPDATE meetings SET status = ?1, transcript_path = ?2, transcript_text = ?3, \
             transcript_segments = ?4, duration_seconds = ?5, error = NULL, \
             completed_at = CURRENT_TIMESTAMP WHERE id = ?6",
            params![
                MeetingPhase::Completed.as_str(),
                transcript_path,
                transcript_text,
                transcript_segments,
                duration_seconds,
                id,
            ],
        )
        .context("Failed to complete meeting")?;
        Ok(())
    }

    /// Mark meeting as failed with error and persist the recorded duration.
    pub fn fail(conn: &Connection, id: i64, error: &str, duration_seconds: i64) -> Result<()> {
        conn.execute(
            "UPDATE meetings SET status = ?1, error = ?2, duration_seconds = ?3, \
             completed_at = CURRENT_TIMESTAMP WHERE id = ?4",
            params![MeetingPhase::Error.as_str(), error, duration_seconds, id],
        )
        .context("Failed to mark meeting as failed")?;
        Ok(())
    }

    /// Atomically move a failed meeting into `transcribing` as a retry starts.
    ///
    /// Only succeeds if the row is still live and `error`, so a single SQL
    /// statement both rejects a double-retry and — critically — flips the row
    /// out of a terminal state *before* the retry endpoint returns 202. Without
    /// this, the row stays `error` until the spawned task gets around to
    /// updating it, and a DELETE landing in that window would see a terminal
    /// row and hide an already-accepted retry. Returns false if the row wasn't
    /// in the expected state.
    pub fn begin_retry(conn: &Connection, id: i64) -> Result<bool> {
        let affected = conn
            .execute(
                "UPDATE meetings SET status = ?1 \
                 WHERE id = ?2 AND status = ?3 AND deleted_at IS NULL",
                params![
                    MeetingPhase::Transcribing.as_str(),
                    id,
                    MeetingPhase::Error.as_str(),
                ],
            )
            .context("Failed to mark meeting retry in-flight")?;
        Ok(affected > 0)
    }

    /// Mark meeting as cancelled with the recorded duration.
    pub fn cancel(conn: &Connection, id: i64, duration_seconds: i64) -> Result<()> {
        conn.execute(
            "UPDATE meetings SET status = ?1, duration_seconds = ?2, \
             completed_at = CURRENT_TIMESTAMP WHERE id = ?3",
            params![MeetingPhase::Cancelled.as_str(), duration_seconds, id],
        )
        .context("Failed to mark meeting as cancelled")?;
        Ok(())
    }

    /// Soft-delete a meeting: stamp `deleted_at` so it disappears from every
    /// API surface (list, detail, audio, retry) while the row and the on-disk
    /// audio survive.
    ///
    /// Refuses in-flight meetings (recording / review / processing): those ids
    /// are still owned by the meeting machine and background pipeline, so
    /// hiding the row would 404 the active/review UI and break completion
    /// auto-nav.
    ///
    /// The terminal-status predicate lives **inside** the `UPDATE`, so the
    /// guard and the write are one atomic statement. A separate
    /// SELECT-then-UPDATE would leave a window where a concurrent
    /// `POST /meetings/:id/retry` could flip `error` → `transcribing` after the
    /// check but before the write, hiding an in-flight retry despite the 409
    /// contract. Returns [`SoftDeleteOutcome`] so the caller can map it to
    /// 200 / 404 / 409.
    ///
    /// This only hides DB-backed reads. On `Deleted`, the caller must also
    /// clear the in-memory live status if it still references this meeting
    /// (`MeetingStatusHandle::clear_if_current`), or `GET /meetings/status`
    /// keeps reporting the deleted meeting until the next recording.
    pub fn soft_delete(conn: &Connection, id: i64) -> Result<SoftDeleteOutcome> {
        // Build the IN-list from the single terminal-status source. The values
        // are compile-time constants (never user input), so interpolating them
        // is injection-safe; `id` is still bound as a parameter.
        let terminal = MeetingPhase::TERMINAL_STATUSES.join("', '");
        let affected = conn
            .execute(
                &format!(
                    "UPDATE meetings SET deleted_at = CURRENT_TIMESTAMP \
                     WHERE id = ?1 AND deleted_at IS NULL AND status IN ('{terminal}')"
                ),
                params![id],
            )
            .context("Failed to soft-delete meeting")?;

        if affected > 0 {
            return Ok(SoftDeleteOutcome::Deleted);
        }

        // Nothing was hidden — read the live row only to choose between 404 and
        // 409. This is advisory: the guarded UPDATE above already guarantees we
        // never stamp an in-flight meeting, regardless of how this read races.
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM meetings WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to look up meeting after delete")?;

        Ok(match status {
            Some(s) if !MeetingPhase::is_terminal(&s) => SoftDeleteOutcome::InFlight,
            // Either gone (no live row) or a terminal row a concurrent delete
            // claimed first — nothing live remains for us to remove.
            _ => SoftDeleteOutcome::NotFound,
        })
    }

    /// Get a meeting by ID. Soft-deleted meetings are treated as absent.
    pub fn get(conn: &Connection, id: i64) -> Result<Option<MeetingRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, title, status, audio_path, transcript_path, transcript_text, \
                 duration_seconds, started_at, completed_at, error, created_at, deleted_at, \
                 transcript_segments \
                 FROM meetings WHERE id = ?1 AND deleted_at IS NULL",
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
                    deleted_at: row.get(11)?,
                    transcript_segments: row.get(12)?,
                })
            })
            .context("Failed to query meeting")?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// List meetings, newest first. Soft-deleted meetings are excluded.
    pub fn list(conn: &Connection, limit: usize) -> Result<Vec<MeetingRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, title, status, audio_path, transcript_path, transcript_text, \
                 duration_seconds, started_at, completed_at, error, created_at, deleted_at, \
                 transcript_segments \
                 FROM meetings WHERE deleted_at IS NULL \
                 ORDER BY started_at DESC, id DESC LIMIT ?1",
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
                    deleted_at: row.get(11)?,
                    transcript_segments: row.get(12)?,
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
            None,
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

        MeetingRepository::fail(&conn, id, "Transcription timeout", 47).unwrap();

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(meeting.status, "error");
        assert_eq!(meeting.error, Some("Transcription timeout".to_string()));
        assert_eq!(meeting.duration_seconds, Some(47));
        assert!(meeting.completed_at.is_some());
    }

    #[test]
    fn test_cancel_meeting() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, Some("Test"), "/tmp/test.wav").unwrap();

        MeetingRepository::cancel(&conn, id, 12).unwrap();

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(meeting.status, "cancelled");
        assert_eq!(meeting.duration_seconds, Some(12));
        assert!(meeting.completed_at.is_some());
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

    /// Insert a meeting already in a terminal (deletable) state. `insert`
    /// always starts at `recording`, which is in-flight, so terminal-state
    /// tests move it to `completed` first.
    fn insert_completed(conn: &Connection, title: &str, path: &str) -> i64 {
        let id = MeetingRepository::insert(conn, Some(title), path).unwrap();
        MeetingRepository::complete(conn, id, "/tmp/t.txt", "transcript", None, 10).unwrap();
        id
    }

    #[test]
    fn test_soft_delete_hides_from_get_and_list() {
        let conn = setup_db();
        let keep = insert_completed(&conn, "Keep", "/tmp/keep.wav");
        let drop = insert_completed(&conn, "Drop", "/tmp/drop.wav");

        assert_eq!(
            MeetingRepository::soft_delete(&conn, drop).unwrap(),
            SoftDeleteOutcome::Deleted
        );

        // Hidden from get()
        assert!(MeetingRepository::get(&conn, drop).unwrap().is_none());
        // Still retrievable: the surviving meeting
        assert!(MeetingRepository::get(&conn, keep).unwrap().is_some());
        // Hidden from list()
        let listed = MeetingRepository::list(&conn, 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, keep);
    }

    #[test]
    fn test_soft_delete_is_idempotent() {
        let conn = setup_db();
        let id = insert_completed(&conn, "Test", "/tmp/test.wav");

        // First delete affects the row, second finds nothing live.
        assert_eq!(
            MeetingRepository::soft_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::Deleted
        );
        assert_eq!(
            MeetingRepository::soft_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::NotFound
        );
    }

    #[test]
    fn test_soft_delete_unknown_id() {
        let conn = setup_db();
        assert_eq!(
            MeetingRepository::soft_delete(&conn, 9999).unwrap(),
            SoftDeleteOutcome::NotFound
        );
    }

    #[test]
    fn test_soft_delete_rejects_in_flight() {
        let conn = setup_db();
        // `insert` starts at `recording` — an in-flight phase.
        let id = MeetingRepository::insert(&conn, Some("Live"), "/tmp/live.wav").unwrap();

        for phase in [
            MeetingPhase::Recording,
            MeetingPhase::Review,
            MeetingPhase::Compressing,
            MeetingPhase::Transcribing,
        ] {
            MeetingRepository::update_status(&conn, id, phase).unwrap();
            assert_eq!(
                MeetingRepository::soft_delete(&conn, id).unwrap(),
                SoftDeleteOutcome::InFlight,
                "phase {} should be refused",
                phase.as_str()
            );
            // Still visible — not hidden.
            assert!(MeetingRepository::get(&conn, id).unwrap().is_some());
        }
    }

    #[test]
    fn test_begin_retry_only_from_error() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, Some("Test"), "/tmp/test.wav").unwrap();

        // Fresh meeting is `recording`, not retry-eligible.
        assert!(!MeetingRepository::begin_retry(&conn, id).unwrap());

        // After a failure it is — and the transition flips it to transcribing.
        MeetingRepository::fail(&conn, id, "boom", 10).unwrap();
        assert!(MeetingRepository::begin_retry(&conn, id).unwrap());
        assert_eq!(
            MeetingRepository::get(&conn, id).unwrap().unwrap().status,
            "transcribing"
        );

        // A second concurrent retry finds it already in-flight.
        assert!(!MeetingRepository::begin_retry(&conn, id).unwrap());
    }

    #[test]
    fn test_begin_retry_blocks_delete_window() {
        // Reproduces the race the guard closes: once a retry is accepted, the
        // meeting must not be deletable even though it was just `error`.
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, Some("Test"), "/tmp/test.wav").unwrap();
        MeetingRepository::fail(&conn, id, "boom", 10).unwrap();

        // Before retry: terminal, so deletable.
        // (Don't actually delete — just assert begin_retry then flips it.)
        assert!(MeetingRepository::begin_retry(&conn, id).unwrap());

        // After retry is accepted the delete guard refuses it.
        assert_eq!(
            MeetingRepository::soft_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::InFlight
        );
        assert!(MeetingRepository::get(&conn, id).unwrap().is_some());
    }

    #[test]
    fn test_soft_delete_keeps_row_on_disk() {
        let conn = setup_db();
        let id = insert_completed(&conn, "Test", "/tmp/test.wav");

        MeetingRepository::soft_delete(&conn, id).unwrap();

        // The physical row survives with deleted_at stamped — only the
        // repository's filtered reads hide it.
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM meetings WHERE id = ?1 AND deleted_at IS NOT NULL",
                params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
