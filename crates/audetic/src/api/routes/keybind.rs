//! Keybind API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::keybind::{self, InstallResult, KeybindStatusResponse, UninstallResult};
use axum::{
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request body for keybind install.
#[derive(Debug, Deserialize, Default, ToSchema)]
pub struct InstallRequest {
    /// Custom key string (e.g., "SUPER+R" or "SUPER SHIFT, T")
    pub key: Option<String>,
}

/// Result of installing a hyprland binding: the resulting key
/// combination, where the config was edited, and the backup path.
#[derive(Debug, Serialize, ToSchema)]
pub struct InstallResponse {
    pub success: bool,
    pub message: String,
    pub display_key: Option<String>,
    pub backup_path: Option<String>,
    pub config_path: Option<String>,
}

/// Result of removing an Audetic-managed hyprland binding.
#[derive(Debug, Serialize, ToSchema)]
pub struct UninstallResponse {
    pub success: bool,
    pub removed: Option<bool>,
    pub message: String,
    pub backup_path: Option<String>,
    pub config_path: Option<String>,
}

/// Create the keybind router.
pub fn router() -> Router {
    Router::new()
        .route("/status", get(get_status))
        .route("/install", post(install_keybind))
        .route("/", delete(uninstall_keybind))
}

/// Get keybinding status.
#[utoipa::path(
    get,
    path = "/keybind/status",
    tag = "keybind",
    operation_id = "get_keybind_status",
    responses(
        (status = 200, description = "Current keybind installation state", body = KeybindStatusResponse),
    ),
)]
pub async fn get_status() -> ApiResult<Json<KeybindStatusResponse>> {
    let status = keybind::status_response().map_err(ApiError::from)?;
    Ok(Json(status))
}

/// Install a keybinding.
#[utoipa::path(
    post,
    path = "/keybind/install",
    tag = "keybind",
    request_body = InstallRequest,
    responses(
        (status = 200, description = "Install result", body = InstallResponse),
    ),
)]
pub async fn install_keybind(
    Json(request): Json<InstallRequest>,
) -> ApiResult<Json<InstallResponse>> {
    let result = keybind::install(request.key.as_deref(), false).map_err(ApiError::from)?;

    Ok(Json(match result {
        Some(InstallResult {
            backup_path,
            display_key,
            config_path,
        }) => InstallResponse {
            success: true,
            message: format!("Installed keybinding: {}", display_key),
            display_key: Some(display_key),
            backup_path: backup_path.map(|p| p.to_string_lossy().into_owned()),
            config_path: Some(config_path.to_string_lossy().into_owned()),
        },
        None => InstallResponse {
            success: false,
            message: "No changes made (dry run)".to_string(),
            display_key: None,
            backup_path: None,
            config_path: None,
        },
    }))
}

/// Uninstall the keybinding.
#[utoipa::path(
    delete,
    path = "/keybind",
    tag = "keybind",
    responses(
        (status = 200, description = "Uninstall result", body = UninstallResponse),
    ),
)]
pub async fn uninstall_keybind() -> ApiResult<Json<UninstallResponse>> {
    let result = keybind::uninstall(false).map_err(ApiError::from)?;

    Ok(Json(match result {
        Some(UninstallResult {
            removed,
            backup_path,
            config_path,
        }) => UninstallResponse {
            success: true,
            removed: Some(removed),
            message: if removed {
                "Keybinding removed".to_string()
            } else {
                "No keybinding found to remove".to_string()
            },
            backup_path: backup_path.map(|p| p.to_string_lossy().into_owned()),
            config_path: Some(config_path.to_string_lossy().into_owned()),
        },
        None => UninstallResponse {
            success: false,
            removed: None,
            message: "No changes made (dry run)".to_string(),
            backup_path: None,
            config_path: None,
        },
    }))
}
