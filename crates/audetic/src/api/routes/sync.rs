//! Local synchronization administration routes.

use audetic_core::sync::{
    DeviceAddRequest, DeviceAddResponse, DeviceListResponse, DeviceRevokeResponse,
    HubEnableResponse, LocalPairRequest, LocalPairResponse, LocalUnpairResponse,
    SyncStatusResponse,
};
use axum::{
    extract::{Path, State},
    response::Json,
    routing::{get, post},
    Router,
};

use std::sync::Arc;

use crate::api::error::{ApiError, ApiErrorResponse, ApiResult};
use crate::sync::SyncService;

#[derive(Clone)]
pub struct SyncApiState {
    service: Arc<SyncService>,
}

impl SyncApiState {
    pub fn new(service: Arc<SyncService>) -> Self {
        Self { service }
    }
}

pub fn router(state: SyncApiState) -> Router {
    Router::new()
        .route("/sync/status", get(get_status))
        .route("/sync/hub/enable", post(enable_hub))
        .route("/sync/pair", post(pair).delete(unpair))
        .route("/sync/devices", get(list_devices).post(add_device))
        .route(
            "/sync/devices/:device_id",
            axum::routing::delete(revoke_device),
        )
        .with_state(state)
}

#[utoipa::path(
    get,
    path = "/sync/status",
    tag = "sync",
    operation_id = "get_sync_status",
    responses(
        (status = 200, description = "Current synchronization role, identity, and pairing", body = SyncStatusResponse),
        (status = 500, description = "Synchronization state could not be read", body = ApiErrorResponse),
    ),
)]
pub async fn get_status(State(state): State<SyncApiState>) -> ApiResult<Json<SyncStatusResponse>> {
    state
        .service
        .status()
        .await
        .map(Json)
        .map_err(ApiError::from)
}

#[utoipa::path(
    post,
    path = "/sync/hub/enable",
    tag = "sync",
    operation_id = "enable_sync_hub",
    responses(
        (status = 200, description = "Hub role persisted", body = HubEnableResponse),
        (status = 409, description = "Active role or pairing conflicts with Hub mode", body = ApiErrorResponse),
        (status = 500, description = "Hub role could not be persisted", body = ApiErrorResponse),
    ),
)]
pub async fn enable_hub(State(state): State<SyncApiState>) -> ApiResult<Json<HubEnableResponse>> {
    state
        .service
        .enable_hub()
        .await
        .map(Json)
        .map_err(ApiError::from)
}

#[utoipa::path(
    post,
    path = "/sync/pair",
    tag = "sync",
    operation_id = "pair_sync_client",
    request_body = LocalPairRequest,
    responses(
        (status = 200, description = "Client paired with the Hub", body = LocalPairResponse),
        (status = 400, description = "Pairing request is invalid", body = ApiErrorResponse),
        (status = 401, description = "Hub rejected the credential", body = ApiErrorResponse),
        (status = 409, description = "Active role or existing pairing conflicts", body = ApiErrorResponse),
        (status = 502, description = "Hub is unavailable or incompatible", body = ApiErrorResponse),
        (status = 500, description = "Pairing could not be persisted", body = ApiErrorResponse),
    ),
)]
pub async fn pair(
    State(state): State<SyncApiState>,
    Json(request): Json<LocalPairRequest>,
) -> ApiResult<Json<LocalPairResponse>> {
    state
        .service
        .pair(request)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

#[utoipa::path(
    delete,
    path = "/sync/pair",
    tag = "sync",
    operation_id = "unpair_sync_client",
    responses(
        (status = 200, description = "Local Client pairing removed", body = LocalUnpairResponse),
        (status = 409, description = "Active role conflicts with Client administration", body = ApiErrorResponse),
        (status = 500, description = "Pairing could not be removed", body = ApiErrorResponse),
    ),
)]
pub async fn unpair(State(state): State<SyncApiState>) -> ApiResult<Json<LocalUnpairResponse>> {
    state
        .service
        .unpair()
        .await
        .map(Json)
        .map_err(ApiError::from)
}

#[utoipa::path(
    get,
    path = "/sync/devices",
    tag = "sync",
    operation_id = "list_sync_devices",
    responses(
        (status = 200, description = "Hub devices, including revoked devices", body = DeviceListResponse),
        (status = 409, description = "Operation requires an active Hub", body = ApiErrorResponse),
        (status = 500, description = "Devices could not be read", body = ApiErrorResponse),
    ),
)]
pub async fn list_devices(
    State(state): State<SyncApiState>,
) -> ApiResult<Json<DeviceListResponse>> {
    state
        .service
        .list_devices()
        .await
        .map(Json)
        .map_err(ApiError::from)
}

#[utoipa::path(
    post,
    path = "/sync/devices",
    tag = "sync",
    operation_id = "add_sync_device",
    request_body = DeviceAddRequest,
    responses(
        (status = 200, description = "Device and one-time plaintext credential issued", body = DeviceAddResponse),
        (status = 400, description = "Device request is invalid", body = ApiErrorResponse),
        (status = 409, description = "Operation requires an active Hub", body = ApiErrorResponse),
        (status = 500, description = "Device could not be issued", body = ApiErrorResponse),
    ),
)]
pub async fn add_device(
    State(state): State<SyncApiState>,
    Json(request): Json<DeviceAddRequest>,
) -> ApiResult<Json<DeviceAddResponse>> {
    state
        .service
        .issue_device(request.name)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

#[utoipa::path(
    delete,
    path = "/sync/devices/{device_id}",
    tag = "sync",
    operation_id = "revoke_sync_device",
    params(("device_id" = String, Path, description = "Device UUID")),
    responses(
        (status = 200, description = "Device credential revoked", body = DeviceRevokeResponse),
        (status = 400, description = "Device ID is invalid", body = ApiErrorResponse),
        (status = 404, description = "Device does not exist", body = ApiErrorResponse),
        (status = 409, description = "Operation requires an active Hub", body = ApiErrorResponse),
        (status = 500, description = "Device could not be revoked", body = ApiErrorResponse),
    ),
)]
pub async fn revoke_device(
    State(state): State<SyncApiState>,
    Path(device_id): Path<String>,
) -> ApiResult<Json<DeviceRevokeResponse>> {
    state
        .service
        .revoke_device(device_id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

#[cfg(test)]
mod tests {
    use audetic_core::config::{Config, SyncRole};
    use axum::{body::Body, http::Request};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;
    use crate::db::sync::SyncRepository;

    struct TestNode {
        _dir: TempDir,
        service: Arc<SyncService>,
    }

    fn test_node(role: SyncRole) -> TestNode {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("audetic.db");
        let config_path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.sync.role = role;
        config.save_to(&config_path).unwrap();
        let conn = crate::db::init_db_at(&db_path).unwrap();
        let node_id = SyncRepository::node_id(&conn).unwrap();
        drop(conn);
        let service = Arc::new(SyncService::new(role, node_id, db_path, config_path).unwrap());
        TestNode { _dir: dir, service }
    }

    fn request(method: &str, path: &str, body: Option<serde_json::Value>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(path);
        let body = match body {
            Some(body) => {
                builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(&body).unwrap())
            }
            None => Body::empty(),
        };
        builder.body(body).unwrap()
    }

    #[tokio::test]
    async fn status_route_returns_typed_local_state() {
        let node = test_node(SyncRole::Standalone);
        let expected = node.service.status().await.unwrap();
        let response = router(SyncApiState::new(Arc::clone(&node.service)))
            .oneshot(Request::get("/sync/status").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<SyncStatusResponse>(&body).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn router_exposes_the_local_sync_routes_with_exact_methods() {
        let node = test_node(SyncRole::Hub);
        let app = router(SyncApiState::new(Arc::clone(&node.service)));

        for (method, path, body, expected) in [
            ("GET", "/sync/status", None, axum::http::StatusCode::OK),
            ("POST", "/sync/hub/enable", None, axum::http::StatusCode::OK),
            (
                "POST",
                "/sync/pair",
                Some(serde_json::json!({"hub_url": "https://example.com", "credential": "secret"})),
                axum::http::StatusCode::CONFLICT,
            ),
            (
                "DELETE",
                "/sync/pair",
                None,
                axum::http::StatusCode::CONFLICT,
            ),
            ("GET", "/sync/devices", None, axum::http::StatusCode::OK),
            (
                "POST",
                "/sync/devices",
                Some(serde_json::json!({"name": "Laptop"})),
                axum::http::StatusCode::OK,
            ),
            (
                "DELETE",
                "/sync/devices/67e55044-10b1-426f-9247-bb680e5fe0c8",
                None,
                axum::http::StatusCode::NOT_FOUND,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(request(method, path, body))
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{method} {path}");
        }

        for (method, path) in [
            ("POST", "/sync/status"),
            ("GET", "/sync/hub/enable"),
            ("GET", "/sync/pair"),
            ("PUT", "/sync/devices"),
            ("GET", "/sync/devices/67e55044-10b1-426f-9247-bb680e5fe0c8"),
        ] {
            let response = app
                .clone()
                .oneshot(request(method, path, None))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                axum::http::StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path}"
            );
        }

        let unknown = app
            .oneshot(request("GET", "/sync/unknown", None))
            .await
            .unwrap();
        assert_eq!(unknown.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn wrong_role_returns_typed_conflict() {
        let node = test_node(SyncRole::Standalone);
        let response = router(SyncApiState::new(node.service))
            .oneshot(request("GET", "/sync/devices", None))
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let error: ApiErrorResponse = serde_json::from_slice(&body).unwrap();
        assert!(error.error);
        assert!(error.message.contains("requires the hub role"));
        assert!(!error.message.contains("credential"));
    }
}
