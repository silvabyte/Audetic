//! REST API server for Audetic.
//!
//! Provides HTTP endpoints for:
//! - Recording control (toggle, status)
//! - Transcription history
//! - Keybinding management
//! - Provider configuration
//! - Update management
//! - Application logs

pub mod error;
pub mod routes;

use crate::audio::{JobOptions, RecordingPhase, RecordingStatus, RecordingStatusHandle};
use crate::config::{Config, WaybarConfig};
use anyhow::Result;
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
use tower::ServiceBuilder;
use tracing::{error, info};

/// Request body for the toggle recording endpoint.
/// All fields are optional - if not provided, defaults are used from config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToggleRequest {
    /// Whether to copy the transcription to clipboard (default: true)
    #[serde(default)]
    pub copy_to_clipboard: Option<bool>,
    /// Whether to auto-paste/inject text into the focused app (default: from config)
    #[serde(default)]
    pub auto_paste: Option<bool>,
}

#[derive(Clone)]
pub enum ApiCommand {
    /// Toggle recording with optional per-job options
    ToggleRecording(Option<JobOptions>),
}

#[derive(Clone)]
pub struct AppState {
    tx: mpsc::Sender<ApiCommand>,
    status: RecordingStatusHandle,
    waybar_config: WaybarConfig,
}

pub struct ApiServer {
    port: u16,
    state: AppState,
}

impl ApiServer {
    pub fn new(
        tx: mpsc::Sender<ApiCommand>,
        status: RecordingStatusHandle,
        config: &Config,
    ) -> Self {
        Self {
            port: 3737, // WHSP in numbers
            state: AppState {
                tx,
                status,
                waybar_config: config.ui.waybar.clone(),
            },
        }
    }

    pub async fn start(self) -> Result<()> {
        let app = Router::new()
            // Root and recording endpoints (with state)
            .route("/", get(status))
            .route("/toggle", post(toggle_recording))
            .route("/status", get(recording_status))
            .with_state(self.state)
            // Stateless API routes
            .nest("/history", routes::history::router())
            .nest("/keybind", routes::keybind::router())
            .nest("/logs", routes::logs::router())
            .nest("/provider", routes::provider::router())
            .nest("/update", routes::update::router())
            .route("/version", get(version))
            .layer(ServiceBuilder::new());

        let listener = tokio::net::TcpListener::bind(&format!("127.0.0.1:{}", self.port)).await?;

        info!("API server listening on http://127.0.0.1:{}", self.port);
        info!("Endpoints:");
        info!("  GET  /              - Service info");
        info!("  POST /toggle        - Toggle recording");
        info!("  GET  /status        - Get recording status");
        info!("  GET  /version       - Get version info");
        info!("  GET  /history       - List transcription history");
        info!("  GET  /history/:id   - Get single transcription");
        info!("  GET  /keybind/status - Get keybinding status");
        info!("  POST /keybind/install - Install keybinding");
        info!("  DELETE /keybind     - Uninstall keybinding");
        info!("  GET  /logs          - Get application logs");
        info!("  GET  /provider      - Get provider config");
        info!("  GET  /provider/status - Get provider status");
        info!("  GET  /update/check  - Check for updates");
        info!("  POST /update/install - Install update");
        info!("  PUT  /update/auto   - Toggle auto-update");

        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn status() -> Json<Value> {
    Json(json!({
        "service": "audetic",
        "version": env!("CARGO_PKG_VERSION"),
        "status": "running"
    }))
}

async fn version() -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "name": "audetic"
    }))
}

async fn toggle_recording(
    State(state): State<AppState>,
    body: Option<Json<ToggleRequest>>,
) -> Result<Json<Value>, StatusCode> {
    // Parse optional job options from request body
    let job_options = body.and_then(|Json(req)| {
        // Only create JobOptions if at least one field was specified
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
            // Small delay to allow the status to be updated
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            // Get the current status to return job information
            let status = state.status.get().await;

            Ok(Json(json!({
                "success": true,
                "phase": status.phase.as_str(),
                "job_id": status.current_job_id,
                "message": format!("Recording {}", status.phase.as_str())
            })))
        }
        Err(e) => {
            error!("Failed to send toggle command: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

async fn recording_status(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Json<Value> {
    let status = state.status.get().await;

    // Check if waybar style is requested
    if params.get("style") == Some(&"waybar".to_string()) {
        return Json(generate_waybar_response(&status, &state.waybar_config));
    }

    // Build last_completed_job object if available
    let last_completed_job = status.last_completed_job.as_ref().map(|job| {
        json!({
            "job_id": job.job_id,
            "history_id": job.history_id,
            "text": job.text,
            "created_at": job.created_at
        })
    });

    // Default JSON response with full job context
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
