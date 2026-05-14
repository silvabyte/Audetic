//! Post-processing job CRUD + test endpoints.
//!
//! See OpenAPI spec at `/api/openapi.json` for the canonical method/path
//! list. All operations talk to [`JobRepository`] on a per-request
//! connection — same shape as `meetings` and `history`.

use axum::{
    extract::{Path, Query, State},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};

use crate::api::error::{ApiError, ApiResult};
use crate::post_processing::{
    Action, Event, EventKind, Job, JobRepository, NewJob, PostProcessingService, UpdateJob,
    ALL_EVENT_KINDS,
};

/// Shared state for post-processing routes.
#[derive(Clone)]
pub struct PostProcessingApiState {
    pub service: Arc<PostProcessingService>,
}

/// Reject invalid action shapes (empty command, etc.) at the API edge so
/// the DB never holds an unusable row. New `Action` variants extend the
/// match — the compiler will force us to handle them.
fn validate_action(action: &Action) -> ApiResult<()> {
    match action {
        Action::Command { command, .. } => {
            if command.trim().is_empty() {
                return Err(ApiError::bad_request("command is required"));
            }
        }
    }
    Ok(())
}

pub fn router(state: PostProcessingApiState) -> Router {
    Router::new()
        .route("/post-processing/events", get(list_events))
        .route("/post-processing/jobs", get(list_jobs).post(create_job))
        .route(
            "/post-processing/jobs/:id",
            get(get_job).patch(update_job).delete(delete_job),
        )
        .route("/post-processing/jobs/:id/test", post(test_job))
        .with_state(state)
}

/// One supported event in the `events` listing.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventDescriptor {
    /// Wire identifier persisted in `post_processing_jobs.event` and
    /// accepted in the `event` field of new/updated jobs.
    pub name: String,
    /// Human-readable label for UI dropdowns.
    pub label: String,
    /// What the JSON `data` shape contains when this event fires.
    pub description: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EventsListResponse {
    pub events: Vec<EventDescriptor>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobsListResponse {
    pub jobs: Vec<Job>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct JobsListQuery {
    /// Filter to a single event kind (e.g. `meeting.completed`).
    pub event: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteResponse {
    pub success: bool,
    pub id: i64,
}

/// Result body for `POST /jobs/:id/test`.
#[derive(Debug, Serialize, ToSchema)]
pub struct TestJobResponse {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// List every event kind the daemon can fire.
#[utoipa::path(
    get,
    path = "/post-processing/events",
    tag = "post_processing",
    responses(
        (status = 200, description = "Supported event kinds", body = EventsListResponse),
    ),
)]
pub async fn list_events() -> Json<EventsListResponse> {
    let events = ALL_EVENT_KINDS
        .iter()
        .map(|k| EventDescriptor {
            name: k.as_str().to_string(),
            label: match k {
                EventKind::DictationCompleted => "Dictation completed".to_string(),
                EventKind::MeetingCompleted => "Meeting completed".to_string(),
            },
            description: match k {
                EventKind::DictationCompleted => {
                    "Fires after a dictation transcription is saved to history. Payload contains the dictation_id, audio_path, and text.".to_string()
                }
                EventKind::MeetingCompleted => {
                    "Fires after a meeting is fully transcribed. Payload contains the meeting_id, title, audio_path, transcript_path, transcript_text, and duration_seconds.".to_string()
                }
            },
        })
        .collect();
    Json(EventsListResponse { events })
}

/// List all jobs (optionally filtered by event kind).
#[utoipa::path(
    get,
    path = "/post-processing/jobs",
    tag = "post_processing",
    params(JobsListQuery),
    responses(
        (status = 200, description = "Jobs newest first", body = JobsListResponse),
        (status = 400, description = "Unknown event filter"),
    ),
)]
pub async fn list_jobs(
    Query(params): Query<JobsListQuery>,
    State(_state): State<PostProcessingApiState>,
) -> ApiResult<Json<JobsListResponse>> {
    let event_filter = match params.event.as_deref() {
        Some(s) => Some(
            EventKind::from_str(s)
                .ok_or_else(|| ApiError::bad_request(format!("unknown event `{s}`")))?,
        ),
        None => None,
    };

    let jobs = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        JobRepository::list(&conn, event_filter)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?;

    Ok(Json(JobsListResponse { jobs }))
}

/// Create a new job.
#[utoipa::path(
    post,
    path = "/post-processing/jobs",
    tag = "post_processing",
    request_body = NewJob,
    responses(
        (status = 201, description = "Created job", body = Job),
        (status = 400, description = "Invalid input"),
    ),
)]
pub async fn create_job(
    State(_state): State<PostProcessingApiState>,
    Json(new): Json<NewJob>,
) -> ApiResult<(axum::http::StatusCode, Json<Job>)> {
    if new.name.trim().is_empty() {
        return Err(ApiError::bad_request("name is required"));
    }
    validate_action(&new.action)?;

    let job = tokio::task::spawn_blocking(move || -> anyhow::Result<Job> {
        let conn = crate::db::init_db()?;
        let id = JobRepository::insert(&conn, &new)?;
        JobRepository::get(&conn, id)?
            .ok_or_else(|| anyhow::anyhow!("inserted job {id} disappeared"))
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?;

    Ok((axum::http::StatusCode::CREATED, Json(job)))
}

#[utoipa::path(
    get,
    path = "/post-processing/jobs/{id}",
    tag = "post_processing",
    params(("id" = i64, Path, description = "Job id")),
    responses(
        (status = 200, description = "Job", body = Job),
        (status = 404, description = "Not found"),
    ),
)]
pub async fn get_job(
    Path(id): Path<i64>,
    State(_state): State<PostProcessingApiState>,
) -> ApiResult<Json<Job>> {
    let job = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        JobRepository::get(&conn, id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?
    .ok_or_else(|| ApiError::not_found(format!("Job {id} not found")))?;

    Ok(Json(job))
}

#[utoipa::path(
    patch,
    path = "/post-processing/jobs/{id}",
    tag = "post_processing",
    params(("id" = i64, Path, description = "Job id")),
    request_body = UpdateJob,
    responses(
        (status = 200, description = "Updated job", body = Job),
        (status = 404, description = "Not found"),
        (status = 400, description = "Invalid input"),
    ),
)]
pub async fn update_job(
    Path(id): Path<i64>,
    State(_state): State<PostProcessingApiState>,
    Json(patch): Json<UpdateJob>,
) -> ApiResult<Json<Job>> {
    if let Some(name) = &patch.name {
        if name.trim().is_empty() {
            return Err(ApiError::bad_request("name cannot be empty"));
        }
    }
    if let Some(action) = &patch.action {
        validate_action(action)?;
    }

    let job = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Job>> {
        let conn = crate::db::init_db()?;
        let updated = JobRepository::update(&conn, id, &patch)?;
        if !updated {
            return Ok(None);
        }
        JobRepository::get(&conn, id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?
    .ok_or_else(|| ApiError::not_found(format!("Job {id} not found")))?;

    Ok(Json(job))
}

#[utoipa::path(
    delete,
    path = "/post-processing/jobs/{id}",
    tag = "post_processing",
    params(("id" = i64, Path, description = "Job id")),
    responses(
        (status = 200, description = "Deleted", body = DeleteResponse),
        (status = 404, description = "Not found"),
    ),
)]
pub async fn delete_job(
    Path(id): Path<i64>,
    State(_state): State<PostProcessingApiState>,
) -> ApiResult<Json<DeleteResponse>> {
    let deleted = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        JobRepository::delete(&conn, id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?;

    if !deleted {
        return Err(ApiError::not_found(format!("Job {id} not found")));
    }
    Ok(Json(DeleteResponse { success: true, id }))
}

/// Run a job once with a synthetic payload. Useful for "did I write the
/// command right?" before waiting for a real event.
#[utoipa::path(
    post,
    path = "/post-processing/jobs/{id}/test",
    tag = "post_processing",
    params(("id" = i64, Path, description = "Job id")),
    responses(
        (status = 200, description = "Execution result", body = TestJobResponse),
        (status = 404, description = "Not found"),
    ),
)]
pub async fn test_job(
    Path(id): Path<i64>,
    State(state): State<PostProcessingApiState>,
) -> ApiResult<Json<TestJobResponse>> {
    let job = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        JobRepository::get(&conn, id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?
    .ok_or_else(|| ApiError::not_found(format!("Job {id} not found")))?;

    let event = Event::synthetic(job.event);
    let outcome = state
        .service
        .run_job_once(&job, event)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(TestJobResponse {
        success: outcome.success,
        exit_code: outcome.exit_code,
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        timed_out: outcome.timed_out,
    }))
}
