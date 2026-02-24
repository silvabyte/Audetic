//! Meeting recording API endpoints.
//!
//! Provides HTTP endpoints for:
//! - Starting meeting recording (POST /meetings/start)
//! - Stopping meeting recording (POST /meetings/stop)
//! - Toggling meeting recording (POST /meetings/toggle)
//! - Getting meeting status (GET /meetings/status)
//! - Listing meetings (GET /meetings)
//! - Getting a specific meeting (GET /meetings/:id)

use crate::meeting::{MeetingPhase, MeetingStartOptions, MeetingStatusHandle};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::mpsc;
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
        .route("/meetings/toggle", post(toggle_meeting))
        .route("/meetings/status", get(meeting_status))
        .route("/meetings", get(list_meetings))
        .route("/meetings/:id", get(get_meeting))
        .with_state(state)
}

async fn start_meeting(
    State(state): State<MeetingState>,
    body: Option<Json<MeetingStartRequest>>,
) -> Result<Json<Value>, StatusCode> {
    let options = body.map(|Json(req)| MeetingStartOptions { title: req.title });

    info!("Meeting start command received via API");

    match state.tx.send(ApiCommand::MeetingStart(options)).await {
        Ok(_) => {
            // Wait a bit for the machine to process
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            let status = state.status.get().await;
            Ok(Json(json!({
                "success": true,
                "meeting_id": status.meeting_id,
                "message": "Meeting recording started",
                "audio_path": status.audio_path.map(|p| p.to_string_lossy().to_string()),
            })))
        }
        Err(e) => {
            error!("Failed to send meeting start command: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn stop_meeting(
    State(state): State<MeetingState>,
) -> Result<Json<Value>, StatusCode> {
    info!("Meeting stop command received via API");

    match state.tx.send(ApiCommand::MeetingStop).await {
        Ok(_) => {
            // Wait a bit for the machine to process
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            let status = state.status.get().await;
            Ok(Json(json!({
                "success": true,
                "meeting_id": status.meeting_id,
                "message": "Meeting recording stopped, transcription started",
                "duration_seconds": status.duration_seconds(),
            })))
        }
        Err(e) => {
            error!("Failed to send meeting stop command: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn toggle_meeting(
    State(state): State<MeetingState>,
    body: Option<Json<MeetingStartRequest>>,
) -> Result<Json<Value>, StatusCode> {
    let options = body.map(|Json(req)| MeetingStartOptions { title: req.title });

    info!("Meeting toggle command received via API");

    match state.tx.send(ApiCommand::MeetingToggle(options)).await {
        Ok(_) => {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

            let status = state.status.get().await;
            let is_recording = status.phase == MeetingPhase::Recording;

            Ok(Json(json!({
                "success": true,
                "meeting_id": status.meeting_id,
                "phase": status.phase.as_str(),
                "message": if is_recording {
                    "Meeting recording started"
                } else {
                    "Meeting recording stopped, transcription started"
                },
                "duration_seconds": status.duration_seconds(),
                "audio_path": status.audio_path.map(|p| p.to_string_lossy().to_string()),
            })))
        }
        Err(e) => {
            error!("Failed to send meeting toggle command: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
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
) -> Result<Json<Value>, StatusCode> {
    let meeting = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        crate::db::meetings::MeetingRepository::get(&conn, id)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
        None => Err(StatusCode::NOT_FOUND),
    }
}
