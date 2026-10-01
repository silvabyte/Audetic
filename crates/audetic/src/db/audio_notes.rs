//! The single repository for captured and imported Audio Notes.

use anyhow::{bail, Context, Result};
use audetic_core::jobs_client::Segment;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;

use crate::audio_notes::AudioNotePhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftDeleteOutcome {
    Deleted,
    NotFound,
    InFlight,
}

#[derive(Debug, Clone)]
pub struct AudioNoteRecord {
    pub id: i64,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub title_version: i64,
    pub status: String,
    pub audio_path: String,
    pub source_filename: Option<String>,
    pub transcript_path: Option<String>,
    pub transcript_text: Option<String>,
    pub transcript_segments: Option<Vec<Segment>>,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub deleted_at: Option<String>,
    pub capture_source: String,
    pub source_provider: Option<String>,
    pub source_external_id: Option<String>,
    pub source_recorded_at: Option<String>,
    pub classification: Option<Value>,
    pub enrichment_status: String,
    pub enrichment_error: Option<String>,
}

const COLUMNS: &str = "id, title, title_source, title_version, status, audio_path, source_filename, \
    transcript_path, transcript_text, transcript_segments, duration_seconds, started_at, completed_at, \
    error, created_at, deleted_at, capture_source, source_provider, source_external_id, \
    source_recorded_at, classification, enrichment_status, enrichment_error";

// SQLite trim() defaults to ASCII space only. Match Rust's Unicode White_Space
// trimming so the atomic claim rejects the same blank transcripts as domain validation.
const TRANSCRIPT_WHITESPACE: &str = "\t\n\u{b}\u{c}\r \u{85}\u{a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}";

fn decode_row(row: &Row<'_>) -> rusqlite::Result<AudioNoteRecord> {
    let classification = row.get::<_, Option<String>>(20)?;
    Ok(AudioNoteRecord {
        id: row.get(0)?,
        title: row.get(1)?,
        title_source: row.get(2)?,
        title_version: row.get(3)?,
        status: row.get(4)?,
        audio_path: row.get(5)?,
        source_filename: row.get(6)?,
        transcript_path: row.get(7)?,
        transcript_text: row.get(8)?,
        // Legacy malformed segments should not hide otherwise usable text.
        transcript_segments: row
            .get::<_, Option<String>>(9)?
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        duration_seconds: row.get(10)?,
        started_at: row.get(11)?,
        completed_at: row.get(12)?,
        error: row.get(13)?,
        created_at: row.get(14)?,
        deleted_at: row.get(15)?,
        capture_source: row.get(16)?,
        source_provider: row.get(17)?,
        source_external_id: row.get(18)?,
        source_recorded_at: row.get(19)?,
        classification: classification
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    20,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        enrichment_status: row.get(21)?,
        enrichment_error: row.get(22)?,
    })
}

pub struct AudioNoteRepository;

impl AudioNoteRepository {
    pub fn insert(conn: &Connection, title: Option<&str>, audio_path: &str) -> Result<i64> {
        Self::insert_with_source(conn, title, audio_path, None, "microphone")
    }

    pub fn insert_import(
        conn: &Connection,
        title: Option<&str>,
        audio_path: &str,
        source_filename: Option<&str>,
    ) -> Result<i64> {
        Self::insert_with_source(conn, title, audio_path, source_filename, "import")
    }

    fn insert_with_source(
        conn: &Connection,
        title: Option<&str>,
        audio_path: &str,
        source_filename: Option<&str>,
        source: &str,
    ) -> Result<i64> {
        let title = title.map(str::trim).filter(|s| !s.is_empty());
        let filename = source_filename.map(str::trim).filter(|s| !s.is_empty());
        conn.execute(
            "INSERT INTO audio_notes(title,title_source,title_updated_at,audio_path,source_filename,capture_source) \
             VALUES(?1, CASE WHEN ?1 IS NULL THEN NULL ELSE 'manual' END, \
                    CASE WHEN ?1 IS NULL THEN NULL ELSE strftime('%Y-%m-%d %H:%M:%f','now') END, ?2, ?3, ?4)",
            params![title, audio_path, filename, source],
        ).context("Failed to insert Audio Note")?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_status(conn: &Connection, id: i64, phase: AudioNotePhase) -> Result<()> {
        conn.execute(
            "UPDATE audio_notes SET status=?1 WHERE id=?2",
            params![phase.as_str(), id],
        )?;
        Ok(())
    }

    pub fn set_review(conn: &Connection, id: i64, duration_seconds: i64) -> Result<()> {
        conn.execute(
            "UPDATE audio_notes SET status=?1,duration_seconds=?2 WHERE id=?3",
            params![AudioNotePhase::Review.as_str(), duration_seconds, id],
        )?;
        Ok(())
    }

    pub fn update_audio_path(conn: &Connection, id: i64, audio_path: &str) -> Result<()> {
        conn.execute(
            "UPDATE audio_notes SET audio_path=?1 WHERE id=?2",
            params![audio_path, id],
        )?;
        Ok(())
    }

    pub fn set_capture_source(conn: &Connection, id: i64, source: &str) -> Result<()> {
        if !matches!(source, "microphone" | "microphone_and_system" | "import") {
            bail!("Invalid capture source: {source}");
        }
        conn.execute(
            "UPDATE audio_notes SET capture_source=?1 WHERE id=?2",
            params![source, id],
        )?;
        Ok(())
    }

    pub fn set_source_metadata(
        conn: &Connection,
        id: i64,
        provider: Option<&str>,
        external_id: Option<&str>,
        recorded_at: Option<&str>,
    ) -> Result<()> {
        let provider = provider.map(str::trim).filter(|value| !value.is_empty());
        let external_id = external_id.map(str::trim).filter(|value| !value.is_empty());
        if external_id.is_some() && provider.is_none() {
            bail!("Audio Note source provider is required with an external ID");
        }
        conn.execute(
            "UPDATE audio_notes
             SET source_provider = ?1, source_external_id = ?2,
                 source_recorded_at = ?3,
                 started_at = COALESCE(?3, started_at)
             WHERE id = ?4",
            params![provider, external_id, recorded_at, id],
        )?;
        Ok(())
    }

    /// The intelligence layer validates the classification contract; the DB
    /// stores arbitrary JSON so new kinds do not require schema changes.
    pub fn set_classification(conn: &Connection, id: i64, classification: &Value) -> Result<()> {
        conn.execute(
            "UPDATE audio_notes SET classification=?1 WHERE id=?2 AND deleted_at IS NULL",
            params![serde_json::to_string(classification)?, id],
        )?;
        Ok(())
    }

    pub fn set_enrichment_state(
        conn: &Connection,
        id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        if !matches!(status, "pending" | "running" | "completed" | "error") {
            bail!("Invalid enrichment status: {status}");
        }
        conn.execute(
            "UPDATE audio_notes SET enrichment_status=?1,enrichment_error=?2 WHERE id=?3",
            params![status, error, id],
        )?;
        Ok(())
    }

    /// One guarded statement prevents simultaneous retries from spawning
    /// duplicate agent runs. Only pending/failed enrichment on a completed raw
    /// transcript is eligible; completed enrichment is never implicitly rerun.
    pub fn claim_enrichment(conn: &Connection, id: i64) -> Result<bool> {
        Ok(conn
            .execute(
                "UPDATE audio_notes SET enrichment_status='running',enrichment_error=NULL \
             WHERE id=?1 AND deleted_at IS NULL AND status='completed' \
             AND transcript_text IS NOT NULL AND trim(transcript_text,?2)<>'' \
             AND enrichment_status IN ('pending','error')",
                params![id, TRANSCRIPT_WHITESPACE],
            )
            .context("Failed to claim Audio Note enrichment")?
            > 0)
    }

    /// Call once at service startup, before admitting work, not per connection.
    pub fn sweep_interrupted_enrichment(conn: &Connection) -> Result<usize> {
        Ok(conn.execute(
            "UPDATE audio_notes SET enrichment_status='error', \
             enrichment_error='Interrupted: the Audetic daemon stopped during enrichment; retry processing' \
             WHERE enrichment_status='running' AND deleted_at IS NULL", [],
        )?)
    }

    pub fn set_manual_title(conn: &Connection, id: i64, title: &str) -> Result<bool> {
        let title = title.trim();
        if title.is_empty() {
            bail!("Audio Note title cannot be blank");
        }
        Ok(conn.execute(
            "UPDATE audio_notes SET title=?1,title_source='manual', \
             title_updated_at=strftime('%Y-%m-%d %H:%M:%f','now'),title_version=title_version+1 \
             WHERE id=?2 AND deleted_at IS NULL",
            params![title, id],
        )? > 0)
    }

    pub fn set_generated_title_if_unowned(
        conn: &Connection,
        id: i64,
        title: &str,
        title_version: i64,
    ) -> Result<bool> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(false);
        }
        Ok(conn.execute(
            "UPDATE audio_notes SET title=?1,title_source='generated',title_updated_at=strftime('%Y-%m-%d %H:%M:%f','now') \
             WHERE id=?2 AND deleted_at IS NULL AND title IS NULL AND title_source IS NULL AND title_version=?3",
            params![title,id,title_version],
        )? > 0)
    }

    pub fn release_title_for_regeneration(conn: &Connection, id: i64) -> Result<bool> {
        Ok(conn.execute(
            "UPDATE audio_notes SET title=NULL,title_source=NULL,title_updated_at=strftime('%Y-%m-%d %H:%M:%f','now'), \
             title_version=title_version+1 WHERE id=?1 AND deleted_at IS NULL AND status='completed' \
             AND transcript_text IS NOT NULL AND trim(transcript_text)<>''", [id],
        )? > 0)
    }

    pub fn recent_manual_titles(conn: &Connection, limit: usize) -> Result<Vec<String>> {
        let mut stmt = conn.prepare("SELECT title FROM audio_notes WHERE deleted_at IS NULL AND title_source='manual' \
            AND title IS NOT NULL GROUP BY title ORDER BY MAX(COALESCE(title_updated_at,started_at)) DESC, MAX(id) DESC LIMIT ?1")?;
        let rows = stmt.query_map([i64::try_from(limit)?], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn complete(
        conn: &Connection,
        id: i64,
        transcript_path: &str,
        transcript_text: &str,
        transcript_segments: Option<&[Segment]>,
        duration_seconds: i64,
    ) -> Result<()> {
        let segments = transcript_segments
            .filter(|s| !s.is_empty())
            .map(serde_json::to_string)
            .transpose()?;
        let affected = conn.execute(
            "UPDATE audio_notes SET status='completed',transcript_path=?1,transcript_text=?2,transcript_segments=?3, \
             duration_seconds=?4,error=NULL,completed_at=CURRENT_TIMESTAMP,enrichment_status='pending',enrichment_error=NULL \
             WHERE id=?5 AND deleted_at IS NULL", params![transcript_path,transcript_text,segments,duration_seconds,id],
        ).context("Failed to persist completed Audio Note")?;
        if affected == 0 {
            bail!("Cannot complete missing or deleted Audio Note {id}");
        }
        Ok(())
    }

    pub fn fail(conn: &Connection, id: i64, error: &str, duration_seconds: i64) -> Result<()> {
        conn.execute("UPDATE audio_notes SET status='error',error=?1,duration_seconds=?2,completed_at=CURRENT_TIMESTAMP WHERE id=?3", params![error,duration_seconds,id])?;
        Ok(())
    }

    pub fn cancel(conn: &Connection, id: i64, duration_seconds: i64) -> Result<()> {
        conn.execute("UPDATE audio_notes SET status='cancelled',duration_seconds=?1,completed_at=CURRENT_TIMESTAMP WHERE id=?2", params![duration_seconds,id])?;
        Ok(())
    }

    pub fn sweep_interrupted(conn: &Connection) -> Result<usize> {
        let terminal = AudioNotePhase::TERMINAL_STATUSES.join("', '");
        Ok(conn.execute(&format!("UPDATE audio_notes SET error='Interrupted: the Audetic daemon stopped while this Audio Note was ' || status, \
            status='error',completed_at=CURRENT_TIMESTAMP WHERE status NOT IN ('{terminal}') AND deleted_at IS NULL"), [])?)
    }

    pub fn begin_retry(conn: &Connection, id: i64) -> Result<bool> {
        Ok(conn.execute("UPDATE audio_notes SET status='transcribing' WHERE id=?1 AND status='error' AND deleted_at IS NULL", [id])? > 0)
    }

    pub fn soft_delete(conn: &Connection, id: i64) -> Result<SoftDeleteOutcome> {
        let terminal = AudioNotePhase::TERMINAL_STATUSES.join("', '");
        let changed = conn.execute(
            &format!(
                "UPDATE audio_notes SET deleted_at=CURRENT_TIMESTAMP \
            WHERE id=?1 AND deleted_at IS NULL AND status IN ('{terminal}')"
            ),
            [id],
        )?;
        if changed > 0 {
            return Ok(SoftDeleteOutcome::Deleted);
        }
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM audio_notes WHERE id=?1 AND deleted_at IS NULL",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(match status {
            Some(s) if !AudioNotePhase::is_terminal(&s) => SoftDeleteOutcome::InFlight,
            _ => SoftDeleteOutcome::NotFound,
        })
    }

    pub fn get(conn: &Connection, id: i64) -> Result<Option<AudioNoteRecord>> {
        Ok(conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM audio_notes WHERE id=?1 AND deleted_at IS NULL"),
                [id],
                decode_row,
            )
            .optional()?)
    }

    pub fn list(conn: &Connection, limit: usize) -> Result<Vec<AudioNoteRecord>> {
        Self::search(conn, None, None, limit, 0)
    }

    pub fn search(
        conn: &Connection,
        query: Option<&str>,
        kind: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<AudioNoteRecord>> {
        // instr treats user text literally (including SQL LIKE wildcard chars).
        let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM audio_notes WHERE deleted_at IS NULL \
            AND (?1 IS NULL OR instr(lower(coalesce(title,'') || ' ' || coalesce(transcript_text,'')), lower(?1)) > 0) \
            AND (?2 IS NULL OR json_extract(classification,'$.kind')=?2) \
            ORDER BY started_at DESC,id DESC LIMIT ?3 OFFSET ?4"))?;
        let rows = stmt.query_map(
            params![query, kind, i64::try_from(limit)?, i64::try_from(offset)?],
            decode_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&conn).unwrap();
        conn
    }

    fn completed(conn: &Connection, title: &str) -> i64 {
        let id = AudioNoteRepository::insert(conn, Some(title), "audio.wav").unwrap();
        AudioNoteRepository::complete(conn, id, "text.txt", "Buy 20% milk", None, 10).unwrap();
        id
    }

    #[test]
    fn capture_import_and_segments_round_trip() {
        let conn = db();
        let id = AudioNoteRepository::insert_import(
            &conn,
            Some("  Plan  "),
            "a.wav",
            Some("original.wav"),
        )
        .unwrap();
        let segment = Segment {
            start: 0.,
            end: 1.,
            text: "hello".into(),
        };
        AudioNoteRepository::complete(&conn, id, "t.txt", "hello", Some(&[segment]), 1).unwrap();
        let note = AudioNoteRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(note.capture_source, "import");
        assert_eq!(note.source_filename.as_deref(), Some("original.wav"));
        assert_eq!(note.title.as_deref(), Some("Plan"));
        assert_eq!(note.transcript_segments.unwrap()[0].text, "hello");
        assert!(AudioNoteRepository::complete(&conn, 999, "t", "text", None, 0).is_err());
    }

    #[test]
    fn search_filters_paginates_and_hides_deleted_notes() {
        let conn = db();
        let first = completed(&conn, "Old");
        let second = completed(&conn, "Shopping");
        AudioNoteRepository::set_classification(
            &conn,
            second,
            &serde_json::json!({"kind":"shopping-list","metadata":{"extra":1}}),
        )
        .unwrap();
        assert_eq!(
            AudioNoteRepository::search(&conn, Some("20%"), Some("shopping-list"), 10, 0).unwrap()
                [0]
            .id,
            second
        );
        assert_eq!(
            AudioNoteRepository::search(&conn, None, None, 1, 1).unwrap()[0].id,
            first
        );
        AudioNoteRepository::soft_delete(&conn, second).unwrap();
        assert!(AudioNoteRepository::get(&conn, second).unwrap().is_none());
        assert_eq!(AudioNoteRepository::list(&conn, 10).unwrap().len(), 1);
    }

    #[test]
    fn title_ownership_rejects_stale_generation() {
        let conn = db();
        let id = completed(&conn, "Manual");
        assert!(!AudioNoteRepository::set_generated_title_if_unowned(&conn, id, "No", 0).unwrap());
        assert!(AudioNoteRepository::release_title_for_regeneration(&conn, id).unwrap());
        assert!(
            !AudioNoteRepository::set_generated_title_if_unowned(&conn, id, "Stale", 0).unwrap()
        );
        assert!(
            AudioNoteRepository::set_generated_title_if_unowned(&conn, id, "Fresh", 1).unwrap()
        );
        assert!(AudioNoteRepository::set_manual_title(&conn, id, " Human ").unwrap());
        assert_eq!(
            AudioNoteRepository::recent_manual_titles(&conn, 10).unwrap(),
            vec!["Human"]
        );
    }

    #[test]
    fn retries_are_claimed_and_recovery_is_explicit() {
        let conn = db();
        let id = AudioNoteRepository::insert(&conn, None, "a").unwrap();
        assert!(!AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        assert_eq!(
            AudioNoteRepository::soft_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::InFlight
        );
        AudioNoteRepository::set_review(&conn, id, 30).unwrap();
        assert_eq!(AudioNoteRepository::sweep_interrupted(&conn).unwrap(), 1);
        assert_eq!(
            AudioNoteRepository::get(&conn, id)
                .unwrap()
                .unwrap()
                .duration_seconds,
            Some(30)
        );
        assert!(AudioNoteRepository::begin_retry(&conn, id).unwrap());
        assert!(!AudioNoteRepository::begin_retry(&conn, id).unwrap());
        AudioNoteRepository::complete(&conn, id, "t", "hello", None, 30).unwrap();
        assert!(AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        assert!(!AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        crate::db::migrate(&conn).unwrap();
        assert_eq!(
            AudioNoteRepository::get(&conn, id)
                .unwrap()
                .unwrap()
                .enrichment_status,
            "running"
        );
        assert_eq!(
            AudioNoteRepository::sweep_interrupted_enrichment(&conn).unwrap(),
            1
        );
        assert!(AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        AudioNoteRepository::set_enrichment_state(&conn, id, "completed", None).unwrap();
    }

    #[test]
    fn concurrent_enrichment_claim_has_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.sqlite3");
        let conn = crate::db::init_db_at(&path).unwrap();
        let id = completed(&conn, "Concurrent");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let barrier = barrier.clone();
                let path = path.clone();
                std::thread::spawn(move || {
                    let conn = crate::db::init_db_at(&path).unwrap();
                    barrier.wait();
                    AudioNoteRepository::claim_enrichment(&conn, id).unwrap()
                })
            })
            .collect();
        let claimed = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap().then_some(()))
            .count();
        assert_eq!(claimed, 1);
        assert_eq!(
            AudioNoteRepository::get(&conn, id)
                .unwrap()
                .unwrap()
                .enrichment_status,
            "running"
        );
    }

    #[test]
    fn enrichment_claim_rejects_completed_missing_empty_and_inflight_notes() {
        let conn = db();
        let id = completed(&conn, "Eligible");
        assert!(AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        assert!(!AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        AudioNoteRepository::set_enrichment_state(&conn, id, "completed", None).unwrap();
        assert!(!AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        AudioNoteRepository::set_enrichment_state(&conn, id, "error", Some("retry me")).unwrap();
        assert!(AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        assert!(AudioNoteRepository::get(&conn, id)
            .unwrap()
            .unwrap()
            .enrichment_error
            .is_none());
        for text in [
            None,
            Some(""),
            Some("   "),
            Some("\n\t\r"),
            Some("\u{a0}\u{3000}"),
        ] {
            conn.execute(
                "UPDATE audio_notes SET transcript_text=?1,enrichment_status='pending' WHERE id=?2",
                params![text, id],
            )
            .unwrap();
            assert!(!AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        }
        conn.execute(
            "UPDATE audio_notes SET transcript_text='saved text',status='transcribing' WHERE id=?1",
            [id],
        )
        .unwrap();
        assert!(!AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        assert!(!AudioNoteRepository::claim_enrichment(&conn, 999).unwrap());
    }

    #[test]
    fn completion_clears_old_error_and_legacy_bad_segments_fall_back_to_text() {
        let conn = db();
        let id = AudioNoteRepository::insert(&conn, None, "audio.wav").unwrap();
        AudioNoteRepository::fail(&conn, id, "old failure", 20).unwrap();
        AudioNoteRepository::complete(&conn, id, "text.txt", "Recovered", None, 20).unwrap();
        conn.execute(
            "UPDATE audio_notes SET transcript_segments='not json' WHERE id=?1",
            [id],
        )
        .unwrap();
        let note = AudioNoteRepository::get(&conn, id).unwrap().unwrap();
        assert!(note.error.is_none());
        assert!(note.transcript_segments.is_none());
        assert_eq!(note.transcript_text.as_deref(), Some("Recovered"));
        assert_eq!(AudioNoteRepository::sweep_interrupted(&conn).unwrap(), 0);
        AudioNoteRepository::soft_delete(&conn, id).unwrap();
        assert!(!AudioNoteRepository::claim_enrichment(&conn, id).unwrap());
        assert_eq!(
            AudioNoteRepository::soft_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::NotFound
        );
    }
}
