//! Read-only local synchronization status.

use audetic_core::sync::SyncStatus;
use axum::{extract::State, response::Json, routing::get, Router};

#[derive(Clone)]
pub struct SyncApiState {
    status: SyncStatus,
}

impl SyncApiState {
    pub fn new(status: SyncStatus) -> Self {
        Self { status }
    }
}

pub fn router(state: SyncApiState) -> Router {
    Router::new()
        .route("/sync/status", get(get_status))
        .with_state(state)
}

#[utoipa::path(
    get,
    path = "/sync/status",
    tag = "sync",
    operation_id = "get_sync_status",
    responses(
        (status = 200, description = "Current synchronization role and durable node identity", body = SyncStatus),
    ),
)]
pub async fn get_status(State(state): State<SyncApiState>) -> Json<SyncStatus> {
    Json(state.status)
}

#[cfg(test)]
mod tests {
    use audetic_core::config::SyncRole;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn status_route_returns_typed_local_state() {
        let expected = SyncStatus {
            role: SyncRole::Standalone,
            node_id: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
        };
        let response = router(SyncApiState::new(expected.clone()))
            .oneshot(Request::get("/sync/status").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<SyncStatus>(&body).unwrap(),
            expected
        );
    }

    #[tokio::test]
    async fn status_route_is_read_only() {
        let response = router(SyncApiState::new(SyncStatus {
            role: SyncRole::Standalone,
            node_id: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
        }))
        .oneshot(Request::post("/sync/status").body(Body::empty()).unwrap())
        .await
        .unwrap();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
