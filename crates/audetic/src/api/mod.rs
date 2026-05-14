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
pub mod static_assets;
pub mod url;

use crate::config::Config;
use crate::post_processing::PostProcessingService;
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
    post_processing_state: routes::post_processing::PostProcessingApiState,
}

impl ApiServer {
    pub fn new(
        tx: tokio::sync::mpsc::Sender<ApiCommand>,
        status: crate::audio::RecordingStatusHandle,
        config: &Config,
        post_processing: std::sync::Arc<PostProcessingService>,
    ) -> Self {
        Self {
            port: url::DEFAULT_PORT,
            recording_state: RecordingState {
                tx,
                status,
                waybar_config: config.ui.waybar.clone(),
            },
            meeting_state: None,
            post_processing_state: routes::post_processing::PostProcessingApiState {
                service: post_processing,
            },
        }
    }

    pub fn with_meeting_state(
        mut self,
        meeting_status: crate::meeting::MeetingStatusHandle,
        transcription: std::sync::Arc<
            dyn crate::transcription::job_service::TranscriptionJobService,
        >,
        post_processing: std::sync::Arc<PostProcessingService>,
        inspector: std::sync::Arc<dyn crate::meeting::MediaInspector>,
        meetings_dir: std::path::PathBuf,
    ) -> Self {
        let services = crate::meeting::ProcessingServices {
            transcription: transcription.clone(),
            post_processing,
        };
        self.meeting_state = Some(routes::meetings::MeetingState {
            tx: self.recording_state.tx.clone(),
            status: meeting_status,
            transcription,
            services,
            inspector,
            meetings_dir,
        });
        self
    }

    pub async fn start(self) -> Result<()> {
        // Build the API surface. All routes nest under `/api` so the daemon
        // can serve the bundled web-ui at `/` without colliding with API
        // paths (e.g. /meetings is also a SPA route).
        let mut api = Router::new()
            .route("/", get(status))
            .route("/version", get(version))
            .route("/openapi.json", get(openapi_spec))
            .nest("", routes::recording::router(self.recording_state))
            .nest("/history", routes::history::router())
            .nest("/keybind", routes::keybind::router())
            .nest("/logs", routes::logs::router())
            .nest("/provider", routes::provider::router())
            .nest("/system", routes::system::router())
            .nest("/update", routes::update::router())
            .merge(routes::post_processing::router(self.post_processing_state));

        let has_meeting = self.meeting_state.is_some();
        if let Some(meeting_state) = self.meeting_state {
            api = api.merge(routes::meetings::router(meeting_state));
        }

        // Permissive CORS is safe here: the server binds to 127.0.0.1 only, so
        // the only callers that can reach it are already on this machine.
        // In production the SPA is same-origin (served from `/`); CORS only
        // matters for `bun run dev` against a separately-running daemon.
        let app = Router::new()
            .nest(url::API_PREFIX, api)
            .fallback(static_assets::serve_static)
            .layer(ServiceBuilder::new().layer(CorsLayer::permissive()));

        let listener =
            tokio::net::TcpListener::bind(&format!("{}:{}", url::HOST, self.port)).await?;

        info!("API server listening on http://{}:{}", url::HOST, self.port);
        info!("API spec: {}", url::api_url("/openapi.json"));
        info!(
            "Meeting endpoints {}",
            if has_meeting { "enabled" } else { "disabled" }
        );

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
