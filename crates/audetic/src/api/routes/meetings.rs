//! Meeting recording API endpoints. See OpenAPI spec at
//! `/api/openapi.json` for the canonical method/path list.

use crate::meeting::{
    import_meeting_file, ImportArgs, MediaInspector, MeetingPhase, MeetingStartOptions,
    MeetingStatusHandle, ProcessingServices,
};
use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tower::util::ServiceExt;
use tower_http::services::ServeFile;
use tracing::{error, info, warn};
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
    /// Pipeline dependencies — transcription service and optional hook.
    /// Used by the import endpoint to spawn the same pipeline a live
    /// recording does.
    pub services: ProcessingServices,
    /// Media duration probe — `FfprobeMediaInspector` in production. Used
    /// by the import endpoint to seed `duration_seconds` before kicking
    /// off the pipeline.
    pub inspector: Arc<dyn MediaInspector>,
    /// Durable meetings directory (`~/.local/share/audetic/meetings`).
    /// Uploaded files are staged into a `.uploads` sub-dir, then moved
    /// alongside live recordings on success.
    pub meetings_dir: PathBuf,
}

/// Request body for start/toggle endpoints.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct MeetingStartRequest {
    pub title: Option<String>,
}

/// Confirmation that a meeting recording has begun: the assigned id,
/// where audio is being written, and capture-source state.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingStartResponse {
    pub success: bool,
    pub meeting_id: i64,
    pub audio_path: String,
    pub capture_state: String,
    pub message: String,
}

/// Result of ending a meeting (stop or cancel): the meeting id and how
/// long it ran.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingStopResponse {
    pub success: bool,
    pub meeting_id: i64,
    pub duration_seconds: u64,
    pub message: String,
}

/// Result of a meeting toggle. Shape varies by whether a meeting was
/// started or stopped: `audio_path`/`capture_state` appear on start,
/// `duration_seconds` appears on stop, hence the optional fields.
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

/// Default (non-waybar) meeting status snapshot. The waybar variant
/// has a different shape — see the union response on the handler.
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

/// Summary of one meeting in a list response — enough to render a row
/// without loading the full transcript.
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

/// Paginated list of meeting summaries.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingsListResponse {
    pub meetings: Vec<MeetingSummary>,
}

/// Full meeting record including transcript text when available.
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

/// Pagination + filter knobs shared by list and status endpoints.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct MeetingsListQuery {
    /// Maximum meetings to return (default 20)
    pub limit: Option<usize>,
}

/// Confirmation that an imported media file has been accepted as a new
/// meeting. The processing pipeline runs in the background; clients poll
/// `GET /meetings/{id}` for phase progression and the final transcript.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingImportResponse {
    pub success: bool,
    pub meeting_id: i64,
    pub message: String,
}

pub fn router(state: MeetingState) -> Router {
    Router::new()
        .route("/meetings/start", post(start_meeting))
        .route("/meetings/stop", post(stop_meeting))
        .route("/meetings/confirm", post(confirm_meeting))
        .route("/meetings/cancel", post(cancel_meeting))
        .route("/meetings/toggle", post(toggle_meeting))
        .route("/meetings/status", get(meeting_status))
        .route("/meetings", get(list_meetings))
        .route(
            "/meetings/import",
            // Disable the global 2 MiB body limit on this route only —
            // meeting recordings and video files run into the hundreds of
            // MB. The multipart extractor below streams chunks to disk so
            // memory usage stays bounded regardless of body size.
            post(import_meeting).layer(DefaultBodyLimit::disable()),
        )
        .route("/meetings/:id", get(get_meeting).delete(delete_meeting))
        .route("/meetings/:id/audio", get(meeting_audio))
        .route("/meetings/:id/retry", post(retry_meeting))
        .with_state(state)
}

/// Confirmation that a failed meeting's transcription has been
/// re-queued; the actual work runs in the background.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingRetryResponse {
    pub success: bool,
    pub meeting_id: i64,
    pub message: String,
}

/// Confirmation that a meeting has been deleted. The delete is *soft*: the
/// meeting is hidden from every API surface but its row and on-disk audio
/// survive.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingDeleteResponse {
    pub success: bool,
    pub meeting_id: i64,
    pub message: String,
}

/// Convert an anyhow error from the meeting machine into a client-friendly
/// HTTP response. Conflict-style errors (already recording / not recording)
/// map to 409; everything else is 500.
fn error_response(err: anyhow::Error, context: &str) -> Response {
    // Use the full anyhow chain so wrapped causes (e.g. "Invalid trim range"
    // behind "Failed to trim meeting audio") are visible for both the status
    // mapping below and the client message.
    let msg = format!("{err:#}");
    let status_code = if msg.contains("Invalid trim range") {
        StatusCode::BAD_REQUEST
    } else if msg.contains("already in progress") || msg.contains("No meeting") {
        // Covers "No meeting recording in progress" (stop), "No meeting
        // recording or awaiting review to cancel" (cancel) and "No meeting
        // awaiting review" (confirm).
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
        (status = 200, description = "Meeting stopped; awaiting review before transcription", body = MeetingStopResponse),
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
            message: "Meeting recording stopped; review and confirm to transcribe".to_string(),
        })
        .into_response(),
        Err(resp) => resp,
    }
}

/// Request body for the confirm endpoint. Both bounds are optional; omitting
/// one keeps that edge of the recording. Both omitted sends it untouched.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct MeetingConfirmRequest {
    /// New start of the recording, in seconds (clamped to the recording).
    pub start_seconds: Option<f64>,
    /// New end of the recording, in seconds (clamped to the recording).
    pub end_seconds: Option<f64>,
}

#[utoipa::path(
    post,
    path = "/meetings/confirm",
    tag = "meetings",
    request_body = MeetingConfirmRequest,
    responses(
        (status = 200, description = "Meeting confirmed; transcription queued", body = MeetingStopResponse),
        (status = 400, description = "Invalid trim range"),
        (status = 409, description = "No meeting awaiting review"),
    ),
)]
pub async fn confirm_meeting(
    State(state): State<MeetingState>,
    body: Option<Json<MeetingConfirmRequest>>,
) -> Response {
    info!("Meeting confirm command received via API");

    let (start_seconds, end_seconds) = body
        .map(|Json(r)| (r.start_seconds, r.end_seconds))
        .unwrap_or((None, None));

    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::MeetingConfirm {
        start_seconds,
        end_seconds,
        reply: reply_tx,
    };

    match dispatch(&state.tx, reply_rx, command, "confirm meeting").await {
        Ok(result) => Json(MeetingStopResponse {
            success: true,
            meeting_id: result.meeting_id,
            duration_seconds: result.duration_seconds,
            message: "Meeting confirmed, transcription started in background".to_string(),
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
                phase: "review".to_string(),
                audio_path: None,
                capture_state: None,
                duration_seconds: Some(r.duration_seconds),
                message: "Meeting recording stopped; review and confirm to transcribe".to_string(),
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

/// Stream a meeting's audio file for in-browser playback. Used by the review
/// UI so the user can listen back before choosing trim points. Resolves the
/// file actually on disk — the row points at the `.wav` while review is
/// pending and the `.mp3` after processing. Served via `ServeFile`, which
/// honours HTTP Range requests so the `<audio>` element can seek.
#[utoipa::path(
    get,
    path = "/meetings/{id}/audio",
    tag = "meetings",
    params(
        ("id" = i64, Path, description = "Meeting id"),
    ),
    responses(
        (status = 200, description = "Audio bytes (supports Range)"),
        (status = 404, description = "Meeting or audio file not found"),
    ),
)]
pub async fn meeting_audio(
    Path(id): Path<i64>,
    State(_state): State<MeetingState>,
    request: axum::extract::Request,
) -> Response {
    let lookup = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        crate::db::meetings::MeetingRepository::get(&conn, id)
    })
    .await;

    let meeting = match lookup {
        Ok(Ok(Some(m))) => m,
        Ok(Ok(None)) => return audio_not_found(id),
        Ok(Err(e)) => {
            error!("Failed to load meeting {} for audio: {}", id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": e.to_string() })),
            )
                .into_response();
        }
        Err(e) => {
            error!("DB task panicked loading meeting {} audio: {}", id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": "db task panicked" })),
            )
                .into_response();
        }
    };

    // Resolve the file on disk: pending-review rows point at the .wav, while
    // processed rows point at the .mp3 (and older rows may have a stale .wav
    // path whose .mp3 sibling is the real file).
    let stored = std::path::PathBuf::from(&meeting.audio_path);
    let resolved = if stored.exists() {
        stored
    } else {
        let mp3 = stored.with_extension("mp3");
        if mp3.exists() {
            mp3
        } else {
            return audio_not_found(id);
        }
    };

    match ServeFile::new(resolved).oneshot(request).await {
        Ok(res) => res.into_response(),
        Err(e) => {
            error!("Failed to serve meeting {} audio: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": "failed to read audio file" })),
            )
                .into_response()
        }
    }
}

fn audio_not_found(id: i64) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "success": false,
            "message": format!("Audio for meeting {} not found", id),
        })),
    )
        .into_response()
}

/// Re-run transcription on the durable mp3 from a previously failed
/// meeting. Useful when the backend was the cause (e.g. the 5-min
/// Bun-fetch idle bug in InferenceServerManager) and the audio is fine.
///
/// Validates: meeting exists, is in `error` state, and its mp3 is still
/// on disk. Spawns the retry in a tokio task and returns 202
/// immediately so the renderer can begin polling for the status flip.
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

/// Soft-delete a meeting.
///
/// The user-facing label is "Delete", but the row is only hidden — we stamp
/// `deleted_at` so it drops out of every API surface (list, detail, audio,
/// retry) while the recording stays on disk. Recovery is a manual DB edit.
///
/// In-flight meetings (recording / review / processing) are refused with 409:
/// their id is still owned by the meeting machine and background pipeline, so
/// hiding the row would 404 the active/review UI and break completion
/// auto-nav. Stop or cancel the meeting first. Returns 404 if the meeting
/// doesn't exist or was already deleted.
#[utoipa::path(
    delete,
    path = "/meetings/{id}",
    tag = "meetings",
    params(
        ("id" = i64, Path, description = "Meeting id"),
    ),
    responses(
        (status = 200, description = "Meeting deleted (hidden from all views)", body = MeetingDeleteResponse),
        (status = 404, description = "Meeting not found or already deleted"),
        (status = 409, description = "Meeting is still in progress; stop or cancel it first"),
    ),
)]
pub async fn delete_meeting(Path(id): Path<i64>, State(_state): State<MeetingState>) -> Response {
    info!("Meeting {} delete requested", id);

    let outcome = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        crate::db::meetings::MeetingRepository::soft_delete(&conn, id)
    })
    .await;

    use crate::db::meetings::SoftDeleteOutcome;
    match outcome {
        Ok(Ok(SoftDeleteOutcome::Deleted)) => (
            StatusCode::OK,
            Json(MeetingDeleteResponse {
                success: true,
                meeting_id: id,
                message: format!("Meeting {id} deleted"),
            }),
        )
            .into_response(),
        Ok(Ok(SoftDeleteOutcome::NotFound)) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "success": false,
                "message": format!("Meeting {id} not found"),
            })),
        )
            .into_response(),
        Ok(Ok(SoftDeleteOutcome::InFlight)) => (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "message": format!(
                    "Meeting {id} is still in progress; stop or cancel it before deleting"
                ),
            })),
        )
            .into_response(),
        Ok(Err(e)) => {
            error!("Failed to delete meeting {}: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": e.to_string() })),
            )
                .into_response()
        }
        Err(e) => {
            error!("DB task panicked deleting meeting {}: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": "db task panicked" })),
            )
                .into_response()
        }
    }
}

/// Import a media file as a new meeting.
///
/// Accepts a `multipart/form-data` body with:
/// - `file`: the audio or video bytes (required)
/// - `title`: optional human-readable title; defaults to the filename
///   stem if absent
///
/// The file is streamed chunk-by-chunk into a temp file under the meetings
/// directory, then handed to `meeting::import_meeting_file`, which moves
/// it into place, inserts the DB row, and spawns the processing pipeline.
/// Returns 202 with the new meeting id; clients poll `GET /meetings/{id}`
/// for status. The response intentionally omits the storage path —
/// callers shouldn't depend on the filesystem layout.
#[utoipa::path(
    post,
    path = "/meetings/import",
    tag = "meetings",
    request_body(
        content_type = "multipart/form-data",
        description = "File upload with optional title",
    ),
    responses(
        (status = 202, description = "Import accepted; poll /meetings/:id", body = MeetingImportResponse),
        (status = 400, description = "Missing file part or unsupported extension"),
        (status = 500, description = "Failed to stage upload or insert meeting row"),
    ),
)]
pub async fn import_meeting(
    State(state): State<MeetingState>,
    mut multipart: Multipart,
) -> Response {
    info!("Meeting import command received via API");

    let uploads_dir = state.meetings_dir.join(".uploads");
    if let Err(e) = tokio::fs::create_dir_all(&uploads_dir).await {
        return import_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to create uploads dir: {e}"),
        );
    }

    let mut staged: Option<(PathBuf, Option<String>)> = None;
    let mut title: Option<String> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                cleanup_staged(staged.as_ref().map(|(p, _)| p)).await;
                return import_error(
                    StatusCode::BAD_REQUEST,
                    format!("Malformed multipart body: {e}"),
                );
            }
        };

        match field.name() {
            Some("file") => {
                if staged.is_some() {
                    cleanup_staged(staged.as_ref().map(|(p, _)| p)).await;
                    return import_error(
                        StatusCode::BAD_REQUEST,
                        "Only one `file` part is allowed".to_string(),
                    );
                }
                let original_filename = field.file_name().map(|s| s.to_string());
                let temp_name = format!("upload-{}", uuid::Uuid::new_v4().simple());
                let temp_path = uploads_dir.join(&temp_name);

                match stream_field_to_disk(field, &temp_path).await {
                    Ok(()) => {
                        staged = Some((temp_path, original_filename));
                    }
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&temp_path).await;
                        return import_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to stage upload: {e}"),
                        );
                    }
                }
            }
            Some("title") => match field.text().await {
                Ok(t) => {
                    let trimmed = t.trim();
                    if !trimmed.is_empty() {
                        title = Some(trimmed.to_string());
                    }
                }
                Err(e) => {
                    cleanup_staged(staged.as_ref().map(|(p, _)| p)).await;
                    return import_error(
                        StatusCode::BAD_REQUEST,
                        format!("Failed to read title field: {e}"),
                    );
                }
            },
            _ => {
                // Ignore unknown fields rather than rejecting — keeps the
                // door open for additive form extensions without breaking
                // older clients.
            }
        }
    }

    let (source_path, original_filename) = match staged {
        Some(v) => v,
        None => {
            return import_error(
                StatusCode::BAD_REQUEST,
                "Missing required `file` part".to_string(),
            );
        }
    };

    let args = ImportArgs {
        source_path: source_path.clone(),
        original_filename,
        title,
        services: state.services.clone(),
        inspector: state.inspector.clone(),
        meetings_dir: state.meetings_dir.clone(),
    };

    match import_meeting_file(args).await {
        Ok(result) => (
            StatusCode::ACCEPTED,
            Json(MeetingImportResponse {
                success: true,
                meeting_id: result.meeting_id,
                message: "Import accepted; poll /meetings/:id for status".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            // import_meeting_file cleans up its own destination file on
            // DB-insert failure, but if it bailed before staging (e.g.
            // unsupported extension) the temp upload is still on disk.
            let _ = tokio::fs::remove_file(&source_path).await;
            let msg = e.to_string();
            let lower = msg.to_lowercase();
            let status_code =
                if lower.contains("unsupported") || lower.contains("missing an extension") {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
            import_error(status_code, msg)
        }
    }
}

/// Stream a multipart field's bytes to a file on disk. Bounded memory
/// regardless of upload size — we never collect the whole field into a
/// `Vec`.
async fn stream_field_to_disk(
    mut field: axum::extract::multipart::Field<'_>,
    destination: &std::path::Path,
) -> anyhow::Result<()> {
    let mut file = tokio::fs::File::create(destination).await?;
    while let Some(chunk) = field.chunk().await? {
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(())
}

async fn cleanup_staged(path: Option<&PathBuf>) {
    if let Some(p) = path {
        if let Err(e) = tokio::fs::remove_file(p).await {
            warn!("Failed to clean up staged upload at {:?}: {}", p, e);
        }
    }
}

fn import_error(status: StatusCode, message: String) -> Response {
    error!("Meeting import failed ({}): {}", status, message);
    (
        status,
        Json(json!({
            "success": false,
            "message": message,
        })),
    )
        .into_response()
}
