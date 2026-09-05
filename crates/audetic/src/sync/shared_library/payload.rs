//! Recording Payload resolution and bounded streaming.

use futures_util::TryStreamExt;

use std::io::SeekFrom;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{LibraryError, LibraryPayload, LibraryResult, PayloadRequest, SharedLibrary};
use crate::sync::transition::LibraryRole;
use crate::sync::transport::{PayloadBody, PayloadContentRange, PayloadMetadata};

impl SharedLibrary {
    pub async fn payload(&self, request: PayloadRequest) -> LibraryResult<LibraryPayload> {
        let context = self.context()?;
        if let Some(path) = operational_payload_path(&context.db_path, request.id, request.kind)? {
            return open_payload(path, None, None, request.range.as_deref()).await;
        }
        match &context.role {
            LibraryRole::Standalone => Err(payload_not_found(request.id)),
            LibraryRole::HomeHub => {
                let blob = crate::sync::library::HubLibrary::new(context.db_path)
                    .payload(request.id, request.kind)
                    .map_err(|error| {
                        LibraryError::internal("resolving authoritative Recording Payload", error)
                    })?
                    .ok_or_else(|| payload_not_found(request.id))?;
                open_payload(
                    blob.canonical_path,
                    Some(&blob.media_type),
                    Some(blob.byte_size),
                    request.range.as_deref(),
                )
                .await
            }
            LibraryRole::ConnectedDevice { hub } => context
                .capabilities
                .payloads()
                .stream_payload(hub, request.id, request.kind, request.range.as_deref())
                .await
                .map(Into::into)
                .map_err(super::mutations::map_remote_error),
        }
    }
}

fn payload_not_found(id: audetic_core::sync::RecordId) -> LibraryError {
    LibraryError::NotFound(format!("Recording Payload for {id} not found"))
}

fn operational_payload_path(
    db_path: &Path,
    id: audetic_core::sync::RecordId,
    kind: crate::sync::protocol::RecordKind,
) -> LibraryResult<Option<PathBuf>> {
    let connection = crate::db::open_db_at(db_path)
        .map_err(|error| LibraryError::internal("opening Recording Payload library", error))?;
    let stored = match kind {
        crate::sync::protocol::RecordKind::Dictation => {
            crate::db::get_workflow_by_sync_id(&connection, id)
                .map_err(|error| LibraryError::internal("resolving dictation audio", error))?
                .map(|workflow| match workflow.data {
                    crate::db::WorkflowData::VoiceToText(data) => data.audio_path,
                })
        }
        crate::sync::protocol::RecordKind::Meeting => {
            crate::db::meetings::MeetingRepository::get_by_sync_id(&connection, id)
                .map_err(|error| LibraryError::internal("resolving meeting audio", error))?
                .map(|meeting| meeting.audio_path)
        }
        crate::sync::protocol::RecordKind::Artifact => None,
    };
    stored
        .map(|path| crate::sync::payload::resolve_operational_audio(Path::new(&path)))
        .transpose()
        .map(Option::flatten)
        .map_err(|error| LibraryError::internal("resolving operational Recording Payload", error))
}

async fn open_payload(
    path: PathBuf,
    authoritative_media_type: Option<&str>,
    authoritative_size: Option<u64>,
    range: Option<&str>,
) -> LibraryResult<LibraryPayload> {
    let mut file = tokio::fs::File::open(&path).await.map_err(|error| {
        tracing::warn!(path = %path.display(), %error, "failed to open Recording Payload");
        LibraryError::internal("opening Recording Payload", error)
    })?;
    let actual_length = file
        .metadata()
        .await
        .map_err(|error| LibraryError::internal("reading Recording Payload metadata", error))?
        .len();
    let complete_length = authoritative_size.unwrap_or(actual_length);
    if authoritative_size.is_some_and(|size| size != actual_length) {
        tracing::error!(
            path = %path.display(),
            authoritative_size = complete_length,
            actual_size = actual_length,
            "verified Recording Payload size no longer matches authoritative metadata"
        );
        return Err(LibraryError::Internal {
            operation: "validating Recording Payload metadata",
            source: anyhow::anyhow!("verified blob size mismatch"),
        });
    }
    let media_type = authoritative_media_type
        .map(str::to_owned)
        .unwrap_or_else(|| crate::sync::payload::media_type_for(&path));
    let content_type = http::HeaderValue::from_str(&media_type).ok();
    let accept_ranges = Some(http::HeaderValue::from_static("bytes"));
    let (status, content_length, content_range, body): (u16, u64, Option<_>, PayloadBody) =
        match requested_range(range, complete_length) {
            RequestedRange::Full => {
                let body = tokio_util::io::ReaderStream::new(file).map_err(|error| {
                    crate::sync::transport::HubTransferError::Transport(error.to_string())
                });
                (200, complete_length, None, Box::pin(body))
            }
            RequestedRange::Bytes { start, end } => {
                file.seek(SeekFrom::Start(start))
                    .await
                    .map_err(|error| LibraryError::internal("seeking Recording Payload", error))?;
                let length = end - start + 1;
                let body = tokio_util::io::ReaderStream::new(file.take(length)).map_err(|error| {
                    crate::sync::transport::HubTransferError::Transport(error.to_string())
                });
                (
                    206,
                    length,
                    Some(PayloadContentRange::Bytes {
                        start,
                        end,
                        complete_length,
                    }),
                    Box::pin(body),
                )
            }
            RequestedRange::Unsatisfied => (
                416,
                0,
                Some(PayloadContentRange::Unsatisfied { complete_length }),
                Box::pin(futures_util::stream::empty()),
            ),
        };
    Ok(LibraryPayload {
        status,
        metadata: PayloadMetadata {
            content_type,
            content_length: Some(content_length),
            content_range,
            accept_ranges,
        },
        body,
    })
}

enum RequestedRange {
    Full,
    Bytes { start: u64, end: u64 },
    Unsatisfied,
}

fn requested_range(value: Option<&str>, length: u64) -> RequestedRange {
    let Some(value) = value.and_then(|value| value.strip_prefix("bytes=")) else {
        return RequestedRange::Full;
    };
    if value.contains(',') || length == 0 {
        return RequestedRange::Unsatisfied;
    }
    let Some((start, end)) = value.split_once('-') else {
        return RequestedRange::Unsatisfied;
    };
    if start.is_empty() {
        let Ok(suffix) = end.parse::<u64>() else {
            return RequestedRange::Unsatisfied;
        };
        if suffix == 0 {
            return RequestedRange::Unsatisfied;
        }
        return RequestedRange::Bytes {
            start: length.saturating_sub(suffix),
            end: length - 1,
        };
    }
    let Ok(start) = start.parse::<u64>() else {
        return RequestedRange::Unsatisfied;
    };
    if start >= length {
        return RequestedRange::Unsatisfied;
    }
    let end = if end.is_empty() {
        length - 1
    } else {
        match end.parse::<u64>() {
            Ok(end) if end >= start => end.min(length - 1),
            _ => return RequestedRange::Unsatisfied,
        }
    };
    RequestedRange::Bytes { start, end }
}

#[cfg(test)]
pub(super) async fn open_operational_payload_for_test(
    path: PathBuf,
    range: Option<&str>,
) -> LibraryResult<LibraryPayload> {
    open_payload(path, None, None, range).await
}
