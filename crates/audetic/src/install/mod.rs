//! `audeticd install` / `audeticd uninstall` — bootstrap and tear down the
//! daemon as a system service.
//!
//! Linux installs a systemd user unit and `enable --now`s it. macOS
//! installs a LaunchAgent at `~/Library/LaunchAgents/ai.audetic.daemon.plist`
//! and `launchctl bootstrap`s it. Both flows finish with a readiness probe
//! against 127.0.0.1:3737 and open the web UI in a browser.
//!
//! `uninstall` mirrors `install`: each platform stops and deregisters the
//! service, then removes exactly the artifacts install created. Both sides
//! resolve their paths through the same helpers, so teardown can't silently
//! drift from setup.

use crate::api::url;
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
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
        bail!("`audeticd install` is not supported on this platform");
    }
}

pub struct UninstallOptions {
    /// Skip the confirmation prompt.
    pub yes: bool,
    /// Print the plan and exit without touching anything.
    pub dry_run: bool,
    /// Preserve `config.toml` and the rest of the config directory.
    pub keep_config: bool,
    /// Preserve the transcription history database.
    pub keep_database: bool,
}

pub fn uninstall(opts: UninstallOptions) -> Result<()> {
    #[cfg(target_os = "linux")]
    return linux::uninstall(opts);

    #[cfg(target_os = "macos")]
    return macos::uninstall(opts);

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = opts;
        bail!("`audeticd uninstall` is not supported on this platform");
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// Where the standalone `audetic` CLI lands: `$XDG_BIN_HOME`, else
/// `~/.local/bin` (`dirs::executable_dir()` is `None` on macOS). Everything
/// stays under `$HOME` — no sudo, never `/usr/local/bin`.
///
/// `install` and `uninstall` both go through this, so the CLI that gets
/// removed is always the one that got placed.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn cli_target_path() -> Option<PathBuf> {
    dirs::executable_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("bin")))
        .map(|dir| dir.join("audetic"))
}

/// Copy the standalone `audetic` CLI onto PATH. Best-effort: prints a hint and
/// returns without failing the install if the CLI can't be found or placed.
///
/// `source` is the CLI binary shipped with the daemon — next to `audeticd` in
/// the build output, or inside the installed `Audetic.app` bundle on macOS.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn place_cli_on_path(source: &Path) {
    if !source.exists() {
        println!(
            "  · Standalone `audetic` CLI not found at {}; skipping PATH install.",
            source.display()
        );
        return;
    }

    let Some(target) = cli_target_path() else {
        return;
    };
    let Some(target_dir) = target.parent().map(Path::to_path_buf) else {
        return;
    };

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

/// The set of on-disk artifacts `uninstall` will remove, plus the ones it is
/// deliberately preserving. Built by the per-OS teardown, then printed for
/// confirmation before anything is touched — so `--dry-run` and the real run
/// walk identical code and can't disagree about what would happen.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Default)]
pub(crate) struct UninstallPlan {
    remove: Vec<(PathBuf, &'static str)>,
    keep: Vec<(PathBuf, &'static str)>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl UninstallPlan {
    /// Queue `path` for removal. Paths that don't exist are dropped, so the
    /// printed plan only ever lists things actually on disk.
    pub(crate) fn remove(&mut self, path: PathBuf, label: &'static str) {
        if path.exists() || path.is_symlink() {
            self.remove.push((path, label));
        }
    }

    /// Record `path` as preserved (because of a `--keep-*` flag).
    pub(crate) fn keep(&mut self, path: PathBuf, reason: &'static str) {
        if path.exists() {
            self.keep.push((path, reason));
        }
    }

    fn print(&self) {
        if self.remove.is_empty() {
            println!("✓ No Audetic artifacts found to remove.");
        } else {
            println!("The following will be removed:");
            for (path, label) in &self.remove {
                println!("  ✗ {label} — {}", path.display());
            }
        }
        if !self.keep.is_empty() {
            println!();
            println!("The following will be preserved:");
            for (path, reason) in &self.keep {
                println!("  ✓ {} ({reason})", path.display());
            }
        }
    }

    /// Print the plan, confirm, and delete. Returns `Ok(())` even when nothing
    /// was found — uninstalling a machine that has no install is a no-op, not
    /// an error. Fails only if a removal that was supposed to happen didn't.
    pub(crate) fn execute(self, opts: &UninstallOptions) -> Result<()> {
        println!();
        self.print();
        println!();

        if self.remove.is_empty() {
            return Ok(());
        }
        if opts.dry_run {
            println!("Dry run — nothing was changed.");
            return Ok(());
        }
        if !opts.yes && !confirm("Proceed with uninstall? [y/N] ")? {
            println!("Uninstall cancelled.");
            return Ok(());
        }

        let mut failed = 0usize;
        for (path, label) in &self.remove {
            match remove_path(path) {
                Ok(()) => println!("  ✓ Removed {label}"),
                Err(err) => {
                    eprintln!("  ✗ Failed to remove {} ({err})", path.display());
                    failed += 1;
                }
            }
        }
        if failed > 0 {
            bail!("Uninstall finished with {failed} error(s)");
        }
        Ok(())
    }
}

/// Queue everything inside the config and data directories, honoring the
/// `--keep-*` flags at file granularity rather than directory granularity.
///
/// Per-file is not fussiness: on macOS `config_dir` and `data_dir` are *the
/// same* directory (`~/Library/Application Support/audetic`), so removing
/// either wholesale would take the database with it. Sweeping entries and
/// protecting `config.toml` / `audetic.db*` individually is the one rule that
/// behaves correctly on both platforms.
///
/// It also means leftovers from the retired auto-updater (`updates/`,
/// `update.lock`, `update_state.json`) get cleaned up on the way out.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn plan_state_dirs(
    plan: &mut UninstallPlan,
    config_dir: &Path,
    data_dir: &Path,
    opts: &UninstallOptions,
) {
    let mut dirs = vec![config_dir];
    if data_dir != config_dir {
        dirs.push(data_dir);
    }

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let path = entry.path();

            if opts.keep_config && name == "config.toml" {
                plan.keep(path, "--keep-config");
            } else if opts.keep_database && name.starts_with("audetic.db") {
                plan.keep(path, "--keep-database");
            } else {
                plan.remove(path, describe_state_entry(&name, entry.path().is_dir()));
            }
        }
    }
}

/// Human label for an entry in the config/data directory, for the plan output.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn describe_state_entry(name: &str, is_dir: bool) -> &'static str {
    match name {
        "bin" => "Daemon binary directory",
        "config.toml" => "Config file",
        "audetic.db" => "Transcription database",
        "audetic.db-wal" => "Database write-ahead log",
        "audetic.db-shm" => "Database shared memory",
        "meetings" => "Meeting recordings",
        "agent-runs" => "Agent run artifacts",
        "models" => "Downloaded transcription models",
        "keybind-backups" => "Keybind backups",
        "config-backups" => "Config backups",
        "updates" => "Legacy auto-update cache",
        "update.lock" => "Legacy auto-update lock",
        "update_state.json" => "Legacy auto-update state",
        _ if is_dir => "State directory",
        _ => "State file",
    }
}

/// `rm -rf` for a single path. Everything Audetic writes lives under `$HOME`,
/// so this never needs sudo.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_path(path: &Path) -> Result<()> {
    if path.is_dir() && !path.is_symlink() {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("Failed to remove directory {}", path.display()))
    } else {
        std::fs::remove_file(path).with_context(|| format!("Failed to remove {}", path.display()))
    }
}

/// Remove `dir` only if it is now empty. Used to sweep up the parent
/// directories left behind once their contents are gone; a directory that
/// still holds preserved files is left alone.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn remove_dir_if_empty(dir: &Path) {
    let _ = std::fs::remove_dir(dir);
}

/// Prompt on stdin. A non-tty stdin (piped, no answer) reads as "no".
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    std::io::stdout()
        .flush()
        .context("Failed to flush stdout")?;
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return Ok(false);
    }
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
