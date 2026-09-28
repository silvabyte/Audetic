//! Audio note artifact API; database location is injected into the router.
use axum::{
    extract::{Path, State},
    response::Json,
    routing::get,
    Router,
};
use serde::Serialize;
use std::path::PathBuf;
use utoipa::ToSchema;

use crate::api::error::{ApiError, ApiResult};
use crate::audio_note_artifacts::{
    generate_audio_note_artifact, GenerateArtifactRequest, GenerateArtifactResponse,
};
use crate::db::audio_note_artifacts::{AudioNoteArtifact, AudioNoteArtifactRepository};

pub fn router(db_path: PathBuf) -> Router {
    Router::new()
        .route(
            "/audio-notes/:id/artifacts",
            get(list_audio_note_artifacts).post(generate_artifact),
        )
        .route(
            "/audio-notes/:id/artifacts/:artifact_id",
            get(get_audio_note_artifact).delete(delete_audio_note_artifact),
        )
        .with_state(db_path)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AudioNoteArtifactsResponse {
    pub artifacts: Vec<AudioNoteArtifact>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteArtifactResponse {
    pub success: bool,
    pub id: i64,
}

#[utoipa::path(get, path = "/audio-notes/{id}/artifacts", tag = "audio_note_artifacts",
    params(("id" = i64, Path, description = "Audio note id")),
    responses((status = 200, description = "Audio note artifacts", body = AudioNoteArtifactsResponse)))]
pub async fn list_audio_note_artifacts(
    State(db_path): State<PathBuf>,
    Path(id): Path<i64>,
) -> ApiResult<Json<AudioNoteArtifactsResponse>> {
    let artifacts = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        AudioNoteArtifactRepository::list_for_live_note(&conn, id)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::from)?;
    Ok(Json(AudioNoteArtifactsResponse { artifacts }))
}

#[utoipa::path(post, path = "/audio-notes/{id}/artifacts", tag = "audio_note_artifacts",
    params(("id" = i64, Path, description = "Audio note id")), request_body = GenerateArtifactRequest,
    responses((status = 200, description = "Generated artifact", body = GenerateArtifactResponse),
        (status = 400, description = "Invalid request or generation failed; failed artifact remains available")))]
pub async fn generate_artifact(
    State(db_path): State<PathBuf>,
    Path(id): Path<i64>,
    Json(request): Json<GenerateArtifactRequest>,
) -> ApiResult<Json<GenerateArtifactResponse>> {
    let artifact = generate_audio_note_artifact(id, request, db_path)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(Json(GenerateArtifactResponse { artifact }))
}

#[utoipa::path(get, path = "/audio-notes/{id}/artifacts/{artifact_id}", tag = "audio_note_artifacts",
    params(("id" = i64, Path, description = "Audio note id"), ("artifact_id" = i64, Path, description = "Artifact id")),
    responses((status = 200, description = "Artifact", body = AudioNoteArtifact), (status = 404, description = "Not found")))]
pub async fn get_audio_note_artifact(
    State(db_path): State<PathBuf>,
    Path((id, artifact_id)): Path<(i64, i64)>,
) -> ApiResult<Json<AudioNoteArtifact>> {
    let artifact = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        AudioNoteArtifactRepository::get_for_live_note(&conn, id, artifact_id)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::from)?
    .ok_or_else(|| ApiError::not_found("Artifact not found"))?;
    Ok(Json(artifact))
}

#[utoipa::path(delete, path = "/audio-notes/{id}/artifacts/{artifact_id}", tag = "audio_note_artifacts",
    params(("id" = i64, Path, description = "Audio note id"), ("artifact_id" = i64, Path, description = "Artifact id")),
    responses((status = 200, description = "Deleted", body = DeleteArtifactResponse), (status = 404, description = "Not found")))]
pub async fn delete_audio_note_artifact(
    State(db_path): State<PathBuf>,
    Path((id, artifact_id)): Path<(i64, i64)>,
) -> ApiResult<Json<DeleteArtifactResponse>> {
    let deleted = tokio::task::spawn_blocking(move || {
        let conn = crate::db::init_db_at(&db_path)?;
        AudioNoteArtifactRepository::delete_for_live_note(&conn, id, artifact_id)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map_err(ApiError::from)?;
    if !deleted {
        return Err(ApiError::not_found("Artifact not found"));
    }
    Ok(Json(DeleteArtifactResponse {
        success: true,
        id: artifact_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::audio_notes::AudioNoteRepository;
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn unified_routes_use_injected_database_and_hide_deleted_parents() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("artifacts.db");
        let conn = crate::db::init_db_at(&db_path).unwrap();
        let note = AudioNoteRepository::insert(
            &conn,
            None,
            &dir.path().join("audio.wav").to_string_lossy(),
        )
        .unwrap();
        AudioNoteRepository::complete(
            &conn,
            note,
            &dir.path().join("transcript.txt").to_string_lossy(),
            "Raw transcript",
            None,
            3,
        )
        .unwrap();
        let artifact = AudioNoteArtifactRepository::insert_pending(
            &conn,
            note,
            "intent",
            "Intent",
            Some("request_intent"),
            None,
        )
        .unwrap();
        let data = serde_json::json!({"intent":"buy milk"});
        AudioNoteArtifactRepository::complete_with_json(
            &conn,
            artifact,
            "# Intent",
            Some(&data),
            "",
            "",
        )
        .unwrap();
        let app = router(db_path);
        let uri = format!("/audio-notes/{note}/artifacts/{artifact}");
        let response = app
            .clone()
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(json["note_id"], note);
        assert_eq!(json["content_json"], data);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/meetings/{note}/artifacts"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        AudioNoteRepository::soft_delete(&conn, note).unwrap();
        let response = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
