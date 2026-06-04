//! Provider API routes.
//!
//! Read endpoints (`GET /provider`, `GET /provider/status`) expose a sanitized
//! view. The config endpoints (`GET`/`PUT /provider/config`, `POST
//! /provider/reset`) let the CLL's setup wizard read and write the raw
//! `WhisperConfig` — the daemon owns the on-disk `config.toml` (and its backups)
//! so there is a single writer. `POST /provider/test` runs a transcription with
//! the configured provider so the slim CLI never has to link the provider stack.

use crate::api::error::{ApiError, ApiResult};
use crate::config::{Config, WhisperConfig};
use crate::global;
use crate::transcription::{
    get_provider_info, get_provider_status, test_provider, ProviderInfo, ProviderStatus,
    ProviderTestResult,
};
use anyhow::{Context, Result};
use axum::{
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use utoipa::ToSchema;

const MAX_CONFIG_BACKUPS: usize = 3;

/// Request body for `POST /provider/test`.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ProviderTestRequest {
    /// Optional path to an audio file to transcribe. When omitted, the daemon
    /// only validates that the configured provider initializes.
    pub file: Option<String>,
}

/// Create the provider router.
pub fn router() -> Router {
    Router::new()
        .route("/", get(get_config))
        .route("/status", get(get_status))
        .route("/config", get(get_raw_config).put(set_raw_config))
        .route("/reset", post(reset_config))
        .route("/test", post(run_test))
}

/// Get provider configuration.
#[utoipa::path(
    get,
    path = "/provider",
    tag = "provider",
    operation_id = "get_provider_config",
    responses(
        (status = 200, description = "Current provider configuration", body = ProviderInfo),
    ),
)]
pub async fn get_config() -> ApiResult<Json<ProviderInfo>> {
    let info = get_provider_info().map_err(ApiError::from)?;
    Ok(Json(info))
}

/// Get provider status and health.
#[utoipa::path(
    get,
    path = "/provider/status",
    tag = "provider",
    operation_id = "get_provider_status",
    responses(
        (status = 200, description = "Provider availability", body = ProviderStatus),
    ),
)]
pub async fn get_status() -> ApiResult<Json<ProviderStatus>> {
    let status = get_provider_status().map_err(ApiError::from)?;
    Ok(Json(status))
}

/// Get the raw `WhisperConfig` (including any API key) so the CLI wizard can
/// pre-fill existing values. Loopback-only, same trust boundary as reading
/// `~/.config/audetic/config.toml` directly.
#[utoipa::path(
    get,
    path = "/provider/config",
    tag = "provider",
    operation_id = "get_provider_raw_config",
    responses(
        (status = 200, description = "Raw whisper/provider config", body = WhisperConfig),
    ),
)]
pub async fn get_raw_config() -> ApiResult<Json<WhisperConfig>> {
    let config = Config::load().map_err(ApiError::from)?;
    Ok(Json(config.whisper))
}

/// Replace the provider configuration. Backs up the existing `config.toml`
/// before writing.
#[utoipa::path(
    put,
    path = "/provider/config",
    tag = "provider",
    request_body = WhisperConfig,
    responses(
        (status = 200, description = "The persisted provider config", body = WhisperConfig),
    ),
)]
pub async fn set_raw_config(Json(whisper): Json<WhisperConfig>) -> ApiResult<Json<WhisperConfig>> {
    backup_config_file().map_err(ApiError::from)?;
    let mut config = Config::load().map_err(ApiError::from)?;
    config.whisper = whisper;
    config.save().map_err(ApiError::from)?;
    Ok(Json(config.whisper))
}

/// Reset the provider configuration to defaults. Backs up first.
#[utoipa::path(
    post,
    path = "/provider/reset",
    tag = "provider",
    responses(
        (status = 200, description = "The reset provider config", body = WhisperConfig),
    ),
)]
pub async fn reset_config() -> ApiResult<Json<WhisperConfig>> {
    backup_config_file().map_err(ApiError::from)?;
    let mut config = Config::load().map_err(ApiError::from)?;
    config.whisper = WhisperConfig::default();
    config.save().map_err(ApiError::from)?;
    Ok(Json(config.whisper))
}

/// Test the currently-configured provider, optionally against an audio file.
#[utoipa::path(
    post,
    path = "/provider/test",
    tag = "provider",
    request_body = ProviderTestRequest,
    responses(
        (status = 200, description = "Provider test result", body = ProviderTestResult),
    ),
)]
pub async fn run_test(
    Json(request): Json<ProviderTestRequest>,
) -> ApiResult<Json<ProviderTestResult>> {
    let path = request.file.as_deref().map(Path::new);
    let result = test_provider(path).await.map_err(ApiError::from)?;
    Ok(Json(result))
}

/// Back up the current `config.toml` to `<data_dir>/config-backups/`, keeping
/// the most recent [`MAX_CONFIG_BACKUPS`]. No-op when no config exists yet.
fn backup_config_file() -> Result<Option<PathBuf>> {
    let config_path = global::config_file()?;
    if !config_path.exists() {
        return Ok(None);
    }

    let backup_dir = global::data_dir()?.join("config-backups");
    std::fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create backup directory: {:?}", backup_dir))?;

    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let backup_path = backup_dir.join(format!("config.toml.backup-{timestamp}"));
    std::fs::copy(&config_path, &backup_path)
        .with_context(|| format!("Failed to back up {:?}", config_path))?;

    rotate_backups(&backup_dir)?;
    Ok(Some(backup_path))
}

fn rotate_backups(backup_dir: &Path) -> Result<()> {
    let mut backups: Vec<PathBuf> = std::fs::read_dir(backup_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("config.toml.backup-"))
                .unwrap_or(false)
        })
        .collect();

    backups.sort_by(|a, b| {
        let a_time = std::fs::metadata(a).and_then(|m| m.modified()).ok();
        let b_time = std::fs::metadata(b).and_then(|m| m.modified()).ok();
        b_time.cmp(&a_time)
    });

    for old_backup in backups.iter().skip(MAX_CONFIG_BACKUPS) {
        let _ = std::fs::remove_file(old_backup);
    }
    Ok(())
}
