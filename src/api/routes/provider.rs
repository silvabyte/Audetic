//! Provider API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::transcription::{get_provider_info, get_provider_status, ProviderInfo, ProviderStatus};
use axum::{response::Json, routing::get, Router};

/// Create the provider router.
pub fn router() -> Router {
    Router::new()
        .route("/", get(get_config))
        .route("/status", get(get_status))
}

/// GET /provider - Get provider configuration.
async fn get_config() -> ApiResult<Json<ProviderInfo>> {
    let info = get_provider_info().map_err(ApiError::from)?;
    Ok(Json(info))
}

/// GET /provider/status - Get provider status and health.
async fn get_status() -> ApiResult<Json<ProviderStatus>> {
    let status = get_provider_status().map_err(ApiError::from)?;
    Ok(Json(status))
}
