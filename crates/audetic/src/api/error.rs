//! API error handling for consistent JSON error responses.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiErrorBody {
    pub error: bool,
    pub message: String,
}

/// API error type that converts to JSON responses.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ApiErrorBody {
            error: true,
            message: self.message,
        });
        (self.status, body).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal(err.to_string())
    }
}

impl From<crate::sync::shared_library::LibraryError> for ApiError {
    fn from(error: crate::sync::shared_library::LibraryError) -> Self {
        use crate::sync::shared_library::LibraryError;
        match error {
            LibraryError::NotFound(message) => Self::not_found(message),
            LibraryError::Conflict(message) => Self::conflict(message),
            LibraryError::Unavailable(message) => Self::unavailable(message),
            LibraryError::Invalid(message) => Self::bad_request(message),
            LibraryError::Internal { operation, source } => {
                tracing::error!(%operation, error = %source, "Shared Library operation failed");
                Self::internal("Shared Library operation failed")
            }
        }
    }
}

/// Result type for API handlers.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::shared_library::LibraryError;

    #[test]
    fn library_errors_have_one_stable_http_mapping() {
        let cases = [
            (
                LibraryError::NotFound("missing".into()),
                StatusCode::NOT_FOUND,
            ),
            (
                LibraryError::Conflict("changed".into()),
                StatusCode::CONFLICT,
            ),
            (
                LibraryError::Unavailable("offline".into()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                LibraryError::Invalid("invalid".into()),
                StatusCode::BAD_REQUEST,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(ApiError::from(error).into_response().status(), expected);
        }
    }
}
