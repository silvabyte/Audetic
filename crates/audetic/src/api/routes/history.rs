//! History API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::history::{self, HistoryEntry, SearchParams};
use audetic_core::sync::RecordId;
use axum::{
    extract::{Path, Query, State},
    response::Json,
    routing::get,
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use utoipa::IntoParams;

#[derive(Clone, Default)]
pub struct HistoryApiState {
    service: Option<Arc<crate::sync::SyncService>>,
}

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
    /// Number of canonical merged results to skip.
    pub offset: Option<usize>,
}

/// Create the history router.
pub fn router(service: Option<Arc<crate::sync::SyncService>>) -> Router {
    Router::new()
        .route("/", get(list_history))
        .route("/:id", get(get_history_by_id))
        .with_state(HistoryApiState { service })
}

/// List transcription history.
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
    State(state): State<HistoryApiState>,
    Query(params): Query<HistoryQueryParams>,
) -> ApiResult<Json<Vec<HistoryEntry>>> {
    let search_params = SearchParams {
        query: params.q,
        from: params.from,
        to: params.to,
        limit: params.limit.unwrap_or(20),
        offset: params.offset.unwrap_or(0),
    };

    let entries = if let Some(service) = state.service {
        service
            .history(&search_params)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
    } else {
        history::search(&search_params).map_err(ApiError::from)?
    };
    Ok(Json(entries))
}

/// Get a single transcription.
#[utoipa::path(
    get,
    path = "/history/{id}",
    tag = "history",
    params(
        ("id" = String, Path, description = "Stable transcription UUID"),
    ),
    responses(
        (status = 200, description = "Transcription entry", body = HistoryEntry),
        (status = 404, description = "Not found"),
    ),
)]
pub async fn get_history_by_id(
    State(state): State<HistoryApiState>,
    Path(id): Path<RecordId>,
) -> ApiResult<Json<HistoryEntry>> {
    let entry = if let Some(service) = state.service {
        service
            .history_entry(id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
    } else {
        history::get_by_id(id).map_err(ApiError::from)?
    }
    .ok_or_else(|| ApiError::not_found(format!("Transcription {} not found", id)))?;

    Ok(Json(entry))
}
