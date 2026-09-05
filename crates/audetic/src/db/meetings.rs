//! Meeting record persistence.
//!
//! CRUD operations for the `meetings` table. Follows the same pattern as
//! `operations.rs` — raw SQL with rusqlite, no ORM.

use anyhow::{Context, Result};
use audetic_core::sync::{DeviceId, RecordId, SyncRole};
use rusqlite::{params, Connection, OptionalExtension};

use crate::meeting::status::MeetingPhase;
use audetic_core::jobs_client::Segment;

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
    /// An upload attempt may already have committed on the active Home Hub.
    /// The caller must create the hub tombstone before hiding the local row.
    RequiresHub,
}

/// A meeting record from the database.
#[derive(Debug, Clone)]
pub struct MeetingRecord {
    pub id: i64,
    pub sync_id: RecordId,
    pub origin_device_id: DeviceId,
    pub sync_version: u64,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub title_version: i64,
    pub status: String,
    pub audio_path: String,
    pub source_filename: Option<String>,
    pub transcript_path: Option<String>,
    pub transcript_text: Option<String>,
    /// Per-segment `{start,end,text}` timestamps, or `None` for meetings
    /// transcribed before timestamps were captured (or whose stored JSON was
    /// malformed). Stored as a JSON array in the `transcript_segments` column;
    /// the repository owns that encoding so callers work with typed segments.
    pub transcript_segments: Option<Vec<Segment>>,
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
        Self::insert_with_source(conn, title, audio_path, None)
    }

    /// Insert an imported meeting while retaining its original filename for
    /// presentation before a canonical Meeting Title exists.
    pub fn insert_import(
        conn: &Connection,
        title: Option<&str>,
        audio_path: &str,
        source_filename: Option<&str>,
    ) -> Result<i64> {
        Self::insert_with_source(conn, title, audio_path, source_filename)
    }

    fn insert_with_source(
        conn: &Connection,
        title: Option<&str>,
        audio_path: &str,
        source_filename: Option<&str>,
    ) -> Result<i64> {
        let title = title.map(str::trim).filter(|title| !title.is_empty());
        let source_filename = source_filename
            .map(str::trim)
            .filter(|filename| !filename.is_empty());
        let transaction = conn.unchecked_transaction()?;
        let identity =
            crate::db::sync_identity::SyncIdentityRepository::get_or_create_device(&transaction)?;
        let sync_id = RecordId::new();
        transaction.execute(
            "INSERT INTO meetings \
             (title, title_source, title_updated_at, status, audio_path, source_filename, sync_id, origin_device_id) \
             VALUES (?1, CASE WHEN ?1 IS NULL THEN NULL ELSE 'manual' END, \
                     CASE WHEN ?1 IS NULL THEN NULL \
                          ELSE strftime('%Y-%m-%d %H:%M:%f', 'now') END, ?2, ?3, ?4, ?5, ?6)",
            params![
                title,
                MeetingPhase::Recording.as_str(),
                audio_path,
                source_filename,
                sync_id.to_string(),
                identity.device_id.to_string(),
            ],
        )
        .context("Failed to insert meeting")?;

        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(id)
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

    /// Assign a non-empty Manual Title to one live meeting.
    pub fn set_manual_title(conn: &Connection, id: i64, title: &str) -> Result<bool> {
        let title = title.trim();
        if title.is_empty() {
            anyhow::bail!("Meeting Title cannot be blank");
        }
        let transaction = conn.unchecked_transaction()?;
        let affected = transaction
            .execute(
                "UPDATE meetings SET \
                 title = ?1, \
                 title_source = 'manual', \
                 title_updated_at = strftime('%Y-%m-%d %H:%M:%f', 'now'), \
                 title_version = title_version + 1, sync_version = sync_version + 1 \
                 WHERE id = ?2 AND deleted_at IS NULL",
                params![title, id],
            )
            .context("Failed to update meeting Manual Title")?;
        // Sync publication follows the local commit so an outbox failure can
        // never roll back the person's title edit.
        transaction.commit()?;
        if affected > 0 {
            Self::enqueue_if_completed(conn, id)?;
        }
        Ok(affected > 0)
    }

    /// Claim an unowned meeting title for generation. The guarded update is
    /// the Manual Title precedence rule: a person can edit while generation is
    /// running and the late agent result will then be discarded.
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
        let transaction = conn.unchecked_transaction()?;
        let affected = transaction
            .execute(
                "UPDATE meetings SET title = ?1, title_source = 'generated', \
                 title_updated_at = strftime('%Y-%m-%d %H:%M:%f', 'now'), \
                 sync_version = sync_version + 1 \
                 WHERE id = ?2 AND deleted_at IS NULL \
                 AND title IS NULL AND title_source IS NULL AND title_version = ?3",
                params![title, id, title_version],
            )
            .context("Failed to set Generated Title")?;
        // Publish after committing the local title change.
        transaction.commit()?;
        if affected > 0 {
            Self::enqueue_if_completed(conn, id)?;
        }
        Ok(affected > 0)
    }

    /// Intentionally return title ownership to generation. Only completed
    /// meetings with transcript text are eligible.
    pub fn release_title_for_regeneration(conn: &Connection, id: i64) -> Result<bool> {
        let affected = conn
            .execute(
                "UPDATE meetings SET title = NULL, title_source = NULL, \
                 title_updated_at = strftime('%Y-%m-%d %H:%M:%f', 'now'), \
                 title_version = title_version + 1 \
                 WHERE id = ?1 AND deleted_at IS NULL AND status = ?2 \
                 AND transcript_text IS NOT NULL AND trim(transcript_text) <> ''",
                params![id, MeetingPhase::Completed.as_str()],
            )
            .context("Failed to release Meeting Title for regeneration")?;
        Ok(affected > 0)
    }

    /// Distinct Manual Titles ordered by the latest meeting where each title
    /// was assigned or reused.
    pub fn recent_manual_titles(conn: &Connection, limit: usize) -> Result<Vec<String>> {
        let mut stmt = conn
            .prepare(
                "SELECT title FROM meetings \
                 WHERE deleted_at IS NULL AND title_source = 'manual' AND title IS NOT NULL \
                 GROUP BY title \
                 ORDER BY MAX(COALESCE(title_updated_at, started_at)) DESC, MAX(id) DESC \
                 LIMIT ?1",
            )
            .context("Failed to prepare recent Manual Titles query")?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get(0))
            .context("Failed to query recent Manual Titles")?;
        let mut titles = Vec::new();
        for row in rows {
            titles.push(row?);
        }
        Ok(titles)
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
        transcript_segments: Option<&[Segment]>,
        duration_seconds: i64,
    ) -> Result<()> {
        // Encode segments as the JSON array stored in `transcript_segments`.
        // Empty or absent segments (and any serialization hiccup) store NULL so
        // the UI falls back to plain text — this is the one place that knows the
        // column's on-disk encoding.
        let segments_json = transcript_segments
            .filter(|s| !s.is_empty())
            .and_then(|s| serde_json::to_string(s).ok());
        let initial_settings = crate::db::sync_settings::SyncSettingsRepository::get(conn)?;
        let before = Self::get_from(conn, id)?.context("meeting disappeared before completion")?;
        let staging = if matches!(
            initial_settings.role,
            SyncRole::HomeHub | SyncRole::ConnectedDevice
        ) && initial_settings.upload_recording_payloads
        {
            crate::db::operations::attempt_recording_staging(
                conn,
                std::path::Path::new(&before.audio_path),
            )
        } else {
            crate::db::operations::RecordingStaging::default()
        };
        let staged_path = staging.staged.as_ref().map(|value| value.path.clone());
        let result = (|| -> Result<bool> {
            let transaction = conn.unchecked_transaction()?;
            let settings = crate::db::sync_settings::SyncSettingsRepository::get(&transaction)?;
            let current = Self::get_from(&transaction, id)?
                .context("meeting disappeared before completion")?;
            let staging_applies = settings.upload_recording_payloads
                && initial_settings.upload_recording_payloads
                && matches!(settings.role, SyncRole::HomeHub | SyncRole::ConnectedDevice)
                && current.sync_id == before.sync_id
                && current.audio_path == before.audio_path;
            let recording_payload = staging_applies
                .then(|| {
                    staging
                        .staged
                        .as_ref()
                        .map(|value| value.descriptor.clone())
                })
                .flatten()
                .unwrap_or_else(crate::sync::protocol::RecordingPayloadDescriptor::unavailable);
            transaction
                .execute(
                    "UPDATE meetings SET status = ?1, transcript_path = ?2, transcript_text = ?3, \
             transcript_segments = ?4, duration_seconds = ?5, error = NULL, \
             completed_at = CURRENT_TIMESTAMP WHERE id = ?6",
                    params![
                        MeetingPhase::Completed.as_str(),
                        transcript_path,
                        transcript_text,
                        segments_json,
                        duration_seconds,
                        id,
                    ],
                )
                .context("Failed to complete meeting")?;
            let meeting =
                Self::get_from(&transaction, id)?.context("completed meeting disappeared")?;
            if matches!(settings.role, SyncRole::HomeHub | SyncRole::ConnectedDevice) {
                let snapshot = meeting.snapshot_with_payload(recording_payload.clone())?;
                crate::db::sync_outbox::SyncOutboxRepository::enqueue_snapshot(
                    &transaction,
                    &snapshot.into(),
                )?;
                if staging_applies {
                    if let Some(error) = staging.error.as_deref() {
                        crate::db::sync_outbox::SyncOutboxRepository::enqueue_blob_staging_failure(
                            &transaction,
                            meeting.sync_id,
                            crate::sync::protocol::RecordKind::Meeting,
                            error,
                        )?;
                    } else {
                        crate::db::sync_outbox::SyncOutboxRepository::enqueue_blob(
                            &transaction,
                            meeting.sync_id,
                            crate::sync::protocol::RecordKind::Meeting,
                            &recording_payload,
                            staging
                                .staged
                                .as_ref()
                                .map(|payload| payload.path.as_path()),
                        )?;
                    }
                } else if !settings.upload_recording_payloads {
                    crate::db::sync_outbox::SyncOutboxRepository::enqueue_blob(
                        &transaction,
                        meeting.sync_id,
                        crate::sync::protocol::RecordKind::Meeting,
                        &recording_payload,
                        None,
                    )?;
                }
            }
            transaction.commit()?;
            Ok(staging_applies && staging.staged.is_some())
        })();
        let keep_staged = result.as_ref().is_ok_and(|keep| *keep);
        drop(staging);
        if !keep_staged {
            if let Some(path) = staged_path {
                if let Err(error) =
                    crate::db::sync_outbox::SyncOutboxRepository::reclaim_staged_paths(
                        conn,
                        &[path],
                    )
                {
                    tracing::warn!(%error, "failed to reclaim unowned meeting staging file");
                }
            }
        }
        result.map(|_| ())
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

    /// Sweep meetings orphaned by a daemon crash/restart into `error`.
    ///
    /// The meeting machine's state is in-memory only, so if the process dies
    /// mid-pipeline every non-terminal row (`recording`, `review`,
    /// `compressing`, `transcribing`) is unreachable on the next boot: the
    /// machine restarts Idle, `confirm` refuses, and nothing else ever
    /// touches the row — it sits "transcribing" forever. Called once at
    /// service startup, before the meeting machine or API accept work, so a
    /// non-terminal row here can only be a crash orphan, never a live
    /// meeting.
    ///
    /// Rows move to `error` (the one state with a recovery path — the retry
    /// endpoint re-submits the audio still on disk). `duration_seconds` is
    /// deliberately left untouched, unlike [`Self::fail`]: the in-memory
    /// duration died with the old process and whatever the row already holds
    /// is the best information we have. The error message embeds the prior
    /// status via SQL so the single statement stays atomic. Returns the
    /// number of rows swept.
    pub fn sweep_interrupted(conn: &Connection) -> Result<usize> {
        // Same injection-safe interpolation of the compile-time terminal
        // status list as `soft_delete`; anything NOT terminal is in-flight,
        // so a future new in-flight phase is swept by default instead of
        // stranded.
        let terminal = MeetingPhase::TERMINAL_STATUSES.join("', '");
        let affected = conn
            .execute(
                &format!(
                    "UPDATE meetings SET \
                     error = 'Interrupted: the Audetic daemon stopped while this meeting was ' \
                             || status, \
                     status = ?1, \
                     completed_at = CURRENT_TIMESTAMP \
                     WHERE status NOT IN ('{terminal}') AND deleted_at IS NULL"
                ),
                params![MeetingPhase::Error.as_str()],
            )
            .context("Failed to sweep interrupted meetings")?;
        Ok(affected)
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
        let transaction = conn.unchecked_transaction()?;
        let record_id = transaction
            .query_row("SELECT sync_id FROM meetings WHERE id=?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        transaction
            .execute(
                "UPDATE meetings SET status = ?1, duration_seconds = ?2, \
             completed_at = CURRENT_TIMESTAMP WHERE id = ?3",
                params![MeetingPhase::Cancelled.as_str(), duration_seconds, id],
            )
            .context("Failed to mark meeting as cancelled")?;
        let paths = record_id
            .map(|record_id| {
                crate::db::sync_outbox::SyncOutboxRepository::remove_record_state(
                    &transaction,
                    record_id.parse().map_err(anyhow::Error::msg)?,
                    crate::sync::protocol::RecordKind::Meeting,
                )
            })
            .transpose()?
            .unwrap_or_default();
        transaction.commit()?;
        if let Err(error) =
            crate::db::sync_outbox::SyncOutboxRepository::reclaim_staged_paths(conn, &paths)
        {
            tracing::warn!(%error, "failed to reclaim cancelled meeting staging files");
        }
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
        Self::soft_delete_impl(conn, id, true)
    }

    /// Finish local cleanup after the Home Hub has accepted the meeting
    /// deletion. This bypasses the upload-race guard because the authoritative
    /// tombstone now wins any delayed snapshot.
    pub fn soft_delete_after_hub_delete(conn: &Connection, id: i64) -> Result<SoftDeleteOutcome> {
        Self::soft_delete_impl(conn, id, false)
    }

    /// Cancel transient artifact work and remove every durable upload row that
    /// could recreate a meeting or one of its children after deletion.
    pub fn cleanup_deleted_sync_work(conn: &Connection, record_id: RecordId) -> Result<()> {
        let transaction = conn.unchecked_transaction()?;
        let paths = Self::cleanup_deleted_sync_work_from(&transaction, None, record_id)?;
        transaction.commit()?;
        if let Err(error) =
            crate::db::sync_outbox::SyncOutboxRepository::reclaim_staged_paths(conn, &paths)
        {
            tracing::warn!(%error, "failed to reclaim deleted meeting staging files");
        }
        Ok(())
    }

    fn cleanup_deleted_sync_work_from(
        conn: &Connection,
        local_id: Option<i64>,
        record_id: RecordId,
    ) -> Result<Vec<std::path::PathBuf>> {
        if let Some(local_id) = local_id {
            conn.execute(
                "DELETE FROM sync_outbox_items WHERE kind = 'artifact' AND record_id IN \
                 (SELECT sync_id FROM meeting_artifacts WHERE meeting_id = ?1)",
                params![local_id],
            )?;
        }
        conn.execute(
            "DELETE FROM sync_outbox_items WHERE kind = 'artifact' AND record_id IN \
             (SELECT artifact_record_id FROM sync_artifact_runs WHERE parent_record_id = ?1)",
            params![record_id.to_string()],
        )?;
        conn.execute(
            "DELETE FROM sync_artifact_runs WHERE parent_record_id = ?1",
            params![record_id.to_string()],
        )?;
        crate::db::sync_outbox::SyncOutboxRepository::remove_record_state(
            conn,
            record_id,
            crate::sync::protocol::RecordKind::Meeting,
        )
    }

    fn soft_delete_impl(
        conn: &Connection,
        id: i64,
        guard_possible_upload: bool,
    ) -> Result<SoftDeleteOutcome> {
        // Build the IN-list from the single terminal-status source. The values
        // are compile-time constants (never user input), so interpolating them
        // is injection-safe; `id` is still bound as a parameter.
        let terminal = MeetingPhase::TERMINAL_STATUSES.join("', '");
        let transaction = conn.unchecked_transaction()?;
        let record_id: Option<String> = transaction
            .query_row(
                "SELECT sync_id FROM meetings WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let status: Option<String> = transaction
            .query_row(
                "SELECT status FROM meetings WHERE id = ?1 AND deleted_at IS NULL",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        if status
            .as_deref()
            .is_some_and(|status| !MeetingPhase::is_terminal(status))
        {
            transaction.commit()?;
            return Ok(SoftDeleteOutcome::InFlight);
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
            let may_have_uploaded_children = if let Some(parent_record_id) = record_id.as_deref() {
                transaction.query_row(
                    "SELECT EXISTS( \
                         SELECT 1 FROM sync_outbox_items o \
                         WHERE o.kind = 'artifact' \
                           AND (o.attempts > 0 OR o.accepted_hub_revision IS NOT NULL OR o.state = 'synced') \
                           AND ( \
                             o.record_id IN (SELECT sync_id FROM meeting_artifacts WHERE meeting_id = ?1) \
                             OR o.record_id IN (SELECT artifact_record_id FROM sync_artifact_runs WHERE parent_record_id = ?2) \
                           ) \
                     )",
                    params![id, parent_record_id],
                    |row| row.get::<_, bool>(0),
                )?
            } else {
                false
            };
            let meeting_may_have_uploaded = record_id
                .as_deref()
                .map(|record_id| {
                    record_id
                        .parse()
                        .map_err(anyhow::Error::msg)
                        .and_then(|record_id| {
                            crate::db::sync_outbox::SyncOutboxRepository::may_have_reached_hub(
                                &transaction,
                                record_id,
                                crate::sync::protocol::RecordKind::Meeting,
                            )
                        })
                })
                .transpose()?
                .unwrap_or(false);
            if active_sync && (meeting_may_have_uploaded || may_have_uploaded_children) {
                transaction.commit()?;
                return Ok(SoftDeleteOutcome::RequiresHub);
            }
        }
        let affected = transaction
            .execute(
                &format!(
                    "UPDATE meetings SET deleted_at = CURRENT_TIMESTAMP \
                     WHERE id = ?1 AND deleted_at IS NULL AND status IN ('{terminal}')"
                ),
                params![id],
            )
            .context("Failed to soft-delete meeting")?;

        if affected > 0 {
            let record_id: RecordId = record_id
                .context("deleted meeting lost its sync UUID")?
                .parse()
                .map_err(anyhow::Error::msg)?;
            let paths = Self::cleanup_deleted_sync_work_from(&transaction, Some(id), record_id)?;
            transaction.commit()?;
            if let Err(error) =
                crate::db::sync_outbox::SyncOutboxRepository::reclaim_staged_paths(conn, &paths)
            {
                tracing::warn!(%error, "failed to reclaim deleted meeting staging files");
            }
            return Ok(SoftDeleteOutcome::Deleted);
        }

        // Nothing was hidden — read the live row only to choose between 404 and
        // 409. This is advisory: the guarded UPDATE above already guarantees we
        // never stamp an in-flight meeting, regardless of how this read races.
        let status: Option<String> = transaction
            .query_row(
                "SELECT status FROM meetings WHERE id = ?1 AND deleted_at IS NULL",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to look up meeting after delete")?;

        transaction.commit()?;
        Ok(match status {
            Some(s) if !MeetingPhase::is_terminal(&s) => SoftDeleteOutcome::InFlight,
            // Either gone (no live row) or a terminal row a concurrent delete
            // claimed first — nothing live remains for us to remove.
            _ => SoftDeleteOutcome::NotFound,
        })
    }

    /// Get a meeting by ID. Soft-deleted meetings are treated as absent.
    pub fn get(conn: &Connection, id: i64) -> Result<Option<MeetingRecord>> {
        Self::get_from(conn, id)
    }

    fn get_from(conn: &Connection, id: i64) -> Result<Option<MeetingRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, title, title_source, title_version, status, audio_path, source_filename, \
                 transcript_path, transcript_text, duration_seconds, started_at, completed_at, \
                 error, created_at, deleted_at, transcript_segments, sync_id, origin_device_id, sync_version \
                 FROM meetings WHERE id = ?1 AND deleted_at IS NULL",
            )
            .context("Failed to prepare meeting query")?;

        let mut rows = stmt
            .query_map(params![id], |row| {
                Ok(MeetingRecord {
                    id: row.get(0)?,
                    sync_id: row
                        .get::<_, String>(16)?
                        .parse()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    origin_device_id: row
                        .get::<_, String>(17)?
                        .parse()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    sync_version: row.get(18)?,
                    title: row.get(1)?,
                    title_source: row.get(2)?,
                    title_version: row.get(3)?,
                    status: row.get(4)?,
                    audio_path: row.get(5)?,
                    source_filename: row.get(6)?,
                    transcript_path: row.get(7)?,
                    transcript_text: row.get(8)?,
                    duration_seconds: row.get(9)?,
                    started_at: row.get(10)?,
                    completed_at: row.get(11)?,
                    error: row.get(12)?,
                    created_at: row.get(13)?,
                    deleted_at: row.get(14)?,
                    // Tolerate malformed/legacy JSON by decoding to None, so a
                    // bad value just drops back to the plain-text transcript.
                    transcript_segments: row
                        .get::<_, Option<String>>(15)?
                        .as_deref()
                        .and_then(|json| serde_json::from_str(json).ok()),
                })
            })
            .context("Failed to query meeting")?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn get_by_sync_id(conn: &Connection, sync_id: RecordId) -> Result<Option<MeetingRecord>> {
        let id = conn
            .query_row(
                "SELECT id FROM meetings WHERE sync_id = ?1 AND deleted_at IS NULL",
                [sync_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        id.map(|id| Self::get(conn, id))
            .transpose()
            .map(Option::flatten)
    }

    pub fn internal_id(conn: &Connection, sync_id: RecordId) -> Result<Option<i64>> {
        conn.query_row(
            "SELECT id FROM meetings WHERE sync_id = ?1 AND deleted_at IS NULL",
            [sync_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .context("Failed to resolve meeting UUID")
    }

    /// List meetings, newest first. Soft-deleted meetings are excluded.
    pub fn list(conn: &Connection, limit: usize) -> Result<Vec<MeetingRecord>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, title, title_source, title_version, status, audio_path, source_filename, \
                 transcript_path, transcript_text, duration_seconds, started_at, completed_at, \
                 error, created_at, deleted_at, transcript_segments, sync_id, origin_device_id, sync_version \
                 FROM meetings WHERE deleted_at IS NULL \
                 ORDER BY started_at DESC, id DESC LIMIT ?1",
            )
            .context("Failed to prepare meetings list query")?;

        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(MeetingRecord {
                    id: row.get(0)?,
                    sync_id: row
                        .get::<_, String>(16)?
                        .parse()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    origin_device_id: row
                        .get::<_, String>(17)?
                        .parse()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    sync_version: row.get(18)?,
                    title: row.get(1)?,
                    title_source: row.get(2)?,
                    title_version: row.get(3)?,
                    status: row.get(4)?,
                    audio_path: row.get(5)?,
                    source_filename: row.get(6)?,
                    transcript_path: row.get(7)?,
                    transcript_text: row.get(8)?,
                    duration_seconds: row.get(9)?,
                    started_at: row.get(10)?,
                    completed_at: row.get(11)?,
                    error: row.get(12)?,
                    created_at: row.get(13)?,
                    deleted_at: row.get(14)?,
                    // Tolerate malformed/legacy JSON by decoding to None, so a
                    // bad value just drops back to the plain-text transcript.
                    transcript_segments: row
                        .get::<_, Option<String>>(15)?
                        .as_deref()
                        .and_then(|json| serde_json::from_str(json).ok()),
                })
            })
            .context("Failed to list meetings")?;

        let mut meetings = Vec::new();
        for row in rows {
            meetings.push(row?);
        }

        Ok(meetings)
    }

    fn enqueue_if_completed(conn: &Connection, id: i64) -> Result<()> {
        let transaction = conn.unchecked_transaction()?;
        let meeting = Self::get_from(&transaction, id)?
            .context("meeting disappeared during snapshot enqueue")?;
        if meeting.status != MeetingPhase::Completed.as_str() {
            transaction.commit()?;
            return Ok(());
        }
        let settings = crate::db::sync_settings::SyncSettingsRepository::get(&transaction)?;
        if matches!(settings.role, SyncRole::HomeHub | SyncRole::ConnectedDevice) {
            let existing = crate::db::sync_outbox::SyncOutboxRepository::payload_descriptor(
                &transaction,
                meeting.sync_id,
            )?;
            let recording_payload = existing
                .unwrap_or_else(crate::sync::protocol::RecordingPayloadDescriptor::unavailable);
            crate::db::sync_outbox::SyncOutboxRepository::enqueue_snapshot(
                &transaction,
                &meeting
                    .snapshot_with_payload(recording_payload.clone())?
                    .into(),
            )?;
            if !settings.upload_recording_payloads
                && crate::db::sync_outbox::SyncOutboxRepository::payload_descriptor(
                    &transaction,
                    meeting.sync_id,
                )?
                .is_none()
            {
                crate::db::sync_outbox::SyncOutboxRepository::enqueue_blob(
                    &transaction,
                    meeting.sync_id,
                    crate::sync::protocol::RecordKind::Meeting,
                    &recording_payload,
                    None,
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

impl MeetingRecord {
    pub fn snapshot(&self) -> Result<crate::sync::protocol::MeetingSnapshot> {
        self.snapshot_with_payload(Default::default())
    }

    pub fn snapshot_with_payload(
        &self,
        recording_payload: crate::sync::protocol::RecordingPayloadDescriptor,
    ) -> Result<crate::sync::protocol::MeetingSnapshot> {
        let transcript_text = self
            .transcript_text
            .clone()
            .filter(|text| !text.trim().is_empty())
            .context("successful meeting has no transcript")?;
        let completed_at = self
            .completed_at
            .clone()
            .context("successful meeting has no completion timestamp")?;
        Ok(crate::sync::protocol::MeetingSnapshot {
            kind: crate::sync::protocol::RecordKind::Meeting,
            schema_version: 1,
            record_id: self.sync_id,
            origin_device_id: self.origin_device_id,
            local_version: self.sync_version,
            created_at: portable_timestamp(&self.started_at)?,
            updated_at: portable_timestamp(&completed_at)?,
            payload: crate::sync::protocol::MeetingPayload {
                title: self.title.clone(),
                title_source: self.title_source.clone(),
                title_version: self.title_version.try_into().unwrap_or_default(),
                source_filename: self.source_filename.clone(),
                transcript_text,
                transcript_segments: self.transcript_segments.clone(),
                duration_seconds: self
                    .duration_seconds
                    .unwrap_or_default()
                    .try_into()
                    .unwrap_or_default(),
                status: self.status.clone(),
                completed_at: portable_timestamp(&completed_at)?,
                recording_payload,
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
        .with_context(|| format!("invalid meeting timestamp {value:?}"))
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
        let id = MeetingRepository::insert(&conn, Some("  Standup  "), "/tmp/meeting.wav").unwrap();
        assert!(id > 0);

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(meeting.title.as_deref(), Some("Standup"));
        assert_eq!(meeting.title_source.as_deref(), Some("manual"));
    }

    #[test]
    fn test_insert_blank_title_as_absent() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, Some("  \t  "), "/tmp/meeting.wav").unwrap();

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(meeting.title, None);
        assert_eq!(meeting.title_source, None);
    }

    #[test]
    fn imported_filename_is_presentation_metadata_not_a_meeting_title() {
        let conn = setup_db();
        let id = MeetingRepository::insert_import(
            &conn,
            None,
            "/tmp/imported.mp3",
            Some("Quarterly Planning Recording.mp3"),
        )
        .unwrap();

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(meeting.title, None);
        assert_eq!(meeting.title_source, None);
        assert_eq!(
            meeting.source_filename.as_deref(),
            Some("Quarterly Planning Recording.mp3")
        );
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
        // No segments passed → column stays NULL.
        assert!(meeting.transcript_segments.is_none());
    }

    #[test]
    fn test_complete_persists_and_decodes_segments() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, Some("Meeting"), "/tmp/test.wav").unwrap();

        let segments = vec![
            Segment {
                start: 0.0,
                end: 2.5,
                text: "Hello".into(),
            },
            Segment {
                start: 2.5,
                end: 5.0,
                text: "world".into(),
            },
        ];
        MeetingRepository::complete(&conn, id, "/tmp/t.txt", "Hello world", Some(&segments), 10)
            .unwrap();

        // Round-trips through the column as typed segments, no caller-side JSON.
        let got = MeetingRepository::get(&conn, id)
            .unwrap()
            .unwrap()
            .transcript_segments
            .expect("segments persisted");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].start, 0.0);
        assert_eq!(got[1].text, "world");
    }

    #[test]
    fn test_complete_empty_segments_stored_as_none() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, Some("Meeting"), "/tmp/test.wav").unwrap();

        // An empty slice collapses to NULL so the UI falls back to plain text,
        // rather than storing a useless "[]".
        MeetingRepository::complete(&conn, id, "/tmp/t.txt", "txt", Some(&[]), 10).unwrap();

        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert!(meeting.transcript_segments.is_none());
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
    fn test_sweep_interrupted_errors_in_flight_rows_only() {
        let conn = setup_db();

        // One row per in-flight phase a crash can strand...
        let mut in_flight = Vec::new();
        for phase in [
            MeetingPhase::Recording,
            MeetingPhase::Review,
            MeetingPhase::Compressing,
            MeetingPhase::Transcribing,
        ] {
            let id = MeetingRepository::insert(&conn, None, "/tmp/stuck.wav").unwrap();
            MeetingRepository::update_status(&conn, id, phase).unwrap();
            in_flight.push((id, phase));
        }

        // ...and terminal rows that must be untouched.
        let done = MeetingRepository::insert(&conn, None, "/tmp/done.wav").unwrap();
        MeetingRepository::complete(&conn, done, "/tmp/t.txt", "txt", None, 10).unwrap();
        let failed = MeetingRepository::insert(&conn, None, "/tmp/failed.wav").unwrap();
        MeetingRepository::fail(&conn, failed, "boom", 5).unwrap();

        let swept = MeetingRepository::sweep_interrupted(&conn).unwrap();
        assert_eq!(swept, in_flight.len());

        for (id, phase) in in_flight {
            let m = MeetingRepository::get(&conn, id).unwrap().unwrap();
            assert_eq!(m.status, "error", "{} should be swept", phase.as_str());
            // Error message records what the meeting was doing when it died.
            let err = m.error.expect("swept row has an error message");
            assert!(err.contains(phase.as_str()), "error should embed {phase:?}");
            assert!(m.completed_at.is_some());
        }

        // Terminal rows keep their status and error text.
        assert_eq!(
            MeetingRepository::get(&conn, done).unwrap().unwrap().status,
            "completed"
        );
        let failed_row = MeetingRepository::get(&conn, failed).unwrap().unwrap();
        assert_eq!(failed_row.status, "error");
        assert_eq!(failed_row.error, Some("boom".to_string()));
    }

    #[test]
    fn test_sweep_interrupted_preserves_duration() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, None, "/tmp/stuck.wav").unwrap();
        MeetingRepository::set_review(&conn, id, 4136).unwrap();
        MeetingRepository::update_status(&conn, id, MeetingPhase::Transcribing).unwrap();

        MeetingRepository::sweep_interrupted(&conn).unwrap();

        // Unlike `fail`, the sweep must not clobber the recorded duration.
        let m = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(m.duration_seconds, Some(4136));
    }

    #[test]
    fn test_sweep_interrupted_skips_soft_deleted_and_empty_db() {
        let conn = setup_db();
        assert_eq!(MeetingRepository::sweep_interrupted(&conn).unwrap(), 0);

        // A soft-deleted row is hidden from every surface, including retry —
        // erroring it would resurrect nothing. (Soft-delete only accepts
        // terminal rows, so simulate legacy/manual state directly.)
        let id = MeetingRepository::insert(&conn, None, "/tmp/gone.wav").unwrap();
        MeetingRepository::update_status(&conn, id, MeetingPhase::Transcribing).unwrap();
        conn.execute(
            "UPDATE meetings SET deleted_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id],
        )
        .unwrap();
        assert_eq!(MeetingRepository::sweep_interrupted(&conn).unwrap(), 0);
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

    #[test]
    fn manual_title_edit_affects_one_meeting_and_rejects_blank_input() {
        let conn = setup_db();
        let first = MeetingRepository::insert(&conn, None, "/tmp/first.wav").unwrap();
        let second = MeetingRepository::insert(&conn, None, "/tmp/second.wav").unwrap();

        assert!(MeetingRepository::set_manual_title(&conn, first, "  Planning Review  ").unwrap());
        let first_record = MeetingRepository::get(&conn, first).unwrap().unwrap();
        assert_eq!(first_record.title.as_deref(), Some("Planning Review"));
        assert_eq!(first_record.title_source.as_deref(), Some("manual"));
        assert_eq!(
            MeetingRepository::get(&conn, second)
                .unwrap()
                .unwrap()
                .title,
            None
        );

        assert!(MeetingRepository::set_manual_title(&conn, first, "   ").is_err());
        let unchanged = MeetingRepository::get(&conn, first).unwrap().unwrap();
        assert_eq!(unchanged.title.as_deref(), Some("Planning Review"));
        assert_eq!(unchanged.title_source.as_deref(), Some("manual"));
    }

    #[test]
    fn generated_title_only_claims_an_unowned_meeting() {
        let conn = setup_db();
        let manual =
            MeetingRepository::insert(&conn, Some("Manual Wins"), "/tmp/manual.wav").unwrap();
        let untitled = MeetingRepository::insert(&conn, None, "/tmp/generated.wav").unwrap();

        assert!(!MeetingRepository::set_generated_title_if_unowned(
            &conn,
            manual,
            "Generated Loses",
            0,
        )
        .unwrap());
        assert!(MeetingRepository::set_generated_title_if_unowned(
            &conn,
            untitled,
            "Specific Generated Topic",
            0,
        )
        .unwrap());

        let manual_record = MeetingRepository::get(&conn, manual).unwrap().unwrap();
        assert_eq!(manual_record.title.as_deref(), Some("Manual Wins"));
        assert_eq!(manual_record.title_source.as_deref(), Some("manual"));
        let generated_record = MeetingRepository::get(&conn, untitled).unwrap().unwrap();
        assert_eq!(
            generated_record.title.as_deref(),
            Some("Specific Generated Topic")
        );
        assert_eq!(generated_record.title_source.as_deref(), Some("generated"));
    }

    #[test]
    fn regeneration_releases_manual_ownership_before_generation() {
        let conn = setup_db();
        let id =
            MeetingRepository::insert(&conn, Some("Manual Planning"), "/tmp/manual.wav").unwrap();
        MeetingRepository::complete(&conn, id, "/tmp/manual.txt", "Transcript text", None, 10)
            .unwrap();

        assert!(MeetingRepository::release_title_for_regeneration(&conn, id).unwrap());
        let title_version = MeetingRepository::get(&conn, id)
            .unwrap()
            .unwrap()
            .title_version;
        assert!(MeetingRepository::set_generated_title_if_unowned(
            &conn,
            id,
            "Generated Planning Decisions",
            title_version,
        )
        .unwrap());
        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(
            meeting.title.as_deref(),
            Some("Generated Planning Decisions")
        );
        assert_eq!(meeting.title_source.as_deref(), Some("generated"));
    }

    #[test]
    fn regeneration_rejects_an_older_generation_result() {
        let conn = setup_db();
        let id = MeetingRepository::insert(&conn, None, "/tmp/meeting.wav").unwrap();
        MeetingRepository::complete(&conn, id, "/tmp/meeting.txt", "Transcript text", None, 10)
            .unwrap();
        let stale_version = MeetingRepository::get(&conn, id)
            .unwrap()
            .unwrap()
            .title_version;

        assert!(MeetingRepository::release_title_for_regeneration(&conn, id).unwrap());
        let current_version = MeetingRepository::get(&conn, id)
            .unwrap()
            .unwrap()
            .title_version;
        assert_ne!(stale_version, current_version);
        assert!(!MeetingRepository::set_generated_title_if_unowned(
            &conn,
            id,
            "Stale Automatic Generation",
            stale_version,
        )
        .unwrap());
        assert!(MeetingRepository::set_generated_title_if_unowned(
            &conn,
            id,
            "Fresh Explicit Regeneration",
            current_version,
        )
        .unwrap());
    }

    #[test]
    fn recent_manual_titles_are_distinct_and_ordered_by_latest_use() {
        let conn = setup_db();
        let older_duplicate =
            MeetingRepository::insert(&conn, Some("Weekly Planning"), "/tmp/one.wav").unwrap();
        let other =
            MeetingRepository::insert(&conn, Some("Design Review"), "/tmp/two.wav").unwrap();
        let newer_duplicate =
            MeetingRepository::insert(&conn, Some("Weekly Planning"), "/tmp/three.wav").unwrap();
        let generated = MeetingRepository::insert(&conn, None, "/tmp/four.wav").unwrap();
        MeetingRepository::set_generated_title_if_unowned(&conn, generated, "Generated Topic", 0)
            .unwrap();
        conn.execute(
            "UPDATE meetings SET title_updated_at = ?1 WHERE id = ?2",
            params!["2026-01-01 09:00:00", older_duplicate],
        )
        .unwrap();
        conn.execute(
            "UPDATE meetings SET title_updated_at = ?1 WHERE id = ?2",
            params!["2026-01-02 09:00:00", other],
        )
        .unwrap();
        conn.execute(
            "UPDATE meetings SET title_updated_at = ?1 WHERE id = ?2",
            params!["2026-01-03 09:00:00", newer_duplicate],
        )
        .unwrap();

        assert_eq!(
            MeetingRepository::recent_manual_titles(&conn, 10).unwrap(),
            vec!["Weekly Planning", "Design Review"]
        );
    }

    #[test]
    fn payload_failure_does_not_poison_later_meeting_metadata_snapshots() {
        let conn = setup_db();
        crate::db::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
        conn.execute(
            "UPDATE sync_settings SET role='home_hub' WHERE singleton=1",
            [],
        )
        .unwrap();
        let id = MeetingRepository::insert(&conn, None, "/tmp/missing.wav").unwrap();
        MeetingRepository::complete(&conn, id, "/tmp/m.txt", "transcript", None, 10).unwrap();
        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        conn.execute(
            "UPDATE sync_outbox_blobs SET checksum=?2,staged_path='/missing',byte_size=7,
                 media_type='audio/wav',availability='needs_attention',state='needs_attention',
                 last_error='upload exhausted' WHERE record_id=?1",
            params![
                meeting.sync_id.to_string(),
                "239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"
            ],
        )
        .unwrap();

        assert!(MeetingRepository::set_manual_title(&conn, id, "Updated title").unwrap());
        let json: String = conn
            .query_row(
                "SELECT snapshot_json FROM sync_outbox_items WHERE record_id=?1",
                [meeting.sync_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let snapshot: crate::sync::protocol::Snapshot = serde_json::from_str(&json).unwrap();
        let crate::sync::protocol::Snapshot::Meeting(snapshot) = snapshot else {
            panic!("expected meeting snapshot")
        };
        assert_eq!(snapshot.payload.title.as_deref(), Some("Updated title"));
        assert_eq!(
            snapshot.payload.recording_payload.availability,
            audetic_core::sync::PayloadAvailability::Pending
        );
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

    #[test]
    fn soft_delete_requires_hub_after_upload_claim_and_force_cleanup_is_safe() {
        let mut conn = setup_db();
        crate::db::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
        conn.execute(
            "UPDATE sync_settings SET role = 'home_hub' WHERE singleton = 1",
            [],
        )
        .unwrap();
        let id = MeetingRepository::insert(&conn, None, "/tmp/claimed.wav").unwrap();
        MeetingRepository::complete(&conn, id, "/tmp/claimed.txt", "claimed", None, 10).unwrap();
        crate::db::sync_outbox::SyncOutboxRepository::claim_items(
            &mut conn,
            0,
            "worker",
            "2026-09-04T10:00:00Z",
            "2026-09-04T10:00:30Z",
            25,
        )
        .unwrap();

        assert_eq!(
            MeetingRepository::soft_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::RequiresHub
        );
        assert!(MeetingRepository::get(&conn, id).unwrap().is_some());
        assert_eq!(
            MeetingRepository::soft_delete_after_hub_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::Deleted
        );
        assert!(MeetingRepository::get(&conn, id).unwrap().is_none());
    }

    #[test]
    fn meeting_delete_cleans_artifact_runs_and_every_related_outbox_row() {
        let conn = setup_db();
        let id = insert_completed(&conn, "Cleanup", "/tmp/cleanup.wav");
        let meeting = MeetingRepository::get(&conn, id).unwrap().unwrap();
        let artifact_id = RecordId::new();
        let run_id = RecordId::new();
        conn.execute(
            "INSERT INTO sync_artifact_runs \
             (run_id, artifact_record_id, parent_record_id, origin_device_id, kind, title, status) \
             VALUES (?1, ?2, ?3, ?4, 'summary', 'Summary', 'error')",
            params![
                run_id.to_string(),
                artifact_id.to_string(),
                meeting.sync_id.to_string(),
                meeting.origin_device_id.to_string()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_outbox_items \
             (record_id, kind, local_version, snapshot_json, state, last_error) \
             VALUES (?1, 'artifact', 1, '{}', 'needs_attention', 'rejected')",
            [artifact_id.to_string()],
        )
        .unwrap();

        assert_eq!(
            MeetingRepository::soft_delete_after_hub_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::Deleted
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM sync_artifact_runs", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM sync_outbox_items", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn meeting_delete_removes_blob_row_and_reclaims_unshared_staging() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("audetic.db");
        let conn = crate::db::migrate_db_at(&db_path).unwrap();
        crate::db::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
        conn.execute(
            "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
            [],
        )
        .unwrap();
        let source = temp.path().join("delete.wav");
        std::fs::write(&source, b"delete payload").unwrap();
        let id = MeetingRepository::insert(&conn, None, source.to_string_lossy().as_ref()).unwrap();
        MeetingRepository::complete(&conn, id, "/tmp/delete.txt", "done", None, 1).unwrap();
        let staged: String = conn
            .query_row("SELECT staged_path FROM sync_outbox_blobs", [], |row| {
                row.get(0)
            })
            .unwrap();

        assert_eq!(
            MeetingRepository::soft_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::Deleted
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM sync_outbox_blobs", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(!std::path::Path::new(&staged).exists());
    }

    #[test]
    fn meeting_cancellation_removes_preexisting_item_and_blob_state() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("audetic.db");
        let conn = crate::db::migrate_db_at(&db_path).unwrap();
        crate::db::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
        conn.execute(
            "UPDATE sync_settings SET role='home_hub',upload_recording_payloads=1 WHERE singleton=1",
            [],
        )
        .unwrap();
        let source = temp.path().join("cancel.wav");
        std::fs::write(&source, b"cancel payload").unwrap();
        let id = MeetingRepository::insert(&conn, None, source.to_string_lossy().as_ref()).unwrap();
        MeetingRepository::complete(&conn, id, "/tmp/cancel.txt", "done", None, 1).unwrap();
        let staged: String = conn
            .query_row("SELECT staged_path FROM sync_outbox_blobs", [], |row| {
                row.get(0)
            })
            .unwrap();
        MeetingRepository::update_status(&conn, id, MeetingPhase::Review).unwrap();

        MeetingRepository::cancel(&conn, id, 1).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT (SELECT COUNT(*) FROM sync_outbox_items) +
                        (SELECT COUNT(*) FROM sync_outbox_blobs)",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
        assert!(!std::path::Path::new(&staged).exists());
    }

    #[test]
    fn meeting_delete_requires_hub_when_child_artifact_upload_started() {
        let conn = setup_db();
        crate::db::sync_settings::SyncSettingsRepository::get(&conn).unwrap();
        conn.execute(
            "UPDATE sync_settings SET role = 'home_hub' WHERE singleton = 1",
            [],
        )
        .unwrap();
        let id = insert_completed(&conn, "Child upload", "/tmp/child.wav");
        let artifact = crate::db::meeting_artifacts::MeetingArtifactRepository::insert_pending(
            &conn, id, "summary", "Summary", None, None,
        )
        .unwrap();
        crate::db::meeting_artifacts::MeetingArtifactRepository::complete(
            &conn,
            artifact,
            "# Summary",
            "",
            "",
        )
        .unwrap();
        let artifact =
            crate::db::meeting_artifacts::MeetingArtifactRepository::get(&conn, artifact)
                .unwrap()
                .unwrap();
        conn.execute(
            "UPDATE sync_outbox_items SET attempts = 1, state = 'uploading' \
             WHERE record_id = ?1 AND kind = 'artifact'",
            [artifact.id.to_string()],
        )
        .unwrap();
        conn.execute(
            "UPDATE sync_outbox_items SET attempts = 0, state = 'pending' \
             WHERE record_id = ?1 AND kind = 'meeting'",
            [artifact.meeting_id.to_string()],
        )
        .unwrap();

        assert_eq!(
            MeetingRepository::soft_delete(&conn, id).unwrap(),
            SoftDeleteOutcome::RequiresHub
        );
        assert!(MeetingRepository::get(&conn, id).unwrap().is_some());
    }
}
