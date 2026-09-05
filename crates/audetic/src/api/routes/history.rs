//! History API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::history::{self, HistoryEntry, SearchParams};
use audetic_core::sync::RecordId;
use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use utoipa::IntoParams;

#[derive(Clone, Default)]
pub struct HistoryApiState {
    service: Option<Arc<crate::sync::SyncService>>,
}

/// Query parameters for history search.
#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct HistoryQueryParams {
    /// Search query
    pub q: Option<String>,
    /// Start date (YYYY-MM-DD)
    pub from: Option<String>,
    /// End date (YYYY-MM-DD)
    pub to: Option<String>,
    /// Maximum results (default 20)
    pub limit: Option<usize>,
    /// Number of canonical merged results to skip.
    pub offset: Option<usize>,
}

/// Create the history router.
pub fn router(service: Option<Arc<crate::sync::SyncService>>) -> Router {
    Router::new()
        .route("/", get(list_history))
        .route("/:id", get(get_history_by_id))
        .route("/:id/audio", get(history_audio))
        .with_state(HistoryApiState { service })
}

#[utoipa::path(
    get,
    path = "/history/{id}/audio",
    tag = "history",
    params(("id" = String, Path, description = "Stable transcription UUID")),
    responses(
        (status = 200, description = "Recording Payload bytes"),
        (status = 206, description = "Recording Payload byte range"),
        (status = 404, description = "Recording Payload unavailable")
    )
)]
pub async fn history_audio(
    State(state): State<HistoryApiState>,
    Path(id): Path<RecordId>,
    request: axum::extract::Request,
) -> Response {
    let db_path = state
        .service
        .as_ref()
        .map(|service| service.db_path().to_path_buf())
        .or_else(|| crate::global::db_file().ok());
    if let Some(db_path) = db_path {
        let local =
            tokio::task::spawn_blocking(move || -> anyhow::Result<Option<std::path::PathBuf>> {
                let conn = crate::db::open_db_at(&db_path)?;
                let workflow = crate::db::get_workflow_by_sync_id(&conn, id)?;
                let Some(workflow) = workflow else {
                    return Ok(None);
                };
                match workflow.data {
                    crate::db::WorkflowData::VoiceToText(data) => {
                        crate::sync::payload::resolve_operational_audio(std::path::Path::new(
                            &data.audio_path,
                        ))
                        .map_err(Into::into)
                    }
                }
            })
            .await;
        if let Ok(Ok(Some(path))) = local {
            return ServeFile::new(path)
                .oneshot(request)
                .await
                .map(IntoResponse::into_response)
                .unwrap_or_else(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    }
    let Some(service) = state.service else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    match service
        .payload(
            id,
            crate::sync::protocol::RecordKind::Dictation,
            range.as_deref(),
        )
        .await
    {
        Ok(Some(source)) => super::payload::serve(source, request).await,
        Ok(None) => axum::http::StatusCode::NOT_FOUND.into_response(),
        Err(_) => axum::http::StatusCode::BAD_GATEWAY.into_response(),
    }
}

/// List transcription history.
#[utoipa::path(
    get,
    path = "/history",
    tag = "history",
    params(HistoryQueryParams),
    responses(
        (status = 200, description = "Transcription entries matching the query", body = Vec<HistoryEntry>),
    ),
)]
pub async fn list_history(
    State(state): State<HistoryApiState>,
    Query(params): Query<HistoryQueryParams>,
) -> ApiResult<Json<Vec<HistoryEntry>>> {
    let search_params = SearchParams {
        query: params.q,
        from: params.from,
        to: params.to,
        limit: params.limit.unwrap_or(20),
        offset: params.offset.unwrap_or(0),
    };

    let entries = if let Some(service) = state.service {
        service
            .history(&search_params)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
    } else {
        history::search(&search_params).map_err(ApiError::from)?
    };
    Ok(Json(entries))
}

/// Get a single transcription.
#[utoipa::path(
    get,
    path = "/history/{id}",
    tag = "history",
    params(
        ("id" = String, Path, description = "Stable transcription UUID"),
    ),
    responses(
        (status = 200, description = "Transcription entry", body = HistoryEntry),
        (status = 404, description = "Not found"),
    ),
)]
pub async fn get_history_by_id(
    State(state): State<HistoryApiState>,
    Path(id): Path<RecordId>,
) -> ApiResult<Json<HistoryEntry>> {
    let entry = if let Some(service) = state.service {
        service
            .history_entry(id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?
    } else {
        history::get_by_id(id).map_err(ApiError::from)?
    }
    .ok_or_else(|| ApiError::not_found(format!("Transcription {} not found", id)))?;

    Ok(Json(entry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;

    #[tokio::test]
    async fn audio_proxy_falls_back_to_verified_hub_association_and_preserves_range() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("audetic.db");
        let conn = crate::db::migrate_db_at(&db_path).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO sync_settings(singleton) VALUES(1)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE sync_settings SET role='home_hub' WHERE singleton=1",
            [],
        )
        .unwrap();
        drop(conn);
        let library = crate::sync::library::HubLibrary::new(db_path.clone());
        let record_id = RecordId::new();
        let bytes = b"0123456789";
        let checksum = format!("{:x}", Sha256::digest(bytes));
        library
            .apply_snapshots(vec![crate::sync::protocol::DictationSnapshot {
                kind: crate::sync::protocol::RecordKind::Dictation,
                schema_version: 1,
                record_id,
                origin_device_id: audetic_core::sync::DeviceId::new(),
                local_version: 1,
                created_at: "2026-09-04T10:00:00Z".into(),
                updated_at: "2026-09-04T10:00:00Z".into(),
                payload: crate::sync::protocol::DictationPayload {
                    text: "remote audio".into(),
                    recording_payload: crate::sync::protocol::RecordingPayloadDescriptor::pending(
                        checksum.clone(),
                        bytes.len() as u64,
                        "audio/wav".into(),
                    ),
                },
            }])
            .unwrap();
        library
            .accept_blob_stream(
                &checksum,
                bytes.len() as u64,
                "audio/wav",
                futures_util::stream::iter([Ok::<_, std::io::Error>(bytes::Bytes::from_static(
                    bytes,
                ))]),
            )
            .await
            .unwrap();
        let service = Arc::new(crate::sync::SyncService::production(db_path));
        let response = router(Some(service))
            .oneshot(
                Request::get(format!("/{record_id}/audio"))
                    .header(header::RANGE, "bytes=2-5")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "2345"
        );
    }
}
