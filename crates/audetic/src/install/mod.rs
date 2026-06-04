//! `audetic install` — bootstrap the daemon as a system service.
//!
//! Linux installs a systemd user unit and `enable --now`s it. macOS
//! installs a LaunchAgent at `~/Library/LaunchAgents/ai.audetic.daemon.plist`
//! and `launchctl bootstrap`s it. Both flows finish with a readiness probe
//! against 127.0.0.1:3737 and open the web UI in a browser.

use crate::api::url;
use anyhow::{bail, Context, Result};
use std::path::Path;
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

/// Copy the standalone `audetic` CLI onto PATH at `~/.local/bin/audetic`
/// (`$XDG_BIN_HOME` if set; `dirs::executable_dir()` is `None` on macOS and
/// falls back to `~/.local/bin`). Everything stays under `$HOME` — no sudo,
/// never `/usr/local/bin` — matching the installer's contract. Best-effort:
/// prints a hint and returns without failing the install if the CLI can't be
/// found or placed.
///
/// `source` is the CLI binary shipped with the daemon — next to `audeticd` in
/// the Linux archive, or inside the installed `Audetic.app` bundle on macOS.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn place_cli_on_path(source: &Path) {
    if !source.exists() {
        println!(
            "  · Standalone `audetic` CLI not found at {}; skipping PATH install.",
            source.display()
        );
        return;
    }

    let Some(target_dir) =
        dirs::executable_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local").join("bin")))
    else {
        return;
    };
    let target = target_dir.join("audetic");

    if std::fs::create_dir_all(&target_dir).is_err() {
        println!(
            "  · Could not create {}; skipping CLI install.",
            target_dir.display()
        );
        return;
    }

    // Replace any stale copy so re-installs/upgrades refresh the CLI.
    let _ = std::fs::remove_file(&target);
    match std::fs::copy(source, &target) {
        Ok(_) => {
            let _ = set_executable(&target);
            println!("  · Installed `audetic` CLI → {}", target.display());
            if !on_path(&target_dir) {
                println!(
                    "    Note: {} is not on your PATH. Add it to use `audetic` directly.",
                    target_dir.display()
                );
            }
        }
        Err(err) => println!(
            "  · Could not install `audetic` CLI to {} ({err}); the daemon is still installed.",
            target.display()
        ),
    }
}

/// Whether `dir` appears in the `PATH` environment variable.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p == dir))
        .unwrap_or(false)
}

/// `chmod 0o755` — the copied CLI must be executable.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .with_context(|| format!("Failed to stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("Failed to chmod {}", path.display()))?;
    Ok(())
}

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
