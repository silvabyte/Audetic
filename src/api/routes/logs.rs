//! Logs API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::logs::{self, LogsOptions, LogsResult};
use axum::{extract::Query, response::Json, routing::get, Router};
use serde::Deserialize;

/// Query parameters for logs.
#[derive(Debug, Deserialize, Default)]
pub struct LogsQueryParams {
    /// Number of log entries (default 30)
    pub lines: Option<usize>,
}

/// Create the logs router.
pub fn router() -> Router {
    Router::new().route("/", get(get_logs))
}

/// GET /logs - Get application and transcription logs.
async fn get_logs(Query(params): Query<LogsQueryParams>) -> ApiResult<Json<LogsResult>> {
    let options = LogsOptions::new(params.lines.unwrap_or(30));
    let result = logs::get_logs(&options).map_err(ApiError::from)?;
    Ok(Json(result))
}
