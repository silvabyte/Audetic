//! `audetic install` — bootstrap the daemon as a system service.
//!
//! Linux installs a systemd user unit and `enable --now`s it. macOS
//! installs a LaunchAgent at `~/Library/LaunchAgents/ai.audetic.daemon.plist`
//! and `launchctl bootstrap`s it. Both flows finish with a readiness probe
//! against 127.0.0.1:3737 and open the web UI in a browser.

use crate::api::url;
use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};

pub struct InstallOptions {
    pub no_launch: bool,
}

pub async fn run(opts: InstallOptions) -> Result<()> {
    #[cfg(target_os = "linux")]
    return linux::run(opts).await;

    #[cfg(target_os = "macos")]
    return macos::run(opts).await;

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = opts;
        bail!("`audetic install` is not supported on this platform");
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// Poll the daemon's HTTP API until it responds OK or the timeout fires.
///
/// Shared between Linux and macOS — the readiness check is identical once
/// the supervisor has been told to start the service.
async fn wait_for_daemon(timeout: Duration) -> Result<()> {
    let probe_url = url::api_url(url::paths::VERSION);
    let bind_addr = format!("{}:{}", url::HOST, url::DEFAULT_PORT);
    println!("  · Waiting for daemon to bind {bind_addr}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(1000))
        .build()
        .context("Failed to build HTTP client for readiness probe")?;

    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(resp) = client.get(&probe_url).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    bail!(
        "Daemon did not respond on {bind_addr} within {}s. \
         Check service logs for the failure ({}).",
        timeout.as_secs(),
        log_hint(),
    );
}

#[cfg(target_os = "linux")]
fn log_hint() -> &'static str {
    "`journalctl --user -u audeticd.service`"
}

#[cfg(target_os = "macos")]
fn log_hint() -> &'static str {
    "`tail -f ~/Library/Logs/Audetic/audetic.log`"
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn log_hint() -> &'static str {
    "(unsupported platform)"
}
