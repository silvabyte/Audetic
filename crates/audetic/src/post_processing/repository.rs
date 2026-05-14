//! SQLite CRUD over `post_processing_jobs`. Same raw-rusqlite shape as
//! [`crate::db::meetings::MeetingRepository`] — keep it consistent.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, Row};

use super::action::Action;
use super::event::EventKind;
use super::job::{Job, NewJob, UpdateJob};

pub struct JobRepository;

impl JobRepository {
    /// Insert a new job and return the assigned id.
    pub fn insert(conn: &Connection, new: &NewJob) -> Result<i64> {
        let action_type = new.action.type_tag();
        let action_config = new.action.config_json().to_string();
        conn.execute(
            "INSERT INTO post_processing_jobs \
             (name, event, action_type, action_config, enabled) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                new.name,
                new.event.as_str(),
                action_type,
                action_config,
                new.enabled as i64,
            ],
        )
        .context("Failed to insert post-processing job")?;
        Ok(conn.last_insert_rowid())
    }

    /// Fetch a single job by id.
    pub fn get(conn: &Connection, id: i64) -> Result<Option<Job>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, name, event, action_type, action_config, enabled, \
                 created_at, updated_at \
                 FROM post_processing_jobs WHERE id = ?1",
            )
            .context("Failed to prepare job get query")?;
        let mut rows = stmt
            .query_map(params![id], row_to_job)
            .context("Failed to query job")?;
        match rows.next() {
            Some(Ok(Ok(job))) => Ok(Some(job)),
            Some(Ok(Err(e))) => Err(e),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// List all jobs, newest first. Pass `event_filter` to scope to a
    /// single event kind (used by the dispatch path).
    pub fn list(conn: &Connection, event_filter: Option<EventKind>) -> Result<Vec<Job>> {
        let mut jobs = Vec::new();
        if let Some(kind) = event_filter {
            let mut stmt = conn
                .prepare(
                    "SELECT id, name, event, action_type, action_config, enabled, \
                     created_at, updated_at \
                     FROM post_processing_jobs WHERE event = ?1 \
                     ORDER BY id DESC",
                )
                .context("Failed to prepare jobs list (filtered) query")?;
            let rows = stmt
                .query_map(params![kind.as_str()], row_to_job)
                .context("Failed to query filtered jobs")?;
            for r in rows {
                jobs.push(r??);
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id, name, event, action_type, action_config, enabled, \
                     created_at, updated_at \
                     FROM post_processing_jobs ORDER BY id DESC",
                )
                .context("Failed to prepare jobs list query")?;
            let rows = stmt
                .query_map([], row_to_job)
                .context("Failed to query jobs")?;
            for r in rows {
                jobs.push(r??);
            }
        }
        Ok(jobs)
    }

    /// Jobs that should fire for a given event — used by the dispatcher.
    /// Equivalent to `list(_, Some(kind))` filtered to `enabled = 1`.
    pub fn list_enabled_for_event(conn: &Connection, kind: EventKind) -> Result<Vec<Job>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, name, event, action_type, action_config, enabled, \
                 created_at, updated_at \
                 FROM post_processing_jobs \
                 WHERE event = ?1 AND enabled = 1 ORDER BY id ASC",
            )
            .context("Failed to prepare enabled-jobs query")?;
        let rows = stmt
            .query_map(params![kind.as_str()], row_to_job)
            .context("Failed to query enabled jobs")?;
        let mut jobs = Vec::new();
        for r in rows {
            jobs.push(r??);
        }
        Ok(jobs)
    }

    /// Apply a partial update. Returns `Ok(false)` if the id doesn't exist.
    pub fn update(conn: &Connection, id: i64, patch: &UpdateJob) -> Result<bool> {
        let existing = match Self::get(conn, id)? {
            Some(j) => j,
            None => return Ok(false),
        };

        let name = patch.name.clone().unwrap_or(existing.name);
        let event = patch.event.unwrap_or(existing.event);
        let action = patch.action.clone().unwrap_or(existing.action);
        let enabled = patch.enabled.unwrap_or(existing.enabled);
        let action_type = action.type_tag();
        let action_config = action.config_json().to_string();

        conn.execute(
            "UPDATE post_processing_jobs SET \
             name = ?1, event = ?2, action_type = ?3, action_config = ?4, \
             enabled = ?5, updated_at = CURRENT_TIMESTAMP \
             WHERE id = ?6",
            params![
                name,
                event.as_str(),
                action_type,
                action_config,
                enabled as i64,
                id,
            ],
        )
        .context("Failed to update job")?;
        Ok(true)
    }

    /// Delete by id. Returns `Ok(false)` if the row didn't exist.
    pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
        let n = conn
            .execute(
                "DELETE FROM post_processing_jobs WHERE id = ?1",
                params![id],
            )
            .context("Failed to delete job")?;
        Ok(n > 0)
    }
}

/// rusqlite gives us `Result<T, rusqlite::Error>` per row, but the
/// `Job` materialization can also fail (bad JSON in `action_config`).
/// Wrap the inner result in `anyhow::Result` so callers see one error
/// type.
fn row_to_job(row: &Row) -> rusqlite::Result<Result<Job>> {
    let id: i64 = row.get(0)?;
    let name: String = row.get(1)?;
    let event_str: String = row.get(2)?;
    let action_type: String = row.get(3)?;
    let action_config: String = row.get(4)?;
    let enabled: i64 = row.get(5)?;
    let created_at: String = row.get(6)?;
    let updated_at: String = row.get(7)?;

    Ok((|| {
        let event = EventKind::from_str(&event_str)
            .ok_or_else(|| anyhow::anyhow!("unknown event kind in db: `{event_str}`"))?;
        let action = Action::from_storage(&action_type, &action_config)?;
        Ok(Job {
            id,
            name,
            event,
            action,
            enabled: enabled != 0,
            created_at,
            updated_at,
        })
    })())
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

    fn sample_new(name: &str) -> NewJob {
        NewJob {
            name: name.to_string(),
            event: EventKind::DictationCompleted,
            action: Action::Command {
                command: "tee /tmp/out".to_string(),
                timeout_seconds: 5,
            },
            enabled: true,
        }
    }

    #[test]
    fn insert_then_get_round_trips() {
        let conn = setup_db();
        let id = JobRepository::insert(&conn, &sample_new("notify")).unwrap();
        let job = JobRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(job.id, id);
        assert_eq!(job.name, "notify");
        assert_eq!(job.event, EventKind::DictationCompleted);
        assert!(job.enabled);
        match job.action {
            Action::Command {
                command,
                timeout_seconds,
            } => {
                assert_eq!(command, "tee /tmp/out");
                assert_eq!(timeout_seconds, 5);
            }
        }
    }

    #[test]
    fn get_missing_id_returns_none() {
        let conn = setup_db();
        assert!(JobRepository::get(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn list_returns_all_jobs_when_unfiltered() {
        let conn = setup_db();
        JobRepository::insert(&conn, &sample_new("a")).unwrap();
        JobRepository::insert(
            &conn,
            &NewJob {
                event: EventKind::MeetingCompleted,
                ..sample_new("b")
            },
        )
        .unwrap();
        let all = JobRepository::list(&conn, None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn list_filters_by_event() {
        let conn = setup_db();
        JobRepository::insert(&conn, &sample_new("a")).unwrap();
        JobRepository::insert(
            &conn,
            &NewJob {
                event: EventKind::MeetingCompleted,
                ..sample_new("b")
            },
        )
        .unwrap();
        let meeting_only = JobRepository::list(&conn, Some(EventKind::MeetingCompleted)).unwrap();
        assert_eq!(meeting_only.len(), 1);
        assert_eq!(meeting_only[0].name, "b");
    }

    #[test]
    fn list_enabled_skips_disabled() {
        let conn = setup_db();
        let id = JobRepository::insert(&conn, &sample_new("a")).unwrap();
        JobRepository::update(
            &conn,
            id,
            &UpdateJob {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        let active =
            JobRepository::list_enabled_for_event(&conn, EventKind::DictationCompleted).unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn update_applies_partial_patch() {
        let conn = setup_db();
        let id = JobRepository::insert(&conn, &sample_new("a")).unwrap();
        let ok = JobRepository::update(
            &conn,
            id,
            &UpdateJob {
                name: Some("renamed".to_string()),
                enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(ok);
        let job = JobRepository::get(&conn, id).unwrap().unwrap();
        assert_eq!(job.name, "renamed");
        assert!(!job.enabled);
        // Untouched fields preserved.
        assert_eq!(job.event, EventKind::DictationCompleted);
    }

    #[test]
    fn update_missing_id_returns_false() {
        let conn = setup_db();
        let ok = JobRepository::update(&conn, 9999, &UpdateJob::default()).unwrap();
        assert!(!ok);
    }

    #[test]
    fn delete_removes_row() {
        let conn = setup_db();
        let id = JobRepository::insert(&conn, &sample_new("a")).unwrap();
        assert!(JobRepository::delete(&conn, id).unwrap());
        assert!(JobRepository::get(&conn, id).unwrap().is_none());
    }

    #[test]
    fn delete_missing_id_returns_false() {
        let conn = setup_db();
        assert!(!JobRepository::delete(&conn, 9999).unwrap());
    }
}
