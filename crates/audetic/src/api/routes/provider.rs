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

/// Get provider configuration.
#[utoipa::path(
    get,
    path = "/provider",
    tag = "provider",
    operation_id = "get_provider_config",
    responses(
        (status = 200, description = "Current provider configuration", body = ProviderInfo),
    ),
)]
pub async fn get_config() -> ApiResult<Json<ProviderInfo>> {
    let info = get_provider_info().map_err(ApiError::from)?;
    Ok(Json(info))
}

/// Get provider status and health.
#[utoipa::path(
    get,
    path = "/provider/status",
    tag = "provider",
    operation_id = "get_provider_status",
    responses(
        (status = 200, description = "Provider availability", body = ProviderStatus),
    ),
)]
pub async fn get_status() -> ApiResult<Json<ProviderStatus>> {
    let status = get_provider_status().map_err(ApiError::from)?;
    Ok(Json(status))
}
