//! Logs API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::logs::{self, LogsOptions, LogsResult};
use axum::{extract::Query, response::Json, routing::get, Router};
use serde::Deserialize;
use utoipa::IntoParams;

/// Query parameters for logs.
#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct LogsQueryParams {
    /// Number of log entries (default 30)
    pub lines: Option<usize>,
}

/// Create the logs router.
pub fn router() -> Router {
    Router::new().route("/", get(get_logs))
}

/// GET /api/logs - Get application and transcription logs.
#[utoipa::path(
    get,
    path = "/logs",
    tag = "logs",
    params(LogsQueryParams),
    responses(
        (status = 200, description = "Combined app + transcription logs", body = LogsResult),
    ),
)]
pub async fn get_logs(Query(params): Query<LogsQueryParams>) -> ApiResult<Json<LogsResult>> {
    let options = LogsOptions::new(params.lines.unwrap_or(30));
    let result = logs::get_logs(&options).map_err(ApiError::from)?;
    Ok(Json(result))
}
