//! Unified, read-only setup assessment.

use audetic_core::setup::SetupAssessment;
use axum::{extract::State, response::Json, routing::get, Router};

use crate::config::WhisperConfig;

#[derive(Clone)]
pub struct SetupApiState {
    active_provider: WhisperConfig,
}

impl SetupApiState {
    pub fn new(active_provider: WhisperConfig) -> Self {
        Self { active_provider }
    }
}

pub fn router(state: SetupApiState) -> Router {
    Router::new()
        .route("/setup", get(get_setup))
        .with_state(state)
}

/// Assess host capabilities used by dictation and meeting recording.
#[utoipa::path(
    get,
    path = "/setup",
    tag = "setup",
    operation_id = "get_setup_assessment",
    responses(
        (status = 200, description = "Current setup capability assessment", body = SetupAssessment),
    ),
)]
pub async fn get_setup(State(state): State<SetupApiState>) -> Json<SetupAssessment> {
    Json(crate::setup::assess(&state.active_provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn setup_route_is_read_only_get() {
        let router = || router(SetupApiState::new(WhisperConfig::default()));
        let get_response = router()
            .oneshot(Request::get("/setup").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(get_response.status(), axum::http::StatusCode::OK);

        let post_response = router()
            .oneshot(Request::post("/setup").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            post_response.status(),
            axum::http::StatusCode::METHOD_NOT_ALLOWED
        );
    }
}
