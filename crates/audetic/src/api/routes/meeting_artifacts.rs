//! Meeting artifact API.

use axum::{extract::Path, response::Json, routing::get, Router};
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::error::{ApiError, ApiResult};
use crate::db::meeting_artifacts::{MeetingArtifact, MeetingArtifactRepository};
use crate::meeting_artifacts::{
    generate_meeting_artifact, GenerateArtifactRequest, GenerateArtifactResponse,
};

pub fn router() -> Router {
    Router::new()
        .route(
            "/meetings/:id/artifacts",
            get(list_meeting_artifacts).post(generate_artifact),
        )
        .route(
            "/meetings/:id/artifacts/:artifact_id",
            get(get_meeting_artifact).delete(delete_meeting_artifact),
        )
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingArtifactsResponse {
    pub artifacts: Vec<MeetingArtifact>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteArtifactResponse {
    pub success: bool,
    pub id: i64,
}

#[utoipa::path(
    get,
    path = "/meetings/{id}/artifacts",
    tag = "meeting_artifacts",
    params(("id" = i64, Path, description = "Meeting id")),
    responses(
        (status = 200, description = "Artifacts generated for a meeting", body = MeetingArtifactsResponse),
    ),
)]
pub async fn list_meeting_artifacts(
    Path(id): Path<i64>,
) -> ApiResult<Json<MeetingArtifactsResponse>> {
    let artifacts = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        MeetingArtifactRepository::list_for_meeting(&conn, id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?;
    Ok(Json(MeetingArtifactsResponse { artifacts }))
}

#[utoipa::path(
    post,
    path = "/meetings/{id}/artifacts",
    tag = "meeting_artifacts",
    params(("id" = i64, Path, description = "Meeting id")),
    request_body = GenerateArtifactRequest,
    responses(
        (status = 200, description = "Generated artifact", body = GenerateArtifactResponse),
        (status = 400, description = "Meeting is not eligible or request is invalid"),
    ),
)]
pub async fn generate_artifact(
    Path(id): Path<i64>,
    Json(request): Json<GenerateArtifactRequest>,
) -> ApiResult<Json<GenerateArtifactResponse>> {
    let artifact = generate_meeting_artifact(id, request)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(GenerateArtifactResponse { artifact }))
}

#[utoipa::path(
    get,
    path = "/meetings/{id}/artifacts/{artifact_id}",
    tag = "meeting_artifacts",
    params(
        ("id" = i64, Path, description = "Meeting id"),
        ("artifact_id" = i64, Path, description = "Artifact id"),
    ),
    responses(
        (status = 200, description = "Meeting artifact", body = MeetingArtifact),
        (status = 404, description = "Artifact not found"),
    ),
)]
pub async fn get_meeting_artifact(
    Path((id, artifact_id)): Path<(i64, i64)>,
) -> ApiResult<Json<MeetingArtifact>> {
    let artifact =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<MeetingArtifact>> {
            let conn = crate::db::init_db()?;
            let artifact = MeetingArtifactRepository::get(&conn, artifact_id)?;
            Ok(artifact.filter(|a| a.meeting_id == id))
        })
        .await
        .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("Artifact {artifact_id} not found")))?;
    Ok(Json(artifact))
}

#[utoipa::path(
    delete,
    path = "/meetings/{id}/artifacts/{artifact_id}",
    tag = "meeting_artifacts",
    params(
        ("id" = i64, Path, description = "Meeting id"),
        ("artifact_id" = i64, Path, description = "Artifact id"),
    ),
    responses(
        (status = 200, description = "Deleted artifact", body = DeleteArtifactResponse),
        (status = 404, description = "Artifact not found"),
    ),
)]
pub async fn delete_meeting_artifact(
    Path((id, artifact_id)): Path<(i64, i64)>,
) -> ApiResult<Json<DeleteArtifactResponse>> {
    let deleted = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db()?;
        MeetingArtifactRepository::delete_for_meeting(&conn, id, artifact_id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?;
    if !deleted {
        return Err(ApiError::not_found(format!(
            "Artifact {artifact_id} not found"
        )));
    }
    Ok(Json(DeleteArtifactResponse {
        success: true,
        id: artifact_id,
    }))
}
