//! Meeting recording API endpoints.
//!
//! Provides HTTP endpoints for:
//! - Starting meeting recording (POST /api/meetings/start)
//! - Stopping meeting recording (POST /api/meetings/stop)
//! - Cancelling meeting recording (POST /api/meetings/cancel)
//! - Toggling meeting recording (POST /api/meetings/toggle)
//! - Getting meeting status (GET /api/meetings/status)
//! - Listing meetings (GET /api/meetings)
//! - Getting a specific meeting (GET /api/meetings/:id)

use crate::meeting::{MeetingPhase, MeetingStartOptions, MeetingStatusHandle};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};
use utoipa::{IntoParams, ToSchema};

use super::recording::ApiCommand;

/// Shared state for meeting routes.
#[derive(Clone)]
pub struct MeetingState {
    pub tx: mpsc::Sender<ApiCommand>,
    pub status: MeetingStatusHandle,
    /// Same transcription service the meeting machine uses. Shared so the
    /// retry endpoint re-runs failed meetings against the same backend
    /// without rebuilding the HTTP client / timeout config.
    pub transcription:
        std::sync::Arc<dyn crate::transcription::job_service::TranscriptionJobService>,
}

/// Request body for start/toggle endpoints.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct MeetingStartRequest {
    pub title: Option<String>,
}

/// Response for POST /api/meetings/start.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingStartResponse {
    pub success: bool,
    pub meeting_id: i64,
    pub audio_path: String,
    pub capture_state: String,
    pub message: String,
}

/// Response for POST /api/meetings/stop and POST /api/meetings/cancel.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingStopResponse {
    pub success: bool,
    pub meeting_id: i64,
    pub duration_seconds: u64,
    pub message: String,
}

/// Response for POST /api/meetings/toggle — shape varies by whether a
/// meeting was started or stopped. Extra fields are only present
/// when relevant; both `duration_seconds` and `audio_path` may be null.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingToggleResponse {
    pub success: bool,
    pub meeting_id: i64,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u64>,
    pub message: String,
}

/// Default (non-waybar) response for GET /api/meetings/status.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingStatusResponse {
    pub active: bool,
    pub meeting_id: Option<i64>,
    pub phase: String,
    pub duration_seconds: Option<i64>,
    pub title: Option<String>,
    pub audio_path: Option<String>,
    pub last_error: Option<String>,
}

/// Summary of one meeting as returned in GET /api/meetings.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingSummary {
    pub id: i64,
    pub title: Option<String>,
    pub status: String,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub audio_path: String,
    pub transcript_path: Option<String>,
}

/// Response for GET /api/meetings.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingsListResponse {
    pub meetings: Vec<MeetingSummary>,
}

/// Response for GET /api/meetings/:id.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingDetailResponse {
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

/// Query parameters accepted by GET /api/meetings and GET /api/meetings/status.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct MeetingsListQuery {
    /// Maximum meetings to return (default 20)
    pub limit: Option<usize>,
}

pub fn router(state: MeetingState) -> Router {
    Router::new()
        .route("/meetings/start", post(start_meeting))
        .route("/meetings/stop", post(stop_meeting))
        .route("/meetings/cancel", post(cancel_meeting))
        .route("/meetings/toggle", post(toggle_meeting))
        .route("/meetings/status", get(meeting_status))
        .route("/meetings", get(list_meetings))
        .route("/meetings/:id", get(get_meeting))
        .route("/meetings/:id/retry", post(retry_meeting))
        .with_state(state)
}

/// Response for POST /api/meetings/:id/retry.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingRetryResponse {
    pub success: bool,
    pub meeting_id: i64,
    pub message: String,
}

/// Convert an anyhow error from the meeting machine into a client-friendly
/// HTTP response. Conflict-style errors (already recording / not recording)
/// map to 409; everything else is 500.
fn error_response(err: anyhow::Error, context: &str) -> Response {
    let msg = err.to_string();
    let status_code = if msg.contains("already in progress")
        || msg.contains("No meeting recording in progress")
        || msg.contains("No meeting recording in progress to cancel")
    {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };

    error!("{}: {}", context, msg);
    (
        status_code,
        Json(json!({
            "success": false,
            "message": msg,
        })),
    )
        .into_response()
}

/// Helper: send an ApiCommand and await the machine's reply.
async fn dispatch<T>(
    tx: &mpsc::Sender<ApiCommand>,
    reply: oneshot::Receiver<anyhow::Result<T>>,
    command: ApiCommand,
    op: &str,
) -> Result<T, Response> {
    if let Err(e) = tx.send(command).await {
        error!("Failed to dispatch {}: {}", op, e);
        return Err(error_response(
            anyhow::anyhow!("event loop unavailable: {e}"),
            op,
        ));
    }

    match reply.await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(e)) => Err(error_response(e, op)),
        Err(e) => {
            error!("{} reply channel closed: {}", op, e);
            Err(error_response(
                anyhow::anyhow!("reply channel closed: {e}"),
                op,
            ))
        }
    }
}

#[utoipa::path(
    post,
    path = "/meetings/start",
    tag = "meetings",
    request_body = MeetingStartRequest,
    responses(
        (status = 200, description = "Meeting started", body = MeetingStartResponse),
        (status = 409, description = "A meeting is already in progress"),
    ),
)]
pub async fn start_meeting(
    State(state): State<MeetingState>,
    body: Option<Json<MeetingStartRequest>>,
) -> Response {
    info!("Meeting start command received via API");

    let options = body.map(|Json(req)| MeetingStartOptions { title: req.title });
    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::MeetingStart {
        options,
        reply: reply_tx,
    };

    match dispatch(&state.tx, reply_rx, command, "start meeting").await {
        Ok(result) => Json(MeetingStartResponse {
            success: true,
            meeting_id: result.meeting_id,
            audio_path: result.audio_path.to_string_lossy().into_owned(),
            capture_state: result.capture_state.tag().to_string(),
            message: format!(
                "Meeting recording started ({})",
                result.capture_state.as_str()
            ),
        })
        .into_response(),
        Err(resp) => resp,
    }
}

#[utoipa::path(
    post,
    path = "/meetings/stop",
    tag = "meetings",
    responses(
        (status = 200, description = "Meeting stopped and transcription queued", body = MeetingStopResponse),
        (status = 409, description = "No meeting recording in progress"),
    ),
)]
pub async fn stop_meeting(State(state): State<MeetingState>) -> Response {
    info!("Meeting stop command received via API");

    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::MeetingStop { reply: reply_tx };

    match dispatch(&state.tx, reply_rx, command, "stop meeting").await {
        Ok(result) => Json(MeetingStopResponse {
            success: true,
            meeting_id: result.meeting_id,
            duration_seconds: result.duration_seconds,
            message: "Meeting recording stopped, transcription started in background".to_string(),
        })
        .into_response(),
        Err(resp) => resp,
    }
}

#[utoipa::path(
    post,
    path = "/meetings/cancel",
    tag = "meetings",
    responses(
        (status = 200, description = "Meeting cancelled without transcribing", body = MeetingStopResponse),
        (status = 409, description = "No meeting recording in progress to cancel"),
    ),
)]
pub async fn cancel_meeting(State(state): State<MeetingState>) -> Response {
    info!("Meeting cancel command received via API");

    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::MeetingCancel { reply: reply_tx };

    match dispatch(&state.tx, reply_rx, command, "cancel meeting").await {
        Ok(result) => Json(MeetingStopResponse {
            success: true,
            meeting_id: result.meeting_id,
            duration_seconds: result.duration_seconds,
            message: "Meeting recording cancelled".to_string(),
        })
        .into_response(),
        Err(resp) => resp,
    }
}

#[utoipa::path(
    post,
    path = "/meetings/toggle",
    tag = "meetings",
    request_body = MeetingStartRequest,
    responses(
        (status = 200, description = "Meeting started or stopped", body = MeetingToggleResponse),
    ),
)]
pub async fn toggle_meeting(
    State(state): State<MeetingState>,
    body: Option<Json<MeetingStartRequest>>,
) -> Response {
    info!("Meeting toggle command received via API");

    let options = body.map(|Json(req)| MeetingStartOptions { title: req.title });
    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::MeetingToggle {
        options,
        reply: reply_tx,
    };

    match dispatch(&state.tx, reply_rx, command, "toggle meeting").await {
        Ok(outcome) => match outcome {
            crate::meeting::ToggleOutcome::Started(r) => Json(MeetingToggleResponse {
                success: true,
                meeting_id: r.meeting_id,
                phase: "recording".to_string(),
                audio_path: Some(r.audio_path.to_string_lossy().into_owned()),
                capture_state: Some(r.capture_state.tag().to_string()),
                duration_seconds: None,
                message: format!("Meeting recording started ({})", r.capture_state.as_str()),
            })
            .into_response(),
            crate::meeting::ToggleOutcome::Stopped(r) => Json(MeetingToggleResponse {
                success: true,
                meeting_id: r.meeting_id,
                phase: "compressing".to_string(),
                audio_path: None,
                capture_state: None,
                duration_seconds: Some(r.duration_seconds),
                message: "Meeting recording stopped, transcription started in background"
                    .to_string(),
            })
            .into_response(),
        },
        Err(resp) => resp,
    }
}

#[utoipa::path(
    get,
    path = "/meetings/status",
    tag = "meetings",
    params(
        ("style" = Option<String>, Query, description = "Set to `waybar` for Waybar-formatted response"),
    ),
    responses(
        (status = 200, description = "Meeting status (default JSON shape)", body = MeetingStatusResponse),
    ),
)]
pub async fn meeting_status(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<MeetingState>,
) -> Json<Value> {
    let status = state.status.get().await;
    let is_active = status.phase == MeetingPhase::Recording;

    // Waybar style response
    if params.get("style") == Some(&"waybar".to_string()) {
        let (text, class, tooltip) = if is_active {
            let duration = status.duration_seconds().unwrap_or(0);
            let minutes = duration / 60;
            let seconds = duration % 60;
            (
                "\u{f0d6b}".to_string(),
                "audetic-meeting".to_string(),
                format!("Meeting recording: {:02}:{:02}", minutes, seconds),
            )
        } else {
            (
                String::new(),
                "audetic-meeting-idle".to_string(),
                "No meeting recording".to_string(),
            )
        };

        return Json(json!({
            "text": text,
            "class": class,
            "tooltip": tooltip,
        }));
    }

    Json(json!({
        "active": is_active,
        "meeting_id": status.meeting_id,
        "phase": status.phase.as_str(),
        "duration_seconds": status.duration_seconds(),
        "title": status.title,
        "audio_path": status.audio_path.map(|p| p.to_string_lossy().to_string()),
        "last_error": status.last_error,
    }))
}

#[utoipa::path(
    get,
    path = "/meetings",
    tag = "meetings",
    params(MeetingsListQuery),
    responses(
        (status = 200, description = "Recent meetings, newest first", body = MeetingsListResponse),
    ),
)]
pub async fn list_meetings(
    Query(params): Query<MeetingsListQuery>,
    State(_state): State<MeetingState>,
) -> Result<Json<MeetingsListResponse>, StatusCode> {
    let limit = params.limit.unwrap_or(20);

    let meetings = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        crate::db::meetings::MeetingRepository::list(&conn, limit)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let entries: Vec<MeetingSummary> = meetings
        .into_iter()
        .map(|m| MeetingSummary {
            id: m.id,
            title: m.title,
            status: m.status,
            duration_seconds: m.duration_seconds,
            started_at: m.started_at,
            audio_path: m.audio_path,
            transcript_path: m.transcript_path,
        })
        .collect();

    Ok(Json(MeetingsListResponse { meetings: entries }))
}

#[utoipa::path(
    get,
    path = "/meetings/{id}",
    tag = "meetings",
    params(
        ("id" = i64, Path, description = "Meeting id"),
    ),
    responses(
        (status = 200, description = "Meeting detail", body = MeetingDetailResponse),
        (status = 404, description = "Meeting not found"),
    ),
)]
pub async fn get_meeting(
    Path(id): Path<i64>,
    State(_state): State<MeetingState>,
) -> Result<Json<MeetingDetailResponse>, Response> {
    let meeting = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        crate::db::meetings::MeetingRepository::get(&conn, id)
    })
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": "db task panicked" })),
        )
            .into_response()
    })?
    .map_err(|e| {
        error!("failed to read meeting {}: {}", id, e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "success": false, "message": e.to_string() })),
        )
            .into_response()
    })?;

    match meeting {
        Some(m) => Ok(Json(MeetingDetailResponse {
            id: m.id,
            title: m.title,
            status: m.status,
            audio_path: m.audio_path,
            transcript_path: m.transcript_path,
            transcript_text: m.transcript_text,
            duration_seconds: m.duration_seconds,
            started_at: m.started_at,
            completed_at: m.completed_at,
            error: m.error,
            created_at: m.created_at,
        })),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "success": false,
                "message": format!("Meeting {} not found", id),
            })),
        )
            .into_response()),
    }
}

/// POST /api/meetings/:id/retry — re-run transcription on the durable mp3 from a
/// previously failed meeting. Useful when the backend was the cause (e.g. the
/// 5-min Bun-fetch idle bug in InferenceServerManager) and the audio is fine.
///
/// Validates: meeting exists, is in `error` state, and its mp3 is still on
/// disk. Spawns the retry in a tokio task and returns 202 immediately so the
/// renderer can begin polling `/meetings/:id` for the status flip.
#[utoipa::path(
    post,
    path = "/meetings/{id}/retry",
    tag = "meetings",
    params(
        ("id" = i64, Path, description = "Meeting id"),
    ),
    responses(
        (status = 202, description = "Retry kicked off; poll /meetings/:id", body = MeetingRetryResponse),
        (status = 404, description = "Meeting not found"),
        (status = 409, description = "Meeting is not in a retry-eligible state, or audio file missing"),
    ),
)]
pub async fn retry_meeting(Path(id): Path<i64>, State(state): State<MeetingState>) -> Response {
    info!("Meeting {} retry requested", id);

    let join = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        crate::db::meetings::MeetingRepository::get(&conn, id)
    })
    .await;

    let meeting = match join {
        Ok(Ok(Some(m))) => m,
        Ok(Ok(None)) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "success": false,
                    "message": format!("Meeting {} not found", id),
                })),
            )
                .into_response();
        }
        Ok(Err(e)) => {
            error!("Failed to load meeting {}: {}", id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "success": false,
                    "message": e.to_string(),
                })),
            )
                .into_response();
        }
        Err(e) => {
            error!("DB task panicked while loading meeting {}: {}", id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "success": false,
                    "message": "db task panicked",
                })),
            )
                .into_response();
        }
    };

    // Only retry from a terminal failure. Re-running a `completed` meeting is
    // a no-op the user almost certainly didn't intend; re-running an in-flight
    // one would race with the live machine.
    if meeting.status != MeetingPhase::Error.as_str() {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "message": format!(
                    "Meeting {} is in state '{}'; only failed meetings can be retried",
                    id, meeting.status
                ),
            })),
        )
            .into_response();
    }

    // Resolve the file actually on disk. Older meetings (before we kept the
    // DB row in sync with the WAV → MP3 compression swap) have a stale
    // `.wav` path; the durable mp3 next to it is what we actually want.
    let stored_path = std::path::PathBuf::from(&meeting.audio_path);
    let resolved_path = if stored_path.exists() {
        stored_path
    } else {
        let mp3_sibling = stored_path.with_extension("mp3");
        if mp3_sibling.exists() {
            info!(
                "Meeting {} stored path missing; using mp3 sibling: {:?}",
                id, mp3_sibling
            );
            // Heal the row so future calls don't pay this lookup again.
            let mp3_str = mp3_sibling.to_string_lossy().into_owned();
            let _ = tokio::task::spawn_blocking(move || {
                let conn = crate::db::init_db()?;
                crate::db::meetings::MeetingRepository::update_audio_path(&conn, id, &mp3_str)
            })
            .await;
            mp3_sibling
        } else {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "success": false,
                    "message": format!(
                        "Audio file no longer on disk: {} (and no .mp3 sibling)",
                        meeting.audio_path
                    ),
                })),
            )
                .into_response();
        }
    };

    let duration = meeting.duration_seconds.unwrap_or(0);
    let transcription = state.transcription.clone();
    tokio::spawn(async move {
        crate::meeting::retry_meeting_transcription(id, resolved_path, duration, transcription)
            .await;
    });

    (
        StatusCode::ACCEPTED,
        Json(MeetingRetryResponse {
            success: true,
            meeting_id: id,
            message: "Retry started; poll /meetings/:id for status".to_string(),
        }),
    )
        .into_response()
}
