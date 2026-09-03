//! [`PostProcessingService`] — the entry point state machines call when
//! an event happens.
//!
//! `dispatch` is fire-and-forget: it spawns a tokio task per matched
//! enabled job and returns immediately. Failures inside a job are logged
//! via `tracing` and surface as audit logs (future), but never propagate
//! back to the caller — a slow or failing hook must not stall the
//! meeting/dictation pipeline.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use super::event::Event;
use super::executors::{executor_for, ExecutionOutcome};
use super::job::Job;
use super::repository::JobRepository;

/// Service handed to the meeting and recording machines.
///
/// The DB connection is opened per-dispatch. Carrying the path keeps tests
/// isolated from the user's database without holding a shared connection.
#[derive(Clone)]
pub struct PostProcessingService {
    db_path: PathBuf,
}

impl PostProcessingService {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Fire matching jobs for `event`. Returns immediately; spawned
    /// tasks run independently of the caller's future.
    pub fn dispatch(&self, event: Event) {
        let kind = event.kind();
        let payload = event.to_envelope();

        let jobs = match load_jobs(&self.db_path, kind) {
            Ok(j) => j,
            Err(e) => {
                warn!(
                    "post-processing: failed to load jobs for {}: {}",
                    kind.as_str(),
                    e
                );
                return;
            }
        };

        if jobs.is_empty() {
            return;
        }

        info!(
            "post-processing: dispatching {} job(s) for {}",
            jobs.len(),
            kind.as_str()
        );

        for job in jobs {
            let payload = payload.clone();
            tokio::spawn(async move {
                run_one(job, payload).await;
            });
        }
    }

    /// Run a single job with a synthetic payload — used by the
    /// `POST /api/post-processing/jobs/:id/test` endpoint. Unlike
    /// `dispatch`, this awaits the executor and returns the captured
    /// outcome so the UI can render exit code + stdout/stderr.
    pub async fn run_job_once(&self, job: &Job, event: Event) -> anyhow::Result<ExecutionOutcome> {
        let payload = event.to_envelope();
        let executor = executor_for(&job.action);
        executor.execute(&payload).await
    }
}

fn load_jobs(db_path: &Path, kind: super::event::EventKind) -> anyhow::Result<Vec<Job>> {
    let conn = crate::db::init_db_at(db_path)?;
    JobRepository::list_enabled_for_event(&conn, kind)
}

async fn run_one(job: Job, payload: serde_json::Value) {
    let executor = executor_for(&job.action);
    match executor.execute(&payload).await {
        Ok(outcome) if outcome.success => {
            info!(
                "post-processing job {} (`{}`) ok (exit {:?})",
                job.id, job.name, outcome.exit_code
            );
            if !outcome.stdout.is_empty() {
                info!(
                    "post-processing job {} stdout: {}",
                    job.id,
                    outcome.stdout.trim()
                );
            }
        }
        Ok(outcome) => {
            warn!(
                "post-processing job {} (`{}`) failed (exit {:?}, timed_out={}): {}",
                job.id,
                job.name,
                outcome.exit_code,
                outcome.timed_out,
                outcome.stderr.trim()
            );
        }
        Err(e) => {
            warn!(
                "post-processing job {} (`{}`) executor error: {}",
                job.id, job.name, e
            );
        }
    }
}
