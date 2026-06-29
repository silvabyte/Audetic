//! macOS install: place Audetic.app under `~/Applications/`, drop a
//! LaunchAgent plist at `~/Library/LaunchAgents/ai.audetic.daemon.plist`,
//! `launchctl bootstrap` it, probe for readiness, `open` the UI.
//!
//! The daemon must be invoked from inside an `Audetic.app` bundle (so the
//! responsible-process attribution for TCC ends up on the bundle, not on
//! the terminal that launched it). If `current_exe()` isn't pointing
//! inside a `.app`, install fails with a hint.

use super::{wait_for_daemon, InstallOptions};
use crate::api::url;
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const PLIST_TEMPLATE: &str = include_str!("audetic.daemon.plist.tmpl");
const MENUBAR_PLIST_TEMPLATE: &str = include_str!("audetic.menubar.plist.tmpl");
const LABEL: &str = "ai.audetic.daemon";
const MENUBAR_LABEL: &str = "ai.audetic.menubar";
const BUNDLE_NAME: &str = "Audetic.app";
const MENUBAR_APP_NAME: &str = "Audetic Menu Bar.app";

pub async fn run(opts: InstallOptions) -> Result<()> {
    let paths = InstallPaths::resolve()?;
    let app_url = url::app_url();

    println!("→ Installing audeticd as a LaunchAgent");
    ensure_runtime_dirs(&paths)?;
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
    source_bundle: PathBuf,
    installed_bundle: PathBuf,
    installed_binary: PathBuf,
    plist_path: PathBuf,
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
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("app"))
            .ok_or_else(|| {
                anyhow!(
                    "audeticd must be invoked from inside an `Audetic.app` bundle on macOS; \
                     current_exe is {}. Build the bundle with `make macos-app` and run \
                     `./target/release/Audetic.app/Contents/MacOS/audeticd install`.",
                    current.display()
                )
            })?;

        let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not resolve $HOME"))?;
        let installed_bundle = home.join("Applications").join(BUNDLE_NAME);
        let installed_binary = installed_bundle
            .join("Contents")
            .join("MacOS")
            .join("audeticd");
        let plist_path = home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LABEL}.plist"));
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
            log_dir,
            log_path,
            config_dir,
            data_dir,
            home,
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
    if paths.source_bundle == paths.installed_bundle {
        println!(
            "  · Bundle already at {} (skipping copy)",
            paths.installed_bundle.display()
        );
        return Ok(());
    }

    println!(
        "  · Copying {} → {}",
        paths.source_bundle.display(),
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
        .arg(&paths.source_bundle)
        .arg(&paths.installed_bundle)
        .status()
        .context("Failed to run `cp -R` to copy the bundle")?;
    if !status.success() {
        bail!(
            "`cp -R {} {}` exited with {status}",
            paths.source_bundle.display(),
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

    // Idempotency: tear down a previous registration if present. `bootout`
    // returns non-zero when nothing is registered yet, which is fine —
    // suppress the error and continue.
    let _ = Command::new("launchctl")
        .args(["bootout", &service_target])
        .status();

    let plist = paths
        .plist_path
        .to_str()
        .ok_or_else(|| anyhow!("Plist path contains non-UTF8 bytes"))?;

    println!("  · launchctl bootstrap {domain} {plist}");
    let status = Command::new("launchctl")
        .args(["bootstrap", &domain, plist])
        .status()
        .context("Failed to run `launchctl bootstrap`")?;
    if !status.success() {
        bail!("`launchctl bootstrap {domain} {plist}` exited with {status}");
    }

    // `bootstrap` queues the load but the daemon's first `play()` can lag a
    // beat; `kickstart` makes sure it starts immediately.
    let _ = Command::new("launchctl")
        .args(["kickstart", "-k", &service_target])
        .status();

    Ok(())
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

    let plist_path = paths
        .home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{MENUBAR_LABEL}.plist"));
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

    // Idempotency: tear down a previous registration if present.
    let _ = Command::new("launchctl")
        .args(["bootout", &service_target])
        .status();

    let plist = plist_path
        .to_str()
        .ok_or_else(|| anyhow!("Plist path contains non-UTF8 bytes"))?;

    let status = Command::new("launchctl")
        .args(["bootstrap", &domain, plist])
        .status()
        .context("Failed to run `launchctl bootstrap` for the menu bar agent")?;
    if !status.success() {
        bail!("`launchctl bootstrap {domain} {plist}` exited with {status}");
    }

    let _ = Command::new("launchctl")
        .args(["kickstart", "-k", &service_target])
        .status();

    Ok(())
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
