//! Meeting recording API endpoints.
//!
//! Provides HTTP endpoints for:
//! - Starting meeting recording (POST /meetings/start)
//! - Stopping meeting recording (POST /meetings/stop)
//! - Cancelling meeting recording (POST /meetings/cancel)
//! - Toggling meeting recording (POST /meetings/toggle)
//! - Getting meeting status (GET /meetings/status)
//! - Listing meetings (GET /meetings)
//! - Getting a specific meeting (GET /meetings/:id)

use crate::meeting::{MeetingPhase, MeetingStartOptions, MeetingStatusHandle};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

use super::recording::ApiCommand;

/// Shared state for meeting routes.
#[derive(Clone)]
pub struct MeetingState {
    pub tx: mpsc::Sender<ApiCommand>,
    pub status: MeetingStatusHandle,
}

/// Request body for start/toggle endpoints.
#[derive(Debug, Default, serde::Deserialize)]
pub struct MeetingStartRequest {
    pub title: Option<String>,
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
        .with_state(state)
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

/// Helper: send an ApiCommand and await the machine's reply. Returns a 500
/// if the dispatch loop has hung up.
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

async fn start_meeting(
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
        Ok(result) => Json(json!({
            "success": true,
            "meeting_id": result.meeting_id,
            "audio_path": result.audio_path.to_string_lossy(),
            "capture_state": result.capture_state.as_str(),
            "message": format!("Meeting recording started ({})", result.capture_state.as_str()),
        }))
        .into_response(),
        Err(resp) => resp,
    }
}

async fn stop_meeting(State(state): State<MeetingState>) -> Response {
    info!("Meeting stop command received via API");

    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::MeetingStop { reply: reply_tx };

    match dispatch(&state.tx, reply_rx, command, "stop meeting").await {
        Ok(result) => Json(json!({
            "success": true,
            "meeting_id": result.meeting_id,
            "duration_seconds": result.duration_seconds,
            "message": "Meeting recording stopped, transcription started in background",
        }))
        .into_response(),
        Err(resp) => resp,
    }
}

async fn cancel_meeting(State(state): State<MeetingState>) -> Response {
    info!("Meeting cancel command received via API");

    let (reply_tx, reply_rx) = oneshot::channel();
    let command = ApiCommand::MeetingCancel { reply: reply_tx };

    match dispatch(&state.tx, reply_rx, command, "cancel meeting").await {
        Ok(result) => Json(json!({
            "success": true,
            "meeting_id": result.meeting_id,
            "duration_seconds": result.duration_seconds,
            "message": "Meeting recording cancelled",
        }))
        .into_response(),
        Err(resp) => resp,
    }
}

async fn toggle_meeting(
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
            crate::meeting::ToggleOutcome::Started(r) => Json(json!({
                "success": true,
                "meeting_id": r.meeting_id,
                "phase": "recording",
                "audio_path": r.audio_path.to_string_lossy(),
                "capture_state": r.capture_state.as_str(),
                "message": format!("Meeting recording started ({})", r.capture_state.as_str()),
            }))
            .into_response(),
            crate::meeting::ToggleOutcome::Stopped(r) => Json(json!({
                "success": true,
                "meeting_id": r.meeting_id,
                "phase": "compressing",
                "duration_seconds": r.duration_seconds,
                "message": "Meeting recording stopped, transcription started in background",
            }))
            .into_response(),
        },
        Err(resp) => resp,
    }
}

async fn meeting_status(
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
                "\u{f0d6b}".to_string(), // 󰍫 meeting icon
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

async fn list_meetings(
    Query(params): Query<HashMap<String, String>>,
    State(_state): State<MeetingState>,
) -> Result<Json<Value>, StatusCode> {
    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let meetings = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        crate::db::meetings::MeetingRepository::list(&conn, limit)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let entries: Vec<Value> = meetings
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "title": m.title,
                "status": m.status,
                "duration_seconds": m.duration_seconds,
                "started_at": m.started_at,
                "audio_path": m.audio_path,
                "transcript_path": m.transcript_path,
            })
        })
        .collect();

    Ok(Json(json!({ "meetings": entries })))
}

async fn get_meeting(
    Path(id): Path<i64>,
    State(_state): State<MeetingState>,
) -> Result<Json<Value>, Response> {
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
        Some(m) => Ok(Json(json!({
            "id": m.id,
            "title": m.title,
            "status": m.status,
            "audio_path": m.audio_path,
            "transcript_path": m.transcript_path,
            "transcript_text": m.transcript_text,
            "duration_seconds": m.duration_seconds,
            "started_at": m.started_at,
            "completed_at": m.completed_at,
            "error": m.error,
            "created_at": m.created_at,
        }))),
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
