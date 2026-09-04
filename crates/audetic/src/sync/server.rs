use audetic_core::sync::HubId;
use axum::extract::{Request, State};
use axum::http::{header::ORIGIN, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use thiserror::Error;
use tokio::net::TcpListener;
use utoipa::OpenApi;

use std::future::Future;
use std::io;
use std::sync::Arc;

use super::identity::{parse_stored_tailscale_login, parse_tailscale_login, LoginParseError};
use super::protocol::{
    HubApiError, HubInfo, ProtocolRange, HUB_ID_HEADER, HUB_INFO_ROUTE, PROTOCOL_VERSION,
    PROTOCOL_VERSION_HEADER, TAILSCALE_FUNNEL_REQUEST_HEADER,
};

#[derive(Clone, Debug)]
pub struct HubServerConfig {
    pub hub_id: HubId,
    pub owner_login: String,
    pub device_name: Option<String>,
    pub audetic_version: String,
}

impl HubServerConfig {
    pub fn new(hub_id: HubId, owner_login: &str) -> Result<Self, LoginParseError> {
        Ok(Self {
            hub_id,
            owner_login: parse_stored_tailscale_login(owner_login)?,
            device_name: None,
            audetic_version: env!("CARGO_PKG_VERSION").to_owned(),
        })
    }

    pub fn with_device_name(mut self, device_name: impl Into<String>) -> Self {
        self.device_name = Some(device_name.into());
        self
    }
}

#[derive(Debug, Error)]
pub enum HubServerError {
    #[error("the Home Hub listener must bind to a loopback address, not {0}")]
    NonLoopback(std::net::SocketAddr),
    #[error("Home Hub listener failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct HubServer {
    state: Arc<HubServerConfig>,
}

impl HubServer {
    pub fn new(config: HubServerConfig) -> Self {
        Self {
            state: Arc::new(config),
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route(HUB_INFO_ROUTE, get(info))
            .layer(middleware::from_fn_with_state(
                self.state.clone(),
                enforce_hub_policy,
            ))
            .with_state(self.state.clone())
    }

    pub async fn serve(self, listener: TcpListener) -> Result<(), HubServerError> {
        self.serve_with_shutdown(listener, std::future::pending::<()>())
            .await
    }

    pub async fn serve_with_shutdown(
        self,
        listener: TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), HubServerError> {
        let address = listener.local_addr()?;
        if !address.ip().is_loopback() {
            return Err(HubServerError::NonLoopback(address));
        }
        axum::serve(listener, self.router())
            .with_graceful_shutdown(shutdown)
            .await?;
        Ok(())
    }
}

#[utoipa::path(
    get,
    path = "/v1/info",
    tag = "hub_discovery",
    params(
        ("Tailscale-User-Login" = String, Header, description = "Exact identity injected by Tailscale Serve"),
        ("X-Audetic-Protocol-Version" = u16, Header, description = "Required sync protocol version"),
        ("X-Audetic-Hub-ID" = Option<String>, Header, description = "Expected stable Hub ID when reconnecting"),
    ),
    responses(
        (status = 200, description = "Compatible Home Hub identity and protocol range", body = HubInfo),
        (status = 400, description = "Missing or malformed protocol/Hub ID header", body = HubApiError),
        (status = 403, description = "Untrusted origin, Funnel request, or wrong Tailscale identity", body = HubApiError),
        (status = 409, description = "The expected Hub ID does not match this Home Hub", body = HubApiError),
        (status = 426, description = "Unsupported sync protocol", body = HubApiError),
    )
)]
async fn info(State(state): State<Arc<HubServerConfig>>) -> Json<HubInfo> {
    Json(HubInfo {
        hub_id: state.hub_id,
        owner_login: state.owner_login.clone(),
        device_name: state.device_name.clone(),
        protocol: ProtocolRange::supported(),
        audetic_version: state.audetic_version.clone(),
    })
}

async fn enforce_hub_policy(
    State(state): State<Arc<HubServerConfig>>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if headers.contains_key(ORIGIN) {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "browser_origin_rejected",
            "browser Origin requests are not accepted by the Home Hub listener",
        );
    }
    if headers.contains_key(TAILSCALE_FUNNEL_REQUEST_HEADER) {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "funnel_rejected",
            "Tailscale Funnel traffic is not accepted",
        );
    }

    let identities = headers
        .get_all(super::identity::TAILSCALE_USER_LOGIN_HEADER)
        .iter()
        .collect::<Vec<_>>();
    let [identity] = identities.as_slice() else {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "identity_required",
            "Tailscale-User-Login is required",
        );
    };
    let Ok(identity) = parse_tailscale_login(identity) else {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "invalid_identity",
            "Tailscale-User-Login is malformed",
        );
    };
    if identity != state.owner_login {
        return rejection(
            &state,
            StatusCode::FORBIDDEN,
            "wrong_identity",
            "the authenticated Tailscale identity does not own this Home Hub",
        );
    }

    let Some(version) = parse_protocol_version(headers) else {
        return rejection(
            &state,
            StatusCode::BAD_REQUEST,
            "protocol_required",
            "a valid Audetic protocol version header is required",
        );
    };
    if version != PROTOCOL_VERSION {
        return rejection(
            &state,
            StatusCode::UPGRADE_REQUIRED,
            "incompatible_protocol",
            "the requested Audetic sync protocol is not supported",
        );
    }

    let expected_hub_ids = headers.get_all(HUB_ID_HEADER).iter().collect::<Vec<_>>();
    if expected_hub_ids.is_empty() && request.uri().path() != HUB_INFO_ROUTE {
        return rejection(
            &state,
            StatusCode::BAD_REQUEST,
            "hub_id_required",
            "the expected Hub ID is required for non-discovery requests",
        );
    }
    if expected_hub_ids.len() > 1 {
        return rejection(
            &state,
            StatusCode::BAD_REQUEST,
            "invalid_hub_id",
            "exactly one expected Hub ID may be supplied",
        );
    }
    if let Some(expected_hub_id) = expected_hub_ids.first() {
        let Some(expected_hub_id) = expected_hub_id
            .to_str()
            .ok()
            .and_then(|id| id.parse::<HubId>().ok())
        else {
            return rejection(
                &state,
                StatusCode::BAD_REQUEST,
                "invalid_hub_id",
                "the expected Hub ID is malformed",
            );
        };
        if expected_hub_id != state.hub_id {
            return rejection(
                &state,
                StatusCode::CONFLICT,
                "wrong_hub_id",
                "the expected Hub ID does not match this Home Hub",
            );
        }
    }

    let mut response = next.run(request).await;
    insert_hub_id(response.headers_mut(), state.hub_id);
    response
}

fn parse_protocol_version(headers: &HeaderMap) -> Option<u16> {
    let versions = headers
        .get_all(PROTOCOL_VERSION_HEADER)
        .iter()
        .collect::<Vec<_>>();
    let [version] = versions.as_slice() else {
        return None;
    };
    version.to_str().ok()?.parse().ok()
}

fn rejection(
    state: &HubServerConfig,
    status: StatusCode,
    code: &'static str,
    message: &'static str,
) -> Response {
    let mut response = (status, Json(HubApiError::new(code, message))).into_response();
    insert_hub_id(response.headers_mut(), state.hub_id);
    response
}

fn insert_hub_id(headers: &mut HeaderMap, hub_id: HubId) {
    headers.insert(
        HUB_ID_HEADER,
        HeaderValue::from_str(&hub_id.to_string()).expect("Hub IDs are valid header values"),
    );
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Audetic Home Hub API",
        description = "Narrow daemon-to-daemon Shared Library protocol. Not a browser API.",
        version = env!("CARGO_PKG_VERSION"),
        license(name = "MIT"),
    ),
    servers(
        (url = "http://127.0.0.1:3738", description = "Loopback listener behind Tailscale Serve"),
    ),
    paths(info),
    components(schemas(HubInfo, ProtocolRange, HubApiError, audetic_core::sync::HubId)),
    tags((name = "hub_discovery", description = "Home Hub discovery and compatibility")),
)]
pub struct HubApiDoc;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Method, Request};
    use tower::ServiceExt;
    use uuid::Uuid;

    fn hub_id() -> HubId {
        HubId::from_uuid(Uuid::new_v4())
    }

    fn server() -> (HubServer, HubId) {
        let hub_id = hub_id();
        (
            HubServer::new(HubServerConfig::new(hub_id, "Alice@Example.com").unwrap()),
            hub_id,
        )
    }

    fn request(owner: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(Method::GET)
            .uri("/v1/info")
            .header(super::super::identity::TAILSCALE_USER_LOGIN_HEADER, owner)
            .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION.to_string())
    }

    #[tokio::test]
    async fn info_returns_identity_and_echoes_the_actual_hub_id() {
        let (server, hub_id) = server();
        let response = server
            .router()
            .oneshot(request("Alice@Example.com").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[HUB_ID_HEADER], hub_id.to_string());
        let info: HubInfo =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(info.hub_id, hub_id);
        assert_eq!(info.owner_login, "Alice@Example.com");
    }

    #[tokio::test]
    async fn identity_is_required_and_compared_without_case_or_whitespace_normalization() {
        let (server, _) = server();
        for owner in [None, Some("alice@example.com"), Some(" Alice@Example.com ")] {
            let mut builder = Request::builder().method(Method::GET).uri("/v1/info");
            if let Some(owner) = owner {
                builder =
                    builder.header(super::super::identity::TAILSCALE_USER_LOGIN_HEADER, owner);
            }
            let response = server
                .router()
                .oneshot(
                    builder
                        .header(PROTOCOL_VERSION_HEADER, "1")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{owner:?}");
        }
    }

    #[tokio::test]
    async fn rfc_2047_identity_uses_the_same_exact_decoder_as_setup() {
        let hub_id = hub_id();
        let server = HubServer::new(
            HubServerConfig::new(hub_id, "=?utf-8?q?m=C3=A1t@example.com?=").unwrap(),
        );
        let response = server
            .router()
            .oneshot(
                request("=?utf-8?q?m=C3=A1t@example.com?=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_or_duplicated_identity_headers_fail_closed() {
        let (server, _) = server();
        let malformed = request("=?utf-8?q?broken=Q0?=")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            server
                .router()
                .clone()
                .oneshot(malformed)
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );

        let mut duplicated = request("Alice@Example.com").body(Body::empty()).unwrap();
        duplicated.headers_mut().append(
            super::super::identity::TAILSCALE_USER_LOGIN_HEADER,
            HeaderValue::from_static("Alice@Example.com"),
        );
        assert_eq!(
            server.router().oneshot(duplicated).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn browser_origin_and_funnel_marked_requests_are_rejected() {
        let (server, _) = server();
        for (header, value) in [
            (ORIGIN.as_str(), "https://example.com"),
            (TAILSCALE_FUNNEL_REQUEST_HEADER, "true"),
        ] {
            let response = server
                .router()
                .oneshot(
                    request("Alice@Example.com")
                        .header(header, value)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{header}");
        }
    }

    #[tokio::test]
    async fn protocol_is_required_and_incompatible_versions_are_rejected() {
        let (server, _) = server();
        let missing = Request::builder()
            .method(Method::GET)
            .uri("/v1/info")
            .header(
                super::super::identity::TAILSCALE_USER_LOGIN_HEADER,
                "Alice@Example.com",
            )
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            server
                .router()
                .clone()
                .oneshot(missing)
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );

        let mut incompatible = request("Alice@Example.com").body(Body::empty()).unwrap();
        incompatible
            .headers_mut()
            .insert(PROTOCOL_VERSION_HEADER, HeaderValue::from_static("2"));
        assert_eq!(
            server
                .router()
                .oneshot(incompatible)
                .await
                .unwrap()
                .status(),
            StatusCode::UPGRADE_REQUIRED
        );
    }

    #[tokio::test]
    async fn supplied_hub_id_must_match_before_the_request_reaches_the_handler() {
        let (server, actual) = server();
        let response = server
            .router()
            .oneshot(
                request("Alice@Example.com")
                    .header(HUB_ID_HEADER, hub_id().to_string())
                    .body(Body::from("body that must not be consumed"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.headers()[HUB_ID_HEADER], actual.to_string());
    }

    #[tokio::test]
    async fn listener_refuses_non_loopback_bindings() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (server, _) = server();

        assert!(matches!(
            server.serve(listener).await,
            Err(HubServerError::NonLoopback(_))
        ));
    }

    #[test]
    fn hub_api_document_contains_only_the_slice_one_discovery_operation() {
        let document = HubApiDoc::openapi();
        assert_eq!(
            document.servers.as_ref().unwrap()[0].url,
            super::super::protocol::HUB_LOOPBACK_BASE_URL
        );
        assert_eq!(document.paths.paths.len(), 1);
        assert!(document.paths.paths.contains_key("/v1/info"));
        let operation = document
            .paths
            .paths
            .get("/v1/info")
            .and_then(|item| item.get.as_ref());
        assert!(operation.is_some());
        assert!(document
            .components
            .as_ref()
            .is_some_and(|components| components.schemas.contains_key("HubId")));
    }
}
