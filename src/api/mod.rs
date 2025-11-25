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

use crate::audio::{RecordingPhase, RecordingStatus, RecordingStatusHandle};
use crate::config::{Config, WaybarConfig};
use anyhow::Result;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tower::ServiceBuilder;
use tracing::{error, info};

#[derive(Clone)]
pub enum ApiCommand {
    ToggleRecording,
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

async fn toggle_recording(State(state): State<AppState>) -> Result<Json<Value>, StatusCode> {
    match state.tx.send(ApiCommand::ToggleRecording).await {
        Ok(_) => {
            info!("Toggle recording command received via API");
            Ok(Json(json!({
                "success": true,
                "message": "Recording toggled"
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

    // Default JSON response
    Json(json!({
        "recording": status.phase == RecordingPhase::Recording,
        "phase": status.phase.as_str(),
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
