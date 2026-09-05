use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::ServeFile;

pub async fn serve(source: crate::sync::service::PayloadSource, request: Request) -> Response {
    match source {
        crate::sync::service::PayloadSource::Local(blob) => {
            match ServeFile::new(blob.canonical_path).oneshot(request).await {
                Ok(mut response) => {
                    if let Ok(value) = HeaderValue::from_str(&blob.media_type) {
                        response.headers_mut().insert(header::CONTENT_TYPE, value);
                    }
                    response.into_response()
                }
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        crate::sync::service::PayloadSource::Remote(remote) => {
            let status =
                StatusCode::from_u16(remote.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut response = Response::builder().status(status);
            if let Some(value) = remote.metadata.content_type {
                response = response.header(header::CONTENT_TYPE, value);
            }
            if let Some(value) = remote.metadata.content_length {
                response = response.header(header::CONTENT_LENGTH, value);
            }
            if let Some(value) = remote.metadata.content_range {
                response = response.header(header::CONTENT_RANGE, value.to_header_value());
            }
            if let Some(value) = remote.metadata.accept_ranges {
                response = response.header(header::ACCEPT_RANGES, value);
            }
            response
                .body(Body::from_stream(remote.body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn remote_payload_stream_preserves_range_response_metadata_and_bytes() {
        let source = crate::sync::service::PayloadSource::Remote(
            crate::sync::transport::StreamingPayloadResponse {
                status: StatusCode::PARTIAL_CONTENT.as_u16(),
                metadata: crate::sync::transport::PayloadMetadata {
                    content_type: Some(HeaderValue::from_static("audio/wav")),
                    content_length: Some(4),
                    content_range: Some(crate::sync::transport::PayloadContentRange::Bytes {
                        start: 2,
                        end: 5,
                        complete_length: 10,
                    }),
                    accept_ranges: Some(HeaderValue::from_static("bytes")),
                },
                body: Box::pin(futures_util::stream::once(async {
                    Ok(bytes::Bytes::from_static(b"2345"))
                })),
            },
        );
        let request = Request::get("/payload")
            .header(header::RANGE, "bytes=2-5")
            .body(Body::empty())
            .unwrap();

        let response = serve(source, request).await;

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/wav");
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "4");
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "2345"
        );
    }
}
