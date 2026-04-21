//! Update API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::update::{UpdateConfig, UpdateEngine, UpdateOptions, UpdateReport};
use axum::{
    response::Json,
    routing::{get, post, put},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// Request body for update install.
#[derive(Debug, Deserialize, Default)]
pub struct UpdateInstallRequest {
    /// Channel override (e.g., "stable", "beta")
    pub channel: Option<String>,
    /// Force update even if versions match
    pub force: Option<bool>,
}

/// Request body for auto-update toggle.
#[derive(Debug, Deserialize)]
pub struct AutoUpdateRequest {
    /// Enable or disable auto-update
    pub enabled: bool,
}

/// Create the update router.
pub fn router() -> Router {
    Router::new()
        .route("/check", get(check_update))
        .route("/install", post(install_update))
        .route("/auto", put(set_auto_update))
}

/// GET /update/check - Check for available updates.
async fn check_update() -> ApiResult<Json<UpdateReport>> {
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

/// POST /update/install - Install an update.
async fn install_update(
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

/// PUT /update/auto - Enable or disable auto-update.
async fn set_auto_update(Json(request): Json<AutoUpdateRequest>) -> ApiResult<Json<Value>> {
    let config = UpdateConfig::detect(None).map_err(ApiError::from)?;
    let engine = UpdateEngine::new(config).map_err(ApiError::from)?;

    let state = engine
        .set_auto_update(request.enabled)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(json!({
        "success": true,
        "auto_update": state.auto_update,
        "message": if state.auto_update {
            "Auto-update enabled"
        } else {
            "Auto-update disabled"
        },
    })))
}
