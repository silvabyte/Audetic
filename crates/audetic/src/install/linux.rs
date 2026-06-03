//! Linux install: systemd user unit at `~/.config/systemd/user/audeticd.service`,
//! `enable --now`, readiness probe, `xdg-open` the UI. Also places the standalone
//! `audetic` CLI on PATH (`~/.local/bin/audetic`).

use super::{wait_for_daemon, InstallOptions};
use crate::api::url;
use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const SERVICE_TEMPLATE: &str = include_str!("audetic.service.tmpl");
const SERVICE_NAME: &str = "audeticd.service";

pub async fn run(opts: InstallOptions) -> Result<()> {
    let paths = InstallPaths::resolve()?;
    let app_url = url::app_url();

    println!("→ Installing audeticd as a systemd user service");
    place_binary(&paths)?;
    place_cli();
    ensure_runtime_dirs(&paths)?;
    write_unit(&paths)?;
    daemon_reload()?;
    enable_and_start()?;
    wait_for_daemon(Duration::from_secs(15)).await?;
    println!("✓ audeticd.service is active");

    if opts.no_launch {
        println!("  Open {app_url} in your browser to finish onboarding.");
    } else {
        match open_browser(&app_url) {
            Ok(()) => println!("→ Opened {app_url}"),
            Err(err) => println!("  Open {app_url} in your browser to finish onboarding ({err})"),
        }
    }
    Ok(())
}

struct InstallPaths {
    installed_dir: PathBuf,
    installed_binary: PathBuf,
    systemd_unit: PathBuf,
    // Listed in the unit's `ReadWritePaths=` — must exist before the
    // service starts or systemd fails with status=226/NAMESPACE.
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl InstallPaths {
    fn resolve() -> Result<Self> {
        let data = dirs::data_local_dir()
            .ok_or_else(|| anyhow!("Could not resolve XDG_DATA_HOME / ~/.local/share"))?;
        let config = dirs::config_local_dir()
            .ok_or_else(|| anyhow!("Could not resolve XDG_CONFIG_HOME / ~/.config"))?;

        let data_dir = data.join("audetic");
        let installed_dir = data_dir.join("bin");
        let installed_binary = installed_dir.join("audeticd");
        let systemd_unit = config.join("systemd").join("user").join(SERVICE_NAME);
        let config_dir = config.join("audetic");

        Ok(Self {
            installed_dir,
            installed_binary,
            systemd_unit,
            config_dir,
            data_dir,
        })
    }
}

fn ensure_runtime_dirs(paths: &InstallPaths) -> Result<()> {
    for dir in [&paths.config_dir, &paths.data_dir] {
        fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    Ok(())
}

fn place_binary(paths: &InstallPaths) -> Result<()> {
    let current = std::env::current_exe()
        .context("Could not determine the path of the running audetic binary")?;
    fs::create_dir_all(&paths.installed_dir)
        .with_context(|| format!("Failed to create {}", paths.installed_dir.display()))?;

    if same_file(&current, &paths.installed_binary) {
        println!(
            "  · Binary already at {} (skipping copy)",
            paths.installed_binary.display()
        );
    } else {
        println!("  · Copying binary → {}", paths.installed_binary.display());
        fs::copy(&current, &paths.installed_binary).with_context(|| {
            format!(
                "Failed to copy {} → {}",
                current.display(),
                paths.installed_binary.display()
            )
        })?;
        set_executable(&paths.installed_binary)?;
    }
    Ok(())
}

/// Best-effort: copy the standalone `audetic` CLI (shipped next to `audeticd`
/// in the release archive) onto PATH at `~/.local/bin/audetic`. Never fails the
/// install — if the CLI isn't found alongside the daemon, we just print a hint.
fn place_cli() {
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    let source = match current.parent().map(|dir| dir.join("audetic")) {
        Some(p) if p.exists() => p,
        _ => {
            println!(
                "  · Standalone `audetic` CLI not found next to the daemon; skipping PATH install."
            );
            return;
        }
    };

    let target_dir =
        dirs::executable_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local").join("bin")));
    let Some(target_dir) = target_dir else {
        return;
    };
    let target = target_dir.join("audetic");

    if fs::create_dir_all(&target_dir).is_err() {
        println!(
            "  · Could not create {}; skipping CLI install.",
            target_dir.display()
        );
        return;
    }

    match fs::copy(&source, &target) {
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
fn on_path(dir: &Path) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|p| p == dir))
        .unwrap_or(false)
}

fn write_unit(paths: &InstallPaths) -> Result<()> {
    if let Some(parent) = paths.systemd_unit.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let exec_start = paths
        .installed_binary
        .to_str()
        .ok_or_else(|| anyhow!("Install path contains non-UTF8 bytes; refusing to render unit"))?;
    let unit = SERVICE_TEMPLATE.replace("__EXEC_START__", exec_start);
    fs::write(&paths.systemd_unit, unit)
        .with_context(|| format!("Failed to write {}", paths.systemd_unit.display()))?;
    println!("  · Wrote {}", paths.systemd_unit.display());
    Ok(())
}

fn daemon_reload() -> Result<()> {
    println!("  · systemctl --user daemon-reload");
    let status = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status()
        .context("Failed to run systemctl (is systemd available?)")?;
    if !status.success() {
        bail!("`systemctl --user daemon-reload` exited with {status}");
    }
    Ok(())
}

fn enable_and_start() -> Result<()> {
    println!("  · systemctl --user enable --now {SERVICE_NAME}");
    let status = Command::new("systemctl")
        .args(["--user", "enable", "--now", SERVICE_NAME])
        .status()
        .context("Failed to run systemctl enable --now")?;
    if !status.success() {
        bail!("`systemctl --user enable --now {SERVICE_NAME}` exited with {status}");
    }
    Ok(())
}

fn open_browser(url: &str) -> Result<()> {
    let status = Command::new("xdg-open")
        .arg(url)
        .status()
        .context("Failed to spawn xdg-open (install xdg-utils or open the URL manually)")?;
    if !status.success() {
        bail!("`xdg-open {url}` exited with {status}");
    }
    Ok(())
}

fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("Failed to stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .with_context(|| format!("Failed to chmod {}", path.display()))?;
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a)
        .ok()
        .zip(fs::canonicalize(b).ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false)
}
