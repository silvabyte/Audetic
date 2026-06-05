//! macOS keybind backend.
//!
//! Unlike Linux (which edits the Hyprland config), the daemon owns the binding
//! directly: the chosen chord is persisted in `config.toml` under `[macos]
//! hotkey` and registered as a system-wide global hotkey by [`crate::hotkey`].
//! `install`/`uninstall` persist the choice and push it to the live hotkey loop
//! so changes take effect without a restart.

use super::{InstallResult, KeybindStatus, UninstallResult};
use crate::config::Config;
use crate::hotkey::{self, HotkeyCommand, DEFAULT_HOTKEY};
use anyhow::Result;

/// Report whether the global hotkey is active (and how it reads) or disabled.
pub fn get_status() -> Result<KeybindStatus> {
    let cfg = Config::load()?;
    match hotkey::resolve_chord(&cfg) {
        Some(chord) => {
            let parsed = hotkey::parse_chord(&chord)?;
            Ok(KeybindStatus::Installed {
                display_key: parsed.display,
                command: None,
                config_path: Some(config_path_string()?),
            })
        }
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

    let mut cfg = Config::load()?;
    cfg.macos.hotkey = Some(chord);
    cfg.save()?;

    hotkey::request(HotkeyCommand::Register(parsed.hotkey));

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
