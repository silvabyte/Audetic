//! Keybinding management for the dictation toggle.
//!
//! This is platform-specific:
//! - **Linux**: discovers/parses/edits the Hyprland config so a
//!   `bindd = … exec curl -X POST /api/toggle` line drives the toggle. The
//!   submodules ([`discovery`], `parser`, [`writer`], `backup`) implement this.
//! - **macOS**: the daemon registers a *system-wide* global hotkey itself (see
//!   the [`macos`] submodule and [`crate::hotkey`]) — there is no external
//!   config file to edit.
//!
//! The public surface is the same on both: [`get_status`], [`install`],
//! [`uninstall`], and the platform-tagged [`status_response`] the API serves.
//! Only the implementation behind them differs by `cfg(target_os)`.

// ---- Hyprland (Linux / non-macOS) implementation submodules ----
#[cfg(not(target_os = "macos"))]
mod backup;
#[cfg(not(target_os = "macos"))]
pub mod discovery;
#[cfg(not(target_os = "macos"))]
mod parser;
#[cfg(not(target_os = "macos"))]
pub mod writer;

#[cfg(not(target_os = "macos"))]
pub use backup::BackupManager;
#[cfg(not(target_os = "macos"))]
pub use discovery::{discover_config, ConfigDiscovery};
#[cfg(not(target_os = "macos"))]
pub use parser::{parse_bindings, HyprBinding, Modifier, Modifiers};
#[cfg(not(target_os = "macos"))]
pub use writer::{remove_binding, write_binding};

// ---- macOS native global-hotkey implementation ----
#[cfg(target_os = "macos")]
mod macos;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utoipa::ToSchema;

// ============================================================================
// Platform-agnostic public types
// ============================================================================

/// Which platform's keybind mechanism this daemon build uses. Lets the web UI
/// render the right affordances (Hyprland config vs. native global hotkey).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Macos,
    Linux,
}

/// The platform this daemon was built for.
pub fn current_platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        Platform::Macos
    }
    #[cfg(not(target_os = "macos"))]
    {
        Platform::Linux
    }
}

/// Status of the Audetic toggle keybinding.
///
/// `command`/`config_path` are populated on Linux (the Hyprland binding and the
/// file it lives in) and `None` on macOS, where the daemon owns the binding
/// directly. `Disabled` is macOS-only; `NotInstalled`/`NoConfig` are Linux-only.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum KeybindStatus {
    /// A keybinding is active. `display_key` is human-readable, e.g. `"⌘R"`
    /// (macOS) or `"SUPER + R"` (Hyprland).
    Installed {
        display_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        config_path: Option<String>,
    },
    /// Linux only: a Hyprland config exists but has no Audetic binding.
    NotInstalled {
        #[serde(skip_serializing_if = "Option::is_none")]
        config_path: Option<String>,
    },
    /// Linux only: no Hyprland config was found.
    NoConfig,
    /// macOS only: the global hotkey is turned off.
    Disabled,
}

/// [`KeybindStatus`] plus the platform it came from. Flattened on the wire, so
/// the JSON stays `{ "platform": …, "status": …, … }` — the top-level `status`
/// string the CLI reads is preserved.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KeybindStatusResponse {
    pub platform: Platform,
    #[serde(flatten)]
    pub status: KeybindStatus,
}

/// Result of an install operation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InstallResult {
    /// Backup of the edited config, when one was made (Linux/Hyprland only).
    #[schema(value_type = Option<String>)]
    pub backup_path: Option<PathBuf>,
    /// Human-readable key combination that was installed.
    pub display_key: String,
    /// Path to the config the binding was written to (Hyprland config on Linux;
    /// the daemon's `config.toml` on macOS).
    #[schema(value_type = String)]
    pub config_path: PathBuf,
}

/// Result of an uninstall operation.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UninstallResult {
    /// Whether a binding was actually removed.
    pub removed: bool,
    /// Path to the backup file created (if any).
    #[schema(value_type = Option<String>)]
    pub backup_path: Option<PathBuf>,
    /// Path to the config file modified.
    #[schema(value_type = String)]
    pub config_path: PathBuf,
}

/// Current keybinding status, tagged with the platform. This is what the API
/// serves; [`get_status`] returns the bare [`KeybindStatus`].
pub fn status_response() -> Result<KeybindStatusResponse> {
    Ok(KeybindStatusResponse {
        platform: current_platform(),
        status: get_status()?,
    })
}

// ============================================================================
// macOS dispatch — native global hotkey (see `macos` + `crate::hotkey`)
// ============================================================================

/// Get the current status of the Audetic global hotkey.
#[cfg(target_os = "macos")]
pub fn get_status() -> Result<KeybindStatus> {
    macos::get_status()
}

/// Register (or re-register) the global hotkey. `dry_run` is accepted for API
/// parity with the Hyprland path but has no effect on macOS.
#[cfg(target_os = "macos")]
pub fn install(key: Option<&str>, _dry_run: bool) -> Result<Option<InstallResult>> {
    macos::install(key)
}

/// Disable the global hotkey.
#[cfg(target_os = "macos")]
pub fn uninstall(_dry_run: bool) -> Result<Option<UninstallResult>> {
    macos::uninstall()
}

// ============================================================================
// Hyprland (Linux / non-macOS) implementation
// ============================================================================

#[cfg(not(target_os = "macos"))]
use anyhow::anyhow;
#[cfg(not(target_os = "macos"))]
use discovery::get_all_config_files;

/// Default keybinding configuration for Audetic
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_KEY: &str = "R";
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_MODIFIERS: &[&str] = &["SUPER"];
#[cfg(not(target_os = "macos"))]
pub const FALLBACK_MODIFIERS: &[&str] = &["SUPER", "SHIFT"];
#[cfg(not(target_os = "macos"))]
pub const AUDETIC_SECTION_MARKER: &str = "# Audetic voice-to-text (managed by audetic keybind)";

/// URL the hyprland binding POSTs to. Derived from [`crate::api::url`]
/// so a change to the daemon's host/port/prefix flows here automatically.
#[cfg(not(target_os = "macos"))]
pub fn audetic_toggle_endpoint() -> String {
    crate::api::url::api_url(crate::api::url::paths::TOGGLE)
}

/// Represents a proposed keybinding to install
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone)]
pub struct ProposedBinding {
    pub modifiers: Modifiers,
    pub key: String,
    pub description: String,
    pub command: String,
}

#[cfg(not(target_os = "macos"))]
impl Default for ProposedBinding {
    fn default() -> Self {
        Self {
            modifiers: Modifiers::from_strs(DEFAULT_MODIFIERS),
            key: DEFAULT_KEY.to_string(),
            description: "Audetic".to_string(),
            command: format!("curl -X POST {}", audetic_toggle_endpoint()),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl ProposedBinding {
    /// Create a new proposed binding with custom modifiers and key
    pub fn new(modifiers: &[&str], key: &str) -> Self {
        Self {
            modifiers: Modifiers::from_strs(modifiers),
            key: key.to_string(),
            ..Default::default()
        }
    }

    /// Format the binding as a Hyprland bindd directive
    pub fn to_hyprland_line(&self) -> String {
        format!(
            "bindd = {}, {}, {}, exec, {}",
            self.modifiers, self.key, self.description, self.command
        )
    }

    /// Get a display string for the keybinding (e.g., "SUPER + R")
    pub fn display_key(&self) -> String {
        if self.modifiers.0.is_empty() {
            self.key.clone()
        } else {
            format!("{} + {}", self.modifiers, self.key)
        }
    }
}

/// Result of checking for conflicts
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub struct ConflictCheckResult {
    pub proposed: ProposedBinding,
    pub conflicts: Vec<HyprBinding>,
}

#[cfg(not(target_os = "macos"))]
impl ConflictCheckResult {
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

/// Check if a proposed binding conflicts with existing bindings
#[cfg(not(target_os = "macos"))]
pub fn check_conflicts(
    proposed: &ProposedBinding,
    bindings: &[HyprBinding],
) -> ConflictCheckResult {
    let conflicts: Vec<HyprBinding> = bindings
        .iter()
        .filter(|b| b.key.eq_ignore_ascii_case(&proposed.key) && b.modifiers == proposed.modifiers)
        .cloned()
        .collect();

    ConflictCheckResult {
        proposed: proposed.clone(),
        conflicts,
    }
}

/// Find existing Audetic bindings in the configuration
#[cfg(not(target_os = "macos"))]
pub fn find_audetic_bindings(bindings: &[HyprBinding]) -> Vec<&HyprBinding> {
    bindings
        .iter()
        .filter(|b| {
            b.command.contains("127.0.0.1:3737")
                || b.command.contains("localhost:3737")
                || b.description
                    .as_ref()
                    .map(|d| d.to_lowercase().contains("audetic"))
                    .unwrap_or(false)
        })
        .collect()
}

/// Get the current status of Audetic keybinding by reading the Hyprland config.
#[cfg(not(target_os = "macos"))]
pub fn get_status() -> Result<KeybindStatus> {
    let discovery = discover_config()?;

    let config_path = match discovery.writable_config() {
        Some(p) => p.clone(),
        None => return Ok(KeybindStatus::NoConfig),
    };

    // Parse all config files for Audetic bindings
    let all_files = get_all_config_files(&discovery);
    let mut all_bindings = Vec::new();
    for file in all_files {
        all_bindings.extend(parse_bindings(file));
    }

    let existing = find_audetic_bindings(&all_bindings);

    if let Some(binding) = existing.into_iter().next() {
        Ok(KeybindStatus::Installed {
            display_key: binding.display_key(),
            command: Some(binding.command.clone()),
            config_path: Some(config_path.to_string_lossy().into_owned()),
        })
    } else {
        Ok(KeybindStatus::NotInstalled {
            config_path: Some(config_path.to_string_lossy().into_owned()),
        })
    }
}

/// Install an Audetic keybinding into the Hyprland config.
///
/// # Arguments
/// * `key` - Optional custom key string (e.g., "SUPER SHIFT, R" or "SUPER+T").
///   If None, uses the default binding (SUPER + R).
/// * `dry_run` - If true, only check for conflicts without making changes.
#[cfg(not(target_os = "macos"))]
pub fn install(key: Option<&str>, dry_run: bool) -> Result<Option<InstallResult>> {
    let discovery = discover_config()?;
    let config_path = discovery
        .writable_config()
        .ok_or_else(|| anyhow!("No Hyprland configuration found"))?
        .clone();

    // Parse the key if provided, otherwise use default
    let proposed = if let Some(key_str) = key {
        parse_key_string(key_str)?
    } else {
        ProposedBinding::default()
    };

    // Check for conflicts
    let all_files = get_all_config_files(&discovery);
    let mut all_bindings = Vec::new();
    for file in all_files {
        all_bindings.extend(parse_bindings(file));
    }

    let conflict_result = check_conflicts(&proposed, &all_bindings);

    if conflict_result.has_conflicts() {
        let conflict = &conflict_result.conflicts[0];
        return Err(anyhow!(
            "Keybinding {} conflicts with existing binding: {} ({}:{})",
            proposed.display_key(),
            conflict.command,
            conflict.source.file.display(),
            conflict.source.line
        ));
    }

    if dry_run {
        return Ok(None);
    }

    // Create backup and write
    let backup_manager = BackupManager::new()?;
    let backup_path = backup_manager.create_backup(&config_path)?;

    write_binding(&config_path, &proposed)?;

    Ok(Some(InstallResult {
        backup_path: Some(backup_path),
        display_key: proposed.display_key(),
        config_path,
    }))
}

/// Uninstall the Audetic keybinding from the Hyprland config.
#[cfg(not(target_os = "macos"))]
pub fn uninstall(dry_run: bool) -> Result<Option<UninstallResult>> {
    let discovery = discover_config()?;
    let config_path = discovery
        .writable_config()
        .ok_or_else(|| anyhow!("No Hyprland configuration found"))?
        .clone();

    if dry_run {
        return Ok(None);
    }

    let backup_manager = BackupManager::new()?;
    let backup_path = backup_manager.create_backup(&config_path)?;

    let removed = remove_binding(&config_path)?;

    Ok(Some(UninstallResult {
        removed,
        backup_path: Some(backup_path),
        config_path,
    }))
}

/// Parse a key string like "SUPER SHIFT, R" or "SUPER+R" into a ProposedBinding.
#[cfg(not(target_os = "macos"))]
pub fn parse_key_string(s: &str) -> Result<ProposedBinding> {
    // Handle formats:
    // "SUPER SHIFT, R"
    // "SUPER+R"
    // "SUPER, R"

    let normalized = s.replace(['+', ','], " ");
    let parts: Vec<&str> = normalized.split_whitespace().collect();

    if parts.is_empty() {
        return Err(anyhow!("Invalid key string: {}", s));
    }

    // Last part is the key, rest are modifiers
    let key = parts.last().unwrap().to_uppercase();
    let mod_strs: Vec<&str> = parts[..parts.len() - 1].to_vec();

    if mod_strs.is_empty() {
        return Err(anyhow!("No modifiers specified in: {}", s));
    }

    Ok(ProposedBinding::new(&mod_strs, &key))
}
