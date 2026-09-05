//! Meeting recording API endpoints. See OpenAPI spec at
//! `/api/openapi.json` for the canonical method/path list.

use audetic_core::sync::{DeviceId, PayloadAvailability, RecordId, UploadState};
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
use tracing::{error, info, warn};
use utoipa::{IntoParams, ToSchema};

use crate::api::error::{ApiError, ApiResult};
use crate::app::DaemonCommand as ApiCommand;
use crate::meeting::{
    import_meeting_file, ImportArgs, MediaInspector, MeetingPhase, MeetingStartOptions,
    MeetingStatusHandle, ProcessingServices,
};

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
    pub library: Option<Arc<crate::sync::shared_library::SharedLibrary>>,
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
    pub meeting_id: RecordId,
    pub capture_state: String,
    pub message: String,
}

/// Result of ending a meeting (stop or cancel): the meeting id and how
/// long it ran.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingStopResponse {
    pub success: bool,
    pub meeting_id: RecordId,
    pub duration_seconds: u64,
    pub message: String,
}

/// Result of a meeting toggle. Shape varies by whether a meeting was
/// started or stopped: `audio_path`/`capture_state` appear on start,
/// `duration_seconds` appears on stop, hence the optional fields.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingToggleResponse {
    pub success: bool,
    pub meeting_id: RecordId,
    pub phase: String,
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
    pub capture_degraded: bool,
    pub meeting_id: Option<RecordId>,
    pub phase: String,
    pub duration_seconds: Option<i64>,
    pub title: Option<String>,
    pub last_error: Option<String>,
}

/// Summary of one meeting in a list response — enough to render a row
/// without loading the full transcript.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingSummary {
    pub id: RecordId,
    pub origin_device_id: DeviceId,
    pub title: Option<String>,
    pub title_source: Option<MeetingTitleSource>,
    pub source_filename: Option<String>,
    pub status: String,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub upload_state: Option<UploadState>,
    pub payload_availability: PayloadAvailability,
    pub source: String,
    pub offline: bool,
    pub read_only: bool,
}

/// Paginated list of meeting summaries.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingsListResponse {
    pub meetings: Vec<MeetingSummary>,
}

/// Full meeting record including transcript text when available.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingDetailResponse {
    pub id: RecordId,
    pub origin_device_id: DeviceId,
    pub title: Option<String>,
    pub title_source: Option<MeetingTitleSource>,
    pub source_filename: Option<String>,
    pub status: String,
    pub transcript_text: Option<String>,
    /// Per-segment timestamps for clickable transcript lines. `None` for
    /// meetings transcribed before timestamps were captured.
    pub transcript_segments: Option<Vec<audetic_core::jobs_client::Segment>>,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub upload_state: Option<UploadState>,
    pub payload_availability: PayloadAvailability,
    pub source: String,
    pub offline: bool,
    pub read_only: bool,
}

/// Pagination + filter knobs shared by list and status endpoints.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct MeetingsListQuery {
    /// Maximum meetings to return (default 20, maximum 100).
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub q: Option<String>,
}

/// Confirmation that an imported media file has been accepted as a new
/// meeting. The processing pipeline runs in the background; clients poll
/// `GET /meetings/{id}` for phase progression and the final transcript.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingImportResponse {
    pub success: bool,
    pub meeting_id: RecordId,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeetingTitleSource {
    Manual,
    Generated,
}

impl MeetingTitleSource {
    fn from_stored(source: Option<&str>) -> Option<Self> {
        match source {
            Some("manual") => Some(Self::Manual),
            Some("generated") => Some(Self::Generated),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct RecentMeetingTitlesQuery {
    /// Maximum distinct Manual Titles to return (default 10, maximum 50).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecentMeetingTitlesResponse {
    pub titles: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MeetingTitleUpdateRequest {
    /// New non-empty Manual Title.
    pub title: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingTitleResponse {
    pub meeting_id: RecordId,
    pub title: Option<String>,
    pub title_source: Option<MeetingTitleSource>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingTitleRegenerationResponse {
    pub success: bool,
    pub meeting_id: RecordId,
    pub message: String,
}

pub fn router(mut state: MeetingState) -> Router {
    if state.library.is_none() {
        state.library = Some(Arc::new(state.services.local_library()));
    }
    Router::new()
        .route("/meetings/start", post(start_meeting))
        .route("/meetings/stop", post(stop_meeting))
        .route("/meetings/confirm", post(confirm_meeting))
        .route("/meetings/cancel", post(cancel_meeting))
        .route("/meetings/toggle", post(toggle_meeting))
        .route("/meetings/status", get(meeting_status))
        .route("/meetings/recent-titles", get(recent_meeting_titles))
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
        .route(
            "/meetings/:id/title",
            axum::routing::patch(update_meeting_title),
        )
        .route(
            "/meetings/:id/regenerate-title",
            post(regenerate_meeting_title),
        )
        .route("/meetings/:id/audio", get(meeting_audio))
        .route("/meetings/:id/retry", post(retry_meeting))
        .with_state(state)
}

/// Confirmation that a failed meeting's transcription has been
/// re-queued; the actual work runs in the background.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingRetryResponse {
    pub success: bool,
    pub meeting_id: RecordId,
    pub message: String,
}

/// Confirmation that a meeting has been deleted. The delete is *soft*: the
/// meeting is hidden from every API surface but its row and on-disk audio
/// survive.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingDeleteResponse {
    pub success: bool,
    pub meeting_id: RecordId,
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

fn parse_record_id(value: &str) -> Result<RecordId, ApiError> {
    value.parse().map_err(ApiError::bad_request)
}

fn sync_id_for_local(state: &MeetingState, local_id: i64) -> anyhow::Result<RecordId> {
    match state.library.as_ref() {
        Some(library) => library
            .public_meeting_id(local_id)
            .map_err(anyhow::Error::new),
        None => state.services.public_meeting_id(local_id),
    }
}

/// Helper: send a daemon command and await the machine's reply.
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
        Ok(result) => {
            let meeting_id = match sync_id_for_local(&state, result.meeting_id) {
                Ok(id) => id,
                Err(error) => return error_response(error, "resolve meeting UUID"),
            };
            Json(MeetingStartResponse {
                success: true,
                meeting_id,
                capture_state: result.capture_state.tag().to_string(),
                message: format!(
                    "Meeting recording started ({})",
                    result.capture_state.as_str()
                ),
            })
            .into_response()
        }
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
        Ok(result) => {
            let meeting_id = match sync_id_for_local(&state, result.meeting_id) {
                Ok(id) => id,
                Err(error) => return error_response(error, "resolve meeting UUID"),
            };
            Json(MeetingStopResponse {
                success: true,
                meeting_id,
                duration_seconds: result.duration_seconds,
                message: "Meeting recording stopped; review and confirm to transcribe".to_string(),
            })
            .into_response()
        }
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
        Ok(result) => {
            let meeting_id = match sync_id_for_local(&state, result.meeting_id) {
                Ok(id) => id,
                Err(error) => return error_response(error, "resolve meeting UUID"),
            };
            Json(MeetingStopResponse {
                success: true,
                meeting_id,
                duration_seconds: result.duration_seconds,
                message: "Meeting confirmed, transcription started in background".to_string(),
            })
            .into_response()
        }
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
        Ok(result) => {
            let meeting_id = match sync_id_for_local(&state, result.meeting_id) {
                Ok(id) => id,
                Err(error) => return error_response(error, "resolve meeting UUID"),
            };
            Json(MeetingStopResponse {
                success: true,
                meeting_id,
                duration_seconds: result.duration_seconds,
                message: "Meeting recording cancelled".to_string(),
            })
            .into_response()
        }
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
            crate::meeting::ToggleOutcome::Started(r) => {
                let meeting_id = match sync_id_for_local(&state, r.meeting_id) {
                    Ok(id) => id,
                    Err(error) => return error_response(error, "resolve meeting UUID"),
                };
                Json(MeetingToggleResponse {
                    success: true,
                    meeting_id,
                    phase: "recording".to_string(),
                    capture_state: Some(r.capture_state.tag().to_string()),
                    duration_seconds: None,
                    message: format!("Meeting recording started ({})", r.capture_state.as_str()),
                })
                .into_response()
            }
            crate::meeting::ToggleOutcome::Stopped(r) => {
                let meeting_id = match sync_id_for_local(&state, r.meeting_id) {
                    Ok(id) => id,
                    Err(error) => return error_response(error, "resolve meeting UUID"),
                };
                Json(MeetingToggleResponse {
                    success: true,
                    meeting_id,
                    phase: "review".to_string(),
                    capture_state: None,
                    duration_seconds: Some(r.duration_seconds),
                    message: "Meeting recording stopped; review and confirm to transcribe"
                        .to_string(),
                })
                .into_response()
            }
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

    let public_id = status
        .meeting_id
        .and_then(|id| sync_id_for_local(&state, id).ok());
    Json(default_meeting_status_json(&status, public_id))
}

fn default_meeting_status_json(
    status: &crate::meeting::MeetingState,
    public_id: Option<RecordId>,
) -> Value {
    json!({
        "active": status.phase == MeetingPhase::Recording,
        "capture_degraded": status.capture_degraded,
        "meeting_id": public_id,
        "phase": status.phase.as_str(),
        "duration_seconds": status.duration_seconds(),
        "title": status.title,
        "last_error": status.last_error,
    })
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
    State(state): State<MeetingState>,
) -> ApiResult<Json<MeetingsListResponse>> {
    let limit = params
        .limit
        .unwrap_or(20)
        .clamp(1, crate::sync::protocol::MAX_MEETING_PAGE);
    let offset = params.offset.unwrap_or(0);
    let library = state
        .library
        .as_ref()
        .ok_or_else(|| ApiError::internal("Shared Library unavailable"))?;
    let meetings = library
        .meetings(crate::sync::shared_library::MeetingPageRequest {
            query: params.q,
            offset,
            limit,
        })
        .await
        .map_err(ApiError::from)?;
    Ok(Json(MeetingsListResponse {
        meetings: meetings.into_iter().map(summary_from_library).collect(),
    }))
}

fn summary_from_library(m: crate::sync::shared_library::LibraryMeeting) -> MeetingSummary {
    MeetingSummary {
        id: m.id,
        origin_device_id: m.origin_device_id,
        title: m.title,
        title_source: MeetingTitleSource::from_stored(m.title_source.as_deref()),
        source_filename: m.source_filename,
        status: m.status,
        duration_seconds: m.duration_seconds,
        started_at: m.started_at,
        upload_state: m.upload_state,
        payload_availability: m.payload_availability,
        source: m.access.source().into(),
        offline: m.access.offline(),
        read_only: m.access.read_only(),
    }
}

fn detail_from_library(m: crate::sync::shared_library::LibraryMeeting) -> MeetingDetailResponse {
    MeetingDetailResponse {
        id: m.id,
        origin_device_id: m.origin_device_id,
        title: m.title,
        title_source: MeetingTitleSource::from_stored(m.title_source.as_deref()),
        source_filename: m.source_filename,
        status: m.status,
        transcript_text: m.transcript_text,
        transcript_segments: m.transcript_segments,
        duration_seconds: m.duration_seconds,
        started_at: m.started_at,
        completed_at: m.completed_at,
        error: m.error,
        created_at: m.created_at,
        upload_state: m.upload_state,
        payload_availability: m.payload_availability,
        source: m.access.source().into(),
        offline: m.access.offline(),
        read_only: m.access.read_only(),
    }
}

#[utoipa::path(
    get,
    path = "/meetings/recent-titles",
    tag = "meetings",
    params(RecentMeetingTitlesQuery),
    responses(
        (status = 200, description = "Distinct recent Manual Titles ordered by latest use", body = RecentMeetingTitlesResponse),
    ),
)]
pub async fn recent_meeting_titles(
    Query(params): Query<RecentMeetingTitlesQuery>,
    State(state): State<MeetingState>,
) -> ApiResult<Json<RecentMeetingTitlesResponse>> {
    let limit = params.limit.unwrap_or(10).min(50);
    let titles = state
        .library
        .as_ref()
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?
        .recent_meeting_titles(limit)
        .map_err(ApiError::from)?;
    Ok(Json(RecentMeetingTitlesResponse { titles }))
}

#[utoipa::path(
    patch,
    path = "/meetings/{id}/title",
    tag = "meetings",
    params(("id" = String, Path, description = "Meeting UUID")),
    request_body = MeetingTitleUpdateRequest,
    responses(
        (status = 200, description = "Meeting Title updated with manual ownership", body = MeetingTitleResponse),
        (status = 400, description = "Meeting Title is blank"),
        (status = 404, description = "Meeting not found"),
    ),
)]
pub async fn update_meeting_title(
    Path(id): Path<String>,
    State(state): State<MeetingState>,
    Json(request): Json<MeetingTitleUpdateRequest>,
) -> ApiResult<Json<MeetingTitleResponse>> {
    let record_id = parse_record_id(&id)?;
    let title = request.title.trim().to_string();
    if title.is_empty() {
        return Err(ApiError::bad_request("Meeting Title cannot be blank"));
    }
    let library = state
        .library
        .as_ref()
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    let meeting = library
        .update_meeting_title(record_id, title)
        .await
        .map_err(ApiError::from)?;
    if let Some(local_id) = meeting.local_id {
        state
            .status
            .set_title_if_current(local_id, meeting.title.clone())
            .await;
    }
    Ok(Json(MeetingTitleResponse {
        meeting_id: meeting.meeting_id,
        title: meeting.title,
        title_source: MeetingTitleSource::from_stored(meeting.title_source.as_deref()),
    }))
}

#[utoipa::path(
    post,
    path = "/meetings/{id}/regenerate-title",
    tag = "meetings",
    params(("id" = String, Path, description = "Meeting UUID")),
    responses(
        (status = 202, description = "Title ownership released and regeneration started", body = MeetingTitleRegenerationResponse),
        (status = 404, description = "Meeting not found"),
        (status = 409, description = "Meeting is not completed or has no transcript"),
    ),
)]
pub async fn regenerate_meeting_title(
    Path(id): Path<String>,
    State(state): State<MeetingState>,
) -> ApiResult<(StatusCode, Json<MeetingTitleRegenerationResponse>)> {
    let record_id = parse_record_id(&id)?;
    let library = state
        .library
        .as_ref()
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    let local_id = library
        .regenerate_meeting_title(record_id)
        .await
        .map_err(ApiError::from)?;
    if let Some(local_id) = local_id {
        state.status.set_title_if_current(local_id, None).await;
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(MeetingTitleRegenerationResponse {
            success: true,
            meeting_id: record_id,
            message: if local_id.is_some() {
                "Title regeneration started".to_string()
            } else {
                "Generated title committed to Home Hub".to_string()
            },
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/meetings/{id}",
    tag = "meetings",
    params(
        ("id" = String, Path, description = "Meeting UUID"),
    ),
    responses(
        (status = 200, description = "Meeting detail", body = MeetingDetailResponse),
        (status = 404, description = "Meeting not found"),
    ),
)]
pub async fn get_meeting(
    Path(id): Path<String>,
    State(state): State<MeetingState>,
) -> Result<Json<MeetingDetailResponse>, Response> {
    let record_id = parse_record_id(&id).map_err(IntoResponse::into_response)?;
    state
        .library
        .as_ref()
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable").into_response())?
        .meeting(record_id)
        .await
        .map_err(|error| ApiError::from(error).into_response())
        .map(detail_from_library)
        .map(Json)
}

/// Stream a meeting's audio file for in-browser playback. Used by the review
/// UI so the user can listen back before choosing trim points. Resolves the
/// file actually on disk and honours HTTP Range requests so the `<audio>`
/// element can seek.
#[utoipa::path(
    get,
    path = "/meetings/{id}/audio",
    tag = "meetings",
    params(
        ("id" = String, Path, description = "Meeting UUID"),
    ),
    responses(
        (status = 200, description = "Audio bytes (supports Range)"),
        (status = 404, description = "Meeting or audio file not found"),
    ),
)]
pub async fn meeting_audio(
    Path(id): Path<String>,
    State(state): State<MeetingState>,
    request: axum::extract::Request,
) -> Response {
    let record_id = match parse_record_id(&id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Some(library) = &state.library else {
        return audio_not_found(record_id);
    };
    let range = request
        .headers()
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok());
    match library
        .payload(crate::sync::shared_library::PayloadRequest {
            id: record_id,
            kind: crate::sync::protocol::RecordKind::Meeting,
            range: range.map(str::to_owned),
        })
        .await
    {
        Ok(source) => super::payload::serve(source),
        Err(error) => ApiError::from(error).into_response(),
    }
}

fn audio_not_found(id: RecordId) -> Response {
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
        ("id" = String, Path, description = "Meeting UUID"),
    ),
    responses(
        (status = 202, description = "Retry kicked off; poll /meetings/:id", body = MeetingRetryResponse),
        (status = 404, description = "Meeting not found"),
        (status = 409, description = "Meeting is not in a retry-eligible state, or audio file missing"),
    ),
)]
pub async fn retry_meeting(Path(id): Path<String>, State(state): State<MeetingState>) -> Response {
    info!("Meeting {} retry requested", id);
    let record_id = match parse_record_id(&id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Some(library) = state.library.as_ref() else {
        return ApiError::internal("Shared Library service unavailable").into_response();
    };
    let prepared = match library.prepare_meeting_retry(record_id).await {
        Ok(value) => value,
        Err(error) => {
            error!("Failed to prepare meeting {} retry: {}", id, error);
            return ApiError::from(error).into_response();
        }
    };
    let local_id = prepared.local_id;
    let record_id = prepared.record_id;
    let resolved_path = prepared.audio_path;
    let duration = prepared.duration_seconds;

    let transcription = state.transcription.clone();
    tokio::spawn(async move {
        crate::meeting::retry_meeting_transcription(
            local_id,
            resolved_path,
            duration,
            transcription,
        )
        .await;
    });

    (
        StatusCode::ACCEPTED,
        Json(MeetingRetryResponse {
            success: true,
            meeting_id: record_id,
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
/// If the live status handle still describes this meeting (it keeps the most
/// recent terminal meeting so the UI can show the outcome), it is reset too,
/// so `GET /meetings/status` doesn't keep reporting a deleted meeting.
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
        ("id" = String, Path, description = "Meeting UUID"),
    ),
    responses(
        (status = 200, description = "Meeting deleted (hidden from all views)", body = MeetingDeleteResponse),
        (status = 404, description = "Meeting not found or already deleted"),
        (status = 409, description = "Meeting is still in progress; stop or cancel it first"),
    ),
)]
pub async fn delete_meeting(Path(id): Path<String>, State(state): State<MeetingState>) -> Response {
    info!("Meeting {} delete requested", id);
    let record_id = match parse_record_id(&id) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Some(library) = &state.library else {
        return ApiError::internal("Shared Library service unavailable").into_response();
    };
    match library.delete_meeting(record_id).await {
        Ok(result) => {
            let local_id = result.local_id;
            if let Some(local_id) = local_id {
                if state.status.clear_if_current(local_id).await {
                    info!("Meeting {} cleared from live status after delete", id);
                }
            }
            (
                StatusCode::OK,
                Json(MeetingDeleteResponse {
                    success: true,
                    meeting_id: record_id,
                    message: format!("Meeting {id} deleted"),
                }),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to delete meeting {}: {}", id, e);
            ApiError::from(e).into_response()
        }
    }
}

/// Import a media file as a new meeting.
///
/// Accepts a `multipart/form-data` body with:
/// - `file`: the audio or video bytes (required)
/// - `title`: optional Manual Title; absent or blank imports remain untitled
///   until transcript-derived generation succeeds
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
        Ok(result) => {
            let meeting_id = match sync_id_for_local(&state, result.meeting_id) {
                Ok(id) => id,
                Err(error) => return error_response(error, "resolve meeting UUID"),
            };
            (
                StatusCode::ACCEPTED,
                Json(MeetingImportResponse {
                    success: true,
                    meeting_id,
                    message: "Import accepted; poll /meetings/:id for status".to_string(),
                }),
            )
                .into_response()
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_workflow_statuses_preserve_conflict_and_unavailable_semantics() {
        assert_eq!(
            ApiError::from(crate::sync::shared_library::LibraryError::Conflict(
                "Meeting has no transcript".into(),
            ))
            .into_response()
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            ApiError::from(crate::sync::shared_library::LibraryError::Unavailable(
                "Home Hub unavailable".into(),
            ))
            .into_response()
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn default_status_json_exposes_capture_health() {
        let status = MeetingStatusHandle::default();
        status
            .start_recording(1, None, PathBuf::from("/tmp/meeting.wav"), false, true)
            .await;

        let recording = default_meeting_status_json(&status.get().await, None);
        assert_eq!(recording["capture_degraded"], true);

        status
            .apply_microphone_recovery(crate::audio::capture_recovery::CaptureRecovery::Capturing)
            .await;
        status.mark_system_degraded().await;
        let system_degraded = default_meeting_status_json(&status.get().await, None);
        assert_eq!(system_degraded["capture_degraded"], true);
        status
            .apply_system_recovery(crate::audio::capture_recovery::CaptureRecovery::Capturing)
            .await;
        let recovered = default_meeting_status_json(&status.get().await, None);
        assert_eq!(recovered["capture_degraded"], false);

        status.enter_review(1).await;
        let review = default_meeting_status_json(&status.get().await, None);
        assert_eq!(review["capture_degraded"], false);
    }
}
