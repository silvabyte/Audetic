//! Keybind API routes.

use audetic_core::keybind::KeybindTarget;
use axum::{
    extract::Query,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use crate::api::error::{ApiError, ApiResult};
use crate::keybind::{self, InstallResult, KeybindStatuses, UninstallResult};

/// Request body for install or server-authoritative preview.
#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct InstallRequest {
    /// Shortcut action. Defaults to dictation.
    #[serde(default)]
    pub target: KeybindTarget,
    /// Custom key string (for example `SUPER+R` or `SUPER SHIFT, T`).
    pub key: Option<String>,
    /// Preview without changing the config.
    #[serde(default)]
    pub dry_run: bool,
    /// Alias for clients that call this operation a preview.
    #[serde(default)]
    pub preview: bool,
}

/// Query parameters for removing or previewing removal of one target.
#[derive(Debug, Deserialize, Default, IntoParams)]
pub struct UninstallRequest {
    #[serde(default)]
    pub target: KeybindTarget,
    #[serde(default)]
    pub dry_run: bool,
}

pub fn router() -> Router {
    Router::new()
        .route("/status", get(get_status))
        .route("/install", post(install_keybind))
        .route("/", delete(uninstall_keybind))
}

/// Get status for both stable shortcut targets.
#[utoipa::path(
    get,
    path = "/keybind/status",
    tag = "keybind",
    operation_id = "get_keybind_status",
    responses(
        (status = 200, description = "Dictation and meeting keybind installation state", body = KeybindStatuses),
    ),
)]
pub async fn get_status() -> ApiResult<Json<KeybindStatuses>> {
    keybind::get_statuses()
        .map(Json)
        .map_err(|error| ApiError::bad_request(error.to_string()))
}

/// Install or preview one keybinding target.
#[utoipa::path(
    post,
    path = "/keybind/install",
    tag = "keybind",
    operation_id = "install_keybind",
    request_body = InstallRequest,
    responses(
        (status = 200, description = "Install or preview result, including generated line and conflicts", body = InstallResult),
        (status = 400, description = "Invalid key or unavailable Hyprland config"),
    ),
)]
pub async fn install_keybind(
    Json(request): Json<InstallRequest>,
) -> ApiResult<Json<InstallResult>> {
    keybind::install(
        request.target,
        request.key.as_deref(),
        request.dry_run || request.preview,
    )
    .map(Json)
    .map_err(|error| ApiError::bad_request(error.to_string()))
}

/// Remove or preview removing one managed target.
#[utoipa::path(
    delete,
    path = "/keybind",
    tag = "keybind",
    operation_id = "uninstall_keybind",
    params(UninstallRequest),
    responses(
        (status = 200, description = "Target-scoped uninstall result", body = UninstallResult),
        (status = 400, description = "Hyprland config is unavailable"),
    ),
)]
pub async fn uninstall_keybind(
    Query(request): Query<UninstallRequest>,
) -> ApiResult<Json<UninstallResult>> {
    keybind::uninstall(request.target, request.dry_run)
        .map(Json)
        .map_err(|error| ApiError::bad_request(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_request_defaults_to_dictation_and_accepts_preview_alias() {
        let request: InstallRequest =
            serde_json::from_value(serde_json::json!({ "preview": true })).unwrap();

        assert_eq!(request.target, KeybindTarget::Dictation);
        assert!(request.preview);
        assert!(!request.dry_run);
    }

    #[test]
    fn install_request_accepts_meeting_target_and_dry_run() {
        let request: InstallRequest = serde_json::from_value(serde_json::json!({
            "target": "meeting",
            "key": "SUPER ALT+M",
            "dry_run": true
        }))
        .unwrap();

        assert_eq!(request.target, KeybindTarget::Meeting);
        assert_eq!(request.key.as_deref(), Some("SUPER ALT+M"));
        assert!(request.dry_run);
    }
}
