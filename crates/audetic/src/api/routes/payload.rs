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
            for name in [
                header::CONTENT_TYPE,
                header::CONTENT_LENGTH,
                header::CONTENT_RANGE,
                header::ACCEPT_RANGES,
            ] {
                if let Some(value) = remote
                    .headers
                    .get(name.as_str())
                    .and_then(|value| HeaderValue::from_str(value).ok())
                {
                    response = response.header(name, value);
                }
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
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn remote_payload_stream_preserves_range_response_metadata_and_bytes() {
        let source = crate::sync::service::PayloadSource::Remote(
            crate::sync::transport::StreamingPayloadResponse {
                status: StatusCode::PARTIAL_CONTENT.as_u16(),
                headers: BTreeMap::from([
                    ("content-type".to_owned(), "audio/wav".to_owned()),
                    ("content-length".to_owned(), "4".to_owned()),
                    ("content-range".to_owned(), "bytes 2-5/10".to_owned()),
                    ("accept-ranges".to_owned(), "bytes".to_owned()),
                ]),
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
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
        assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "2345"
        );
    }
}
