use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

pub fn serve(source: crate::sync::shared_library::LibraryPayload) -> Response {
    let status = StatusCode::from_u16(source.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = Response::builder().status(status);
    if let Some(value) = source.metadata.content_type {
        response = response.header(header::CONTENT_TYPE, value);
    }
    if let Some(value) = source.metadata.content_length {
        response = response.header(header::CONTENT_LENGTH, value);
    }
    if let Some(value) = source.metadata.content_range {
        response = response.header(header::CONTENT_RANGE, value.to_header_value());
    }
    if let Some(value) = source.metadata.accept_ranges {
        response = response.header(header::ACCEPT_RANGES, value);
    }
    response
        .body(Body::from_stream(source.body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::HeaderValue;

    #[tokio::test]
    async fn remote_payload_stream_preserves_range_response_metadata_and_bytes() {
        let source = crate::sync::shared_library::LibraryPayload {
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
        };

        let response = serve(source);

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
