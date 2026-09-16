//! Authenticated, loopback-only HTTP transport for Hub synchronization.

use audetic_core::sync::{TransportPairRequest, TransportPairResponse, TransportStatusResponse};
use axum::extract::{Extension, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::net::TcpListener;
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::db::sync::AuthenticatedDevice;

use super::{SyncError, SyncResult, SyncService};

pub const TRANSPORT_HOST: &str = audetic_core::url::HOST;
pub const TRANSPORT_PORT: u16 = audetic_core::url::SYNC_TRANSPORT_PORT;
pub const TRANSPORT_PAIR_PATH: &str = audetic_core::url::sync_transport_paths::PAIR;
pub const TRANSPORT_STATUS_PATH: &str = audetic_core::url::sync_transport_paths::STATUS;

pub fn transport_bind_addr() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), TRANSPORT_PORT)
}

#[derive(Clone)]
pub struct TransportServer {
    service: SyncService,
}

impl TransportServer {
    pub fn new(service: SyncService) -> Self {
        Self { service }
    }

    pub fn router(&self) -> Router {
        transport_router(self.service.clone())
    }

    pub async fn bind(&self) -> SyncResult<TcpListener> {
        TcpListener::bind(transport_bind_addr())
            .await
            .map_err(|_| SyncError::Internal)
    }

    pub async fn serve(self) -> SyncResult<()> {
        let listener = self.bind().await?;
        self.serve_listener(listener).await
    }

    pub async fn serve_listener(self, listener: TcpListener) -> SyncResult<()> {
        let address = listener.local_addr().map_err(|_| SyncError::Internal)?;
        if !address.ip().is_loopback() {
            return Err(SyncError::BadRequest(
                "sync transport listeners must use a loopback address",
            ));
        }
        axum::serve(listener, self.router())
            .await
            .map_err(|_| SyncError::Internal)
    }
}

fn transport_router(service: SyncService) -> Router {
    Router::new()
        .route(TRANSPORT_PAIR_PATH, post(pair))
        .route(TRANSPORT_STATUS_PATH, get(status))
        .route_layer(middleware::from_fn_with_state(
            service.clone(),
            authenticate,
        ))
        .with_state(service)
}

async fn authenticate(
    State(service): State<SyncService>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(credential) = bearer_credential(request.headers().get(header::AUTHORIZATION)) else {
        return unauthorized_response();
    };
    match service.authenticate_transport(credential).await {
        Ok(device) => {
            request.extensions_mut().insert(device);
            next.run(request).await
        }
        Err(SyncError::Unauthorized) => unauthorized_response(),
        Err(error) => error.into_response(),
    }
}

fn bearer_credential(value: Option<&HeaderValue>) -> Option<String> {
    let value = value?.to_str().ok()?;
    let (scheme, credential) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer")
        || credential.is_empty()
        || credential.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(credential.to_string())
}

#[utoipa::path(
    post,
    path = "/api/sync/v1/pair",
    tag = "sync_transport",
    operation_id = "transport_pair",
    security(("bearer_auth" = [])),
    request_body = TransportPairRequest,
    responses(
        (status = 200, description = "Credential bound to this Client Node", body = TransportPairResponse),
        (status = 400, description = "Invalid Client node identity", body = TransportErrorResponse),
        (status = 401, description = "Missing or invalid bearer credential", body = TransportErrorResponse),
        (status = 409, description = "Credential is already bound to another Client Node", body = TransportErrorResponse),
        (status = 500, description = "Pairing could not be persisted", body = TransportErrorResponse),
    ),
)]
async fn pair(
    State(service): State<SyncService>,
    Extension(authenticated): Extension<AuthenticatedDevice>,
    Json(request): Json<TransportPairRequest>,
) -> Result<Json<TransportPairResponse>, SyncError> {
    service
        .bind_transport(authenticated, request)
        .await
        .map(Json)
}

#[utoipa::path(
    get,
    path = "/api/sync/v1/status",
    tag = "sync_transport",
    operation_id = "transport_status",
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Bound transport identity and protocol version", body = TransportStatusResponse),
        (status = 401, description = "Missing or invalid bearer credential", body = TransportErrorResponse),
        (status = 409, description = "Credential has not been paired yet", body = TransportErrorResponse),
        (status = 500, description = "Transport status could not be read", body = TransportErrorResponse),
    ),
)]
async fn status(
    State(service): State<SyncService>,
    Extension(authenticated): Extension<AuthenticatedDevice>,
) -> Result<Json<TransportStatusResponse>, SyncError> {
    service.transport_status(authenticated).map(Json)
}

#[derive(Debug, Serialize, ToSchema)]
struct TransportErrorResponse {
    error: &'static str,
}

impl IntoResponse for SyncError {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => unauthorized_response(),
            Self::BadRequest(_) => transport_error(StatusCode::BAD_REQUEST, "bad_request"),
            Self::RoleConflict { .. } | Self::PairingConflict => {
                transport_error(StatusCode::CONFLICT, "conflict")
            }
            Self::NotFound => transport_error(StatusCode::NOT_FOUND, "not_found"),
            Self::RemoteUnavailable => {
                transport_error(StatusCode::SERVICE_UNAVAILABLE, "remote_unavailable")
            }
            Self::Internal => transport_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        }
    }
}

fn unauthorized_response() -> Response {
    let mut response = transport_error(StatusCode::UNAUTHORIZED, "unauthorized");
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn transport_error(status: StatusCode, error: &'static str) -> Response {
    (status, Json(TransportErrorResponse { error })).into_response()
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Audetic sync transport API",
        description = "Authenticated loopback transport used only for Audetic synchronization.",
        version = env!("CARGO_PKG_VERSION"),
    ),
    servers(
        (url = "http://127.0.0.1:3738", description = "Local Sync Hub transport"),
    ),
    paths(pair, status),
    components(
        schemas(
            TransportPairRequest,
            TransportPairResponse,
            TransportStatusResponse,
            TransportErrorResponse,
        ),
    ),
    modifiers(&TransportSecurity),
    tags(
        (name = "sync_transport", description = "Authenticated Hub pairing transport"),
    ),
)]
pub struct TransportApiDoc;

struct TransportSecurity;

impl Modify for TransportSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use audetic_core::config::Config;
    use audetic_core::sync::{LocalPairRequest, SYNC_PROTOCOL_VERSION};
    use axum::body::Body;
    use axum::http::Request;
    use base64::Engine;
    use tempfile::TempDir;
    use tower::ServiceExt;
    use uuid::Uuid;

    use std::collections::BTreeSet;

    use super::*;
    use crate::db::sync::SyncRepository;

    struct TestNode {
        _dir: TempDir,
        service: SyncService,
    }

    fn test_node(active_role: audetic_core::config::SyncRole) -> TestNode {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("audetic.db");
        let config_path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.sync.role = active_role;
        config.save_to(&config_path).unwrap();
        let conn = crate::db::init_db_at(&db_path).unwrap();
        let node_id = SyncRepository::node_id(&conn).unwrap();
        drop(conn);
        let service = SyncService::new(active_role, node_id, db_path, config_path).unwrap();
        TestNode { _dir: dir, service }
    }

    fn authorized_request(method: &str, path: &str, credential: &str, body: Body) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {credential}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap()
    }

    #[tokio::test]
    async fn enabling_hub_updates_dynamic_status_without_hot_switching() {
        let node = test_node(audetic_core::config::SyncRole::Standalone);

        let enabled = node.service.enable_hub().await.unwrap();

        assert_eq!(enabled.role, audetic_core::config::SyncRole::Hub);
        assert!(enabled.restart_required);
        assert_eq!(
            node.service.active_role(),
            audetic_core::config::SyncRole::Standalone
        );
        assert_eq!(
            node.service.status().await.unwrap().role,
            audetic_core::config::SyncRole::Hub
        );
        assert!(matches!(
            node.service.issue_device("Too soon").await,
            Err(SyncError::RoleConflict { .. })
        ));
    }

    #[tokio::test]
    async fn all_credential_failures_are_the_same_401_with_challenge() {
        let node = test_node(audetic_core::config::SyncRole::Hub);
        let issued = node.service.issue_device("Laptop").await.unwrap();
        let encoded = issued
            .credential
            .strip_prefix(super::super::CREDENTIAL_PREFIX)
            .unwrap();
        assert_eq!(
            super::super::URL_SAFE_NO_PAD.decode(encoded).unwrap().len(),
            32
        );
        let router = TransportServer::new(node.service.clone()).router();

        for authorization in [
            None,
            Some("Basic abc"),
            Some("Bearer"),
            Some("Bearer wrong"),
        ] {
            let mut builder = Request::builder().uri(TRANSPORT_STATUS_PATH);
            if let Some(value) = authorization {
                builder = builder.header(header::AUTHORIZATION, value);
            }
            let response = router
                .clone()
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
                "Bearer"
            );
        }

        node.service
            .revoke_device(issued.device.device_id)
            .await
            .unwrap();
        let response = router
            .oneshot(authorized_request(
                "GET",
                TRANSPORT_STATUS_PATH,
                &issued.credential,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer"
        );
    }

    #[tokio::test]
    async fn pending_pair_binds_atomically_then_status_reports_bound_identity() {
        let node = test_node(audetic_core::config::SyncRole::Hub);
        let issued = node.service.issue_device("Laptop").await.unwrap();
        let router = TransportServer::new(node.service.clone()).router();

        let pending = router
            .clone()
            .oneshot(authorized_request(
                "GET",
                TRANSPORT_STATUS_PATH,
                &issued.credential,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(pending.status(), StatusCode::CONFLICT);

        let client_node_id = Uuid::new_v4().to_string();
        let body = serde_json::to_vec(&TransportPairRequest {
            client_node_id: client_node_id.clone(),
        })
        .unwrap();
        for _ in 0..2 {
            let paired = router
                .clone()
                .oneshot(authorized_request(
                    "POST",
                    TRANSPORT_PAIR_PATH,
                    &issued.credential,
                    Body::from(body.clone()),
                ))
                .await
                .unwrap();
            assert_eq!(paired.status(), StatusCode::OK);
        }

        let conflicting = serde_json::to_vec(&TransportPairRequest {
            client_node_id: Uuid::new_v4().to_string(),
        })
        .unwrap();
        let conflicting = router
            .clone()
            .oneshot(authorized_request(
                "POST",
                TRANSPORT_PAIR_PATH,
                &issued.credential,
                Body::from(conflicting),
            ))
            .await
            .unwrap();
        assert_eq!(conflicting.status(), StatusCode::CONFLICT);

        let status = router
            .oneshot(authorized_request(
                "GET",
                TRANSPORT_STATUS_PATH,
                &issued.credential,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let body = axum::body::to_bytes(status.into_body(), usize::MAX)
            .await
            .unwrap();
        let status: TransportStatusResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(status.client_node_id, client_node_id);
        assert_eq!(status.device_id, issued.device.device_id);
        assert_eq!(status.protocol_version, SYNC_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn transport_router_has_no_non_sync_routes() {
        let node = test_node(audetic_core::config::SyncRole::Hub);
        let router = TransportServer::new(node.service).router();
        for path in [
            "/",
            "/api/openapi.json",
            "/api/meetings",
            "/api/sync/status",
        ] {
            let response = router
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[test]
    fn production_address_and_transport_document_are_fixed_and_isolated() {
        assert_eq!(transport_bind_addr(), "127.0.0.1:3738".parse().unwrap());
        let document = TransportApiDoc::openapi();
        assert_eq!(document.servers.unwrap()[0].url, "http://127.0.0.1:3738");
        let paths = document
            .paths
            .paths
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            paths,
            BTreeSet::from([
                TRANSPORT_PAIR_PATH.to_string(),
                TRANSPORT_STATUS_PATH.to_string(),
            ])
        );
    }

    #[tokio::test]
    async fn client_pairs_end_to_end_and_revocation_takes_effect_immediately() {
        let hub = test_node(audetic_core::config::SyncRole::Hub);
        let client = test_node(audetic_core::config::SyncRole::Standalone);
        let issued = hub.service.issue_device("Client laptop").await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = TransportServer::new(hub.service.clone());
        let task = tokio::spawn(server.serve_listener(listener));
        let hub_url = format!("http://{address}");

        let request = LocalPairRequest {
            hub_url: hub_url.clone(),
            credential: issued.credential.clone(),
        };
        let paired = client.service.pair(request.clone()).await.unwrap();
        assert_eq!(paired.paired_hub.protocol_version, SYNC_PROTOCOL_VERSION);
        assert_eq!(paired.paired_hub.device_id, issued.device.device_id);
        assert!(paired.restart_required);
        assert_eq!(client.service.pair(request).await.unwrap(), paired);
        let status = client.service.status().await.unwrap();
        assert_eq!(status.role, audetic_core::config::SyncRole::Client);
        assert_eq!(status.paired_hub, Some(paired.paired_hub));

        let response = reqwest::Client::new()
            .get(format!("{hub_url}{TRANSPORT_STATUS_PATH}"))
            .bearer_auth(&issued.credential)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), StatusCode::OK.as_u16());
        hub.service
            .revoke_device(issued.device.device_id)
            .await
            .unwrap();
        let response = reqwest::Client::new()
            .get(format!("{hub_url}{TRANSPORT_STATUS_PATH}"))
            .bearer_auth(&issued.credential)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            StatusCode::UNAUTHORIZED.as_u16()
        );
        assert_eq!(response.headers()["www-authenticate"], "Bearer");

        let unpaired = client.service.unpair().await.unwrap();
        assert_eq!(unpaired.role, audetic_core::config::SyncRole::Standalone);
        assert!(!unpaired.hub_credential_revoked);
        let status = client.service.status().await.unwrap();
        assert_eq!(status.role, audetic_core::config::SyncRole::Standalone);
        assert!(status.paired_hub.is_none());

        task.abort();
    }
}
