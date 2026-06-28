//! Local transcription model management routes.
//!
//! `GET /models` lists the catalog with install + download state; `POST
//! /models/{id}/download` kicks off a background download; `GET /models/{id}`
//! reports a single model's status (poll this for download progress). The
//! daemon owns the models directory, so the CLI and web UI drive everything
//! here over HTTP.

use crate::api::error::{ApiError, ApiResult};
use crate::transcription::models::{self, ModelDescriptor};
use axum::{
    extract::Path,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Serialize;
use utoipa::ToSchema;

/// Response for `GET /models`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ModelsListResponse {
    pub models: Vec<ModelDescriptor>,
}

pub fn router() -> Router {
    Router::new()
        .route("/", get(list_models))
        .route("/:id", get(get_model))
        .route("/:id/download", post(download_model))
}

/// List all local-transcription models with install + download status.
#[utoipa::path(
    get,
    path = "/models",
    tag = "models",
    operation_id = "list_models",
    responses(
        (status = 200, description = "Available local models", body = ModelsListResponse),
    ),
)]
pub async fn list_models() -> ApiResult<Json<ModelsListResponse>> {
    let models = models::list().map_err(ApiError::from)?;
    Ok(Json(ModelsListResponse { models }))
}

/// Get one model's status (poll for download progress).
#[utoipa::path(
    get,
    path = "/models/{id}",
    tag = "models",
    operation_id = "get_model",
    params(("id" = String, Path, description = "Model id")),
    responses(
        (status = 200, description = "Model status", body = ModelDescriptor),
        (status = 404, description = "Unknown model"),
    ),
)]
pub async fn get_model(Path(id): Path<String>) -> ApiResult<Json<ModelDescriptor>> {
    match models::describe(&id).map_err(ApiError::from)? {
        Some(descriptor) => Ok(Json(descriptor)),
        None => Err(ApiError::not_found(format!("Unknown model '{id}'"))),
    }
}

/// Start downloading a model in the background. Idempotent — returns the
/// model's current status; poll `GET /models/{id}` for progress.
#[utoipa::path(
    post,
    path = "/models/{id}/download",
    tag = "models",
    operation_id = "download_model",
    params(("id" = String, Path, description = "Model id")),
    responses(
        (status = 200, description = "Download started or already present", body = ModelDescriptor),
        (status = 404, description = "Unknown model"),
    ),
)]
pub async fn download_model(Path(id): Path<String>) -> ApiResult<Json<ModelDescriptor>> {
    models::start_download(&id).map_err(ApiError::from)?;
    let descriptor = models::describe(&id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("Unknown model '{id}'")))?;
    Ok(Json(descriptor))
}
