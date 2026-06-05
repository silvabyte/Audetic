//! macOS keybind backend.
//!
//! Unlike Linux (which edits the Hyprland config), the daemon owns the binding
//! directly: the chosen chord is persisted in `config.toml` under `[macos]
//! hotkey` and registered as a system-wide global hotkey by [`crate::hotkey`].
//! `install`/`uninstall` persist the choice and push it to the live hotkey loop
//! so changes take effect without a restart.

use super::{InstallResult, KeybindStatus, UninstallResult};
use crate::config::Config;
use crate::hotkey::{self, HotkeyCommand, LiveStatus, DEFAULT_HOTKEY};
use anyhow::{Context, Result};

/// Report the global hotkey status.
///
/// Reflects the *live* registration the controller actually performed (not just
/// config), so a chord the OS rejected at startup shows as `Failed` rather than
/// a misleading `Installed`. Falls back to config in the brief window before the
/// controller publishes its first result.
pub fn get_status() -> Result<KeybindStatus> {
    match hotkey::live_status() {
        Some(LiveStatus::Active { display }) => Ok(KeybindStatus::Installed {
            display_key: display,
            command: None,
            config_path: Some(config_path_string()?),
        }),
        Some(LiveStatus::Disabled) => Ok(KeybindStatus::Disabled),
        Some(LiveStatus::Failed { display, error }) => Ok(KeybindStatus::Failed {
            display_key: display,
            error,
        }),
        // Controller hasn't published yet — fall back to the configured intent.
        None => status_from_config(),
    }
}

fn status_from_config() -> Result<KeybindStatus> {
    let cfg = Config::load()?;
    match hotkey::resolve_chord(&cfg) {
        Some(chord) => Ok(KeybindStatus::Installed {
            display_key: hotkey::parse_chord(&chord)?.display,
            command: None,
            config_path: Some(config_path_string()?),
        }),
        None => Ok(KeybindStatus::Disabled),
    }
}

/// Set (or change) the global hotkey. `key` is a chord like `"CMD+R"`; `None`
/// uses the built-in default. Validates the chord before persisting, then
/// re-registers it live.
pub fn install(key: Option<&str>) -> Result<Option<InstallResult>> {
    let chord = key
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_HOTKEY.to_string());

    // Validate + build the hotkey before touching config, so a bad chord is
    // rejected with a clear error and leaves config untouched.
    let parsed = hotkey::parse_chord(&chord)?;

    // Register on the live loop FIRST and only persist if the OS accepted it.
    // On failure the previous (working) binding stays active and the error
    // propagates to the caller — config, status, and the live hotkey can never
    // disagree, and we never report success for a binding that isn't running.
    hotkey::register_sync(parsed.hotkey, &parsed.display)
        .with_context(|| format!("Failed to register hotkey '{chord}'"))?;

    let mut cfg = Config::load()?;
    cfg.macos.hotkey = Some(chord);
    cfg.save()?;

    Ok(Some(InstallResult {
        backup_path: None,
        display_key: parsed.display,
        config_path: crate::global::config_file()?,
    }))
}

/// Disable the global hotkey: persist an empty chord and unregister it live.
pub fn uninstall() -> Result<Option<UninstallResult>> {
    let mut cfg = Config::load()?;
    cfg.macos.hotkey = Some(String::new());
    cfg.save()?;

    hotkey::request(HotkeyCommand::Unregister);

    Ok(Some(UninstallResult {
        removed: true,
        backup_path: None,
        config_path: crate::global::config_file()?,
    }))
}

fn config_path_string() -> Result<String> {
    Ok(crate::global::config_file()?.to_string_lossy().into_owned())
}
