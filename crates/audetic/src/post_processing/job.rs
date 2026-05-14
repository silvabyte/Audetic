//! Job: a persisted (event, action, enabled) record.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::action::Action;
use super::event::EventKind;

/// A row from `post_processing_jobs`, materialized into a strong type.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Job {
    pub id: i64,
    pub name: String,
    pub event: EventKind,
    pub action: Action,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Request body for `POST /api/post-processing/jobs`. Same shape the
/// CLI builds when piping args from `audetic post-processing add`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct NewJob {
    pub name: String,
    pub event: EventKind,
    pub action: Action,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

/// Request body for `PATCH /api/post-processing/jobs/:id`. All fields
/// optional — only what's supplied is updated.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct UpdateJob {
    pub name: Option<String>,
    pub event: Option<EventKind>,
    pub action: Option<Action>,
    pub enabled: Option<bool>,
}

fn default_enabled() -> bool {
    true
}
