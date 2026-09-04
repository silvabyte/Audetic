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
#[cfg(any(target_os = "linux", target_os = "macos"))]
use rusqlite::{Connection, OpenFlags};
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
    match replace_executable(source, &target) {
        Ok(()) => {
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

/// Install `src` at `dest`, replacing whatever is there — even if that file is
/// currently executing.
///
/// Reinstalling over a running binary is the normal case, since `make install`
/// is also the upgrade path. Two things make that awkward:
///
///   - Copying straight onto a live executable fails with `ETXTBSY` ("Text file
///     busy"): the kernel refuses to open a running binary for writing.
///   - Unlinking it first dodges that, but leaves a window where the path does
///     not exist. For the daemon that window is real: the systemd unit is
///     `Restart=always`, so a process that died inside it would be respawned
///     into a missing `ExecStart`.
///
/// Staging a sibling temp file and `rename`ing it over the target avoids both.
/// `rename` swaps the directory entry atomically — the running process keeps
/// its old inode until it exits, and the path is never absent. Staging as a
/// *sibling* keeps the rename within one filesystem, where it is a true atomic
/// swap rather than a copy.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn replace_executable(src: &Path, dest: &Path) -> Result<()> {
    let staging = dest.with_extension("new");

    // A previous install could have been killed between the copy and the rename.
    let _ = std::fs::remove_file(&staging);

    std::fs::copy(src, &staging)
        .with_context(|| format!("Failed to copy {} → {}", src.display(), staging.display()))?;
    set_executable(&staging)?;

    std::fs::rename(&staging, dest).with_context(|| {
        format!(
            "Failed to move {} into place at {}",
            staging.display(),
            dest.display()
        )
    })?;
    Ok(())
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

/// Poll the daemon's HTTP API until the newly installed version responds.
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
                if let Ok(info) = resp.json::<crate::api::VersionInfo>().await {
                    if daemon_version_is_current(&info, env!("CARGO_PKG_VERSION")) {
                        return Ok(());
                    }
                }
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

fn daemon_version_is_current(info: &crate::api::VersionInfo, expected: &str) -> bool {
    info.version == expected
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
    actions: Vec<String>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UninstallOutcome {
    NoArtifacts,
    DryRun,
    Cancelled,
    Removed,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl UninstallOutcome {
    pub(crate) fn removed_anything(self) -> bool {
        self == Self::Removed
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ServeCleanupOutcome {
    Removed,
    AlreadyAbsentOrChanged,
    ManualRequired(String),
}

/// Add a cleanup action only when an existing database contains the exact
/// ownership tuple Audetic persisted while enabling Home Hub mode.
///
/// This is deliberately read-only and migration-free: uninstalling an older
/// release or an already-clean machine must not create an empty database.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn plan_audetic_serve_cleanup(plan: &mut UninstallPlan, db_path: &Path) -> bool {
    match persisted_exact_serve_ownership(db_path) {
        Ok(true) => {
            plan.action(format!(
                "Remove exact Audetic Tailscale Serve mapping (`{}`)",
                crate::sync::tailscale::audetic_serve_cleanup_command()
            ));
            true
        }
        Ok(false) => false,
        Err(error) => {
            println!(
                "  · Could not safely verify Audetic's persisted Serve ownership ({error}); Tailscale Serve will not be changed."
            );
            false
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn persisted_exact_serve_ownership(db_path: &Path) -> Result<bool> {
    if !db_path.exists() {
        return Ok(false);
    }

    let connection = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| {
        format!(
            "Failed to open {} read-only for Serve ownership inspection",
            db_path.display()
        )
    })?;
    connection
        .busy_timeout(Duration::from_secs(1))
        .context("Failed to set Serve ownership inspection timeout")?;
    let ownership = crate::db::sync_serve::SyncServeRepository::get_if_available(&connection)?;
    Ok(ownership
        .as_ref()
        .is_some_and(crate::sync::is_exact_audetic_serve_ownership))
}

/// Run removal through the same adapter used by Home Hub role transitions.
/// The adapter re-reads live Serve/Funnel JSON and removes nothing unless the
/// live mapping is still exactly Audetic's path and proxy.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn cleanup_audetic_serve_if_planned(planned: bool) {
    if !planned {
        return;
    }
    let tailscale =
        crate::sync::tailscale::Tailscale::new(crate::sync::tailscale::SystemCommandRunner);
    print_serve_cleanup_outcome(cleanup_audetic_serve_with(&tailscale));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn cleanup_audetic_serve_with(
    tailscale: &dyn crate::sync::tailscale::TailscaleControl,
) -> ServeCleanupOutcome {
    match tailscale.remove_audetic_serve() {
        Ok(true) => ServeCleanupOutcome::Removed,
        Ok(false) => ServeCleanupOutcome::AlreadyAbsentOrChanged,
        Err(error) => ServeCleanupOutcome::ManualRequired(error.to_string()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn print_serve_cleanup_outcome(outcome: ServeCleanupOutcome) {
    match outcome {
        ServeCleanupOutcome::Removed => {
            println!("  ✓ Removed exact Audetic Tailscale Serve mapping")
        }
        ServeCleanupOutcome::AlreadyAbsentOrChanged => println!(
            "  · Audetic's exact live Serve mapping was absent or changed; left Tailscale untouched"
        ),
        ServeCleanupOutcome::ManualRequired(error) => {
            println!("  · Could not run Tailscale cleanup ({error}).");
            println!("    Remove only Audetic's path mapping when Tailscale is available:");
            println!(
                "    {}",
                crate::sync::tailscale::audetic_serve_cleanup_command()
            );
        }
    }
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

    /// Record a supervisor action that has no on-disk artifact. This keeps
    /// registered legacy services visible in the confirmation plan and makes
    /// confirmation happen before stop/disable side effects.
    pub(crate) fn action(&mut self, description: impl Into<String>) {
        self.actions.push(description.into());
    }

    fn print(&self) {
        if self.remove.is_empty() && self.actions.is_empty() {
            println!("✓ No Audetic artifacts found to remove.");
        } else {
            println!("The following changes will be made:");
            for action in &self.actions {
                println!("  ✗ {action}");
            }
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

    /// Print the plan, confirm, run supervisor teardown, and then delete.
    /// The returned outcome lets platform cleanup distinguish a completed
    /// uninstall from a dry run, cancellation, or no-op.
    pub(crate) fn execute(
        self,
        opts: &UninstallOptions,
        before_remove: impl FnOnce() -> Result<()>,
    ) -> Result<UninstallOutcome> {
        self.execute_with_confirmation(opts, confirm, before_remove)
    }

    fn execute_with_confirmation(
        self,
        opts: &UninstallOptions,
        mut confirmer: impl FnMut(&str) -> Result<bool>,
        before_remove: impl FnOnce() -> Result<()>,
    ) -> Result<UninstallOutcome> {
        println!();
        self.print();
        println!();

        if self.remove.is_empty() && self.actions.is_empty() {
            return Ok(UninstallOutcome::NoArtifacts);
        }
        if opts.dry_run {
            println!("Dry run — nothing was changed.");
            return Ok(UninstallOutcome::DryRun);
        }
        if !opts.yes && !confirmer("Proceed with uninstall? [y/N] ")? {
            println!("Uninstall cancelled.");
            return Ok(UninstallOutcome::Cancelled);
        }

        before_remove()?;

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
        Ok(UninstallOutcome::Removed)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::db::sync_serve::{SyncServeOwnership, SyncServeRepository};
    use crate::sync::tailscale::{
        MappingState, ServeAssessment, TailscaleControl, TailscaleError, TailscaleStatus,
    };

    fn options() -> UninstallOptions {
        UninstallOptions {
            yes: false,
            dry_run: false,
            keep_config: false,
            keep_database: false,
        }
    }

    #[test]
    fn readiness_requires_the_exact_build_version() {
        let info = crate::api::VersionInfo {
            name: "audetic".to_string(),
            version: "1.2.3".to_string(),
            instance_id: "test-process".to_string(),
        };

        assert!(daemon_version_is_current(&info, "1.2.3"));
        assert!(!daemon_version_is_current(&info, "1.2.2"));
    }

    #[test]
    fn cancelled_uninstall_does_not_teardown_or_remove() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("artifact");
        std::fs::write(&artifact, "keep").unwrap();
        let teardown_called = Cell::new(false);
        let mut plan = UninstallPlan::default();
        plan.remove(artifact.clone(), "Test artifact");

        let outcome = plan
            .execute_with_confirmation(
                &options(),
                |_| Ok(false),
                || {
                    teardown_called.set(true);
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(outcome, UninstallOutcome::Cancelled);
        assert!(!teardown_called.get());
        assert!(artifact.exists());
    }

    #[test]
    fn action_only_legacy_service_plan_confirms_before_teardown() {
        let teardown_called = Cell::new(false);
        let mut plan = UninstallPlan::default();
        plan.action("Stop and disable legacy audetic.service");

        let outcome = plan
            .execute_with_confirmation(
                &options(),
                |_| Ok(false),
                || {
                    teardown_called.set(true);
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(outcome, UninstallOutcome::Cancelled);
        assert!(!teardown_called.get());
    }

    #[test]
    fn dry_run_does_not_confirm_teardown_or_remove() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("artifact");
        std::fs::write(&artifact, "keep").unwrap();
        let confirm_called = Cell::new(false);
        let teardown_called = Cell::new(false);
        let mut opts = options();
        opts.dry_run = true;
        let mut plan = UninstallPlan::default();
        plan.remove(artifact.clone(), "Test artifact");

        let outcome = plan
            .execute_with_confirmation(
                &opts,
                |_| {
                    confirm_called.set(true);
                    Ok(true)
                },
                || {
                    teardown_called.set(true);
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(outcome, UninstallOutcome::DryRun);
        assert!(!confirm_called.get());
        assert!(!teardown_called.get());
        assert!(artifact.exists());
    }

    #[test]
    fn confirmed_uninstall_tears_down_before_removing() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("artifact");
        std::fs::write(&artifact, "remove").unwrap();
        let teardown_saw_artifact = Cell::new(false);
        let mut plan = UninstallPlan::default();
        plan.remove(artifact.clone(), "Test artifact");

        let outcome = plan
            .execute_with_confirmation(
                &options(),
                |_| Ok(true),
                || {
                    teardown_saw_artifact.set(artifact.exists());
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(outcome, UninstallOutcome::Removed);
        assert!(teardown_saw_artifact.get());
        assert!(!artifact.exists());
    }

    struct FakeTailscale {
        remove_calls: AtomicUsize,
        remove_result: Result<bool, TailscaleError>,
    }

    impl TailscaleControl for FakeTailscale {
        fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
            panic!("uninstall must not request general Tailscale status")
        }

        fn serve_assessment(&self) -> Result<ServeAssessment, TailscaleError> {
            Ok(ServeAssessment {
                mapping: MappingState::OwnedByAudetic,
                funnel_enabled: false,
            })
        }

        fn apply_audetic_serve(&self) -> Result<bool, TailscaleError> {
            panic!("uninstall must never apply a Serve mapping")
        }

        fn remove_audetic_serve(&self) -> Result<bool, TailscaleError> {
            self.remove_calls.fetch_add(1, Ordering::SeqCst);
            match &self.remove_result {
                Ok(value) => Ok(*value),
                Err(_) => Err(TailscaleError::Execute(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "tailscale unavailable",
                ))),
            }
        }

        fn serve_preview(&self) -> String {
            panic!("uninstall must not request the apply preview")
        }
    }

    fn save_serve_ownership(path: &Path, ownership: SyncServeOwnership) {
        let connection = crate::db::migrate_db_at(path).unwrap();
        SyncServeRepository::save(&connection, &ownership).unwrap();
    }

    fn exact_ownership() -> SyncServeOwnership {
        SyncServeOwnership {
            https_port: crate::sync::protocol::TAILSCALE_HTTPS_PORT,
            mount_path: crate::sync::protocol::HUB_API_MOUNT_PATH
                .trim_end_matches('/')
                .into(),
            proxy_url: crate::sync::protocol::HUB_LOOPBACK_BASE_URL.into(),
        }
    }

    #[test]
    fn absent_database_does_not_get_created_or_schedule_serve_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("missing").join("audetic.db");
        let mut plan = UninstallPlan::default();

        assert!(!plan_audetic_serve_cleanup(&mut plan, &db_path));
        assert!(!db_path.exists());
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn only_exact_persisted_ownership_schedules_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let exact_path = temp.path().join("exact.db");
        save_serve_ownership(&exact_path, exact_ownership());
        let mut exact_plan = UninstallPlan::default();

        assert!(plan_audetic_serve_cleanup(&mut exact_plan, &exact_path));
        assert_eq!(exact_plan.actions.len(), 1);
        assert!(
            exact_plan.actions[0].contains("tailscale serve --https=8443 --set-path=/audetic off")
        );
        assert!(!exact_plan.actions[0].contains("reset"));

        let drifted_path = temp.path().join("drifted.db");
        let mut drifted = exact_ownership();
        drifted.proxy_url = "http://127.0.0.1:9999".into();
        save_serve_ownership(&drifted_path, drifted);
        let mut drifted_plan = UninstallPlan::default();
        assert!(!plan_audetic_serve_cleanup(
            &mut drifted_plan,
            &drifted_path
        ));
        assert!(drifted_plan.actions.is_empty());
    }

    #[test]
    fn kept_database_is_preserved_but_its_exact_serve_mapping_is_still_scheduled() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("audetic");
        std::fs::create_dir(&state_dir).unwrap();
        let db_path = state_dir.join("audetic.db");
        save_serve_ownership(&db_path, exact_ownership());
        let mut opts = options();
        opts.keep_database = true;
        let mut plan = UninstallPlan::default();

        assert!(plan_audetic_serve_cleanup(&mut plan, &db_path));
        plan_state_dirs(&mut plan, &state_dir, &state_dir, &opts);

        assert!(plan
            .actions
            .iter()
            .any(|action| action.contains("tailscale serve --https=8443 --set-path=/audetic off")));
        assert!(plan.keep.iter().any(|(path, _)| path == &db_path));
        assert!(!plan.remove.iter().any(|(path, _)| path == &db_path));
    }

    #[test]
    fn pre_sync_database_is_safe_and_not_migrated() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("legacy.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute("CREATE TABLE legacy (id INTEGER PRIMARY KEY)", [])
            .unwrap();
        drop(connection);

        assert!(!persisted_exact_serve_ownership(&db_path).unwrap());
        let connection = Connection::open(&db_path).unwrap();
        let migration_table: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!migration_table);
    }

    #[test]
    fn cleanup_uses_injected_adapter_and_reports_manual_exact_command_when_unavailable() {
        let removed = FakeTailscale {
            remove_calls: AtomicUsize::new(0),
            remove_result: Ok(true),
        };
        assert_eq!(
            cleanup_audetic_serve_with(&removed),
            ServeCleanupOutcome::Removed
        );
        assert_eq!(removed.remove_calls.load(Ordering::SeqCst), 1);

        let unavailable = FakeTailscale {
            remove_calls: AtomicUsize::new(0),
            remove_result: Err(TailscaleError::Execute(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "tailscale unavailable",
            ))),
        };
        let outcome = cleanup_audetic_serve_with(&unavailable);
        assert!(matches!(outcome, ServeCleanupOutcome::ManualRequired(_)));
        assert_eq!(unavailable.remove_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            crate::sync::tailscale::audetic_serve_cleanup_command(),
            "tailscale serve --https=8443 --set-path=/audetic off"
        );
    }
}
