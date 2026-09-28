//! AudioNote recording API endpoints. See OpenAPI spec at
//! `/api/openapi.json` for the canonical method/path list.

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

use crate::api::error::{ApiError, ApiResult};
use crate::app::DaemonCommand as ApiCommand;
use crate::audio_notes::{
    import_audio_note_file, AudioNotePhase, AudioNoteStartOptions, AudioNoteStatusHandle,
    ImportArgs, MediaInspector, ProcessingServices,
};

/// Shared state for Audio Note routes.
#[derive(Clone)]
pub struct AudioNoteState {
    /// Explicit configuration path shared by defaults and settings endpoints.
    pub config_path: PathBuf,
    pub tx: mpsc::Sender<ApiCommand>,
    pub status: AudioNoteStatusHandle,
    /// Same transcription service the capture machine uses. Shared so the
    /// retry endpoint re-runs failed audio_notes against the same backend
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
    /// Durable audio_notes directory (`~/.local/share/audetic/audio-notes`).
    /// Uploaded files are staged into a `.uploads` sub-dir, then moved
    /// alongside live recordings on success.
    pub audio_notes_dir: PathBuf,
}

/// Request body for start/toggle endpoints.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AudioNoteStartRequest {
    pub title: Option<String>,
    pub capture_source: crate::audio_notes::AudioNoteCaptureSource,
    pub review_before_processing: bool,
    /// Omitted/null uses the persisted capture preference; false overrides it.
    pub auto_paste: Option<bool>,
    pub copy_to_clipboard: bool,
}

impl AudioNoteStartRequest {
    fn resolve(self, persisted_auto_paste: bool) -> AudioNoteStartOptions {
        AudioNoteStartOptions {
            title: self.title,
            capture_source: self.capture_source,
            review_before_processing: self.review_before_processing,
            auto_paste: self.auto_paste.unwrap_or(persisted_auto_paste),
            copy_to_clipboard: self.copy_to_clipboard,
        }
    }
}

#[allow(clippy::result_large_err)]
fn optional_start_options(
    body: Result<Json<AudioNoteStartRequest>, axum::extract::rejection::JsonRejection>,
    config_path: &std::path::Path,
    starting: bool,
) -> Result<Option<AudioNoteStartOptions>, Response> {
    let request = match body {
        Ok(Json(request)) => request,
        Err(axum::extract::rejection::JsonRejection::MissingJsonContentType(_)) => {
            AudioNoteStartRequest::default()
        }
        Err(error) => return Err(error.into_response()),
    };
    // An explicit request remains usable even if the preferences file cannot
    // be read. Omitted preference must not silently turn configured paste off.
    let default = match request.auto_paste.or((!starting).then_some(false)) {
        Some(value) => value,
        None => {
            crate::audio_notes::settings::load_config(config_path)
                .map_err(|error| ApiError::from(error).into_response())?
                .behavior
                .auto_paste
        }
    };
    Ok(Some(request.resolve(default)))
}

/// Persisted capture defaults. Changing this does not alter an active note.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AudioNoteSettings {
    pub auto_paste: bool,
}

#[utoipa::path(get, path = "/audio-notes/settings", tag = "audio_notes",
    responses((status = 200, description = "Persisted capture defaults", body = AudioNoteSettings),
        (status = 500, description = "Configuration could not be read")))]
pub async fn get_audio_note_settings(
    State(state): State<AudioNoteState>,
) -> ApiResult<Json<AudioNoteSettings>> {
    let config = tokio::task::spawn_blocking(move || {
        crate::audio_notes::settings::load_config(&state.config_path)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(ApiError::from)?;
    Ok(Json(AudioNoteSettings {
        auto_paste: config.behavior.auto_paste,
    }))
}

#[utoipa::path(put, path = "/audio-notes/settings", tag = "audio_notes", request_body = AudioNoteSettings,
    responses((status = 200, description = "Capture defaults saved; effective for the next capture", body = AudioNoteSettings),
        (status = 500, description = "Configuration could not be saved")))]
pub async fn set_audio_note_settings(
    State(state): State<AudioNoteState>,
    Json(settings): Json<AudioNoteSettings>,
) -> ApiResult<Json<AudioNoteSettings>> {
    let auto_paste = settings.auto_paste;
    tokio::task::spawn_blocking(move || {
        crate::audio_notes::settings::save_auto_paste(&state.config_path, auto_paste)
    })
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?
    .map_err(ApiError::from)?;
    Ok(Json(settings))
}

/// Confirmation that capture has begun: the assigned note id,
/// where audio is being written, and capture-source state.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteStartResponse {
    pub success: bool,
    pub note_id: i64,
    pub audio_path: String,
    pub capture_state: String,
    pub message: String,
}

/// Result of ending capture (stop or cancel): the note id and how
/// long it ran.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteStopResponse {
    pub success: bool,
    pub note_id: i64,
    pub duration_seconds: u64,
    pub message: String,
}

/// Result of a capture toggle. Shape varies by whether capture was
/// started or stopped: `audio_path`/`capture_state` appear on start,
/// `duration_seconds` appears on stop, hence the optional fields.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteToggleResponse {
    pub success: bool,
    pub note_id: i64,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u64>,
    pub message: String,
}

/// Default (non-waybar) capture status snapshot. The waybar variant
/// has a different shape — see the union response on the handler.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteStatusResponse {
    pub active: bool,
    pub capture_degraded: bool,
    pub note_id: Option<i64>,
    pub phase: String,
    pub duration_seconds: Option<i64>,
    pub title: Option<String>,
    pub audio_path: Option<String>,
    pub last_error: Option<String>,
}

/// Summary of one Audio Note in a list response — enough to render a row
/// without loading the full transcript.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteSummary {
    pub capture_source: String,
    pub classification: Option<Value>,
    pub enrichment_status: String,
    pub enrichment_error: Option<String>,
    /// Persisted raw transcript, not enriched output.
    pub transcript_text: Option<String>,
    pub id: i64,
    pub title: Option<String>,
    pub title_source: Option<AudioNoteTitleSource>,
    pub source_filename: Option<String>,
    pub status: String,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub audio_path: String,
    pub transcript_path: Option<String>,
}

/// Paginated list of Audio Note summaries.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNotesListResponse {
    pub notes: Vec<AudioNoteSummary>,
}

/// Full Audio Note record including transcript text when available.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteDetailResponse {
    pub capture_source: String,
    pub classification: Option<Value>,
    pub enrichment_status: String,
    pub enrichment_error: Option<String>,
    pub id: i64,
    pub title: Option<String>,
    pub title_source: Option<AudioNoteTitleSource>,
    pub source_filename: Option<String>,
    pub status: String,
    pub audio_path: String,
    pub transcript_path: Option<String>,
    pub transcript_text: Option<String>,
    /// Per-segment timestamps for clickable transcript lines. `None` for
    /// audio_notes transcribed before timestamps were captured.
    pub transcript_segments: Option<Vec<audetic_core::jobs_client::Segment>>,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
}

/// Pagination + filter knobs shared by list and status endpoints.
#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct AudioNotesListQuery {
    pub query: Option<String>,
    pub kind: Option<String>,
    pub offset: Option<usize>,
    /// Maximum audio_notes to return (default 20)
    pub limit: Option<usize>,
}

/// Confirmation that an imported media file has been accepted as a new
/// Audio Note. The processing pipeline runs in the background; clients poll
/// `GET /audio-notes/{id}` for phase progression and the final transcript.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteImportResponse {
    pub success: bool,
    pub note_id: i64,
    pub message: String,
}

/// Streamed multipart upload; original filename determines the media format.
#[derive(ToSchema)]
pub struct AudioNoteImportRequest {
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioNoteTitleSource {
    Manual,
    Generated,
}

impl AudioNoteTitleSource {
    fn from_stored(source: Option<&str>) -> Option<Self> {
        match source {
            Some("manual") => Some(Self::Manual),
            Some("generated") => Some(Self::Generated),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Deserialize, IntoParams)]
pub struct RecentAudioNoteTitlesQuery {
    /// Maximum distinct Manual Titles to return (default 10, maximum 50).
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecentAudioNoteTitlesResponse {
    pub titles: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AudioNoteTitleUpdateRequest {
    /// New non-empty Manual Title.
    pub title: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteTitleResponse {
    pub note_id: i64,
    pub title: Option<String>,
    pub title_source: Option<AudioNoteTitleSource>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteTitleRegenerationResponse {
    pub success: bool,
    pub note_id: i64,
    pub message: String,
}

pub fn router(state: AudioNoteState) -> Router {
    Router::new()
        .route(
            "/audio-notes/settings",
            get(get_audio_note_settings).put(set_audio_note_settings),
        )
        .route("/audio-notes/start", post(start_audio_note))
        .route("/audio-notes/stop", post(stop_audio_note))
        .route("/audio-notes/confirm", post(confirm_audio_note))
        .route("/audio-notes/cancel", post(cancel_audio_note))
        .route("/audio-notes/toggle", post(toggle_audio_note))
        .route("/audio-notes/status", get(audio_note_status))
        .route("/audio-notes/recent-titles", get(recent_audio_note_titles))
        .route("/audio-notes", get(list_audio_notes))
        .route(
            "/audio-notes/import",
            // Disable the global 2 MiB body limit on this route only —
            // meeting recordings and video files run into the hundreds of
            // MB. The multipart extractor below streams chunks to disk so
            // memory usage stays bounded regardless of body size.
            post(import_audio_note).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/audio-notes/:id",
            get(get_audio_note).delete(delete_audio_note),
        )
        .route(
            "/audio-notes/:id/title",
            axum::routing::patch(update_audio_note_title),
        )
        .route(
            "/audio-notes/:id/regenerate-title",
            post(regenerate_audio_note_title),
        )
        .route("/audio-notes/:id/audio", get(audio_note_audio))
        .route("/audio-notes/:id/retry", post(retry_audio_note))
        .route("/audio-notes/:id/process", post(process_audio_note))
        .with_state(state)
}

/// Confirmation that a failed meeting's transcription has been
/// re-queued; the actual work runs in the background.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteRetryResponse {
    pub success: bool,
    pub note_id: i64,
    pub message: String,
}

/// Retry classification and downstream processors without retranscribing or
/// redelivering text. The intelligence layer owns the atomic work claim.
#[utoipa::path(post, path = "/audio-notes/{id}/process", tag = "audio_notes",
    params(("id" = i64, Path, description = "Audio note id")),
    responses((status = 202, description = "Enrichment scheduled", body = AudioNoteRetryResponse),
        (status = 404, description = "Audio note not found"),
        (status = 409, description = "Transcription incomplete or enrichment already running")))]
pub async fn process_audio_note(
    Path(id): Path<i64>,
    State(state): State<AudioNoteState>,
) -> ApiResult<(StatusCode, Json<AudioNoteRetryResponse>)> {
    let db_path = state.services.db_path.clone();
    let note = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        crate::db::audio_notes::AudioNoteRepository::get(&conn, id)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::from)?
    .ok_or_else(|| ApiError::not_found(format!("Audio note {id} not found")))?;
    if note.status != "completed"
        || note
            .transcript_text
            .as_deref()
            .is_none_or(|t| t.trim().is_empty())
        || !matches!(note.enrichment_status.as_str(), "pending" | "error")
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "Audio note is not eligible for enrichment",
        ));
    }
    tokio::spawn(async move {
        if let Err(error) =
            crate::note_intelligence::enrich_audio_note(id, state.services.db_path).await
        {
            warn!(note_id = id, "Enrichment failed: {error:#}");
        }
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(AudioNoteRetryResponse {
            success: true,
            note_id: id,
            message: "Enrichment scheduled".to_string(),
        }),
    ))
}

/// Confirmation that a meeting has been deleted. The delete is *soft*: the
/// meeting is hidden from every API surface but its row and on-disk audio
/// survive.
#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteDeleteResponse {
    pub success: bool,
    pub note_id: i64,
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
    } else if msg.contains("already in progress") || msg.contains("No audio note") {
        // Missing active capture/review and already-active capture are conflicts.
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
    path = "/audio-notes/start",
    tag = "audio_notes",
    request_body = Option<AudioNoteStartRequest>,
    responses(
        (status = 200, description = "AudioNote started", body = AudioNoteStartResponse),
        (status = 409, description = "Capture or review is already in progress"),
    ),
)]
pub async fn start_audio_note(
    State(state): State<AudioNoteState>,
    body: Result<Json<AudioNoteStartRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    info!("AudioNote start command received via API");

    let options = match optional_start_options(body, &state.config_path, true) {
        Ok(options) => options,
        Err(response) => return response,
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::AudioNoteStart {
        options,
        reply: reply_tx,
    };

    match dispatch(&state.tx, reply_rx, command, "start capture").await {
        Ok(result) => Json(AudioNoteStartResponse {
            success: true,
            note_id: result.note_id,
            audio_path: result.audio_path.to_string_lossy().into_owned(),
            capture_state: result.capture_state.tag().to_string(),
            message: format!(
                "AudioNote recording started ({})",
                result.capture_state.as_str()
            ),
        })
        .into_response(),
        Err(resp) => resp,
    }
}

#[utoipa::path(
    post,
    path = "/audio-notes/stop",
    tag = "audio_notes",
    responses(
        (status = 200, description = "Capture stopped; review or transcription begins according to capture options", body = AudioNoteStopResponse),
        (status = 409, description = "No capture in progress"),
    ),
)]
pub async fn stop_audio_note(State(state): State<AudioNoteState>) -> Response {
    info!("AudioNote stop command received via API");

    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::AudioNoteStop { reply: reply_tx };

    match dispatch(&state.tx, reply_rx, command, "stop capture").await {
        Ok(result) => Json(AudioNoteStopResponse {
            success: true,
            note_id: result.note_id,
            duration_seconds: result.duration_seconds,
            message: "Audio note recording stopped".to_string(),
        })
        .into_response(),
        Err(resp) => resp,
    }
}

/// Request body for the confirm endpoint. Both bounds are optional; omitting
/// one keeps that edge of the recording. Both omitted sends it untouched.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct AudioNoteConfirmRequest {
    /// New start of the recording, in seconds (clamped to the recording).
    pub start_seconds: Option<f64>,
    /// New end of the recording, in seconds (clamped to the recording).
    pub end_seconds: Option<f64>,
}

#[utoipa::path(
    post,
    path = "/audio-notes/confirm",
    tag = "audio_notes",
    request_body = Option<AudioNoteConfirmRequest>,
    responses(
        (status = 200, description = "AudioNote confirmed; transcription queued", body = AudioNoteStopResponse),
        (status = 400, description = "Invalid trim range"),
        (status = 409, description = "No Audio Note awaiting review"),
    ),
)]
pub async fn confirm_audio_note(
    State(state): State<AudioNoteState>,
    body: Result<Json<AudioNoteConfirmRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    info!("AudioNote confirm command received via API");

    let (start_seconds, end_seconds) = match body {
        Ok(Json(r)) => (r.start_seconds, r.end_seconds),
        Err(axum::extract::rejection::JsonRejection::MissingJsonContentType(_)) => (None, None),
        Err(error) => return error.into_response(),
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::AudioNoteConfirm {
        start_seconds,
        end_seconds,
        reply: reply_tx,
    };

    match dispatch(&state.tx, reply_rx, command, "confirm capture").await {
        Ok(result) => Json(AudioNoteStopResponse {
            success: true,
            note_id: result.note_id,
            duration_seconds: result.duration_seconds,
            message: "AudioNote confirmed, transcription started in background".to_string(),
        })
        .into_response(),
        Err(resp) => resp,
    }
}

#[utoipa::path(
    post,
    path = "/audio-notes/cancel",
    tag = "audio_notes",
    responses(
        (status = 200, description = "AudioNote cancelled without transcribing", body = AudioNoteStopResponse),
        (status = 409, description = "No capture or review to cancel"),
    ),
)]
pub async fn cancel_audio_note(State(state): State<AudioNoteState>) -> Response {
    info!("AudioNote cancel command received via API");

    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::AudioNoteCancel { reply: reply_tx };

    match dispatch(&state.tx, reply_rx, command, "cancel capture").await {
        Ok(result) => Json(AudioNoteStopResponse {
            success: true,
            note_id: result.note_id,
            duration_seconds: result.duration_seconds,
            message: "AudioNote recording cancelled".to_string(),
        })
        .into_response(),
        Err(resp) => resp,
    }
}

#[utoipa::path(
    post,
    path = "/audio-notes/toggle",
    tag = "audio_notes",
    request_body = Option<AudioNoteStartRequest>,
    responses(
        (status = 200, description = "AudioNote started or stopped", body = AudioNoteToggleResponse),
    ),
)]
pub async fn toggle_audio_note(
    State(state): State<AudioNoteState>,
    body: Result<Json<AudioNoteStartRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    info!("AudioNote toggle command received via API");

    let starting = state.status.get().await.phase != AudioNotePhase::Recording;
    let options = match optional_start_options(body, &state.config_path, starting) {
        Ok(options) => options,
        Err(response) => return response,
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::AudioNoteToggle {
        options,
        reply: reply_tx,
    };

    match dispatch(&state.tx, reply_rx, command, "toggle capture").await {
        Ok(outcome) => match outcome {
            crate::audio_notes::ToggleOutcome::Started(r) => Json(AudioNoteToggleResponse {
                success: true,
                note_id: r.note_id,
                phase: "recording".to_string(),
                audio_path: Some(r.audio_path.to_string_lossy().into_owned()),
                capture_state: Some(r.capture_state.tag().to_string()),
                duration_seconds: None,
                message: format!("AudioNote recording started ({})", r.capture_state.as_str()),
            })
            .into_response(),
            crate::audio_notes::ToggleOutcome::Stopped(r) => Json(AudioNoteToggleResponse {
                success: true,
                note_id: r.note_id,
                phase: state.status.get().await.phase.as_str().to_string(),
                audio_path: None,
                capture_state: None,
                duration_seconds: Some(r.duration_seconds),
                message: "Audio note recording stopped".to_string(),
            })
            .into_response(),
        },
        Err(resp) => resp,
    }
}

#[utoipa::path(
    get,
    path = "/audio-notes/status",
    tag = "audio_notes",
    params(
        ("style" = Option<String>, Query, description = "Set to `waybar` for Waybar-formatted response"),
    ),
    responses(
        (status = 200, description = "AudioNote status (default JSON shape)", body = AudioNoteStatusResponse),
    ),
)]
pub async fn audio_note_status(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AudioNoteState>,
) -> Json<Value> {
    let status = state.status.get().await;
    let is_active = status.phase == AudioNotePhase::Recording;

    // Waybar style response
    if params.get("style") == Some(&"waybar".to_string()) {
        let (text, class, tooltip) = if is_active {
            let duration = status.duration_seconds().unwrap_or(0);
            let minutes = duration / 60;
            let seconds = duration % 60;
            (
                "\u{f0d6b}".to_string(),
                "audetic-note".to_string(),
                format!("AudioNote recording: {:02}:{:02}", minutes, seconds),
            )
        } else {
            (
                String::new(),
                "audetic-note-idle".to_string(),
                "No active capture".to_string(),
            )
        };

        return Json(json!({
            "text": text,
            "class": class,
            "tooltip": tooltip,
        }));
    }

    Json(default_audio_note_status_json(&status))
}

fn default_audio_note_status_json(status: &crate::audio_notes::AudioNoteState) -> Value {
    json!({
        "active": status.phase == AudioNotePhase::Recording,
        "capture_degraded": status.capture_degraded,
        "note_id": status.note_id,
        "phase": status.phase.as_str(),
        "duration_seconds": status.duration_seconds(),
        "title": status.title,
        "audio_path": status.audio_path.as_ref().map(|p| p.to_string_lossy().to_string()),
        "last_error": status.last_error,
    })
}

#[utoipa::path(
    get,
    path = "/audio-notes",
    tag = "audio_notes",
    params(AudioNotesListQuery),
    responses(
        (status = 200, description = "Recent audio_notes, newest first", body = AudioNotesListResponse),
    ),
)]
pub async fn list_audio_notes(
    Query(params): Query<AudioNotesListQuery>,
    State(state): State<AudioNoteState>,
) -> Result<Json<AudioNotesListResponse>, StatusCode> {
    let limit = params.limit.unwrap_or(20).min(200);
    let db_path = state.services.db_path.clone();

    let audio_notes = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        crate::db::audio_notes::AudioNoteRepository::search(
            &conn,
            params.query.as_deref(),
            params.kind.as_deref(),
            limit,
            params.offset.unwrap_or(0),
        )
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let entries: Vec<AudioNoteSummary> = audio_notes
        .into_iter()
        .map(|m| AudioNoteSummary {
            capture_source: m.capture_source,
            classification: m.classification,
            enrichment_status: m.enrichment_status,
            enrichment_error: m.enrichment_error,
            transcript_text: m.transcript_text,
            id: m.id,
            title: m.title,
            title_source: AudioNoteTitleSource::from_stored(m.title_source.as_deref()),
            source_filename: m.source_filename,
            status: m.status,
            duration_seconds: m.duration_seconds,
            started_at: m.started_at,
            audio_path: m.audio_path,
            transcript_path: m.transcript_path,
        })
        .collect();

    Ok(Json(AudioNotesListResponse { notes: entries }))
}

#[utoipa::path(
    get,
    path = "/audio-notes/recent-titles",
    tag = "audio_notes",
    params(RecentAudioNoteTitlesQuery),
    responses(
        (status = 200, description = "Distinct recent Manual Titles ordered by latest use", body = RecentAudioNoteTitlesResponse),
    ),
)]
pub async fn recent_audio_note_titles(
    Query(params): Query<RecentAudioNoteTitlesQuery>,
    State(state): State<AudioNoteState>,
) -> ApiResult<Json<RecentAudioNoteTitlesResponse>> {
    let limit = params.limit.unwrap_or(10).min(50);
    let db_path = state.services.db_path.clone();
    let titles = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        crate::db::audio_notes::AudioNoteRepository::recent_manual_titles(&conn, limit)
    })
    .await
    .map_err(|error| ApiError::internal(format!("db task panicked: {error}")))?
    .map_err(ApiError::from)?;
    Ok(Json(RecentAudioNoteTitlesResponse { titles }))
}

#[utoipa::path(
    patch,
    path = "/audio-notes/{id}/title",
    tag = "audio_notes",
    params(("id" = i64, Path, description = "AudioNote id")),
    request_body = AudioNoteTitleUpdateRequest,
    responses(
        (status = 200, description = "AudioNote Title updated with manual ownership", body = AudioNoteTitleResponse),
        (status = 400, description = "AudioNote Title is blank"),
        (status = 404, description = "AudioNote not found"),
    ),
)]
pub async fn update_audio_note_title(
    Path(id): Path<i64>,
    State(state): State<AudioNoteState>,
    Json(request): Json<AudioNoteTitleUpdateRequest>,
) -> ApiResult<Json<AudioNoteTitleResponse>> {
    let title = request.title.trim().to_string();
    if title.is_empty() {
        return Err(ApiError::bad_request("AudioNote Title cannot be blank"));
    }
    let db_path = state.services.db_path.clone();
    let meeting = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let conn = crate::db::init_db_at(&db_path)?;
        if !crate::db::audio_notes::AudioNoteRepository::set_manual_title(&conn, id, &title)? {
            return Ok(None);
        }
        crate::db::audio_notes::AudioNoteRepository::get(&conn, id)
    })
    .await
    .map_err(|error| ApiError::internal(format!("db task panicked: {error}")))?
    .map_err(ApiError::from)?
    .ok_or_else(|| ApiError::not_found(format!("AudioNote {id} not found")))?;

    state
        .status
        .set_title_if_current(id, meeting.title.clone())
        .await;
    Ok(Json(AudioNoteTitleResponse {
        note_id: id,
        title: meeting.title,
        title_source: AudioNoteTitleSource::from_stored(meeting.title_source.as_deref()),
    }))
}

#[utoipa::path(
    post,
    path = "/audio-notes/{id}/regenerate-title",
    tag = "audio_notes",
    params(("id" = i64, Path, description = "AudioNote id")),
    responses(
        (status = 202, description = "Title ownership released and regeneration started", body = AudioNoteTitleRegenerationResponse),
        (status = 404, description = "AudioNote not found"),
        (status = 409, description = "AudioNote is not completed or has no transcript"),
    ),
)]
pub async fn regenerate_audio_note_title(
    Path(id): Path<i64>,
    State(state): State<AudioNoteState>,
) -> ApiResult<(StatusCode, Json<AudioNoteTitleRegenerationResponse>)> {
    let db_path = state.services.db_path.clone();
    tokio::task::spawn_blocking(move || {
        crate::audio_notes::title::prepare_title_regeneration(id, &db_path)
    })
    .await
    .map_err(|error| ApiError::internal(format!("db task panicked: {error}")))?
    .map_err(|error| {
        let message = error.to_string();
        if message.contains("not found") {
            ApiError::not_found(message)
        } else {
            ApiError::new(StatusCode::CONFLICT, message)
        }
    })?;
    state.status.set_title_if_current(id, None).await;
    crate::audio_notes::title::spawn_title_generation_at(id, state.services.db_path.clone());
    Ok((
        StatusCode::ACCEPTED,
        Json(AudioNoteTitleRegenerationResponse {
            success: true,
            note_id: id,
            message: "Title regeneration started".to_string(),
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/audio-notes/{id}",
    tag = "audio_notes",
    params(
        ("id" = i64, Path, description = "AudioNote id"),
    ),
    responses(
        (status = 200, description = "AudioNote detail", body = AudioNoteDetailResponse),
        (status = 404, description = "AudioNote not found"),
    ),
)]
pub async fn get_audio_note(
    Path(id): Path<i64>,
    State(state): State<AudioNoteState>,
) -> Result<Json<AudioNoteDetailResponse>, Response> {
    let db_path = state.services.db_path.clone();
    let meeting = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        crate::db::audio_notes::AudioNoteRepository::get(&conn, id)
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
        Some(m) => Ok(Json(AudioNoteDetailResponse {
            capture_source: m.capture_source,
            classification: m.classification,
            enrichment_status: m.enrichment_status,
            enrichment_error: m.enrichment_error,
            id: m.id,
            title: m.title,
            title_source: AudioNoteTitleSource::from_stored(m.title_source.as_deref()),
            source_filename: m.source_filename,
            status: m.status,
            audio_path: m.audio_path,
            transcript_path: m.transcript_path,
            transcript_text: m.transcript_text,
            transcript_segments: m.transcript_segments,
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
                "message": format!("AudioNote {} not found", id),
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
    path = "/audio-notes/{id}/audio",
    tag = "audio_notes",
    params(
        ("id" = i64, Path, description = "AudioNote id"),
    ),
    responses(
        (status = 200, description = "Audio bytes (supports Range)"),
        (status = 404, description = "AudioNote or audio file not found"),
    ),
)]
pub async fn audio_note_audio(
    Path(id): Path<i64>,
    State(state): State<AudioNoteState>,
    request: axum::extract::Request,
) -> Response {
    let db_path = state.services.db_path.clone();
    let lookup = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        crate::db::audio_notes::AudioNoteRepository::get(&conn, id)
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
            "message": format!("Audio for note {} not found", id),
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
    path = "/audio-notes/{id}/retry",
    tag = "audio_notes",
    params(
        ("id" = i64, Path, description = "AudioNote id"),
    ),
    responses(
        (status = 202, description = "Retry kicked off; poll /audio-notes/:id", body = AudioNoteRetryResponse),
        (status = 404, description = "AudioNote not found"),
        (status = 409, description = "AudioNote is not in a retry-eligible state, or audio file missing"),
    ),
)]
pub async fn retry_audio_note(
    Path(id): Path<i64>,
    State(state): State<AudioNoteState>,
) -> Response {
    info!("AudioNote {} retry requested", id);
    let db_path = state.services.db_path.clone();
    let join = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        crate::db::audio_notes::AudioNoteRepository::get(&conn, id)
    })
    .await;

    let meeting = match join {
        Ok(Ok(Some(m))) => m,
        Ok(Ok(None)) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "success": false,
                    "message": format!("AudioNote {} not found", id),
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
    if meeting.status != AudioNotePhase::Error.as_str() {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "message": format!(
                    "AudioNote {} is in state '{}'; only failed audio_notes can be retried",
                    id, meeting.status
                ),
            })),
        )
            .into_response();
    }

    // Resolve the file actually on disk. Older audio_notes (before we kept the
    // DB row in sync with the WAV → MP3 compression swap) have a stale
    // `.wav` path; the durable mp3 next to it is what we actually want.
    let stored_path = std::path::PathBuf::from(&meeting.audio_path);
    let resolved_path = if stored_path.exists() {
        stored_path
    } else {
        let mp3_sibling = stored_path.with_extension("mp3");
        if mp3_sibling.exists() {
            info!(
                "AudioNote {} stored path missing; using mp3 sibling: {:?}",
                id, mp3_sibling
            );
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

    // Atomically flip error → transcribing *before* returning 202. The status
    // check above and the spawned task's own transition leave a window where
    // the row is still `error`; a DELETE arriving then would treat this
    // already-accepted retry as a terminal, deletable meeting and hide it. Do
    // the transition here (last, after the file-missing 409s, so a bail can't
    // strand the row in `transcribing`) and reject if the row is no longer the
    // failed meeting we loaded — e.g. a concurrent retry or delete won.
    let db_path = state.services.db_path.clone();
    let marked = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        crate::db::audio_notes::AudioNoteRepository::begin_retry(&conn, id)
    })
    .await;

    match marked {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "success": false,
                    "message": format!(
                        "AudioNote {} is no longer eligible for retry; its state changed",
                        id
                    ),
                })),
            )
                .into_response();
        }
        Ok(Err(e)) => {
            error!("Failed to mark meeting {} retry in-flight: {}", id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": e.to_string() })),
            )
                .into_response();
        }
        Err(e) => {
            error!("DB task panicked marking meeting {} retry: {}", id, e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "success": false, "message": "db task panicked" })),
            )
                .into_response();
        }
    }

    let duration = meeting.duration_seconds.unwrap_or(0);
    let services = state.services.clone();
    tokio::spawn(async move {
        crate::audio_notes::process_audio_note(crate::audio_notes::ProcessingArgs {
            note_id: id,
            audio_path: resolved_path,
            duration_seconds: duration.max(0) as u64,
            services,
            observer: Arc::new(crate::audio_notes::NoopProgressObserver),
            delivery: Default::default(),
        })
        .await;
    });

    (
        StatusCode::ACCEPTED,
        Json(AudioNoteRetryResponse {
            success: true,
            note_id: id,
            message: "Retry started; poll /audio-notes/:id for status".to_string(),
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
/// so `GET /audio-notes/status` doesn't keep reporting a deleted meeting.
///
/// In-flight audio_notes (recording / review / processing) are refused with 409:
/// their id is still owned by the meeting machine and background pipeline, so
/// hiding the row would 404 the active/review UI and break completion
/// auto-nav. Stop or cancel the meeting first. Returns 404 if the meeting
/// doesn't exist or was already deleted.
#[utoipa::path(
    delete,
    path = "/audio-notes/{id}",
    tag = "audio_notes",
    params(
        ("id" = i64, Path, description = "AudioNote id"),
    ),
    responses(
        (status = 200, description = "AudioNote deleted (hidden from all views)", body = AudioNoteDeleteResponse),
        (status = 404, description = "AudioNote not found or already deleted"),
        (status = 409, description = "AudioNote is still in progress; stop or cancel it first"),
    ),
)]
pub async fn delete_audio_note(
    Path(id): Path<i64>,
    State(state): State<AudioNoteState>,
) -> Response {
    info!("AudioNote {} delete requested", id);
    let db_path = state.services.db_path.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        crate::db::audio_notes::AudioNoteRepository::soft_delete(&conn, id)
    })
    .await;

    use crate::db::audio_notes::SoftDeleteOutcome;
    match outcome {
        Ok(Ok(SoftDeleteOutcome::Deleted)) => {
            // The DB row is hidden, but the most recent meeting also lives on
            // in the shared status handle (terminal phases keep id/title/error
            // there so the UI can show the outcome). Clear it if it still
            // points at this meeting, or GET /audio-notes/status would keep
            // reporting the deleted meeting until the next recording starts.
            if state.status.clear_if_current(id).await {
                info!("AudioNote {} cleared from live status after delete", id);
            }
            (
                StatusCode::OK,
                Json(AudioNoteDeleteResponse {
                    success: true,
                    note_id: id,
                    message: format!("AudioNote {id} deleted"),
                }),
            )
                .into_response()
        }
        Ok(Ok(SoftDeleteOutcome::NotFound)) => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "success": false,
                "message": format!("AudioNote {id} not found"),
            })),
        )
            .into_response(),
        Ok(Ok(SoftDeleteOutcome::InFlight)) => (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "message": format!(
                    "AudioNote {id} is still in progress; stop or cancel it before deleting"
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
/// - `title`: optional Manual Title; absent or blank imports remain untitled
///   until transcript-derived generation succeeds
///
/// The file is streamed chunk-by-chunk into a temp file under the audio_notes
/// directory, then handed to `meeting::import_audio_note_file`, which moves
/// it into place, inserts the DB row, and spawns the processing pipeline.
/// Returns 202 with the new meeting id; clients poll `GET /audio-notes/{id}`
/// for status. The response intentionally omits the storage path —
/// callers shouldn't depend on the filesystem layout.
#[utoipa::path(
    post,
    path = "/audio-notes/import",
    tag = "audio_notes",
    request_body(
        content = AudioNoteImportRequest,
        content_type = "multipart/form-data",
        description = "File upload with optional title",
    ),
    responses(
        (status = 202, description = "Import accepted; poll /audio-notes/:id", body = AudioNoteImportResponse),
        (status = 400, description = "Missing file part or unsupported extension"),
        (status = 500, description = "Failed to stage upload or persist Audio Note"),
    ),
)]
pub async fn import_audio_note(
    State(state): State<AudioNoteState>,
    mut multipart: Multipart,
) -> Response {
    info!("AudioNote import command received via API");

    let uploads_dir = state.audio_notes_dir.join(".uploads");
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
        audio_notes_dir: state.audio_notes_dir.clone(),
    };

    match import_audio_note_file(args).await {
        Ok(result) => (
            StatusCode::ACCEPTED,
            Json(AudioNoteImportResponse {
                success: true,
                note_id: result.note_id,
                message: "Import accepted; poll /audio-notes/:id for status".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            // import_audio_note_file cleans up its own destination file on
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
    error!("AudioNote import failed ({}): {}", status, message);
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
    use crate::db::audio_notes::AudioNoteRepository;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };

    struct UnusedTranscription;
    #[async_trait::async_trait]
    impl crate::transcription::job_service::TranscriptionJobService for UnusedTranscription {
        async fn submit_and_poll(
            &self,
            _: &std::path::Path,
            _: Option<&str>,
        ) -> anyhow::Result<crate::transcription::job_service::TranscriptionJobResult> {
            anyhow::bail!("not used by API read tests")
        }
    }

    fn fixture() -> (tempfile::TempDir, Router, i64, i64) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("isolated.db");
        let audio = dir.path().join("note.mp3");
        std::fs::write(&audio, b"0123456789").unwrap();
        let conn = crate::db::init_db_at(&db_path).unwrap();
        let first =
            AudioNoteRepository::insert(&conn, Some("Shopping"), &audio.to_string_lossy()).unwrap();
        AudioNoteRepository::complete(&conn, first, "note.txt", "buy apples", None, 3).unwrap();
        AudioNoteRepository::set_classification(&conn, first, &json!({"kind":"shopping-list"}))
            .unwrap();
        let active =
            AudioNoteRepository::insert(&conn, Some("Current"), &audio.to_string_lossy()).unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let transcription = Arc::new(UnusedTranscription);
        let state = AudioNoteState {
            config_path: dir.path().join("config.toml"),
            tx,
            status: Default::default(),
            transcription: transcription.clone(),
            services: ProcessingServices::new(transcription, db_path),
            inspector: Arc::new(crate::audio_notes::FfprobeMediaInspector),
            audio_notes_dir: dir.path().to_path_buf(),
        };
        (dir, router(state), first, active)
    }

    async fn json_response(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn settings_persist_and_preserve_other_configuration() {
        let (dir, router, _, _) = fixture();
        let response = router
            .clone()
            .oneshot(
                Request::get("/audio-notes/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_response(response).await, json!({"auto_paste": false}));
        let config_path = dir.path().join("config.toml");
        let mut config = crate::config::Config::default();
        config.whisper.provider = Some("openai-api".into());
        config.behavior.audio_feedback = false;
        std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
        let response = router
            .clone()
            .oneshot(
                Request::put("/audio-notes/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"auto_paste":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_response(response).await, json!({"auto_paste": true}));
        let saved = crate::audio_notes::settings::load_config(&config_path).unwrap();
        assert!(saved.behavior.auto_paste);
        assert!(!saved.behavior.audio_feedback);
        assert_eq!(saved.whisper.provider.as_deref(), Some("openai-api"));
        let response = router
            .clone()
            .oneshot(
                Request::get("/audio-notes/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_response(response).await, json!({"auto_paste": true}));
        let response = router
            .oneshot(
                Request::put("/audio-notes/settings")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            crate::audio_notes::settings::load_config(&config_path)
                .unwrap()
                .behavior
                .auto_paste
        );
    }

    #[tokio::test]
    async fn start_and_toggle_resolve_saved_defaults_and_respect_explicit_false() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        crate::audio_notes::settings::save_auto_paste(&config_path, true).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let transcription = Arc::new(UnusedTranscription);
        let status = AudioNoteStatusHandle::default();
        let state = AudioNoteState {
            config_path: config_path.clone(),
            tx,
            status: status.clone(),
            transcription: transcription.clone(),
            services: ProcessingServices::new(transcription, directory.path().join("isolated.db")),
            inspector: Arc::new(crate::audio_notes::FfprobeMediaInspector),
            audio_notes_dir: directory.path().to_path_buf(),
        };
        let app = router(state);
        for (endpoint, body, expected) in [
            ("start", None, true),
            ("toggle", Some(r#"{"auto_paste":false}"#), false),
            ("start", Some("{}"), true),
        ] {
            let app = app.clone();
            let mut request = Request::post(format!("/audio-notes/{endpoint}"));
            if body.is_some() {
                request = request.header("content-type", "application/json");
            }
            let request = request
                .body(body.map_or_else(Body::empty, Body::from))
                .unwrap();
            let response = tokio::spawn(async move { app.oneshot(request).await.unwrap() });
            match rx.recv().await.unwrap() {
                ApiCommand::AudioNoteStart { options, reply } => {
                    assert_eq!(options.unwrap().auto_paste, expected);
                    reply
                        .send(Ok(crate::audio_notes::AudioNoteStartResult {
                            note_id: 42,
                            audio_path: directory.path().join("note.wav"),
                            capture_state: crate::audio_notes::CaptureState::MicOnly,
                        }))
                        .unwrap();
                }
                ApiCommand::AudioNoteToggle { options, reply } => {
                    assert_eq!(options.unwrap().auto_paste, expected);
                    assert!(reply
                        .send(Ok(crate::audio_notes::ToggleOutcome::Started(
                            crate::audio_notes::AudioNoteStartResult {
                                note_id: 42,
                                audio_path: directory.path().join("note.wav"),
                                capture_state: crate::audio_notes::CaptureState::MicOnly
                            }
                        )))
                        .is_ok());
                }
                _ => panic!("unexpected daemon command"),
            }
            assert_eq!(response.await.unwrap().status(), StatusCode::OK);
        }
        status
            .start_recording(99, None, directory.path().join("active.wav"), true, false)
            .await;
        let response = app
            .clone()
            .oneshot(
                Request::put("/audio-notes/settings")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"auto_paste":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(status.get().await.phase, AudioNotePhase::Recording);
        assert_eq!(status.get().await.note_id, Some(99));
        std::fs::write(&config_path, "broken toml [").unwrap();
        let request = tokio::spawn(async move {
            app.oneshot(
                Request::post("/audio-notes/toggle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        });
        match rx.recv().await.unwrap() {
            ApiCommand::AudioNoteToggle { reply, .. } => {
                assert!(reply
                    .send(Ok(crate::audio_notes::ToggleOutcome::Stopped(
                        crate::audio_notes::AudioNoteStopResult {
                            note_id: 99,
                            duration_seconds: 1
                        }
                    )))
                    .is_ok());
            }
            _ => panic!("expected toggle"),
        }
        assert_eq!(request.await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_queries_classification_and_exposes_raw_transcript_and_enrichment() {
        let (_dir, router, first, _) = fixture();
        let response = router
            .oneshot(
                Request::get("/audio-notes?query=apples&kind=shopping-list&limit=10&offset=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_response(response).await;
        assert!(body.get("audio_notes").is_none());
        assert_eq!(body["notes"].as_array().unwrap().len(), 1);
        assert_eq!(body["notes"][0]["id"], first);
        assert_eq!(body["notes"][0]["transcript_text"], "buy apples");
        assert_eq!(body["notes"][0]["classification"]["kind"], "shopping-list");
        assert_eq!(body["notes"][0]["capture_source"], "microphone");
        assert_eq!(body["notes"][0]["enrichment_status"], "pending");
        assert!(body["notes"][0].get("enrichment_error").is_some());
    }

    #[tokio::test]
    async fn audio_preserves_ranges_and_delete_hides_detail_and_playback() {
        let (_dir, router, first, active) = fixture();
        let response = router
            .clone()
            .oneshot(
                Request::get(format!("/audio-notes/{first}/audio"))
                    .header("range", "bytes=2-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(to_bytes(response.into_body(), 100).await.unwrap(), "2345");
        let response = router
            .clone()
            .oneshot(
                Request::delete(format!("/audio-notes/{active}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let response = router
            .clone()
            .oneshot(
                Request::delete(format!("/audio-notes/{first}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        for suffix in ["", "/audio"] {
            let response = router
                .clone()
                .oneshot(
                    Request::get(format!("/audio-notes/{first}{suffix}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn pagination_and_invalid_ids_are_handled() {
        let (_dir, router, first, _) = fixture();
        let response = router
            .clone()
            .oneshot(
                Request::get("/audio-notes?limit=1&offset=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_response(response).await["notes"][0]["id"], first);
        let response = router
            .oneshot(
                Request::get("/audio-notes/not-an-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn malformed_capture_options_never_silently_start_a_default_capture() {
        let (_dir, router, _, _) = fixture();
        let response = router
            .oneshot(
                Request::post("/audio-notes/start")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"capture_source":"import"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn default_status_json_exposes_capture_health() {
        let status = AudioNoteStatusHandle::default();
        status
            .start_recording(1, None, PathBuf::from("/tmp/meeting.wav"), false, true)
            .await;

        let recording = default_audio_note_status_json(&status.get().await);
        assert_eq!(recording["capture_degraded"], true);

        status
            .apply_microphone_recovery(crate::audio::capture_recovery::CaptureRecovery::Capturing)
            .await;
        status.mark_system_degraded().await;
        let system_degraded = default_audio_note_status_json(&status.get().await);
        assert_eq!(system_degraded["capture_degraded"], true);
        status
            .apply_system_recovery(crate::audio::capture_recovery::CaptureRecovery::Capturing)
            .await;
        let recovered = default_audio_note_status_json(&status.get().await);
        assert_eq!(recovered["capture_degraded"], false);

        status.enter_review(1).await;
        let review = default_audio_note_status_json(&status.get().await);
        assert_eq!(review["capture_degraded"], false);
    }
}
