//! Provider API routes.
//!
//! Read endpoints (`GET /provider`, `GET /provider/status`) expose a sanitized
//! view. The config endpoints (`GET`/`PUT /provider/config`, `POST
//! /provider/reset`) let the CLI's setup wizard read and write the raw
//! `WhisperConfig` — the daemon owns the on-disk `config.toml` (and its backups)
//! so there is a single writer. `POST /provider/validate` validates a proposed
//! config without saving it. `POST /provider/test` runs a transcription with the
//! configured provider so the slim CLI never has to link the provider stack.

use crate::api::error::{ApiError, ApiResult};
use crate::config::{Config, WhisperConfig};
use crate::global;
use crate::transcription::{
    get_provider_info_from_config, get_provider_status_from_config, test_provider_with_config,
    ProviderInfo, ProviderStatus, ProviderTestResult,
};
use anyhow::{Context, Result};
use axum::{
    extract::State,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use utoipa::ToSchema;

const MAX_CONFIG_BACKUPS: usize = 3;

/// Provider configuration captured when this daemon process started. Keeping
/// this in memory mirrors the provider instances used by active workflows and
/// avoids writing secrets or restart markers to disk.
#[derive(Clone)]
pub struct ProviderApiState {
    active: WhisperConfig,
}

impl ProviderApiState {
    pub fn new(active: WhisperConfig) -> Self {
        Self { active }
    }
}

/// Sanitized comparison between the provider used by this process and the
/// provider currently persisted on disk.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderRuntimeStatus {
    pub restart_required: bool,
    pub active: ProviderInfo,
    pub active_status: ProviderStatus,
    pub persisted: ProviderInfo,
    pub persisted_status: ProviderStatus,
}

/// Request body for `POST /provider/test`.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct ProviderTestRequest {
    /// Optional path to an audio file to transcribe. When omitted, the daemon
    /// only validates that the configured provider initializes.
    pub file: Option<String>,
}

/// Create the provider router.
pub fn router(state: ProviderApiState) -> Router {
    Router::new()
        .route("/", get(get_config))
        .route("/status", get(get_status))
        .route("/runtime", get(get_runtime_status))
        .route("/config", get(get_raw_config).put(set_raw_config))
        .route("/validate", post(validate_config))
        .route("/reset", post(reset_config))
        .route("/test", post(run_test))
        .with_state(state)
}

/// Get the sanitized provider configuration active in this daemon process.
#[utoipa::path(
    get,
    path = "/provider",
    tag = "provider",
    operation_id = "get_provider_config",
    responses(
        (status = 200, description = "Active provider configuration", body = ProviderInfo),
    ),
)]
pub async fn get_config(State(state): State<ProviderApiState>) -> Json<ProviderInfo> {
    Json(get_provider_info_from_config(&state.active))
}

/// Get status and health for the provider active in this daemon process.
#[utoipa::path(
    get,
    path = "/provider/status",
    tag = "provider",
    operation_id = "get_provider_status",
    responses(
        (status = 200, description = "Active provider availability", body = ProviderStatus),
    ),
)]
pub async fn get_status(State(state): State<ProviderApiState>) -> ApiResult<Json<ProviderStatus>> {
    let status = get_provider_status_from_config(&state.active).map_err(ApiError::from)?;
    Ok(Json(status))
}

/// Compare the active process provider with the latest persisted provider.
#[utoipa::path(
    get,
    path = "/provider/runtime",
    tag = "provider",
    operation_id = "get_provider_runtime_status",
    responses(
        (status = 200, description = "Active and persisted provider state", body = ProviderRuntimeStatus),
    ),
)]
pub async fn get_runtime_status(
    State(state): State<ProviderApiState>,
) -> ApiResult<Json<ProviderRuntimeStatus>> {
    let persisted = Config::load().map_err(ApiError::from)?.whisper;
    runtime_status(&state.active, &persisted)
        .map(Json)
        .map_err(ApiError::from)
}

fn runtime_status(
    active: &WhisperConfig,
    persisted: &WhisperConfig,
) -> Result<ProviderRuntimeStatus> {
    Ok(ProviderRuntimeStatus {
        restart_required: active != persisted,
        active: get_provider_info_from_config(active),
        active_status: get_provider_status_from_config(active)?,
        persisted: get_provider_info_from_config(persisted),
        persisted_status: get_provider_status_from_config(persisted)?,
    })
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

/// Validate a proposed provider configuration without persisting it.
///
/// This deliberately uses the same validation and provider initialization path
/// as `GET /provider/status`, including the on-disk catalog check for local
/// models.
#[utoipa::path(
    post,
    path = "/provider/validate",
    tag = "provider",
    operation_id = "validate_provider_config",
    request_body = WhisperConfig,
    responses(
        (status = 200, description = "Status of the proposed provider config", body = ProviderStatus),
    ),
)]
pub async fn validate_config(
    Json(whisper): Json<WhisperConfig>,
) -> ApiResult<Json<ProviderStatus>> {
    let status = get_provider_status_from_config(&whisper).map_err(ApiError::from)?;
    Ok(Json(status))
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

/// Test the provider active in this daemon process, optionally against an audio file.
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
    State(state): State<ProviderApiState>,
    Json(request): Json<ProviderTestRequest>,
) -> ApiResult<Json<ProviderTestResult>> {
    let path = request.file.as_deref().map(Path::new);
    let result = test_provider_with_config(&state.active, path)
        .await
        .map_err(ApiError::from)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn validate_endpoint_returns_typed_status_for_proposed_config() {
        let proposed = WhisperConfig {
            provider: Some("not-a-provider".to_string()),
            ..WhisperConfig::default()
        };
        let response = router(ProviderApiState::new(WhisperConfig::default()))
            .oneshot(
                Request::post("/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&proposed).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let status: ProviderStatus = serde_json::from_slice(&body).unwrap();
        assert!(matches!(
            status,
            ProviderStatus::ConfigError { provider, .. } if provider == "not-a-provider"
        ));
    }

    #[tokio::test]
    async fn provider_status_reports_the_process_snapshot_not_disk_config() {
        let active = WhisperConfig {
            provider: Some("definitely-not-a-real-provider".to_string()),
            ..WhisperConfig::default()
        };
        let response = router(ProviderApiState::new(active))
            .oneshot(Request::get("/status").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let status: ProviderStatus = serde_json::from_slice(&body).unwrap();
        assert!(matches!(
            status,
            ProviderStatus::ConfigError { provider, .. }
                if provider == "definitely-not-a-real-provider"
        ));
    }

    #[test]
    fn proposed_local_config_keeps_the_real_model_install_check() {
        let proposed = WhisperConfig {
            provider: Some("local".to_string()),
            model: Some("model-that-is-not-in-the-catalog".to_string()),
            ..WhisperConfig::default()
        };

        let status = get_provider_status_from_config(&proposed).unwrap();
        assert!(matches!(
            status,
            ProviderStatus::ConfigError { error, .. }
                if error.contains("Unknown local model")
        ));
    }

    #[test]
    fn runtime_status_detects_persisted_provider_changes_without_exposing_secrets() {
        let active = WhisperConfig {
            provider: Some("audetic-api".to_string()),
            api_key: Some("active-secret".to_string()),
            ..WhisperConfig::default()
        };
        let persisted = WhisperConfig {
            provider: Some("openai-api".to_string()),
            api_key: Some("persisted-secret".to_string()),
            ..WhisperConfig::default()
        };

        let status = runtime_status(&active, &persisted).unwrap();
        assert!(status.restart_required);
        assert_eq!(status.active.provider.as_deref(), Some("audetic-api"));
        assert_eq!(status.persisted.provider.as_deref(), Some("openai-api"));
        let serialized = serde_json::to_string(&status).unwrap();
        assert!(!serialized.contains("active-secret"));
        assert!(!serialized.contains("persisted-secret"));
    }

    #[test]
    fn runtime_status_does_not_require_restart_for_unchanged_config() {
        let config = WhisperConfig::default();
        assert!(!runtime_status(&config, &config).unwrap().restart_required);
    }
}
