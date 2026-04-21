//! Keybinding management for Hyprland integration.
//!
//! This module provides functionality to discover, parse, and modify
//! Hyprland keybinding configurations for Audetic.
//!
//! # High-level API
//!
//! For most use cases, use the high-level functions:
//! - [`get_status()`] - Check current keybind status
//! - [`install()`] - Install a keybinding
//! - [`uninstall()`] - Remove a keybinding
//!
//! # Low-level API
//!
//! For more control, use the submodules directly:
//! - [`discovery`] - Find Hyprland config files
//! - [`parser`] - Parse keybinding configurations
//! - [`writer`] - Modify config files
//! - [`backup`] - Manage config backups

mod backup;
pub mod discovery;
mod parser;
pub mod writer;

pub use backup::BackupManager;
pub use discovery::{discover_config, ConfigDiscovery};
pub use parser::{parse_bindings, HyprBinding, Modifier, Modifiers};
pub use writer::{remove_binding, write_binding};

use anyhow::{anyhow, Result};
use discovery::get_all_config_files;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default keybinding configuration for Audetic
pub const DEFAULT_KEY: &str = "R";
pub const DEFAULT_MODIFIERS: &[&str] = &["SUPER"];
pub const FALLBACK_MODIFIERS: &[&str] = &["SUPER", "SHIFT"];
pub const AUDETIC_SECTION_MARKER: &str = "# Audetic voice-to-text (managed by audetic keybind)";
pub const AUDETIC_TOGGLE_ENDPOINT: &str = "http://127.0.0.1:3737/toggle";

/// Represents a proposed keybinding to install
#[derive(Debug, Clone)]
pub struct ProposedBinding {
    pub modifiers: Modifiers,
    pub key: String,
    pub description: String,
    pub command: String,
}

impl Default for ProposedBinding {
    fn default() -> Self {
        Self {
            modifiers: Modifiers::from_strs(DEFAULT_MODIFIERS),
            key: DEFAULT_KEY.to_string(),
            description: "Audetic".to_string(),
            command: format!("curl -X POST {}", AUDETIC_TOGGLE_ENDPOINT),
        }
    }
}

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
#[derive(Debug)]
pub struct ConflictCheckResult {
    pub proposed: ProposedBinding,
    pub conflicts: Vec<HyprBinding>,
}

impl ConflictCheckResult {
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

/// Check if a proposed binding conflicts with existing bindings
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

/// Status of Audetic keybinding installation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum KeybindStatus {
    /// Audetic keybinding is installed
    Installed {
        #[serde(skip)]
        binding: Box<Option<HyprBinding>>,
        config_path: PathBuf,
        /// Display string for the keybinding (e.g., "SUPER + R")
        display_key: String,
        /// The command bound to the key
        command: String,
    },
    /// No Audetic keybinding found
    NotInstalled { config_path: Option<PathBuf> },
    /// No Hyprland config found
    NoConfig,
}

/// Result of an install operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallResult {
    /// Path to the backup file created
    pub backup_path: PathBuf,
    /// The binding that was installed
    pub display_key: String,
    /// Path to the config file modified
    pub config_path: PathBuf,
}

/// Result of an uninstall operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallResult {
    /// Whether a binding was actually removed
    pub removed: bool,
    /// Path to the backup file created (if any)
    pub backup_path: Option<PathBuf>,
    /// Path to the config file modified
    pub config_path: PathBuf,
}

// ============================================================================
// High-level API functions
// ============================================================================

/// Get the current status of Audetic keybinding.
///
/// This function checks the Hyprland configuration to determine if
/// an Audetic keybinding is installed.
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
            command: binding.command.clone(),
            binding: Box::new(Some(binding.clone())),
            config_path,
        })
    } else {
        Ok(KeybindStatus::NotInstalled {
            config_path: Some(config_path),
        })
    }
}

/// Install an Audetic keybinding.
///
/// # Arguments
/// * `key` - Optional custom key string (e.g., "SUPER SHIFT, R" or "SUPER+T").
///   If None, uses the default binding (SUPER + R).
/// * `dry_run` - If true, only check for conflicts without making changes.
///
/// # Returns
/// * `Ok(Some(InstallResult))` - Binding was installed successfully
/// * `Ok(None)` - Dry run mode, no changes made
/// * `Err(_)` - Installation failed (e.g., conflicts detected)
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
        backup_path,
        display_key: proposed.display_key(),
        config_path,
    }))
}

/// Uninstall the Audetic keybinding.
///
/// # Arguments
/// * `dry_run` - If true, only check without making changes.
///
/// # Returns
/// * `Ok(Some(UninstallResult))` - Result of the uninstall operation
/// * `Ok(None)` - Dry run mode, no changes made
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
