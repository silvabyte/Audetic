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
            for (name, value) in [
                (header::CONTENT_TYPE, remote.content_type),
                (header::CONTENT_LENGTH, remote.content_length),
                (header::CONTENT_RANGE, remote.content_range),
                (header::ACCEPT_RANGES, remote.accept_ranges),
            ] {
                if let Some(value) = value.and_then(|value| HeaderValue::from_str(&value).ok()) {
                    response = response.header(name, value);
                }
            }
            response
                .body(Body::from_stream(remote.response.bytes_stream()))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}
