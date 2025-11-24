//! Keybinding management for Hyprland integration.
//!
//! This module provides functionality to discover, parse, and modify
//! Hyprland keybinding configurations for Audetic.

mod backup;
pub mod discovery;
mod parser;
pub mod writer;

pub use backup::BackupManager;
pub use discovery::{discover_config, ConfigDiscovery};
pub use parser::{parse_bindings, HyprBinding, Modifier, Modifiers};
pub use writer::write_binding;

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
pub fn check_conflicts(proposed: &ProposedBinding, bindings: &[HyprBinding]) -> ConflictCheckResult {
    let conflicts: Vec<HyprBinding> = bindings
        .iter()
        .filter(|b| {
            b.key.eq_ignore_ascii_case(&proposed.key) && b.modifiers == proposed.modifiers
        })
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
#[derive(Debug)]
pub enum KeybindStatus {
    /// Audetic keybinding is installed
    Installed {
        binding: HyprBinding,
        config_path: PathBuf,
    },
    /// No Audetic keybinding found
    NotInstalled { config_path: Option<PathBuf> },
    /// No Hyprland config found
    NoConfig,
}
