//! Keybind API routes.

use crate::api::error::{ApiError, ApiResult};
use crate::keybind::{self, InstallResult, KeybindStatus, UninstallResult};
use axum::{
    response::Json,
    routing::{delete, get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

/// Request body for keybind install.
#[derive(Debug, Deserialize, Default)]
pub struct InstallRequest {
    /// Custom key string (e.g., "SUPER+R" or "SUPER SHIFT, T")
    pub key: Option<String>,
}

/// Create the keybind router.
pub fn router() -> Router {
    Router::new()
        .route("/status", get(get_status))
        .route("/install", post(install_keybind))
        .route("/", delete(uninstall_keybind))
}

/// GET /keybind/status - Get keybinding status.
async fn get_status() -> ApiResult<Json<KeybindStatus>> {
    let status = keybind::get_status().map_err(ApiError::from)?;
    Ok(Json(status))
}

/// POST /keybind/install - Install a keybinding.
async fn install_keybind(Json(request): Json<InstallRequest>) -> ApiResult<Json<Value>> {
    let result = keybind::install(request.key.as_deref(), false).map_err(ApiError::from)?;

    match result {
        Some(InstallResult {
            backup_path,
            display_key,
            config_path,
        }) => Ok(Json(json!({
            "success": true,
            "message": format!("Installed keybinding: {}", display_key),
            "display_key": display_key,
            "backup_path": backup_path,
            "config_path": config_path,
        }))),
        None => Ok(Json(json!({
            "success": false,
            "message": "No changes made (dry run)",
        }))),
    }
}

/// DELETE /keybind - Uninstall the keybinding.
async fn uninstall_keybind() -> ApiResult<Json<Value>> {
    let result = keybind::uninstall(false).map_err(ApiError::from)?;

    match result {
        Some(UninstallResult {
            removed,
            backup_path,
            config_path,
        }) => Ok(Json(json!({
            "success": true,
            "removed": removed,
            "message": if removed {
                "Keybinding removed"
            } else {
                "No keybinding found to remove"
            },
            "backup_path": backup_path,
            "config_path": config_path,
        }))),
        None => Ok(Json(json!({
            "success": false,
            "message": "No changes made (dry run)",
        }))),
    }
}
