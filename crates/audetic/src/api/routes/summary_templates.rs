//! Summary template API.

use axum::{response::Json, routing::get, Router};
use serde::Serialize;
use utoipa::ToSchema;

use crate::summary_templates::{list_templates, SummaryTemplate};

pub fn router() -> Router {
    Router::new().route("/summary/templates", get(list_summary_templates))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SummaryTemplatesResponse {
    pub templates: Vec<SummaryTemplate>,
}

#[utoipa::path(
    get,
    path = "/summary/templates",
    tag = "summary_templates",
    responses(
        (status = 200, description = "Built-in summary templates", body = SummaryTemplatesResponse),
    ),
)]
pub async fn list_summary_templates() -> Json<SummaryTemplatesResponse> {
    Json(SummaryTemplatesResponse {
        templates: list_templates(),
    })
}
