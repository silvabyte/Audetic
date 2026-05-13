//! Update API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::update::{UpdateConfig, UpdateEngine, UpdateOptions, UpdateReport};
use axum::{
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request body for update install.
#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct UpdateInstallRequest {
    /// Channel override (e.g., "stable", "beta")
    pub channel: Option<String>,
    /// Force update even if versions match
    pub force: Option<bool>,
}

/// Request body for auto-update toggle.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AutoUpdateRequest {
    /// Enable or disable auto-update
    pub enabled: bool,
}

/// Response body for the auto-update toggle endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct AutoUpdateResponse {
    pub success: bool,
    pub auto_update: bool,
    pub message: String,
}

/// Response body for the auto-update getter.
#[derive(Debug, Serialize, ToSchema)]
pub struct AutoUpdateState {
    pub enabled: bool,
}

/// Create the update router.
pub fn router() -> Router {
    Router::new()
        .route("/check", get(check_update))
        .route("/install", post(install_update))
        .route("/auto", get(get_auto_update).put(set_auto_update))
}

/// GET /api/update/check - Check for available updates.
#[utoipa::path(
    get,
    path = "/update/check",
    tag = "update",
    responses(
        (status = 200, description = "Current vs available version info", body = UpdateReport),
    ),
)]
pub async fn check_update() -> ApiResult<Json<UpdateReport>> {
    let config = UpdateConfig::detect(None).map_err(ApiError::from)?;
    let engine = UpdateEngine::new(config).map_err(ApiError::from)?;

    let report = engine
        .run_manual(UpdateOptions {
            channel: None,
            check_only: true,
            force: false,
            enable_auto_update: false,
            disable_auto_update: false,
        })
        .await
        .map_err(ApiError::from)?;

    Ok(Json(report))
}

/// POST /api/update/install - Install an update.
#[utoipa::path(
    post,
    path = "/update/install",
    tag = "update",
    request_body = UpdateInstallRequest,
    responses(
        (status = 200, description = "Result of the install attempt", body = UpdateReport),
    ),
)]
pub async fn install_update(
    Json(request): Json<UpdateInstallRequest>,
) -> ApiResult<Json<UpdateReport>> {
    let config = UpdateConfig::detect(request.channel.clone()).map_err(ApiError::from)?;
    let engine = UpdateEngine::new(config).map_err(ApiError::from)?;

    let report = engine
        .run_manual(UpdateOptions {
            channel: request.channel,
            check_only: false,
            force: request.force.unwrap_or(false),
            enable_auto_update: false,
            disable_auto_update: false,
        })
        .await
        .map_err(ApiError::from)?;

    Ok(Json(report))
}

/// GET /api/update/auto - Read the current auto-update flag.
#[utoipa::path(
    get,
    path = "/update/auto",
    tag = "update",
    responses(
        (status = 200, description = "Whether auto-update is enabled", body = AutoUpdateState),
    ),
)]
pub async fn get_auto_update() -> ApiResult<Json<AutoUpdateState>> {
    let config = UpdateConfig::detect(None).map_err(ApiError::from)?;
    let engine = UpdateEngine::new(config).map_err(ApiError::from)?;
    let enabled = engine.get_auto_update().await.map_err(ApiError::from)?;
    Ok(Json(AutoUpdateState { enabled }))
}

/// PUT /api/update/auto - Enable or disable auto-update.
#[utoipa::path(
    put,
    path = "/update/auto",
    tag = "update",
    request_body = AutoUpdateRequest,
    responses(
        (status = 200, description = "Auto-update flag after the change", body = AutoUpdateResponse),
    ),
)]
pub async fn set_auto_update(
    Json(request): Json<AutoUpdateRequest>,
) -> ApiResult<Json<AutoUpdateResponse>> {
    let config = UpdateConfig::detect(None).map_err(ApiError::from)?;
    let engine = UpdateEngine::new(config).map_err(ApiError::from)?;

    let state = engine
        .set_auto_update(request.enabled)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(AutoUpdateResponse {
        success: true,
        auto_update: state.auto_update,
        message: if state.auto_update {
            "Auto-update enabled".to_string()
        } else {
            "Auto-update disabled".to_string()
        },
    }))
}
