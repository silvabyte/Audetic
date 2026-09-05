//! Meeting artifact API.

use audetic_core::sync::RecordId;
use axum::{
    extract::{Path, State},
    response::Json,
    routing::get,
    Router,
};
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;

use crate::api::error::{ApiError, ApiResult};
use crate::db::meeting_artifacts::MeetingArtifact;
use crate::meeting_artifacts::{GenerateArtifactRequest, GenerateArtifactResponse};

#[derive(Clone)]
pub struct MeetingArtifactState {
    sync: Option<Arc<crate::sync::SyncService>>,
}

pub fn router(sync: Option<Arc<crate::sync::SyncService>>) -> Router {
    let sync = sync.or_else(|| {
        crate::sync::SyncService::default_local_library()
            .ok()
            .map(Arc::new)
    });
    Router::new()
        .route(
            "/meetings/:id/artifacts",
            get(list_meeting_artifacts).post(generate_artifact),
        )
        .route(
            "/meetings/:id/artifacts/:artifact_id",
            get(get_meeting_artifact).delete(delete_meeting_artifact),
        )
        .with_state(MeetingArtifactState { sync })
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MeetingArtifactsResponse {
    pub artifacts: Vec<MeetingArtifact>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteArtifactResponse {
    pub success: bool,
    pub id: RecordId,
}

#[utoipa::path(
    get,
    path = "/meetings/{id}/artifacts",
    tag = "meeting_artifacts",
    params(("id" = String, Path, description = "Meeting UUID")),
    responses(
        (status = 200, description = "Artifacts generated for a meeting", body = MeetingArtifactsResponse),
    ),
)]
pub async fn list_meeting_artifacts(
    Path(id): Path<String>,
    State(state): State<MeetingArtifactState>,
) -> ApiResult<Json<MeetingArtifactsResponse>> {
    let record_id: RecordId = id.parse().map_err(ApiError::bad_request)?;
    let sync = state
        .sync
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    let artifacts = sync
        .meeting_artifacts(record_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(Json(MeetingArtifactsResponse { artifacts }))
}

#[utoipa::path(
    post,
    path = "/meetings/{id}/artifacts",
    tag = "meeting_artifacts",
    params(("id" = String, Path, description = "Meeting UUID")),
    request_body = GenerateArtifactRequest,
    responses(
        (status = 200, description = "Generated artifact", body = GenerateArtifactResponse),
        (status = 400, description = "Meeting is not eligible or request is invalid"),
    ),
)]
pub async fn generate_artifact(
    Path(id): Path<String>,
    State(state): State<MeetingArtifactState>,
    Json(request): Json<GenerateArtifactRequest>,
) -> ApiResult<Json<GenerateArtifactResponse>> {
    let record_id: RecordId = id.parse().map_err(ApiError::bad_request)?;
    let sync = state
        .sync
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    let artifact = sync
        .generate_meeting_artifact(record_id, request)
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(Json(GenerateArtifactResponse { artifact }))
}

#[utoipa::path(
    get,
    path = "/meetings/{id}/artifacts/{artifact_id}",
    tag = "meeting_artifacts",
    params(
        ("id" = String, Path, description = "Meeting UUID"),
        ("artifact_id" = String, Path, description = "Artifact UUID"),
    ),
    responses(
        (status = 200, description = "Meeting artifact", body = MeetingArtifact),
        (status = 404, description = "Artifact not found"),
    ),
)]
pub async fn get_meeting_artifact(
    Path((id, artifact_id)): Path<(String, String)>,
    State(state): State<MeetingArtifactState>,
) -> ApiResult<Json<MeetingArtifact>> {
    let meeting_record_id: RecordId = id.parse().map_err(ApiError::bad_request)?;
    let artifact_record_id: RecordId = artifact_id.parse().map_err(ApiError::bad_request)?;
    let sync = state
        .sync
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    let artifact = sync
        .meeting_artifact(meeting_record_id, artifact_record_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("Artifact {artifact_record_id} not found")))?;
    Ok(Json(artifact))
}

#[utoipa::path(
    delete,
    path = "/meetings/{id}/artifacts/{artifact_id}",
    tag = "meeting_artifacts",
    params(
        ("id" = String, Path, description = "Meeting UUID"),
        ("artifact_id" = String, Path, description = "Artifact UUID"),
    ),
    responses(
        (status = 200, description = "Deleted artifact", body = DeleteArtifactResponse),
        (status = 404, description = "Artifact not found"),
    ),
)]
pub async fn delete_meeting_artifact(
    Path((id, artifact_id)): Path<(String, String)>,
    State(state): State<MeetingArtifactState>,
) -> ApiResult<Json<DeleteArtifactResponse>> {
    let meeting_record_id: RecordId = id.parse().map_err(ApiError::bad_request)?;
    let artifact_record_id: RecordId = artifact_id.parse().map_err(ApiError::bad_request)?;
    let sync = state
        .sync
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    match sync
        .delete_meeting_artifact(meeting_record_id, artifact_record_id)
        .await
        .map_err(|error| {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                error.to_string(),
            )
        })? {
        crate::sync::shared_library::ArtifactDeleteResult::Deleted => {}
        crate::sync::shared_library::ArtifactDeleteResult::NotFound => {
            return Err(ApiError::not_found(format!(
                "Artifact {artifact_record_id} not found"
            )));
        }
        crate::sync::shared_library::ArtifactDeleteResult::InFlight => {
            return Err(ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "Artifact is still being generated; wait for it to finish before deleting",
            ));
        }
    }
    Ok(Json(DeleteArtifactResponse {
        success: true,
        id: artifact_record_id,
    }))
}
