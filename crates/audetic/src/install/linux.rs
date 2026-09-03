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
const CURRENT_SERVICE: &str = "audeticd.service";
const SUPERSEDED_SERVICES: &[&str] = &["audetic.service"];
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
    retire_superseded_services(&paths)?;
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
    systemd_user_dir: PathBuf,
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
        let systemd_user_dir = config.join("systemd").join("user");
        let config_dir = config.join("audetic");
        let hyprland_config_dir = resolved_hyprland_config_dir(&config);

        Ok(Self {
            installed_dir,
            installed_binary,
            systemd_user_dir,
            config_dir,
            data_dir,
            hyprland_config_dir,
        })
    }

    fn systemd_unit(&self, service_name: &str) -> PathBuf {
        self.systemd_user_dir.join(service_name)
    }

    fn current_systemd_unit(&self) -> PathBuf {
        self.systemd_unit(CURRENT_SERVICE)
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
    fs::create_dir_all(&paths.systemd_user_dir)
        .with_context(|| format!("Failed to create {}", paths.systemd_user_dir.display()))?;
    let systemd_unit = paths.current_systemd_unit();
    let unit = render_unit(paths)?;
    fs::write(&systemd_unit, unit)
        .with_context(|| format!("Failed to write {}", systemd_unit.display()))?;
    println!("  · Wrote {}", systemd_unit.display());
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

/// Stop and disable superseded identities before writing the current unit.
/// Failures are fatal: continuing could leave two daemons competing for the
/// same database and HTTP port.
fn retire_superseded_services(paths: &InstallPaths) -> Result<()> {
    let services = present_services(SUPERSEDED_SERVICES.iter().copied(), |service| {
        service_is_present(&paths.systemd_unit(service), service)
    });
    if services.is_empty() {
        return Ok(());
    }

    for service in services {
        println!("  · Retiring superseded {service}");
        for verb in ["stop", "disable"] {
            let status = Command::new("systemctl")
                .args(["--user", verb, service])
                .status()
                .with_context(|| {
                    format!("Failed to run systemctl {verb} for superseded service {service}")
                })?;
            if !status.success() {
                bail!("`systemctl --user {verb} {service}` exited with {status}");
            }
        }

        let systemd_unit = paths.systemd_unit(service);
        if systemd_unit.exists() || systemd_unit.is_symlink() {
            fs::remove_file(&systemd_unit).with_context(|| {
                format!(
                    "Failed to remove superseded unit {}",
                    systemd_unit.display()
                )
            })?;
        }
    }
    daemon_reload()?;
    Ok(())
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

    service_presence_detected(unit_exists, load_state.as_deref())
}

fn service_presence_detected(unit_exists: bool, load_state: Option<&str>) -> bool {
    unit_exists || load_state.is_some_and(|state| !state.is_empty() && state != "not-found")
}

fn all_services() -> impl Iterator<Item = &'static str> {
    std::iter::once(CURRENT_SERVICE).chain(SUPERSEDED_SERVICES.iter().copied())
}

fn present_services<'a>(
    services: impl IntoIterator<Item = &'a str>,
    mut is_present: impl FnMut(&str) -> bool,
) -> Vec<&'a str> {
    services
        .into_iter()
        .filter(|service| is_present(service))
        .collect()
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
    println!("  · systemctl --user enable {CURRENT_SERVICE}");
    let status = Command::new("systemctl")
        .args(["--user", "enable", CURRENT_SERVICE])
        .status()
        .context("Failed to run systemctl enable")?;
    if !status.success() {
        bail!("`systemctl --user enable {CURRENT_SERVICE}` exited with {status}");
    }

    import_session_environment()?;

    println!("  · systemctl --user restart {CURRENT_SERVICE}");
    let status = Command::new("systemctl")
        .args(["--user", "restart", CURRENT_SERVICE])
        .status()
        .context("Failed to run systemctl restart")?;
    if !status.success() {
        bail!("`systemctl --user restart {CURRENT_SERVICE}` exited with {status}");
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
    for service in all_services() {
        plan.remove(paths.systemd_unit(service), "Systemd service unit");
    }
    let services = present_services(all_services(), |service| {
        service_is_present(&paths.systemd_unit(service), service)
    });
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
            systemd_user_dir: config_home.join("systemd").join("user"),
            config_dir: config_home.join("audetic"),
            data_dir,
            hyprland_config_dir: hyprland,
        }
    }

    #[test]
    fn service_unit_paths_are_derived_from_the_shared_systemd_directory() {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths(temp.path(), temp.path().join("hypr"));

        assert_eq!(
            paths.current_systemd_unit(),
            paths.systemd_user_dir.join(CURRENT_SERVICE)
        );
        assert_eq!(
            paths.systemd_unit(SUPERSEDED_SERVICES[0]),
            paths.systemd_user_dir.join(SUPERSEDED_SERVICES[0])
        );
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
    fn service_presence_uses_disk_or_systemd_state() {
        assert!(service_presence_detected(true, None));
        assert!(service_presence_detected(false, Some("loaded")));
        assert!(!service_presence_detected(false, Some("not-found")));
        assert!(!service_presence_detected(false, None));
    }

    #[test]
    fn retirement_selection_includes_present_superseded_services_only() {
        let superseded = ["old-a.service", "old-b.service", "old-c.service"];
        let selected = present_services(superseded, |service| service != "old-b.service");

        assert_eq!(selected, vec!["old-a.service", "old-c.service"]);
    }

    #[test]
    fn uninstall_selection_handles_current_and_superseded_services_independently() {
        assert_eq!(
            present_services(all_services(), |service| service == CURRENT_SERVICE),
            vec![CURRENT_SERVICE]
        );
        assert_eq!(
            present_services(all_services(), |service| service != CURRENT_SERVICE),
            SUPERSEDED_SERVICES
        );
        assert!(present_services(all_services(), |_| false).is_empty());
    }

    #[test]
    fn service_inventory_has_current_and_superseded_identities_without_duplicates() {
        use std::collections::HashSet;

        assert_eq!(CURRENT_SERVICE, "audeticd.service");
        assert_eq!(SUPERSEDED_SERVICES, &["audetic.service"]);

        let services = all_services().collect::<Vec<_>>();
        assert_eq!(
            services.len(),
            services.iter().copied().collect::<HashSet<_>>().len()
        );
    }

    #[test]
    fn setup_page_is_derived_from_the_canonical_app_url() {
        assert_eq!(
            format!("{}settings/setup", url::app_url()),
            "http://127.0.0.1:3737/settings/setup"
        );
    }
}
