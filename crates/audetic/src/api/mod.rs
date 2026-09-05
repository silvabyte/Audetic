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

// The API URL surface lives in `audetic-core` so the CLI can build daemon URLs
// without depending on the daemon. Re-exported here as `crate::api::url`.
pub use audetic_core::url;

use anyhow::Result;
use axum::{
    extract::Request,
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::get,
    Extension, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::info;
use utoipa::{OpenApi, ToSchema};

use std::future::Future;

use crate::config::Config;
use crate::post_processing::PostProcessingService;

pub use crate::app::DaemonCommand as ApiCommand;
pub use routes::recording::{RecordingState, ToggleRequest};

/// Response for GET / — service identity and basic status.
#[derive(Debug, Serialize, ToSchema)]
pub struct ServiceInfo {
    pub service: String,
    pub version: String,
    pub status: String,
}

/// Response for GET /version.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct VersionInfo {
    pub name: String,
    pub version: String,
    /// Random identity generated once for this daemon process.
    pub instance_id: String,
}

#[derive(Clone)]
pub(crate) struct ProcessInstanceId(String);

pub struct ApiServer {
    port: u16,
    recording_state: RecordingState,
    meeting_state: Option<routes::meetings::MeetingState>,
    post_processing_state: routes::post_processing::PostProcessingApiState,
    runtime_provider: crate::config::WhisperConfig,
    instance_id: ProcessInstanceId,
    sync_state: Option<routes::sync::SyncApiState>,
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
            runtime_provider: config.whisper.clone(),
            instance_id: ProcessInstanceId(uuid::Uuid::new_v4().to_string()),
            sync_state: None,
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
        let db_path = post_processing.db_path().to_path_buf();
        let services = crate::meeting::ProcessingServices::new(
            transcription.clone(),
            post_processing,
            db_path,
        );
        self.meeting_state = Some(routes::meetings::MeetingState {
            tx: self.recording_state.tx.clone(),
            status: meeting_status,
            transcription,
            services,
            inspector,
            meetings_dir,
            library: None,
        });
        self
    }

    pub fn with_sync_service(mut self, service: std::sync::Arc<crate::sync::SyncService>) -> Self {
        if let Some(meeting_state) = self.meeting_state.as_mut() {
            meeting_state.library = Some(std::sync::Arc::new(service.library()));
        }
        self.sync_state = Some(routes::sync::SyncApiState::new(service));
        self
    }

    pub async fn start(self) -> Result<()> {
        self.start_with_shutdown(std::future::pending::<()>()).await
    }

    pub async fn start_with_shutdown(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<()> {
        // Build the API surface. All routes nest under `/api` so the daemon
        // can serve the bundled web-ui at `/` without colliding with API
        // paths (e.g. /meetings is also a SPA route).
        let shared_library = self
            .sync_state
            .as_ref()
            .map(|state| std::sync::Arc::new(state.service.library()));
        let mut api = Router::new()
            .route("/", get(status))
            .route("/version", get(version))
            .route("/openapi.json", get(openapi_spec))
            .nest("", routes::recording::router(self.recording_state))
            .nest("/history", routes::history::router(shared_library.clone()))
            .nest("/keybind", routes::keybind::router())
            .nest("/logs", routes::logs::router())
            .nest("/models", routes::models::router())
            .nest(
                "/provider",
                routes::provider::router(routes::provider::ProviderApiState::new(
                    self.runtime_provider.clone(),
                )),
            )
            .nest("/system", routes::system::router())
            .merge(routes::setup::router(routes::setup::SetupApiState::new(
                self.runtime_provider.clone(),
            )))
            .merge(routes::transcribe::router())
            .merge(routes::agents::router())
            .merge(routes::summary_templates::router())
            .merge(routes::meeting_artifacts::router(shared_library))
            .merge(routes::post_processing::router(self.post_processing_state))
            .layer(Extension(self.instance_id));

        let has_meeting = self.meeting_state.is_some();
        if let Some(meeting_state) = self.meeting_state {
            api = api.merge(routes::meetings::router(meeting_state));
        }
        if let Some(sync_state) = self.sync_state {
            api = api.merge(routes::sync::router(sync_state));
        }

        let app = Router::new()
            .nest(url::API_PREFIX, api)
            .fallback(static_assets::serve_static)
            .layer(cors_layer())
            .layer(middleware::from_fn(reject_disallowed_origin));

        let listener =
            tokio::net::TcpListener::bind(&format!("{}:{}", url::HOST, self.port)).await?;

        info!("API server listening on http://{}:{}", url::HOST, self.port);
        info!("API spec: {}", url::api_url("/openapi.json"));
        info!(
            "Meeting endpoints {}",
            if has_meeting { "enabled" } else { "disabled" }
        );

        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await?;

        Ok(())
    }
}

/// Same-origin production requests need no CORS grant. The explicit allowlist
/// exists for the separately served Vite dev/preview UI; requests without an
/// Origin header (including the standalone CLI) continue through unchanged.
fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            is_allowed_browser_origin(origin)
        }))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::ACCEPT, header::AUTHORIZATION, header::CONTENT_TYPE])
}

fn is_allowed_browser_origin(origin: &HeaderValue) -> bool {
    const HOSTS: &[&str] = &["localhost", "127.0.0.1"];
    const PORTS: &[u16] = &[3737, 4173, 5173, 5174, 5175];

    origin.to_str().ok().is_some_and(|origin| {
        HOSTS.iter().any(|host| {
            PORTS
                .iter()
                .any(|port| origin == format!("http://{host}:{port}"))
        })
    })
}

/// CORS headers are a browser response policy, not request authorization.
/// Reject browser requests from untrusted origins before handlers can perform
/// mutations, while preserving origin-less CLI and curl access.
async fn reject_disallowed_origin(request: Request, next: Next) -> Response {
    if request
        .headers()
        .get(header::ORIGIN)
        .is_some_and(|origin| !is_allowed_browser_origin(origin))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
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
pub(crate) async fn version(
    Extension(instance_id): Extension<ProcessInstanceId>,
) -> Json<VersionInfo> {
    Json(version_info(&instance_id.0))
}

fn version_info(instance_id: &str) -> VersionInfo {
    VersionInfo {
        name: "audetic".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        instance_id: instance_id.to_string(),
    }
}

/// Serve the OpenAPI 3.x document for the daemon's HTTP API.
async fn openapi_spec() -> Json<Value> {
    let spec = docs::ApiDoc::openapi();
    Json(serde_json::to_value(spec).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::post};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tower::ServiceExt;

    #[test]
    fn cors_only_allows_local_production_and_vite_origins() {
        for origin in [
            "http://127.0.0.1:3737",
            "http://localhost:3737",
            "http://127.0.0.1:4173",
            "http://localhost:5173",
            "http://localhost:5175",
        ] {
            assert!(is_allowed_browser_origin(
                &HeaderValue::from_str(origin).unwrap()
            ));
        }
        for origin in [
            "https://example.com",
            "http://localhost:3000",
            "http://127.0.0.1.evil.test:5173",
        ] {
            assert!(!is_allowed_browser_origin(
                &HeaderValue::from_str(origin).unwrap()
            ));
        }
    }

    #[tokio::test]
    async fn cors_does_not_block_clients_without_an_origin_header() {
        let app = Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(cors_layer())
            .layer(middleware::from_fn(reject_disallowed_origin));
        let response = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert!(response.status().is_success());
    }

    #[tokio::test]
    async fn cors_answers_allowed_vite_preflight() {
        let app = Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(cors_layer())
            .layer(middleware::from_fn(reject_disallowed_origin));
        let response = app
            .oneshot(
                Request::options("/")
                    .header(header::ORIGIN, "http://localhost:5173")
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.status().is_success());
        assert_eq!(
            response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some(&HeaderValue::from_static("http://localhost:5173"))
        );
    }

    #[tokio::test]
    async fn hostile_origin_is_rejected_before_post_handler_runs() {
        let reached = Arc::new(AtomicBool::new(false));
        let handler_reached = Arc::clone(&reached);
        let app = Router::new()
            .route(
                "/",
                post(move || {
                    let handler_reached = Arc::clone(&handler_reached);
                    async move {
                        handler_reached.store(true, Ordering::SeqCst);
                        "mutated"
                    }
                }),
            )
            .layer(cors_layer())
            .layer(middleware::from_fn(reject_disallowed_origin));

        let response = app
            .oneshot(
                Request::post("/")
                    .header(header::ORIGIN, "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!reached.load(Ordering::SeqCst));
    }

    #[test]
    fn version_info_carries_the_process_instance_identity() {
        let first = version_info("process-one");
        let same_process = version_info("process-one");
        let restarted = version_info("process-two");

        assert_eq!(first.instance_id, same_process.instance_id);
        assert_ne!(first.instance_id, restarted.instance_id);
    }
}
