//! One-shot file transcription route.
//!
//! `POST /transcribe` accepts a multipart upload (a `file` part) and transcribes
//! it with whatever provider is configured, returning the text. This exists so
//! the slim CLI — which can't link the transcription engine (crate boundary) —
//! can run on-device transcription through the daemon. Cloud providers also work
//! here, but the CLI only routes through this endpoint when the local engine is
//! selected; otherwise it talks to the cloud jobs API directly.

use crate::api::error::{ApiError, ApiResult};
use axum::{extract::Multipart, response::Json, routing::post, Router};
use serde::Serialize;
use std::path::Path;
use tokio::io::AsyncWriteExt;
use utoipa::ToSchema;

/// Response for `POST /transcribe`.
#[derive(Debug, Serialize, ToSchema)]
pub struct TranscribeResponse {
    pub text: String,
}

pub fn router() -> Router {
    Router::new().route("/transcribe", post(transcribe))
}

/// Transcribe an uploaded audio file using the configured provider.
#[utoipa::path(
    post,
    path = "/transcribe",
    tag = "transcribe",
    operation_id = "transcribe_file",
    request_body(
        content = String,
        description = "multipart/form-data with a `file` part",
        content_type = "multipart/form-data"
    ),
    responses(
        (status = 200, description = "Transcribed text", body = TranscribeResponse),
        (status = 400, description = "Missing or unreadable file"),
    ),
)]
pub async fn transcribe(mut multipart: Multipart) -> ApiResult<Json<TranscribeResponse>> {
    let mut temp_path: Option<std::path::PathBuf> = None;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("Malformed multipart upload: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }

        // Preserve the original extension so format detection works.
        let ext = field
            .file_name()
            .and_then(|name| {
                Path::new(name)
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "wav".to_string());
        let path =
            std::env::temp_dir().join(format!("audetic-transcribe-{}.{ext}", uuid::Uuid::new_v4()));

        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| ApiError::internal(format!("Failed to stage upload: {e}")))?;
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| ApiError::bad_request(format!("Failed reading upload: {e}")))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|e| ApiError::internal(format!("Failed writing upload: {e}")))?;
        }
        file.flush().await.ok();
        temp_path = Some(path);
        break;
    }

    let path = temp_path.ok_or_else(|| ApiError::bad_request("Missing required `file` part"))?;

    let result = crate::transcription::transcribe_with_configured_provider(&path).await;
    let _ = tokio::fs::remove_file(&path).await;

    let text = result.map_err(ApiError::from)?;
    Ok(Json(TranscribeResponse { text }))
}
