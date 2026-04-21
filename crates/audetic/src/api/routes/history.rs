//! History API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::history::{self, HistoryEntry, SearchParams};
use axum::{
    extract::{Path, Query},
    response::Json,
    routing::get,
    Router,
};
use serde::Deserialize;
use utoipa::IntoParams;

/// Query parameters for history search.
#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct HistoryQueryParams {
    /// Search query
    pub q: Option<String>,
    /// Start date (YYYY-MM-DD)
    pub from: Option<String>,
    /// End date (YYYY-MM-DD)
    pub to: Option<String>,
    /// Maximum results (default 20)
    pub limit: Option<usize>,
}

/// Create the history router.
pub fn router() -> Router {
    Router::new()
        .route("/", get(list_history))
        .route("/{id}", get(get_history_by_id))
}

/// GET /history - List transcription history.
#[utoipa::path(
    get,
    path = "/history",
    tag = "history",
    params(HistoryQueryParams),
    responses(
        (status = 200, description = "Transcription entries matching the query", body = Vec<HistoryEntry>),
    ),
)]
pub async fn list_history(
    Query(params): Query<HistoryQueryParams>,
) -> ApiResult<Json<Vec<HistoryEntry>>> {
    let search_params = SearchParams {
        query: params.q,
        from: params.from,
        to: params.to,
        limit: params.limit.unwrap_or(20),
    };

    let entries = history::search(&search_params).map_err(ApiError::from)?;
    Ok(Json(entries))
}

/// GET /history/:id - Get a single transcription.
#[utoipa::path(
    get,
    path = "/history/{id}",
    tag = "history",
    params(
        ("id" = i64, Path, description = "Transcription history id"),
    ),
    responses(
        (status = 200, description = "Transcription entry", body = HistoryEntry),
        (status = 404, description = "Not found"),
    ),
)]
pub async fn get_history_by_id(Path(id): Path<i64>) -> ApiResult<Json<HistoryEntry>> {
    let entry = history::get_by_id(id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("Transcription {} not found", id)))?;

    Ok(Json(entry))
}
