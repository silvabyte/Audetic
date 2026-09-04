//! Meeting artifact API.

use audetic_core::sync::RecordId;
use axum::{
    extract::{Path, State},
    response::Json,
    routing::get,
    Router,
};
use serde::Serialize;
use std::{collections::BTreeMap, sync::Arc};
use utoipa::ToSchema;

use crate::api::error::{ApiError, ApiResult};
use crate::db::meeting_artifacts::{
    ArtifactDeleteOutcome, MeetingArtifact, MeetingArtifactRepository,
};
use crate::meeting_artifacts::{
    generate_meeting_artifact, GenerateArtifactRequest, GenerateArtifactResponse,
};

#[derive(Clone)]
pub struct MeetingArtifactState {
    sync: Option<Arc<crate::sync::SyncService>>,
}

pub fn router(sync: Option<Arc<crate::sync::SyncService>>) -> Router {
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

fn resolve_meeting(value: &str) -> ApiResult<i64> {
    let id: RecordId = value.parse().map_err(ApiError::bad_request)?;
    let conn = crate::db::open_db().map_err(ApiError::from)?;
    crate::db::meetings::MeetingRepository::internal_id(&conn, id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("Meeting {id} not found")))
}
fn resolve_artifact(value: &str) -> ApiResult<(i64, RecordId)> {
    let id: RecordId = value.parse().map_err(ApiError::bad_request)?;
    let conn = crate::db::open_db().map_err(ApiError::from)?;
    let local = MeetingArtifactRepository::internal_id(&conn, id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("Artifact {id} not found")))?;
    Ok((local, id))
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
    if let Some(sync) = &state.sync {
        if let Some(meeting) = sync
            .meeting(record_id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
        {
            if meeting.source == "shared" {
                let mut artifacts = meeting
                    .artifacts
                    .into_iter()
                    .map(shared_artifact)
                    .map(|artifact| (artifact.id, artifact))
                    .collect::<BTreeMap<_, _>>();
                for artifact in local_shared_runs(sync.db_path(), record_id)? {
                    artifacts.insert(artifact.id, artifact);
                }
                let mut artifacts = artifacts.into_values().collect::<Vec<_>>();
                artifacts.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                return Ok(Json(MeetingArtifactsResponse { artifacts }));
            }
        }
    }
    let id = resolve_meeting(&id)?;
    let artifacts = tokio::task::spawn_blocking(move || {
        let conn = crate::db::open_db()?;
        MeetingArtifactRepository::list_for_live_meeting(&conn, id)
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
    if let Some(sync) = &state.sync {
        if let Some(meeting) = sync
            .meeting(record_id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
        {
            if meeting.offline && meeting.read_only {
                return Err(ApiError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "Home Hub is unavailable; shared artifacts are not queued offline",
                ));
            }
            if meeting.source == "shared" {
                let transcript = meeting
                    .transcript_text
                    .as_deref()
                    .ok_or_else(|| ApiError::bad_request("meeting has no transcript"))?;
                let artifact = crate::meeting_artifacts::generate_shared_meeting_artifact(
                    sync.db_path(),
                    record_id,
                    meeting.title.as_deref(),
                    transcript,
                    request,
                )
                .await
                .map_err(|error| ApiError::bad_request(error.to_string()))?;
                return Ok(Json(GenerateArtifactResponse { artifact }));
            }
        }
    }
    let id = resolve_meeting(&id)?;
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
    if let Some(sync) = &state.sync {
        if let Some(meeting) = sync
            .meeting(meeting_record_id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
        {
            if meeting.source == "shared" {
                if let Some(artifact) = local_shared_runs(sync.db_path(), meeting_record_id)?
                    .into_iter()
                    .find(|artifact| artifact.id == artifact_record_id)
                    .or_else(|| {
                        meeting
                            .artifacts
                            .into_iter()
                            .find(|artifact| artifact.record_id == artifact_record_id)
                            .map(shared_artifact)
                    })
                {
                    return Ok(Json(artifact));
                }
                return Err(ApiError::not_found(format!(
                    "Artifact {artifact_record_id} not found"
                )));
            }
        }
    }
    let id = resolve_meeting(&id)?;
    let (artifact_local_id, artifact_record_id) = resolve_artifact(&artifact_id)?;
    let artifact =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<MeetingArtifact>> {
            let conn = crate::db::open_db()?;
            MeetingArtifactRepository::get_for_live_meeting(&conn, id, artifact_local_id)
        })
        .await
        .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
        .map_err(ApiError::from)?
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
    if let Some(sync) = &state.sync {
        // Resolve and validate the complete parent/child relationship before
        // issuing an irreversible Home Hub tombstone. Looking up the artifact
        // UUID alone is not sufficient: a valid artifact under the wrong URL
        // parent must be a 404, not a deletion.
        let meeting = sync
            .meeting(meeting_record_id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
        let Some(meeting) = meeting else {
            return Err(ApiError::not_found(format!(
                "Meeting {meeting_record_id} not found"
            )));
        };
        let conn = crate::db::open_db_at(sync.db_path()).map_err(ApiError::from)?;
        let local_artifact = MeetingArtifactRepository::get_by_sync_id(&conn, artifact_record_id)
            .map_err(ApiError::from)?
            .filter(|artifact| artifact.meeting_id == meeting_record_id);
        let shared_run = MeetingArtifactRepository::list_shared_runs(&conn, meeting_record_id)
            .map_err(ApiError::from)?
            .into_iter()
            .find(|artifact| artifact.id == artifact_record_id);
        let belongs_to_meeting = local_artifact.is_some()
            || shared_run.is_some()
            || meeting
                .artifacts
                .iter()
                .any(|artifact| artifact.record_id == artifact_record_id);
        if !belongs_to_meeting {
            return Err(ApiError::not_found(format!(
                "Artifact {artifact_record_id} not found"
            )));
        }
        if local_artifact
            .as_ref()
            .map(|artifact| artifact.status)
            .or_else(|| shared_run.as_ref().map(|artifact| artifact.status))
            .is_some_and(|status| {
                matches!(
                    status,
                    crate::db::meeting_artifacts::ArtifactStatus::Pending
                        | crate::db::meeting_artifacts::ArtifactStatus::Running
                )
            })
        {
            return Err(ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "Artifact is still being generated; wait for it to finish before deleting",
            ));
        }
        if meeting.source == "shared" {
            sync.delete_shared_record(
                artifact_record_id,
                crate::sync::protocol::RecordKind::Artifact,
            )
            .await
            .map_err(|error| {
                ApiError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    error.to_string(),
                )
            })?;
            if let Some(local_artifact) = local_artifact {
                MeetingArtifactRepository::delete_for_live_meeting(
                    &conn,
                    local_artifact.local_meeting_id,
                    local_artifact.local_id,
                )
                .map_err(ApiError::from)?;
            }
            MeetingArtifactRepository::cleanup_after_shared_delete(
                &conn,
                meeting_record_id,
                artifact_record_id,
            )
            .map_err(ApiError::from)?;
            return Ok(Json(DeleteArtifactResponse {
                success: true,
                id: artifact_record_id,
            }));
        }
        if meeting.read_only {
            return Err(ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Home Hub is unavailable; shared artifact deletions are not queued offline",
            ));
        }
    }
    let id = resolve_meeting(&id)?;
    let (artifact_local_id, artifact_record_id) = resolve_artifact(&artifact_id)?;
    let outcome = tokio::task::spawn_blocking(move || {
        let conn = crate::db::open_db()?;
        MeetingArtifactRepository::delete_for_live_meeting_guarded(&conn, id, artifact_local_id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("db task panicked: {e}")))?
    .map_err(ApiError::from)?;
    match outcome {
        ArtifactDeleteOutcome::Deleted => {}
        ArtifactDeleteOutcome::NotFound => {
            return Err(ApiError::not_found(format!(
                "Artifact {artifact_record_id} not found"
            )));
        }
        ArtifactDeleteOutcome::InFlight => {
            return Err(ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "Artifact is still being generated; wait for it to finish before deleting",
            ));
        }
        ArtifactDeleteOutcome::RequiresHub => {
            let sync = state.sync.as_ref().ok_or_else(|| {
                ApiError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "Home Hub deletion is required",
                )
            })?;
            sync.delete_shared_record(
                artifact_record_id,
                crate::sync::protocol::RecordKind::Artifact,
            )
            .await
            .map_err(|error| {
                ApiError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    error.to_string(),
                )
            })?;
            let conn = crate::db::open_db_at(sync.db_path()).map_err(ApiError::from)?;
            if !MeetingArtifactRepository::delete_for_live_meeting(&conn, id, artifact_local_id)
                .map_err(ApiError::from)?
            {
                return Err(ApiError::internal(
                    "artifact disappeared during local cleanup",
                ));
            }
        }
    }
    Ok(Json(DeleteArtifactResponse {
        success: true,
        id: artifact_record_id,
    }))
}

fn shared_artifact(value: crate::sync::protocol::SharedArtifact) -> MeetingArtifact {
    MeetingArtifact {
        id: value.record_id,
        meeting_id: value.parent_record_id,
        local_id: 0,
        local_meeting_id: 0,
        origin_device_id: value.origin_device_id,
        sync_version: value.local_version,
        kind: value.artifact_kind,
        title: value.title,
        template_id: value.template_id,
        agent_profile_id: None,
        status: crate::db::meeting_artifacts::ArtifactStatus::Completed,
        content_markdown: Some(value.content_markdown),
        error: None,
        stdout: None,
        stderr: None,
        created_at: value.created_at,
        updated_at: value.updated_at,
        completed_at: Some(value.completed_at),
    }
}

fn local_shared_runs(
    db_path: &std::path::Path,
    parent: RecordId,
) -> anyhow::Result<Vec<MeetingArtifact>> {
    let conn = crate::db::open_db_at(db_path)?;
    MeetingArtifactRepository::list_shared_runs(&conn, parent)
}
