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
use utoipa::IntoParams;

#[derive(Clone, Default)]
pub struct HistoryApiState {
    library: Option<Arc<crate::sync::shared_library::SharedLibrary>>,
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
pub fn router(library: Option<Arc<crate::sync::shared_library::SharedLibrary>>) -> Router {
    let library = library.or_else(|| {
        crate::sync::SyncService::default_local_library()
            .ok()
            .map(|system| Arc::new(system.library()))
    });
    Router::new()
        .route("/", get(list_history))
        .route("/:id", get(get_history_by_id))
        .route("/:id/audio", get(history_audio))
        .with_state(HistoryApiState { library })
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
    let Some(library) = state.library else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    match library
        .payload(crate::sync::shared_library::PayloadRequest {
            id,
            kind: crate::sync::protocol::RecordKind::Dictation,
            range,
        })
        .await
    {
        Ok(source) => super::payload::serve(source),
        Err(error) => ApiError::from(error).into_response(),
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

    let entries = if let Some(library) = state.library {
        library
            .dictations(&search_params)
            .await
            .map_err(ApiError::from)?
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
    let entry = if let Some(library) = state.library {
        library.dictation(id).await.map_err(ApiError::from)?
    } else {
        history::get_by_id(id)
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::not_found(format!("Transcription {} not found", id)))?
    };

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
        let library = Arc::new(crate::sync::SyncService::production(db_path).library());
        let response = router(Some(library))
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
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/wav");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "4");
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "2345"
        );
    }

    #[tokio::test]
    async fn library_route_errors_never_expose_database_paths() {
        let private_path = "/private/audetic-review-secret/missing.sqlite";
        let library =
            Arc::new(crate::sync::SyncService::local_library(private_path.into()).library());

        let response = router(Some(library))
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(!body.contains(private_path));
        assert!(body.contains("Shared Library operation failed"));
    }
}
