//! System dependency probes.
//!
//! Reports whether external tools the daemon depends on (FFmpeg today) are
//! available on PATH. The desktop UI uses this to drive its onboarding flow
//! so the user is prompted to install missing tools before they hit an
//! in-band failure (e.g. starting a meeting only to have compression fail).

use crate::system::ffmpeg::{install_blocking, InstallProgress};
use audetic_core::compression::check_ffmpeg_available;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Serialize;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tracing::{error, info};
use utoipa::ToSchema;

const RESTART_DELAY: Duration = Duration::from_millis(350);
const LINUX_SERVICE: &str = "audeticd.service";
const MACOS_SERVICE: &str = "ai.audetic.daemon";

/// Availability of external tools the daemon depends on.
#[derive(Debug, Serialize, ToSchema)]
pub struct SystemDeps {
    /// Whether `ffmpeg` resolves — either app-local sidecar or on PATH.
    /// Required for meeting audio compression before upload.
    pub ffmpeg: bool,
}

/// Acknowledgement returned before the daemon asks its service manager to
/// restart it.
#[derive(Debug, Serialize, ToSchema)]
pub struct RestartAccepted {
    pub accepted: bool,
    pub service: String,
    pub delay_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Only the host variant is constructed outside tests, while pure tests cover
// both fixed command shapes on every development platform.
#[allow(dead_code)]
enum RestartPlatform {
    Linux,
    MacOs,
}

#[derive(Debug, PartialEq, Eq)]
struct RestartCommand {
    program: &'static str,
    args: Vec<String>,
}

/// Phase string for the install status endpoint. Renderer uses this to drive
/// progress UI and decide when to stop polling.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum InstallPhase {
    Idle,
    Starting,
    Downloading,
    Extracting,
    Done,
    Error,
}

/// Flattened install state — easier to consume from TypeScript than a tagged
/// union. Fields not relevant to the current phase are `None`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstallStatusResponse {
    pub phase: InstallPhase,
    /// Set during `Downloading`.
    pub downloaded_bytes: Option<u64>,
    /// Set during `Downloading`. May be 0 before headers arrive.
    pub total_bytes: Option<u64>,
    /// Convenience field; `0..=100` during download, `None` otherwise.
    pub percent: Option<u8>,
    /// Set on `Done`.
    pub binary_path: Option<String>,
    /// Set on `Error`.
    pub message: Option<String>,
}

impl From<&InstallProgress> for InstallStatusResponse {
    fn from(p: &InstallProgress) -> Self {
        match p {
            InstallProgress::Idle => Self {
                phase: InstallPhase::Idle,
                downloaded_bytes: None,
                total_bytes: None,
                percent: None,
                binary_path: None,
                message: None,
            },
            InstallProgress::Starting => Self {
                phase: InstallPhase::Starting,
                downloaded_bytes: None,
                total_bytes: None,
                percent: None,
                binary_path: None,
                message: None,
            },
            InstallProgress::Downloading { downloaded, total } => {
                let percent = if *total > 0 {
                    Some(((*downloaded as f64 / *total as f64) * 100.0).round() as u8)
                } else {
                    None
                };
                Self {
                    phase: InstallPhase::Downloading,
                    downloaded_bytes: Some(*downloaded),
                    total_bytes: Some(*total),
                    percent,
                    binary_path: None,
                    message: None,
                }
            }
            InstallProgress::Extracting => Self {
                phase: InstallPhase::Extracting,
                downloaded_bytes: None,
                total_bytes: None,
                percent: None,
                binary_path: None,
                message: None,
            },
            InstallProgress::Done { binary_path } => Self {
                phase: InstallPhase::Done,
                downloaded_bytes: None,
                total_bytes: None,
                percent: None,
                binary_path: Some(binary_path.to_string_lossy().into_owned()),
                message: None,
            },
            InstallProgress::Error { message } => Self {
                phase: InstallPhase::Error,
                downloaded_bytes: None,
                total_bytes: None,
                percent: None,
                binary_path: None,
                message: Some(message.clone()),
            },
        }
    }
}

/// Process-wide install state. POST kicks it off; GET reads it. The install
/// itself runs on a blocking thread (`spawn_blocking`) since `ffmpeg-sidecar`
/// is synchronous, and updates this same mutex from inside the closure.
fn install_state() -> &'static Arc<Mutex<InstallProgress>> {
    static STATE: OnceLock<Arc<Mutex<InstallProgress>>> = OnceLock::new();
    STATE.get_or_init(|| Arc::new(Mutex::new(InstallProgress::Idle)))
}

fn is_running(p: &InstallProgress) -> bool {
    matches!(
        p,
        InstallProgress::Starting
            | InstallProgress::Downloading { .. }
            | InstallProgress::Extracting
    )
}

pub fn router() -> Router {
    Router::new()
        .route("/deps", get(get_deps))
        .route("/restart", post(restart_daemon))
        .route("/install-ffmpeg", post(start_install_ffmpeg))
        .route("/install-ffmpeg/status", get(get_install_ffmpeg_status))
}

/// Report availability of required external tools.
#[utoipa::path(
    get,
    path = "/system/deps",
    tag = "system",
    operation_id = "get_system_deps",
    responses(
        (status = 200, description = "External tool availability", body = SystemDeps),
    ),
)]
pub async fn get_deps() -> Json<SystemDeps> {
    Json(SystemDeps {
        ffmpeg: check_ffmpeg_available(),
    })
}

/// Ask the host's per-user service manager to restart this daemon.
///
/// There is intentionally no request body: callers cannot supply a command,
/// unit, or launchd label. The only possible targets are the two constants
/// compiled into Audetic.
#[utoipa::path(
    post,
    path = "/system/restart",
    tag = "system",
    operation_id = "restart_daemon",
    responses(
        (status = 202, description = "Daemon restart scheduled", body = RestartAccepted),
    ),
)]
pub async fn restart_daemon() -> impl IntoResponse {
    let service = current_service_name().to_string();
    tokio::spawn(async move {
        tokio::time::sleep(RESTART_DELAY).await;
        if let Err(err) = request_service_restart().await {
            error!(error = %err, "Failed to request daemon restart");
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(RestartAccepted {
            accepted: true,
            service,
            delay_ms: RESTART_DELAY.as_millis() as u64,
        }),
    )
}

fn restart_command_for(
    platform: RestartPlatform,
    uid: Option<&str>,
) -> anyhow::Result<RestartCommand> {
    match platform {
        RestartPlatform::Linux => Ok(RestartCommand {
            program: "systemctl",
            args: ["--user", "restart", LINUX_SERVICE]
                .into_iter()
                .map(str::to_string)
                .collect(),
        }),
        RestartPlatform::MacOs => {
            let uid = uid.ok_or_else(|| anyhow::anyhow!("Could not determine current user id"))?;
            if uid.is_empty() || !uid.chars().all(|character| character.is_ascii_digit()) {
                anyhow::bail!("Current user id was not numeric");
            }
            Ok(RestartCommand {
                program: "launchctl",
                args: vec![
                    "kickstart".to_string(),
                    "-k".to_string(),
                    format!("gui/{uid}/{MACOS_SERVICE}"),
                ],
            })
        }
    }
}

#[cfg(target_os = "linux")]
fn current_service_name() -> &'static str {
    LINUX_SERVICE
}

#[cfg(target_os = "macos")]
fn current_service_name() -> &'static str {
    MACOS_SERVICE
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_service_name() -> &'static str {
    "unsupported"
}

#[cfg(target_os = "linux")]
async fn request_service_restart() -> anyhow::Result<()> {
    execute_restart_command(restart_command_for(RestartPlatform::Linux, None)?).await
}

#[cfg(target_os = "macos")]
async fn request_service_restart() -> anyhow::Result<()> {
    let output = tokio::process::Command::new("id")
        .arg("-u")
        .output()
        .await
        .map_err(|err| anyhow::anyhow!("Failed to run `id -u`: {err}"))?;
    if !output.status.success() {
        anyhow::bail!("`id -u` exited with {}", output.status);
    }
    let uid = std::str::from_utf8(&output.stdout)
        .map_err(|err| anyhow::anyhow!("`id -u` output was not UTF-8: {err}"))?
        .trim();
    execute_restart_command(restart_command_for(RestartPlatform::MacOs, Some(uid))?).await
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn request_service_restart() -> anyhow::Result<()> {
    anyhow::bail!("Daemon restart is only supported on Linux and macOS")
}

async fn execute_restart_command(command: RestartCommand) -> anyhow::Result<()> {
    let status = tokio::process::Command::new(command.program)
        .args(&command.args)
        .status()
        .await
        .map_err(|err| anyhow::anyhow!("Failed to run {}: {err}", command.program))?;
    if !status.success() {
        anyhow::bail!("{} exited with {status}", command.program);
    }
    Ok(())
}

/// Kick off an app-local FFmpeg install.
///
/// Idempotent w.r.t. already-installed: if ffmpeg already resolves (either as
/// the sidecar binary or on PATH), the response is 200 with phase=`done` and
/// no work happens.
///
/// Returns 202 + current state when an install is started (or kicked off
/// after a previous error). Returns 409 + current state when an install is
/// already in flight.
#[utoipa::path(
    post,
    path = "/system/install-ffmpeg",
    tag = "system",
    operation_id = "install_ffmpeg",
    responses(
        (status = 202, description = "Install kicked off", body = InstallStatusResponse),
        (status = 200, description = "FFmpeg already installed", body = InstallStatusResponse),
        (status = 409, description = "Install already in progress", body = InstallStatusResponse),
    ),
)]
pub async fn start_install_ffmpeg() -> impl IntoResponse {
    // Short-circuit if ffmpeg already resolves — flips state to Done so the
    // status endpoint returns the right shape on the next poll.
    if let Some(path) = crate::system::ffmpeg::resolve_ffmpeg_binary() {
        let state_arc = install_state();
        let mut state = state_arc.lock().unwrap();
        *state = InstallProgress::Done {
            binary_path: path.clone(),
        };
        let resp = InstallStatusResponse::from(&*state);
        return (StatusCode::OK, Json(resp));
    }

    let state_arc = install_state();
    {
        let state = state_arc.lock().unwrap();
        if is_running(&state) {
            let resp = InstallStatusResponse::from(&*state);
            return (StatusCode::CONFLICT, Json(resp));
        }
    }

    // Reset to Starting and spawn the blocking install.
    {
        let mut state = state_arc.lock().unwrap();
        *state = InstallProgress::Starting;
    }

    let state_for_task = state_arc.clone();
    info!("Starting bundled FFmpeg install");
    tokio::task::spawn_blocking(move || {
        let cb_state = state_for_task.clone();
        let result = install_blocking(move |p| {
            if let Ok(mut state) = cb_state.lock() {
                *state = p;
            }
        });
        if let Err(e) = result {
            error!("FFmpeg install failed: {}", e);
        } else {
            info!("FFmpeg install completed");
        }
    });

    let resp = InstallStatusResponse::from(&InstallProgress::Starting);
    (StatusCode::ACCEPTED, Json(resp))
}

/// Poll the current install state.
#[utoipa::path(
    get,
    path = "/system/install-ffmpeg/status",
    tag = "system",
    operation_id = "get_install_ffmpeg_status",
    responses(
        (status = 200, description = "Current install state", body = InstallStatusResponse),
    ),
)]
pub async fn get_install_ffmpeg_status() -> Json<InstallStatusResponse> {
    let state = install_state().lock().unwrap();
    Json(InstallStatusResponse::from(&*state))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_restart_command_is_fixed_to_audeticd_user_service() {
        assert_eq!(
            restart_command_for(RestartPlatform::Linux, None).unwrap(),
            RestartCommand {
                program: "systemctl",
                args: vec![
                    "--user".to_string(),
                    "restart".to_string(),
                    "audeticd.service".to_string(),
                ],
            }
        );
    }

    #[test]
    fn macos_restart_command_is_fixed_to_audetic_launch_agent() {
        assert_eq!(
            restart_command_for(RestartPlatform::MacOs, Some("501")).unwrap(),
            RestartCommand {
                program: "launchctl",
                args: vec![
                    "kickstart".to_string(),
                    "-k".to_string(),
                    "gui/501/ai.audetic.daemon".to_string(),
                ],
            }
        );
    }

    #[test]
    fn macos_restart_rejects_non_numeric_user_ids() {
        assert!(restart_command_for(RestartPlatform::MacOs, Some("501/other.service")).is_err());
    }
}
