//! macOS keybind backend.
//!
//! Unlike Linux (which edits the Hyprland config), the daemon owns the binding
//! directly: the chosen chord is persisted in `config.toml` under `[macos]
//! hotkey` and registered as a system-wide global hotkey by [`crate::hotkey`].
//! `install`/`uninstall` persist the choice and push it to the live hotkey loop
//! so changes take effect without a restart.

use super::{InstallResult, KeybindStatus, UninstallResult};
use crate::config::Config;
use crate::hotkey::{self, LiveStatus, DEFAULT_HOTKEY};
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

    // Load config first: a load failure aborts before any live change, and we
    // need the previous binding for rollback if saving fails below.
    let mut cfg = Config::load()?;
    let previous = hotkey::resolve_chord(&cfg);

    // Register on the live loop FIRST and only persist if the OS accepted it.
    // On registration failure the previous (working) binding stays active and
    // the error propagates — we never report success for a non-running binding.
    hotkey::register_sync(parsed.hotkey, &parsed.display)
        .with_context(|| format!("Failed to register hotkey '{chord}'"))?;

    // Persist. If the save fails (read-only config, full disk, …), roll the live
    // registration back to the previous binding so the live hotkey, status, and
    // the still-on-disk config all agree.
    cfg.macos.hotkey = Some(chord.clone());
    if let Err(e) = cfg.save() {
        rollback_live(previous);
        return Err(e).with_context(|| format!("Failed to persist hotkey '{chord}'; rolled back"));
    }

    Ok(Some(InstallResult {
        backup_path: None,
        display_key: parsed.display,
        config_path: crate::global::config_file()?,
    }))
}

/// Disable the global hotkey: unregister it live, then persist the disabled
/// state. Mirrors [`install`]'s ordering and rollback.
pub fn uninstall() -> Result<Option<UninstallResult>> {
    let mut cfg = Config::load()?;
    let previous = hotkey::resolve_chord(&cfg);

    // Unregister live FIRST; only persist "disabled" if the OS actually dropped
    // the binding (a failed unregister leaves the old hotkey live and errors).
    hotkey::unregister_sync().context("Failed to disable hotkey")?;

    cfg.macos.hotkey = Some(String::new());
    if let Err(e) = cfg.save() {
        rollback_live(previous);
        return Err(e).context("Failed to persist disabled state; rolled back");
    }

    Ok(Some(UninstallResult {
        removed: true,
        backup_path: None,
        config_path: crate::global::config_file()?,
    }))
}

/// Best-effort: restore the live registration to `previous` after a failed
/// persist, so the live hotkey matches the still-on-disk config. Errors here are
/// logged, not propagated — we're already returning the original failure.
fn rollback_live(previous: Option<String>) {
    let restored = match previous.as_deref() {
        Some(chord) => match hotkey::parse_chord(chord) {
            Ok(p) => hotkey::register_sync(p.hotkey, &p.display),
            Err(e) => Err(e),
        },
        None => hotkey::unregister_sync(),
    };
    if let Err(e) = restored {
        tracing::error!("failed to roll back hotkey to previous state: {e:#}");
    }
}

fn config_path_string() -> Result<String> {
    Ok(crate::global::config_file()?.to_string_lossy().into_owned())
}
