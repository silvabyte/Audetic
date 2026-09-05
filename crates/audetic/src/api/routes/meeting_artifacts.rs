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
    library: Option<Arc<crate::sync::shared_library::SharedLibrary>>,
}

pub fn router(library: Option<Arc<crate::sync::shared_library::SharedLibrary>>) -> Router {
    let library = library.or_else(|| {
        crate::sync::SyncService::default_local_library()
            .ok()
            .map(|system| Arc::new(system.library()))
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
        .with_state(MeetingArtifactState { library })
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
    let library = state
        .library
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    let artifacts = library.artifacts(record_id).await.map_err(ApiError::from)?;
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
    let library = state
        .library
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    let artifact = library
        .generate_artifact(record_id, request)
        .await
        .map_err(ApiError::from)?;
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
    let library = state
        .library
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    let artifact = library
        .artifact(meeting_record_id, artifact_record_id)
        .await
        .map_err(ApiError::from)?;
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
    let library = state
        .library
        .ok_or_else(|| ApiError::internal("Shared Library service unavailable"))?;
    library
        .delete_artifact(meeting_record_id, artifact_record_id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(DeleteArtifactResponse {
        success: true,
        id: artifact_record_id,
    }))
}
