//! API error handling for consistent JSON error responses.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::sync::SyncError;

/// Stable JSON body returned for local API failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ApiErrorResponse {
    #[schema(example = true)]
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
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ApiErrorResponse {
            error: true,
            message: self.message,
        });
        (self.status, body).into_response()
    }
}

impl From<SyncError> for ApiError {
    fn from(error: SyncError) -> Self {
        let status = match &error {
            SyncError::BadRequest(_) => StatusCode::BAD_REQUEST,
            SyncError::Unauthorized => StatusCode::UNAUTHORIZED,
            SyncError::RoleConflict { .. } | SyncError::PairingConflict => StatusCode::CONFLICT,
            SyncError::NotFound => StatusCode::NOT_FOUND,
            SyncError::RemoteUnavailable => StatusCode::BAD_GATEWAY,
            SyncError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, error.to_string())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        Self::internal(err.to_string())
    }
}

/// Result type for API handlers.
pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_errors_map_to_local_api_statuses() {
        for (error, expected) in [
            (
                SyncError::BadRequest("invalid input"),
                StatusCode::BAD_REQUEST,
            ),
            (SyncError::Unauthorized, StatusCode::UNAUTHORIZED),
            (
                SyncError::RoleConflict {
                    required: "hub",
                    active: "client",
                },
                StatusCode::CONFLICT,
            ),
            (SyncError::PairingConflict, StatusCode::CONFLICT),
            (SyncError::NotFound, StatusCode::NOT_FOUND),
            (SyncError::RemoteUnavailable, StatusCode::BAD_GATEWAY),
            (SyncError::Internal, StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            assert_eq!(ApiError::from(error).status, expected);
        }
    }
}
