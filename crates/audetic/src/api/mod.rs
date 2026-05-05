//! REST API server for Audetic.
//!
//! Provides HTTP endpoints for:
//! - Recording control (toggle, status)
//! - Transcription history
//! - Keybinding management
//! - Provider configuration
//! - Update management
//! - Application logs
//! - OpenAPI spec (/openapi.json)

pub mod docs;
pub mod error;
pub mod routes;

use crate::config::Config;
use anyhow::Result;
use axum::{response::Json, routing::get, Router};
use serde::Serialize;
use serde_json::Value;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tracing::info;
use utoipa::{OpenApi, ToSchema};

pub use routes::recording::{ApiCommand, RecordingState, ToggleRequest};

/// Response for GET / — service identity and basic status.
#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceInfo {
    pub service: String,
    pub version: String,
    pub status: String,
}

/// Response for GET /version.
#[derive(Debug, Serialize, ToSchema)]
pub struct VersionInfo {
    pub name: String,
    pub version: String,
}

pub struct ApiServer {
    port: u16,
    recording_state: RecordingState,
    meeting_state: Option<routes::meetings::MeetingState>,
}

impl ApiServer {
    pub fn new(
        tx: tokio::sync::mpsc::Sender<ApiCommand>,
        status: crate::audio::RecordingStatusHandle,
        config: &Config,
    ) -> Self {
        Self {
            port: 3737, // WHSP in numbers
            recording_state: RecordingState {
                tx,
                status,
                waybar_config: config.ui.waybar.clone(),
            },
            meeting_state: None,
        }
    }

    pub fn with_meeting_state(
        mut self,
        meeting_status: crate::meeting::MeetingStatusHandle,
        transcription: std::sync::Arc<
            dyn crate::transcription::job_service::TranscriptionJobService,
        >,
    ) -> Self {
        self.meeting_state = Some(routes::meetings::MeetingState {
            tx: self.recording_state.tx.clone(),
            status: meeting_status,
            transcription,
        });
        self
    }

    pub async fn start(self) -> Result<()> {
        let mut app = Router::new()
            // Root and version endpoints
            .route("/", get(status))
            .route("/version", get(version))
            // OpenAPI spec
            .route("/openapi.json", get(openapi_spec))
            // Recording control endpoints
            .nest("", routes::recording::router(self.recording_state))
            // Other API routes
            .nest("/history", routes::history::router())
            .nest("/keybind", routes::keybind::router())
            .nest("/logs", routes::logs::router())
            .nest("/provider", routes::provider::router())
            .nest("/system", routes::system::router())
            .nest("/update", routes::update::router());

        // Meeting routes (optional — only if meeting state is wired)
        let has_meeting = self.meeting_state.is_some();
        if let Some(meeting_state) = self.meeting_state {
            app = app.merge(routes::meetings::router(meeting_state));
        }

        // Permissive CORS is safe here: the server binds to 127.0.0.1 only, so
        // the only callers that can reach it are already on this machine. The
        // Electron UI (and any future dev tool) needs CORS to fetch from its
        // dev-server origin.
        let app = app.layer(ServiceBuilder::new().layer(CorsLayer::permissive()));

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
        info!("  GET  /system/deps   - Report external tool availability");
        info!("  POST /system/install-ffmpeg        - Install bundled FFmpeg");
        info!("  GET  /system/install-ffmpeg/status - Poll FFmpeg install state");
        info!("  GET  /update/check  - Check for updates");
        info!("  POST /update/install - Install update");
        info!("  PUT  /update/auto   - Toggle auto-update");
        if has_meeting {
            info!("  POST /meetings/start  - Start meeting recording");
            info!("  POST /meetings/stop   - Stop meeting recording");
            info!("  POST /meetings/toggle - Toggle meeting recording");
            info!("  GET  /meetings/status - Meeting recording status");
            info!("  GET  /meetings        - List meetings");
            info!("  GET  /meetings/:id    - Get meeting details");
        }

        axum::serve(listener, app).await?;

        Ok(())
    }
}

#[utoipa::path(
    get,
    path = "/",
    tag = "service",
    operation_id = "service_status",
    responses(
        (status = 200, description = "Service identity and liveness", body = ServiceInfo),
    ),
)]
pub async fn status() -> Json<ServiceInfo> {
    Json(ServiceInfo {
        service: "audetic".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        status: "running".to_string(),
    })
}

#[utoipa::path(
    get,
    path = "/version",
    tag = "service",
    operation_id = "service_version",
    responses(
        (status = 200, description = "Daemon name and version", body = VersionInfo),
    ),
)]
pub async fn version() -> Json<VersionInfo> {
    Json(VersionInfo {
        name: "audetic".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Serve the OpenAPI 3.x document for the daemon's HTTP API.
async fn openapi_spec() -> Json<Value> {
    let spec = docs::ApiDoc::openapi();
    Json(serde_json::to_value(spec).unwrap_or(Value::Null))
}
