//! Local coding-agent profile API.

use axum::{
    extract::Path,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::error::{ApiError, ApiResult};
use crate::db::agent_profiles::{AgentProfile, AgentProfileRepository};

pub fn router() -> Router {
    Router::new()
        .route("/agent-profiles", get(list_agent_profiles))
        .route("/agent-profiles/:id/test", post(test_agent_profile))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AgentProfilesResponse {
    pub profiles: Vec<AgentProfile>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AgentProfileTestResponse {
    pub id: i64,
    pub available: bool,
    pub executable: String,
    pub resolved_path: Option<String>,
    pub message: String,
}

#[utoipa::path(
    get,
    path = "/agent-profiles",
    tag = "agents",
    responses(
        (status = 200, description = "Configured local agent CLI profiles", body = AgentProfilesResponse),
    ),
)]
pub async fn list_agent_profiles() -> ApiResult<Json<AgentProfilesResponse>> {
    let profiles = tokio::task::spawn_blocking(|| -> anyhow::Result<Vec<AgentProfile>> {
        let conn = crate::db::init_db()?;
        AgentProfileRepository::ensure_builtin_profiles(&conn)?;
        AgentProfileRepository::list(&conn)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?;
    Ok(Json(AgentProfilesResponse { profiles }))
}

#[utoipa::path(
    post,
    path = "/agent-profiles/{id}/test",
    tag = "agents",
    params(("id" = i64, Path, description = "Agent profile id")),
    responses(
        (status = 200, description = "Agent executable availability", body = AgentProfileTestResponse),
        (status = 404, description = "Agent profile not found"),
    ),
)]
pub async fn test_agent_profile(Path(id): Path<i64>) -> ApiResult<Json<AgentProfileTestResponse>> {
    let profile = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<AgentProfile>> {
        let conn = crate::db::init_db()?;
        AgentProfileRepository::ensure_builtin_profiles(&conn)?;
        AgentProfileRepository::get(&conn, id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?
    .ok_or_else(|| ApiError::not_found(format!("Agent profile {id} not found")))?;

    let resolved = which::which(&profile.executable).ok();
    let available = resolved.is_some();
    Ok(Json(AgentProfileTestResponse {
        id,
        available,
        executable: profile.executable.clone(),
        resolved_path: resolved.map(|p| p.to_string_lossy().into_owned()),
        message: if available {
            format!("{} is available", profile.name)
        } else {
            format!(
                "{} executable `{}` was not found on PATH",
                profile.name, profile.executable
            )
        },
    }))
}
