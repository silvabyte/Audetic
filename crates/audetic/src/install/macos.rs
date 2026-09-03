//! macOS install: place Audetic.app under `~/Applications/`, drop a
//! LaunchAgent plist at `~/Library/LaunchAgents/ai.audetic.daemon.plist`,
//! `launchctl bootstrap` it, probe for readiness, `open` the UI.
//!
//! The daemon must be invoked from inside an `Audetic.app` bundle (so the
//! responsible-process attribution for TCC ends up on the bundle, not on
//! the terminal that launched it). If `current_exe()` isn't pointing
//! inside a `.app`, install fails with a hint.
//!
//! Uninstall has no such requirement — it only touches `$HOME`-derived
//! destinations, so it runs from any binary. That's why `source_bundle` is
//! optional: install demands it, teardown doesn't.

use super::{
    remove_dir_if_empty, wait_for_daemon, InstallOptions, UninstallOptions, UninstallPlan,
};
use crate::api::url;
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

const PLIST_TEMPLATE: &str = include_str!("audetic.daemon.plist.tmpl");
const MENUBAR_PLIST_TEMPLATE: &str = include_str!("audetic.menubar.plist.tmpl");
const LABEL: &str = "ai.audetic.daemon";
const MENUBAR_LABEL: &str = "ai.audetic.menubar";
const BUNDLE_NAME: &str = "Audetic.app";
const MENUBAR_APP_NAME: &str = "Audetic Menu Bar.app";

/// How long to wait for a booted-out agent to actually leave launchd's registry.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const TEARDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// `bootstrap` attempts before giving up, in case launchd is still settling
/// past `TEARDOWN_TIMEOUT`.
const BOOTSTRAP_ATTEMPTS: u32 = 3;
const BOOTSTRAP_RETRY_DELAY: Duration = Duration::from_millis(500);

pub async fn run(opts: InstallOptions) -> Result<()> {
    let paths = InstallPaths::resolve()?;
    let app_url = url::app_url();

    println!("→ Installing audeticd as a LaunchAgent");
    ensure_runtime_dirs(&paths)?;

    // Stop the agents before touching the bundle they're running out of.
    // `place_bundle` deletes the installed bundle before re-copying it, and both
    // LaunchAgents are `KeepAlive=true` — so a daemon that exits mid-swap gets
    // respawned by launchd into a half-deleted bundle. Booting them out first
    // makes the replacement quiet. No-op on a first install.
    bootout_agents();

    place_bundle(&paths)?;
    place_cli(&paths);
    write_plist(&paths)?;
    bootstrap_agent(&paths)?;
    wait_for_daemon(Duration::from_secs(15)).await?;
    println!("✓ {LABEL} is active");

    // Best-effort: register the embedded menu-bar agent so it starts on login
    // and right now. Never fail the daemon install over the UI helper.
    register_menubar_agent(&paths);

    if opts.no_launch {
        println!("  Open {app_url} in your browser to finish onboarding.");
    } else {
        match open_url(&app_url) {
            Ok(()) => println!("→ Opened {app_url}"),
            Err(err) => println!("  Open {app_url} in your browser to finish onboarding ({err})"),
        }
    }
    Ok(())
}

struct InstallPaths {
    /// The `Audetic.app` we were launched from, when there is one. `None` when
    /// the daemon is invoked as a bare binary — fine for uninstall, fatal for
    /// install (see `source_bundle()`).
    source_bundle: Option<PathBuf>,
    installed_bundle: PathBuf,
    installed_binary: PathBuf,
    plist_path: PathBuf,
    menubar_plist_path: PathBuf,
    log_dir: PathBuf,
    log_path: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    home: PathBuf,
}

impl InstallPaths {
    fn resolve() -> Result<Self> {
        let current = std::env::current_exe()
            .context("Could not determine the path of the running audetic binary")?;

        // Walk up: Contents/MacOS/audeticd → Contents/MacOS → Contents → Audetic.app
        let source_bundle = current
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("app"));

        let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not resolve $HOME"))?;
        let installed_bundle = home.join("Applications").join(BUNDLE_NAME);
        let installed_binary = installed_bundle
            .join("Contents")
            .join("MacOS")
            .join("audeticd");
        let launch_agents = home.join("Library").join("LaunchAgents");
        let plist_path = launch_agents.join(format!("{LABEL}.plist"));
        let menubar_plist_path = launch_agents.join(format!("{MENUBAR_LABEL}.plist"));
        let log_dir = home.join("Library").join("Logs").join("Audetic");
        let log_path = log_dir.join("audetic.log");

        // `dirs::config_dir()` / `dirs::data_dir()` both resolve to
        // `~/Library/Application Support` on macOS — same tree the rest of
        // the daemon uses for state.
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow!("Could not resolve ~/Library/Application Support"))?
            .join("audetic");
        let data_dir = dirs::data_dir()
            .ok_or_else(|| anyhow!("Could not resolve ~/Library/Application Support"))?
            .join("audetic");

        Ok(Self {
            source_bundle,
            installed_bundle,
            installed_binary,
            plist_path,
            menubar_plist_path,
            log_dir,
            log_path,
            config_dir,
            data_dir,
            home,
        })
    }

    /// The bundle we're running from. Install requires one: TCC attributes the
    /// Microphone / Screen Recording grants to the bundle's code signature, so
    /// a bare binary would install a daemon that can never be granted access.
    fn source_bundle(&self) -> Result<&Path> {
        self.source_bundle.as_deref().ok_or_else(|| {
            anyhow!(
                "audeticd must be invoked from inside an `Audetic.app` bundle on macOS. \
                 Build it with `make macos-app`, then run `make install`."
            )
        })
    }
}

fn ensure_runtime_dirs(paths: &InstallPaths) -> Result<()> {
    for dir in [
        &paths.config_dir,
        &paths.data_dir,
        &paths.log_dir,
        &paths.home.join("Applications"),
        &paths
            .plist_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| paths.home.join("Library/LaunchAgents")),
    ] {
        fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    Ok(())
}

fn place_bundle(paths: &InstallPaths) -> Result<()> {
    let source_bundle = paths.source_bundle()?;

    if source_bundle == paths.installed_bundle {
        println!(
            "  · Bundle already at {} (skipping copy)",
            paths.installed_bundle.display()
        );
        return Ok(());
    }

    println!(
        "  · Copying {} → {}",
        source_bundle.display(),
        paths.installed_bundle.display()
    );

    // Old bundle has to go before the copy or we end up with stale binaries
    // and resources mixed with the new ones.
    if paths.installed_bundle.exists() {
        fs::remove_dir_all(&paths.installed_bundle).with_context(|| {
            format!(
                "Failed to remove existing {}",
                paths.installed_bundle.display()
            )
        })?;
    }

    // `cp -R` preserves the bundle's codesign metadata; re-implementing this
    // in pure Rust would require care around extended attributes and resource
    // forks. Shelling out is plenty.
    let status = Command::new("cp")
        .arg("-R")
        .arg(source_bundle)
        .arg(&paths.installed_bundle)
        .status()
        .context("Failed to run `cp -R` to copy the bundle")?;
    if !status.success() {
        bail!(
            "`cp -R {} {}` exited with {status}",
            source_bundle.display(),
            paths.installed_bundle.display(),
        );
    }
    Ok(())
}

/// Best-effort: copy the installed bundle's standalone `audetic` CLI onto PATH
/// (`~/.local/bin/audetic`, under `$HOME`, no sudo) via the shared placement
/// helper. Never fails the install.
fn place_cli(paths: &InstallPaths) {
    let cli_source = paths
        .installed_bundle
        .join("Contents")
        .join("MacOS")
        .join("audetic");
    super::place_cli_on_path(&cli_source);
}

fn write_plist(paths: &InstallPaths) -> Result<()> {
    let exec = paths
        .installed_binary
        .to_str()
        .ok_or_else(|| anyhow!("Installed binary path contains non-UTF8 bytes"))?;
    let log = paths
        .log_path
        .to_str()
        .ok_or_else(|| anyhow!("Log path contains non-UTF8 bytes"))?;
    let home = paths
        .home
        .to_str()
        .ok_or_else(|| anyhow!("$HOME contains non-UTF8 bytes"))?;

    let plist = PLIST_TEMPLATE
        .replace("__EXEC_START__", exec)
        .replace("__LOG_PATH__", log)
        .replace("__HOME__", home);

    fs::write(&paths.plist_path, plist)
        .with_context(|| format!("Failed to write {}", paths.plist_path.display()))?;
    println!("  · Wrote {}", paths.plist_path.display());
    Ok(())
}

fn current_uid() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("Failed to run `id -u`")?;
    if !output.status.success() {
        bail!("`id -u` exited with {}", output.status);
    }
    Ok(String::from_utf8(output.stdout)
        .context("`id -u` output is not UTF-8")?
        .trim()
        .to_string())
}

fn bootstrap_agent(paths: &InstallPaths) -> Result<()> {
    let uid = current_uid()?;
    let domain = format!("gui/{uid}");
    let service_target = format!("{domain}/{LABEL}");

    let plist = paths
        .plist_path
        .to_str()
        .ok_or_else(|| anyhow!("Plist path contains non-UTF8 bytes"))?;

    println!("  · launchctl bootstrap {domain} {plist}");
    reload_agent(&domain, &service_target, plist)
}

/// Re-register one agent: `bootout`, wait for launchd to let go of the label,
/// then `bootstrap` and `kickstart`.
///
/// The wait is the point. `launchctl bootout` is asynchronous — it returns once
/// SIGTERM is delivered, not once the job is unloaded, and `audeticd` holds a
/// CoreAudio device plus an HTTP listener that can outlive that return.
/// Bootstrapping a label launchd still considers alive fails with its catch-all
/// `Bootstrap failed: 5: Input/output error`, which made `make install` fail
/// intermittently on reinstall.
fn reload_agent(domain: &str, service_target: &str, plist: &str) -> Result<()> {
    // Idempotency: tear down a previous registration if present. `bootout`
    // returns non-zero when nothing is registered yet, which is fine —
    // suppress the error and continue.
    let _ = Command::new("launchctl")
        .args(["bootout", service_target])
        .output();

    if !wait_for_service_gone(service_target, TEARDOWN_TIMEOUT) {
        println!("  · {service_target} is still unloading; bootstrapping anyway");
    }

    let mut last_status: Option<ExitStatus> = None;
    for attempt in 1..=BOOTSTRAP_ATTEMPTS {
        let status = Command::new("launchctl")
            .args(["bootstrap", domain, plist])
            .status()
            .context("Failed to run `launchctl bootstrap`")?;
        if status.success() {
            // `bootstrap` queues the load but the daemon's first `play()` can
            // lag a beat; `kickstart` makes sure it starts immediately.
            let _ = Command::new("launchctl")
                .args(["kickstart", "-k", service_target])
                .status();
            return Ok(());
        }
        last_status = Some(status);
        if attempt < BOOTSTRAP_ATTEMPTS {
            println!(
                "  · bootstrap exited with {status}; retrying ({attempt}/{BOOTSTRAP_ATTEMPTS})"
            );
            std::thread::sleep(BOOTSTRAP_RETRY_DELAY);
        }
    }

    let status = last_status.expect("loop body runs at least once");
    bail!("`launchctl bootstrap {domain} {plist}` exited with {status} after {BOOTSTRAP_ATTEMPTS} attempts");
}

/// Poll `launchctl print` until `service_target` is no longer registered.
/// Returns `false` if it was still registered when `timeout` elapsed — the
/// caller proceeds anyway, since `bootstrap` is retried regardless.
fn wait_for_service_gone(service_target: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let registered = Command::new("launchctl")
            .args(["print", service_target])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if !registered {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(TEARDOWN_POLL_INTERVAL);
    }
}

/// Register the embedded "Audetic Menu Bar.app" as a per-user LaunchAgent
/// (`ai.audetic.menubar`) and start it. Best-effort — prints a hint and
/// returns on any failure rather than aborting the daemon install. The menu
/// bar is a convenience UI helper (status + toggles + global shortcuts), not a
/// required service.
fn register_menubar_agent(paths: &InstallPaths) {
    let menubar_binary = paths
        .installed_bundle
        .join("Contents")
        .join("Library")
        .join("LoginItems")
        .join(MENUBAR_APP_NAME)
        .join("Contents")
        .join("MacOS")
        .join("AudeticMenuBar");

    if !menubar_binary.exists() {
        println!(
            "  · Menu bar app not found at {}; skipping (older bundle?).",
            menubar_binary.display()
        );
        return;
    }

    let plist_path = paths.menubar_plist_path.clone();
    let log_path = paths.log_dir.join("audetic-menubar.log");

    if let Err(err) = write_menubar_plist(paths, &menubar_binary, &plist_path, &log_path) {
        println!("  · Could not write menu bar LaunchAgent ({err}); skipping.");
        return;
    }

    match bootstrap_menubar_agent(&plist_path) {
        Ok(()) => println!("✓ {MENUBAR_LABEL} is active (menu bar)"),
        Err(err) => println!(
            "  · Could not start menu bar agent ({err}); open it from {} manually.",
            paths.installed_bundle.display()
        ),
    }
}

fn write_menubar_plist(
    paths: &InstallPaths,
    menubar_binary: &Path,
    plist_path: &Path,
    log_path: &Path,
) -> Result<()> {
    let exec = menubar_binary
        .to_str()
        .ok_or_else(|| anyhow!("Menu bar binary path contains non-UTF8 bytes"))?;
    let log = log_path
        .to_str()
        .ok_or_else(|| anyhow!("Menu bar log path contains non-UTF8 bytes"))?;
    let home = paths
        .home
        .to_str()
        .ok_or_else(|| anyhow!("$HOME contains non-UTF8 bytes"))?;

    let plist = MENUBAR_PLIST_TEMPLATE
        .replace("__EXEC_START__", exec)
        .replace("__LOG_PATH__", log)
        .replace("__HOME__", home);

    fs::write(plist_path, plist)
        .with_context(|| format!("Failed to write {}", plist_path.display()))?;
    println!("  · Wrote {}", plist_path.display());
    Ok(())
}

fn bootstrap_menubar_agent(plist_path: &Path) -> Result<()> {
    let uid = current_uid()?;
    let domain = format!("gui/{uid}");
    let service_target = format!("{domain}/{MENUBAR_LABEL}");

    let plist = plist_path
        .to_str()
        .ok_or_else(|| anyhow!("Plist path contains non-UTF8 bytes"))?;

    reload_agent(&domain, &service_target, plist).context("Failed to register the menu bar agent")
}

fn open_url(url: &str) -> Result<()> {
    let status = Command::new("open")
        .arg(url)
        .status()
        .context("Failed to spawn `open`")?;
    if !status.success() {
        bail!("`open {url}` exited with {status}");
    }
    Ok(())
}

/// Tear down both LaunchAgents and remove everything `run` installed.
///
/// Runs from any binary, not just one inside `Audetic.app` — teardown only
/// touches `$HOME`-derived destinations, so `make uninstall` works even when
/// the bundle was never built.
///
/// TCC grants are deliberately left alone: `tccutil reset` would also clear
/// permissions for any other build of Audetic on the machine, and the grants
/// are keyed to a code signature that a reinstall reuses anyway.
pub fn uninstall(opts: UninstallOptions) -> Result<()> {
    let paths = InstallPaths::resolve()?;

    println!("→ Uninstalling audeticd (LaunchAgents)");

    let mut plan = UninstallPlan::default();
    plan.remove(paths.installed_bundle.clone(), "Audetic.app bundle");
    plan.remove(paths.plist_path.clone(), "Daemon LaunchAgent");
    plan.remove(paths.menubar_plist_path.clone(), "Menu bar LaunchAgent");
    plan.remove(paths.log_dir.clone(), "Log directory");
    if let Some(cli) = super::cli_target_path() {
        plan.remove(cli, "Standalone `audetic` CLI");
    }
    super::plan_state_dirs(&mut plan, &paths.config_dir, &paths.data_dir, &opts);

    let outcome = plan.execute(&opts, || {
        bootout_agents();
        Ok(())
    })?;

    if outcome.removed_anything() {
        remove_dir_if_empty(&paths.config_dir);
        remove_dir_if_empty(&paths.data_dir);
        println!("✓ Audetic has been uninstalled");
    }
    Ok(())
}

/// Best-effort `launchctl bootout` for the daemon and the menu-bar agent, so
/// neither is holding the database open (or respawning) while we delete their
/// files. `bootout` exits non-zero when nothing is registered — not an error.
fn bootout_agents() {
    let Ok(uid) = current_uid() else {
        println!("  · Could not resolve uid; skipping launchctl teardown");
        return;
    };
    for label in [LABEL, MENUBAR_LABEL] {
        let target = format!("gui/{uid}/{label}");
        println!("  · launchctl bootout {target}");
        let _ = Command::new("launchctl")
            .args(["bootout", &target])
            .output();
        // `bootout` returns on SIGTERM delivery, not on unload — wait for the
        // job to actually go away before the caller deletes or replaces the
        // files it is running out of.
        if !wait_for_service_gone(&target, TEARDOWN_TIMEOUT) {
            println!("  · {target} did not unload within {TEARDOWN_TIMEOUT:?}; continuing");
        }
    }
}
