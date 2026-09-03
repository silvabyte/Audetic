//! Linux install: systemd user unit at `~/.config/systemd/user/audeticd.service`,
//! `enable --now`, readiness probe, `xdg-open` the UI. Also places the standalone
//! `audetic` CLI on PATH (`~/.local/bin/audetic`).
//!
//! Uninstall stops and disables the unit, then removes what install wrote —
//! both flows resolve paths through `InstallPaths`, so they stay in step.

use super::{
    remove_dir_if_empty, wait_for_daemon, InstallOptions, UninstallOptions, UninstallPlan,
};
use crate::api::url;
use anyhow::{anyhow, bail, Context, Result};
use dialoguer::{theme::ColorfulTheme, Select};
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const SERVICE_TEMPLATE: &str = include_str!("audetic.service.tmpl");
const SERVICE_NAME: &str = "audeticd.service";
const LEGACY_SERVICE_NAME: &str = "audetic.service";
const SESSION_ENVIRONMENT: &[&str] = &[
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "PATH",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_CONFIG_HOME",
    "XDG_CURRENT_DESKTOP",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
];

pub async fn run(opts: InstallOptions) -> Result<()> {
    let paths = InstallPaths::resolve()?;
    let app_url = url::app_url();

    println!("→ Installing audeticd as a systemd user service");
    place_binary(&paths)?;
    place_cli();
    ensure_runtime_dirs(&paths)?;
    retire_legacy_service(&paths)?;
    write_unit(&paths)?;
    daemon_reload()?;
    enable_and_restart()?;
    wait_for_daemon(Duration::from_secs(15)).await?;
    println!("✓ audeticd.service is active");

    finish_setup(opts.no_launch, &app_url);
    Ok(())
}

struct InstallPaths {
    installed_dir: PathBuf,
    installed_binary: PathBuf,
    systemd_unit: PathBuf,
    legacy_systemd_unit: PathBuf,
    // Audetic's own paths must exist before the service starts. Hyprland is
    // rendered as an optional ReadWritePaths entry and may be created later.
    config_dir: PathBuf,
    data_dir: PathBuf,
    hyprland_config_dir: PathBuf,
}

impl InstallPaths {
    fn resolve() -> Result<Self> {
        let data = dirs::data_dir()
            .ok_or_else(|| anyhow!("Could not resolve XDG_DATA_HOME / ~/.local/share"))?;
        let config = dirs::config_dir()
            .ok_or_else(|| anyhow!("Could not resolve XDG_CONFIG_HOME / ~/.config"))?;

        let data_dir = data.join("audetic");
        let installed_dir = data_dir.join("bin");
        let installed_binary = installed_dir.join("audeticd");
        let systemd_unit = config.join("systemd").join("user").join(SERVICE_NAME);
        let legacy_systemd_unit = config
            .join("systemd")
            .join("user")
            .join(LEGACY_SERVICE_NAME);
        let config_dir = config.join("audetic");
        let hyprland_config_dir = resolved_hyprland_config_dir(&config);

        Ok(Self {
            installed_dir,
            installed_binary,
            systemd_unit,
            legacy_systemd_unit,
            config_dir,
            data_dir,
            hyprland_config_dir,
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
        return Ok(());
    }

    println!("  · Copying binary → {}", paths.installed_binary.display());

    // Atomic swap, not a copy-in-place: the daemon we're replacing is usually
    // running, and writing onto a live executable fails with ETXTBSY.
    super::replace_executable(&current, &paths.installed_binary)
}

/// Best-effort: copy the standalone `audetic` CLI (shipped next to `audeticd`
/// in the release archive) onto PATH. Delegates to the shared placement helper.
fn place_cli() {
    let Ok(current) = std::env::current_exe() else {
        return;
    };
    let Some(source) = current.parent().map(|dir| dir.join("audetic")) else {
        return;
    };
    super::place_cli_on_path(&source);
}

fn write_unit(paths: &InstallPaths) -> Result<()> {
    if let Some(parent) = paths.systemd_unit.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let unit = render_unit(paths)?;
    fs::write(&paths.systemd_unit, unit)
        .with_context(|| format!("Failed to write {}", paths.systemd_unit.display()))?;
    println!("  · Wrote {}", paths.systemd_unit.display());
    Ok(())
}

/// Render all writable paths from the same XDG resolution used by the daemon.
/// The Hyprland directory is always granted as an optional (`-`-prefixed)
/// path. This lets a user create the directory after installation without the
/// service failing its namespace setup; existing symlink targets are resolved.
fn render_unit(paths: &InstallPaths) -> Result<String> {
    let exec_start = systemd_quote_path(&paths.installed_binary)?;
    let config_dir = systemd_quote_path(&paths.config_dir)?;
    let data_dir = systemd_quote_path(&paths.data_dir)?;
    let hyprland_config_dir = systemd_quote_path(&paths.hyprland_config_dir)?;

    Ok(SERVICE_TEMPLATE
        .replace("__EXEC_START__", &exec_start)
        .replace("__CONFIG_DIR__", &config_dir)
        .replace("__DATA_DIR__", &data_dir)
        .replace("__HYPRLAND_CONFIG_DIR__", &hyprland_config_dir))
}

fn systemd_quote_path(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("Install path contains non-UTF8 bytes; refusing to render unit"))?;
    if path.contains(['\n', '\r', '\0']) {
        bail!("Install path contains characters that cannot be rendered in a systemd unit");
    }
    Ok(format!(
        "\"{}\"",
        path.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn resolved_hyprland_config_dir(config_home: &Path) -> PathBuf {
    let candidate = config_home.join("hypr");
    if candidate.is_dir() {
        // A symlinked Hyprland tree must grant the resolved target, not merely
        // the path beneath ProtectHome that points at it.
        fs::canonicalize(&candidate).unwrap_or(candidate)
    } else {
        candidate
    }
}

/// Stop and disable the pre-daemon-split unit before writing the replacement.
/// If a legacy unit is known to systemd, failures are fatal: continuing could
/// leave two daemons competing for the same database and HTTP port.
fn retire_legacy_service(paths: &InstallPaths) -> Result<()> {
    if !legacy_service_is_present(paths) {
        return Ok(());
    }

    println!("  · Retiring legacy {LEGACY_SERVICE_NAME}");
    for verb in ["stop", "disable"] {
        let status = Command::new("systemctl")
            .args(["--user", verb, LEGACY_SERVICE_NAME])
            .status()
            .with_context(|| format!("Failed to run systemctl {verb} for legacy service"))?;
        if !status.success() {
            bail!("`systemctl --user {verb} {LEGACY_SERVICE_NAME}` exited with {status}");
        }
    }

    if paths.legacy_systemd_unit.exists() || paths.legacy_systemd_unit.is_symlink() {
        fs::remove_file(&paths.legacy_systemd_unit).with_context(|| {
            format!(
                "Failed to remove legacy unit {}",
                paths.legacy_systemd_unit.display()
            )
        })?;
    }
    daemon_reload()?;
    Ok(())
}

fn legacy_service_is_present(paths: &InstallPaths) -> bool {
    service_is_present(&paths.legacy_systemd_unit, LEGACY_SERVICE_NAME)
}

fn service_is_present(unit: &Path, service_name: &str) -> bool {
    let unit_exists = unit.exists() || unit.is_symlink();
    let load_state = Command::new("systemctl")
        .args([
            "--user",
            "show",
            "--property=LoadState",
            "--value",
            service_name,
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());

    legacy_service_needs_retirement(unit_exists, load_state.as_deref())
}

fn legacy_service_needs_retirement(unit_exists: bool, load_state: Option<&str>) -> bool {
    unit_exists || load_state.is_some_and(|state| !state.is_empty() && state != "not-found")
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

/// Enable the unit at boot and make sure the *new* binary is the one running.
///
/// `enable --now` is not enough here. `--now` only *starts* the unit, and
/// starting an already-active unit is a no-op — so reinstalling over a running
/// daemon would report success while leaving the old binary serving. Since
/// `make install` is the upgrade path (`git pull && make install`), that
/// silently defeats the whole point.
///
/// `restart` is the honest verb: it picks up the new `ExecStart` inode, and it
/// also starts a unit that isn't running, which covers first install.
fn enable_and_restart() -> Result<()> {
    println!("  · systemctl --user enable {SERVICE_NAME}");
    let status = Command::new("systemctl")
        .args(["--user", "enable", SERVICE_NAME])
        .status()
        .context("Failed to run systemctl enable")?;
    if !status.success() {
        bail!("`systemctl --user enable {SERVICE_NAME}` exited with {status}");
    }

    import_session_environment()?;

    println!("  · systemctl --user restart {SERVICE_NAME}");
    let status = Command::new("systemctl")
        .args(["--user", "restart", SERVICE_NAME])
        .status()
        .context("Failed to run systemctl restart")?;
    if !status.success() {
        bail!("`systemctl --user restart {SERVICE_NAME}` exited with {status}");
    }
    Ok(())
}

fn import_session_environment() -> Result<()> {
    let variables = present_session_environment(|name| std::env::var_os(name));
    if variables.is_empty() {
        return Ok(());
    }

    println!(
        "  · systemctl --user import-environment {}",
        variables.join(" ")
    );
    let status = Command::new("systemctl")
        .args(["--user", "import-environment"])
        .args(&variables)
        .status()
        .context("Failed to import the current desktop session environment into systemd")?;
    if !status.success() {
        bail!("`systemctl --user import-environment` exited with {status}");
    }
    Ok(())
}

fn present_session_environment(mut get: impl FnMut(&str) -> Option<OsString>) -> Vec<&'static str> {
    SESSION_ENVIRONMENT
        .iter()
        .copied()
        .filter(|name| get(name).is_some())
        .collect()
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

fn finish_setup(no_launch: bool, app_url: &str) {
    let setup_url = format!("{app_url}settings/setup");
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();

    if no_launch || !interactive {
        print_setup_choices(&setup_url);
        return;
    }

    println!();
    let choices = ["Set up in browser", "Set up in terminal", "Later"];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("How would you like to finish setup?")
        .items(&choices)
        .default(0)
        .interact_opt();

    match selection {
        Ok(Some(0)) => match open_browser(&setup_url) {
            Ok(()) => println!("→ Opened {setup_url}"),
            Err(err) => println!("  Open {setup_url} to finish setup ({err})"),
        },
        Ok(Some(1)) => run_terminal_setup(),
        _ => print_setup_choices(&setup_url),
    }
}

fn run_terminal_setup() {
    let Some(cli) = super::cli_target_path() else {
        println!("  Run `audetic setup` to finish setup.");
        return;
    };
    match Command::new(&cli).arg("setup").status() {
        Ok(status) if status.success() => {}
        Ok(status) => println!(
            "  Terminal setup exited with {status}. Run `audetic setup` to continue later."
        ),
        Err(err) => println!("  Run `audetic setup` to finish setup ({err})."),
    }
}

fn print_setup_choices(setup_url: &str) {
    println!("→ Finish setup when ready:");
    println!("  · Terminal: audetic setup");
    println!("  · Browser:  {setup_url}");
}

/// Tear down the systemd user service and remove everything `run` installed.
///
/// Stop/disable happen only after the plan is confirmed, but before files are
/// removed, so cancellation and dry runs cannot disturb a running daemon.
pub fn uninstall(opts: UninstallOptions) -> Result<()> {
    let paths = InstallPaths::resolve()?;

    println!("→ Uninstalling audeticd (systemd user service)");

    let mut plan = UninstallPlan::default();
    plan.remove(paths.systemd_unit.clone(), "Systemd service unit");
    plan.remove(
        paths.legacy_systemd_unit.clone(),
        "Legacy systemd service unit",
    );
    let services = uninstall_services(
        service_is_present(&paths.systemd_unit, SERVICE_NAME),
        service_is_present(&paths.legacy_systemd_unit, LEGACY_SERVICE_NAME),
    );
    for service in &services {
        plan.action(format!("Stop and disable systemd user service {service}"));
    }
    if let Some(cli) = super::cli_target_path() {
        plan.remove(cli, "Standalone `audetic` CLI");
    }
    super::plan_state_dirs(&mut plan, &paths.config_dir, &paths.data_dir, &opts);

    let outcome = plan.execute(&opts, || {
        stop_and_disable(&services);
        Ok(())
    })?;

    if outcome.removed_anything() {
        daemon_reload().ok();
        remove_dir_if_empty(&paths.config_dir);
        remove_dir_if_empty(&paths.data_dir);
        println!("✓ Audetic has been uninstalled");
    }
    Ok(())
}

/// Best-effort `systemctl --user stop`/`disable`. Absent systemd or an
/// unregistered unit are both fine — nothing to stop means nothing to do.
fn stop_and_disable(services: &[&str]) {
    if Command::new("systemctl").arg("--version").output().is_err() {
        println!("  · systemctl not available; skipping service teardown");
        return;
    }
    for service in services {
        println!("  · systemctl --user stop {service}");
        let _ = Command::new("systemctl")
            .args(["--user", "stop", service])
            .status();
        println!("  · systemctl --user disable {service}");
        let _ = Command::new("systemctl")
            .args(["--user", "disable", service])
            .output();
    }
}

fn uninstall_services(modern_present: bool, legacy_present: bool) -> Vec<&'static str> {
    let mut services = Vec::with_capacity(2);
    if modern_present {
        services.push(SERVICE_NAME);
    }
    if legacy_present {
        services.push(LEGACY_SERVICE_NAME);
    }
    services
}

fn same_file(a: &Path, b: &Path) -> bool {
    fs::canonicalize(a)
        .ok()
        .zip(fs::canonicalize(b).ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths(root: &Path, hyprland: PathBuf) -> InstallPaths {
        let config_home = root.join("xdg config");
        let data_dir = root.join("xdg data").join("audetic");
        InstallPaths {
            installed_dir: data_dir.join("bin"),
            installed_binary: data_dir.join("bin").join("audeticd"),
            systemd_unit: config_home.join("systemd").join("user").join(SERVICE_NAME),
            legacy_systemd_unit: config_home
                .join("systemd")
                .join("user")
                .join(LEGACY_SERVICE_NAME),
            config_dir: config_home.join("audetic"),
            data_dir,
            hyprland_config_dir: hyprland,
        }
    }

    #[test]
    fn service_unit_uses_resolved_xdg_paths_and_allows_missing_hyprland() {
        let temp = tempfile::tempdir().unwrap();
        let hyprland = temp.path().join("xdg config").join("hypr");
        let paths = test_paths(temp.path(), hyprland.clone());

        let unit = render_unit(&paths).unwrap();

        assert!(unit.contains(&format!("\"{}\"", paths.config_dir.display())));
        assert!(unit.contains(&format!("\"{}\"", paths.data_dir.display())));
        assert!(!unit.contains("%h/.config/audetic"));
        assert!(unit.contains(&format!("-\"{}\"", hyprland.display())));
        assert!(!unit.contains("__"));
    }

    #[test]
    fn service_unit_includes_existing_resolved_hyprland_directory() {
        let temp = tempfile::tempdir().unwrap();
        let hyprland = temp.path().join("custom hypr");
        fs::create_dir(&hyprland).unwrap();
        let paths = test_paths(temp.path(), hyprland.clone());

        let unit = render_unit(&paths).unwrap();

        assert!(unit.contains(&format!("-\"{}\"", hyprland.display())));
    }

    #[test]
    fn hyprland_directory_is_canonicalized_when_present_and_retained_when_absent() {
        let temp = tempfile::tempdir().unwrap();
        let config_home = temp.path().join("config");
        fs::create_dir(&config_home).unwrap();
        assert_eq!(
            resolved_hyprland_config_dir(&config_home),
            config_home.join("hypr")
        );

        let hyprland = config_home.join("hypr");
        fs::create_dir(&hyprland).unwrap();
        assert_eq!(
            resolved_hyprland_config_dir(&config_home),
            fs::canonicalize(hyprland).unwrap()
        );
    }

    #[test]
    fn session_environment_only_imports_present_variables() {
        let present = ["WAYLAND_DISPLAY", "XDG_RUNTIME_DIR"];
        let variables = present_session_environment(|name| {
            present.contains(&name).then(|| OsString::from("value"))
        });

        assert_eq!(variables, ["WAYLAND_DISPLAY", "XDG_RUNTIME_DIR"]);
    }

    #[test]
    fn legacy_retirement_decision_uses_disk_or_systemd_state() {
        assert!(legacy_service_needs_retirement(true, None));
        assert!(legacy_service_needs_retirement(false, Some("loaded")));
        assert!(!legacy_service_needs_retirement(false, Some("not-found")));
        assert!(!legacy_service_needs_retirement(false, None));
    }

    #[test]
    fn uninstall_decision_includes_modern_and_legacy_services_independently() {
        assert_eq!(
            uninstall_services(true, true),
            vec![SERVICE_NAME, LEGACY_SERVICE_NAME]
        );
        assert_eq!(uninstall_services(false, true), vec![LEGACY_SERVICE_NAME]);
        assert!(uninstall_services(false, false).is_empty());
    }

    #[test]
    fn setup_page_is_derived_from_the_canonical_app_url() {
        assert_eq!(
            format!("{}settings/setup", url::app_url()),
            "http://127.0.0.1:3737/settings/setup"
        );
    }
}
