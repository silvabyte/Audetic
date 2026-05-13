//! `audetic install` — bootstrap the daemon as a systemd user service and
//! launch the web UI in the default browser.
//!
//! User-space install only (no sudo): the running binary copies itself to
//! `~/.local/share/audetic/bin/audetic`, drops a unit file at
//! `~/.config/systemd/user/audetic.service`, and `enable --now`s it. Then
//! waits for the daemon to bind 127.0.0.1:3737 and opens
//! `http://127.0.0.1:3737/` so the user can finish onboarding (ffmpeg,
//! provider config) in the SPA.

use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const SERVICE_TEMPLATE: &str = include_str!("audetic.service.tmpl");
const SERVICE_NAME: &str = "audetic.service";
const DAEMON_VERSION_URL: &str = "http://127.0.0.1:3737/api/version";
const APP_URL: &str = "http://127.0.0.1:3737/";

pub struct InstallOptions {
    pub no_launch: bool,
}

pub async fn run(opts: InstallOptions) -> Result<()> {
    let paths = InstallPaths::resolve()?;

    println!("→ Installing audetic as a systemd user service");
    place_binary(&paths)?;
    ensure_runtime_dirs(&paths)?;
    write_unit(&paths)?;
    daemon_reload()?;
    enable_and_start()?;
    wait_for_daemon(Duration::from_secs(15)).await?;
    println!("✓ audetic.service is active");

    if opts.no_launch {
        println!("  Open {APP_URL} in your browser to finish onboarding.");
    } else {
        match open_browser(APP_URL) {
            Ok(()) => println!("→ Opened {APP_URL}"),
            Err(err) => println!("  Open {APP_URL} in your browser to finish onboarding ({err})"),
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
        let installed_binary = installed_dir.join("audetic");
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
        fs::create_dir_all(dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
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

async fn wait_for_daemon(timeout: Duration) -> Result<()> {
    println!("  · Waiting for daemon to bind 127.0.0.1:3737");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(1000))
        .build()
        .context("Failed to build HTTP client for readiness probe")?;

    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(resp) = client.get(DAEMON_VERSION_URL).send().await {
            if resp.status().is_success() {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    bail!(
        "Daemon did not respond on 127.0.0.1:3737 within {}s. \
         Check `journalctl --user -u audetic.service` for the failure.",
        timeout.as_secs()
    );
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

#[cfg(unix)]
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

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a)
        .ok()
        .zip(fs::canonicalize(b).ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false)
}
