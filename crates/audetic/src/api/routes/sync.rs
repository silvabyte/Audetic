//! Local control surface for Library Sync. Business logic stays in `SyncService`.

use audetic_core::sync::{
    SyncPayloadPolicyRequest, SyncPayloadPolicyResponse, SyncSetupRequest, SyncSetupResult,
    SyncStatus,
};
use axum::extract::State;
use axum::routing::{get, post, put};
use axum::{Json, Router};

use std::sync::Arc;

use crate::api::error::{ApiError, ApiErrorBody, ApiResult};
use crate::sync::{SyncService, SyncServiceError};

#[derive(Clone)]
pub struct SyncApiState {
    pub(crate) service: Arc<SyncService>,
}

impl SyncApiState {
    pub fn new(service: Arc<SyncService>) -> Self {
        Self { service }
    }
}

pub fn router(state: SyncApiState) -> Router {
    Router::new()
        .route("/sync/status", get(get_status))
        .route("/sync/discover", post(discover))
        .route("/sync/configure", post(configure))
        .route("/sync/payload-policy", put(update_payload_policy))
        .route("/sync/retry", post(retry))
        .with_state(state)
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct SyncRetryResponse {
    pub retried_items: u64,
}

#[utoipa::path(
    post,
    path = "/sync/retry",
    tag = "sync",
    operation_id = "retry_sync_outbox",
    responses(
        (status = 200, description = "Delayed and needs-attention items made immediately retryable", body = SyncRetryResponse),
        (status = 500, description = "Outbox could not be updated", body = ApiErrorBody),
    ),
)]
pub async fn retry(State(state): State<SyncApiState>) -> ApiResult<Json<SyncRetryResponse>> {
    state
        .service
        .retry()
        .await
        .map(|retried_items| Json(SyncRetryResponse { retried_items }))
        .map_err(map_service_error)
}

#[utoipa::path(
    get,
    path = "/sync/status",
    tag = "sync",
    operation_id = "get_sync_status",
    responses(
        (status = 200, description = "Persisted role and current sync readiness", body = SyncStatus),
        (status = 500, description = "Persisted sync state could not be read", body = ApiErrorBody),
    ),
)]
pub async fn get_status(State(state): State<SyncApiState>) -> ApiResult<Json<SyncStatus>> {
    state
        .service
        .status()
        .await
        .map(Json)
        .map_err(map_service_error)
}

#[utoipa::path(
    post,
    path = "/sync/discover",
    tag = "sync",
    operation_id = "discover_home_hubs",
    responses(
        (status = 200, description = "Compatible Home Hubs visible on the tailnet", body = SyncSetupResult),
        (status = 400, description = "Tailscale is not authenticated or suitable for discovery", body = ApiErrorBody),
        (status = 503, description = "Tailscale or discovery transport is unavailable", body = ApiErrorBody),
    ),
)]
pub async fn discover(State(state): State<SyncApiState>) -> ApiResult<Json<SyncSetupResult>> {
    state
        .service
        .discover()
        .await
        .map(Json)
        .map_err(map_service_error)
}

#[utoipa::path(
    post,
    path = "/sync/configure",
    tag = "sync",
    operation_id = "configure_sync_role",
    request_body = SyncSetupRequest,
    responses(
        (status = 200, description = "Preview or committed role transition", body = SyncSetupResult),
        (status = 400, description = "Invalid settings or disallowed role transition", body = ApiErrorBody),
        (status = 409, description = "Tailscale Serve or Funnel collision", body = ApiErrorBody),
        (status = 503, description = "The requested role could not be verified or started", body = ApiErrorBody),
        (status = 500, description = "The transition could not be persisted", body = ApiErrorBody),
    ),
)]
pub async fn configure(
    State(state): State<SyncApiState>,
    Json(request): Json<SyncSetupRequest>,
) -> ApiResult<Json<SyncSetupResult>> {
    state
        .service
        .configure(request)
        .await
        .map(Json)
        .map_err(map_service_error)
}

#[utoipa::path(
    put,
    path = "/sync/payload-policy",
    tag = "sync",
    operation_id = "update_sync_payload_policy",
    request_body = SyncPayloadPolicyRequest,
    responses(
        (status = 200, description = "Device-local Recording Payload upload policy updated", body = SyncPayloadPolicyResponse),
        (status = 400, description = "No Shared Library role is active", body = ApiErrorBody),
        (status = 500, description = "The device-local policy could not be persisted", body = ApiErrorBody),
    ),
)]
pub async fn update_payload_policy(
    State(state): State<SyncApiState>,
    Json(request): Json<SyncPayloadPolicyRequest>,
) -> ApiResult<Json<SyncPayloadPolicyResponse>> {
    state
        .service
        .update_recording_payload_policy(request.upload_recording_payloads)
        .await
        .map(|upload_recording_payloads| {
            Json(SyncPayloadPolicyResponse {
                upload_recording_payloads,
            })
        })
        .map_err(map_service_error)
}

fn map_service_error(error: SyncServiceError) -> ApiError {
    let message = error.to_string();
    if error.is_request_error() {
        ApiError::bad_request(message)
    } else if error.is_conflict() {
        ApiError::conflict(message)
    } else if error.is_unavailable() {
        ApiError::unavailable(message)
    } else {
        ApiError::internal(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use audetic_core::sync::{CacheLevel, HubCandidate, HubConnection, HubId, SyncRole};
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use semver::Version;
    use tower::ServiceExt;

    use crate::sync::tailscale::{
        MappingState, ServeAssessment, TailscaleControl, TailscaleError, TailscaleStatus,
    };
    use crate::sync::transport::{
        DiscoveryOutcome, HubProbe, HubTransferError, RemoteLibrary, RemotePayloadSource,
        ReplicationTransport,
    };

    struct FakeTailscale;

    impl TailscaleControl for FakeTailscale {
        fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
            Ok(TailscaleStatus {
                version: Version::parse("1.80.0").unwrap(),
                backend_state: "Running".into(),
                self_dns_name: "device.example.ts.net.".into(),
                owner_login: "owner@example.com".into(),
                self_is_tagged: false,
                peers: vec![crate::sync::tailscale::TailscalePeer {
                    dns_name: "hub.example.ts.net.".into(),
                    online: true,
                    tagged: false,
                }],
            })
        }

        fn serve_assessment(&self) -> Result<ServeAssessment, TailscaleError> {
            Ok(ServeAssessment {
                mapping: MappingState::Vacant,
                funnel_enabled: false,
            })
        }

        fn apply_audetic_serve(&self) -> Result<bool, TailscaleError> {
            Ok(true)
        }

        fn remove_audetic_serve(&self) -> Result<bool, TailscaleError> {
            Ok(true)
        }

        fn serve_preview(&self) -> String {
            "tailscale serve preview".into()
        }
    }

    struct FakeHubs {
        hub: HubConnection,
    }

    #[async_trait]
    impl HubProbe for FakeHubs {
        async fn handshake(&self, hub: &HubConnection) -> Result<HubCandidate, HubTransferError> {
            Ok(HubCandidate {
                connection: hub.clone(),
                device_name: Some("Home".into()),
                protocol_version: 1,
            })
        }

        async fn discover(
            &self,
            _candidates: Vec<String>,
            _expected_owner_login: &str,
        ) -> DiscoveryOutcome {
            DiscoveryOutcome::One(HubCandidate {
                connection: self.hub.clone(),
                device_name: Some("Home".into()),
                protocol_version: 1,
            })
        }
    }

    struct UnusedReplication;
    impl ReplicationTransport for UnusedReplication {}

    struct UnusedRemoteLibrary;
    impl RemoteLibrary for UnusedRemoteLibrary {}

    struct UnusedRemotePayloads;
    impl RemotePayloadSource for UnusedRemotePayloads {}

    fn test_router() -> (Router, tempfile::TempDir, HubConnection) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        crate::db::migrate_db_at(&path).unwrap();
        let hub = HubConnection {
            base_url: "https://hub.example.ts.net:8443/audetic/".into(),
            hub_id: HubId::new(),
            owner_login: "owner@example.com".into(),
        };
        let service = Arc::new(SyncService::with_dependencies(
            path,
            Arc::new(FakeTailscale),
            Arc::new(FakeHubs { hub: hub.clone() }),
            Arc::new(UnusedReplication),
            Arc::new(UnusedRemoteLibrary),
            Arc::new(UnusedRemotePayloads),
            "127.0.0.1:0".parse().unwrap(),
        ));
        (router(SyncApiState::new(service)), temp, hub)
    }

    #[tokio::test]
    async fn status_and_discovery_routes_return_typed_state() {
        let (app, _temp, hub) = test_router();
        let status = app
            .clone()
            .oneshot(Request::get("/sync/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let status: SyncStatus =
            serde_json::from_slice(&to_bytes(status.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(status.role, SyncRole::Standalone);

        let discovery = app
            .oneshot(Request::post("/sync/discover").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(discovery.status(), StatusCode::OK);
        let result: SyncSetupResult =
            serde_json::from_slice(&to_bytes(discovery.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(result.discovered_hubs[0].connection, hub);
    }

    #[tokio::test]
    async fn configure_route_verifies_and_persists_connected_device() {
        let (app, _temp, hub) = test_router();
        let request = SyncSetupRequest {
            role: SyncRole::ConnectedDevice,
            device_name: Some("Laptop".into()),
            hub: Some(hub.clone()),
            upload_recording_payloads: true,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: false,
            confirm_serve_change: false,
        };
        let response = app
            .oneshot(
                Request::post("/sync/configure")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result: SyncSetupResult =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(result.status.role, SyncRole::ConnectedDevice);
        assert_eq!(result.status.hub, Some(hub));
        assert!(result.status.hub_reachable);
    }

    #[tokio::test]
    async fn configure_route_maps_domain_validation_to_json_bad_request() {
        let (app, _temp, _) = test_router();
        let request = SyncSetupRequest {
            role: SyncRole::ConnectedDevice,
            device_name: None,
            hub: None,
            upload_recording_payloads: false,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: false,
            confirm_serve_change: false,
        };
        let response = app
            .oneshot(
                Request::post("/sync/configure")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"], true);
    }

    #[tokio::test]
    async fn payload_policy_route_uses_the_narrow_device_local_operation() {
        let (app, _temp, hub) = test_router();
        let setup = SyncSetupRequest {
            role: SyncRole::ConnectedDevice,
            device_name: None,
            hub: Some(hub),
            upload_recording_payloads: false,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: false,
            confirm_serve_change: false,
        };
        let configured = app
            .clone()
            .oneshot(
                Request::post("/sync/configure")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&setup).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(configured.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::put("/sync/payload-policy")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&SyncPayloadPolicyRequest {
                            upload_recording_payloads: true,
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result: SyncPayloadPolicyResponse =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(result.upload_recording_payloads);
    }

    #[tokio::test]
    async fn configure_route_rejects_payload_policy_changes_for_an_active_role() {
        let (app, _temp, hub) = test_router();
        let setup = SyncSetupRequest {
            role: SyncRole::ConnectedDevice,
            device_name: None,
            hub: Some(hub.clone()),
            upload_recording_payloads: false,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: false,
            confirm_serve_change: false,
        };
        let configured = app
            .clone()
            .oneshot(
                Request::post("/sync/configure")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&setup).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(configured.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(
                Request::post("/sync/configure")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&SyncSetupRequest {
                            upload_recording_payloads: true,
                            ..setup
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(body["message"]
            .as_str()
            .is_some_and(|message| message.contains("/sync/payload-policy")));

        let status = app
            .oneshot(Request::get("/sync/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status: SyncStatus =
            serde_json::from_slice(&to_bytes(status.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(!status.upload_recording_payloads);
    }
}
