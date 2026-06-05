//! Recording (dictation) control endpoints. See OpenAPI spec at
//! `/api/openapi.json` for the canonical method/path list — don't
//! enumerate them here.

use crate::audio::{JobOptions, RecordingPhase, RecordingStatus, RecordingStatusHandle};
use crate::config::WaybarConfig;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{error, info};
use utoipa::ToSchema;

/// Request body for the toggle recording endpoint.
/// All fields are optional - if not provided, defaults are used from config.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ToggleRequest {
    /// Whether to copy the transcription to clipboard (default: true)
    #[serde(default)]
    pub copy_to_clipboard: Option<bool>,
    /// Whether to auto-paste/inject text into the focused app (default: from config)
    #[serde(default)]
    pub auto_paste: Option<bool>,
}

/// Result of toggling recording: lifecycle phase, the job id when one
/// is being processed, and a human-readable status message.
#[derive(Debug, Serialize, ToSchema)]
pub struct ToggleResponse {
    pub success: bool,
    pub phase: String,
    pub job_id: Option<String>,
    pub message: String,
}

/// The `last_completed_job` nested block inside `RecordingStatusResponse`.
#[derive(Debug, Serialize, ToSchema)]
pub struct CompletedJobSummary {
    pub job_id: String,
    pub history_id: Option<i64>,
    pub text: String,
    pub created_at: String,
}

/// Default (non-waybar) recording status snapshot. The waybar variant
/// is a different shape — see the union response on the handler.
#[derive(Debug, Serialize, ToSchema)]
pub struct RecordingStatusResponse {
    pub recording: bool,
    pub phase: String,
    pub job_id: Option<String>,
    pub last_completed_job: Option<CompletedJobSummary>,
    pub last_error: Option<String>,
}

/// Commands dispatched from the HTTP layer to the main event loop.
///
/// `ToggleRecording` is fire-and-forget — the recording pipeline is driven by
/// a keybinding where immediate return is a feature, and errors self-correct
/// on the next keypress.
///
/// The meeting variants carry a `tokio::sync::oneshot` reply channel so the
/// HTTP handler can `.await` the machine's actual `Result` and surface proper
/// status codes / error messages to the CLI.
pub enum ApiCommand {
    /// Toggle recording with optional per-job options
    ToggleRecording(Option<JobOptions>),
    /// Start meeting recording
    MeetingStart {
        options: Option<crate::meeting::MeetingStartOptions>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::MeetingStartResult>>,
    },
    /// Stop meeting recording
    MeetingStop {
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::MeetingStopResult>>,
    },
    /// Cancel the in-progress meeting recording without transcribing
    MeetingCancel {
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::MeetingStopResult>>,
    },
    /// Confirm a meeting awaiting review, optionally trimming `[start, end)`
    /// seconds, then send it for transcription.
    MeetingConfirm {
        start_seconds: Option<f64>,
        end_seconds: Option<f64>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::MeetingStopResult>>,
    },
    /// Toggle meeting recording
    MeetingToggle {
        options: Option<crate::meeting::MeetingStartOptions>,
        reply: tokio::sync::oneshot::Sender<anyhow::Result<crate::meeting::ToggleOutcome>>,
    },
}

#[derive(Clone)]
pub struct RecordingState {
    pub tx: mpsc::Sender<ApiCommand>,
    pub status: RecordingStatusHandle,
    pub waybar_config: WaybarConfig,
}

/// Creates the recording router with all recording-related endpoints.
pub fn router(state: RecordingState) -> Router {
    Router::new()
        .route("/toggle", post(toggle_recording))
        .route("/status", get(recording_status))
        .with_state(state)
}

/// Toggles recording on or off with optional per-job options.
#[utoipa::path(
    post,
    path = "/toggle",
    tag = "recording",
    request_body(content = ToggleRequest, description = "Optional per-job overrides"),
    responses(
        (status = 200, description = "Toggle dispatched; reflects immediate phase", body = ToggleResponse),
    ),
)]
pub async fn toggle_recording(
    State(state): State<RecordingState>,
    body: Option<Json<ToggleRequest>>,
) -> Result<Json<ToggleResponse>, StatusCode> {
    let job_options = body.and_then(|Json(req)| {
        if req.copy_to_clipboard.is_some() || req.auto_paste.is_some() {
            Some(JobOptions {
                copy_to_clipboard: req.copy_to_clipboard.unwrap_or(true),
                auto_paste: req.auto_paste.unwrap_or(true),
            })
        } else {
            None
        }
    });

    info!(
        "Toggle recording command received via API with options: {:?}",
        job_options
    );

    match state
        .tx
        .send(ApiCommand::ToggleRecording(job_options))
        .await
    {
        Ok(_) => {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            let status = state.status.get().await;

            Ok(Json(ToggleResponse {
                success: true,
                phase: status.phase.as_str().to_string(),
                job_id: status.current_job_id.clone(),
                message: format!("Recording {}", status.phase.as_str()),
            }))
        }
        Err(e) => {
            error!("Failed to send toggle command: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Gets the current recording status.
///
/// Pass `?style=waybar` for a Waybar-formatted `{text, class, tooltip}` payload.
#[utoipa::path(
    get,
    path = "/status",
    tag = "recording",
    params(
        ("style" = Option<String>, Query, description = "Set to `waybar` for Waybar-formatted response"),
    ),
    responses(
        (status = 200, description = "Recording status (default JSON shape)", body = RecordingStatusResponse),
    ),
)]
pub async fn recording_status(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<RecordingState>,
) -> Json<Value> {
    let status = state.status.get().await;

    if params.get("style") == Some(&"waybar".to_string()) {
        return Json(generate_waybar_response(&status, &state.waybar_config));
    }

    let last_completed_job = status.last_completed_job.as_ref().map(|job| {
        json!({
            "job_id": job.job_id,
            "history_id": job.history_id,
            "text": job.text,
            "created_at": job.created_at
        })
    });

    Json(json!({
        "recording": status.phase == RecordingPhase::Recording,
        "phase": status.phase.as_str(),
        "job_id": status.current_job_id,
        "last_completed_job": last_completed_job,
        "last_error": status.last_error,
    }))
}

fn generate_waybar_response(status: &RecordingStatus, config: &WaybarConfig) -> Value {
    let (text, class, tooltip) = match status.phase {
        RecordingPhase::Idle => (
            config.idle_text.clone(),
            "audetic-idle".to_string(),
            config.idle_tooltip.clone(),
        ),
        RecordingPhase::Recording => (
            config.recording_text.clone(),
            "audetic-recording".to_string(),
            config.recording_tooltip.clone(),
        ),
        RecordingPhase::Processing => (
            "󰦖".to_string(),
            "audetic-processing".to_string(),
            "Processing transcription".to_string(),
        ),
        RecordingPhase::Error => (
            "".to_string(),
            "audetic-error".to_string(),
            status
                .last_error
                .clone()
                .unwrap_or_else(|| "Recording error".to_string()),
        ),
    };

    json!({
        "text": text,
        "class": class,
        "tooltip": tooltip
    })
}
