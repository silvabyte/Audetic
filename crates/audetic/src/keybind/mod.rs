//! Keybinding management for Hyprland integration.
//!
//! Discovery, parsing, conflict detection, preview, and mutation live behind
//! this module so the daemon remains the only writer of Hyprland config.

mod backup;
pub mod discovery;
mod parser;
pub mod writer;

pub use backup::BackupManager;
pub use discovery::{discover_config, ConfigDiscovery};
pub use parser::{parse_bindings, HyprBinding, Modifier, Modifiers};
pub use writer::{remove_binding, write_binding};

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use audetic_core::keybind::KeybindTarget;
use discovery::get_all_config_files;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Default keybinding configuration for Audetic targets.
pub const DEFAULT_KEY: &str = "R";
pub const DEFAULT_MODIFIERS: &[&str] = &["SUPER"];
pub const MEETING_DEFAULT_MODIFIERS: &[&str] = &["SUPER", "SHIFT"];
pub const AUDETIC_SECTION_MARKER: &str = "# Audetic voice-to-text (managed by audetic keybind)";
pub const AUDETIC_MEETING_SECTION_MARKER: &str =
    "# Audetic meeting recording (managed by audetic keybind)";

pub fn target_endpoint(target: KeybindTarget) -> String {
    crate::api::url::api_url(target.endpoint_path())
}

/// Kept as a named helper for existing parser tests and call sites.
pub fn audetic_toggle_endpoint() -> String {
    target_endpoint(KeybindTarget::Dictation)
}

pub fn marker_for_target(target: KeybindTarget) -> &'static str {
    match target {
        KeybindTarget::Dictation => AUDETIC_SECTION_MARKER,
        KeybindTarget::Meeting => AUDETIC_MEETING_SECTION_MARKER,
    }
}

/// Represents a proposed keybinding to install.
#[derive(Debug, Clone)]
pub struct ProposedBinding {
    pub modifiers: Modifiers,
    pub key: String,
    pub description: String,
    pub command: String,
}

impl Default for ProposedBinding {
    fn default() -> Self {
        Self::for_target(KeybindTarget::Dictation)
    }
}

impl ProposedBinding {
    pub fn for_target(target: KeybindTarget) -> Self {
        let (modifiers, description) = match target {
            KeybindTarget::Dictation => (DEFAULT_MODIFIERS, "Audetic"),
            KeybindTarget::Meeting => (MEETING_DEFAULT_MODIFIERS, "Audetic Meeting"),
        };

        Self {
            modifiers: Modifiers::from_strs(modifiers),
            key: DEFAULT_KEY.to_string(),
            description: description.to_string(),
            command: format!("curl -X POST {}", target_endpoint(target)),
        }
    }

    /// Create a new proposed binding with custom modifiers and key.
    pub fn new(target: KeybindTarget, modifiers: &[&str], key: &str) -> Self {
        Self {
            modifiers: Modifiers::from_strs(modifiers),
            key: key.to_string(),
            ..Self::for_target(target)
        }
    }

    /// Format the exact Hyprland line written by an install.
    pub fn to_hyprland_line(&self) -> String {
        format!(
            "bindd = {}, {}, {}, exec, {}",
            self.modifiers, self.key, self.description, self.command
        )
    }

    pub fn display_key(&self) -> String {
        if self.modifiers.0.is_empty() {
            self.key.clone()
        } else {
            format!("{} + {}", self.modifiers, self.key)
        }
    }
}

/// A binding occupying the requested shortcut.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KeybindConflict {
    pub display_key: String,
    pub command: String,
    #[schema(value_type = String)]
    pub config_path: PathBuf,
    pub line: usize,
    pub managed_target: Option<KeybindTarget>,
}

/// Status of one stable Audetic keybind target.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum KeybindStatus {
    Installed {
        target: KeybindTarget,
        #[serde(skip)]
        #[schema(value_type = ())]
        binding: Box<Option<HyprBinding>>,
        #[schema(value_type = String)]
        config_path: PathBuf,
        display_key: String,
        command: String,
        generated_line: String,
    },
    NotInstalled {
        target: KeybindTarget,
        #[schema(value_type = Option<String>)]
        config_path: Option<PathBuf>,
    },
    NoConfig {
        target: KeybindTarget,
    },
}

/// Status response for every stable target.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KeybindStatuses {
    pub dictation: KeybindStatus,
    pub meeting: KeybindStatus,
}

impl KeybindStatuses {
    pub fn get(&self, target: KeybindTarget) -> &KeybindStatus {
        match target {
            KeybindTarget::Dictation => &self.dictation,
            KeybindTarget::Meeting => &self.meeting,
        }
    }
}

/// Server-authoritative install or preview result.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct InstallResult {
    pub success: bool,
    pub target: KeybindTarget,
    pub preview: bool,
    pub changed: bool,
    pub already_installed: bool,
    pub message: String,
    pub generated_line: String,
    pub display_key: String,
    #[schema(value_type = String)]
    pub config_path: PathBuf,
    #[schema(value_type = Option<String>)]
    pub backup_path: Option<PathBuf>,
    pub conflicts: Vec<KeybindConflict>,
}

/// Result of removing or previewing removal of one target.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UninstallResult {
    pub target: KeybindTarget,
    pub preview: bool,
    /// True when the target was removed, or would be removed in preview mode.
    pub removed: bool,
    #[schema(value_type = Option<String>)]
    pub backup_path: Option<PathBuf>,
    #[schema(value_type = String)]
    pub config_path: PathBuf,
}

pub fn get_statuses() -> Result<KeybindStatuses> {
    let discovery = discover_config()?;
    let config_path = discovery.writable_config().cloned();
    let bindings = all_bindings(&discovery);

    Ok(KeybindStatuses {
        dictation: status_from_bindings(KeybindTarget::Dictation, config_path.clone(), &bindings),
        meeting: status_from_bindings(KeybindTarget::Meeting, config_path, &bindings),
    })
}

pub fn get_status(target: KeybindTarget) -> Result<KeybindStatus> {
    let statuses = get_statuses()?;
    Ok(statuses.get(target).clone())
}

fn status_from_bindings(
    target: KeybindTarget,
    config_path: Option<PathBuf>,
    bindings: &[HyprBinding],
) -> KeybindStatus {
    if let Some(binding) = bindings
        .iter()
        .find(|binding| binding_target(binding) == Some(target))
    {
        KeybindStatus::Installed {
            target,
            display_key: binding.display_key(),
            command: binding.command.clone(),
            generated_line: binding.raw_line.clone(),
            binding: Box::new(Some(binding.clone())),
            config_path: binding.source.file.clone(),
        }
    } else if let Some(config_path) = config_path {
        KeybindStatus::NotInstalled {
            target,
            config_path: Some(config_path),
        }
    } else {
        KeybindStatus::NoConfig { target }
    }
}

/// Preview or install one target. Preview and conflict outcomes never mutate.
pub fn install(target: KeybindTarget, key: Option<&str>, dry_run: bool) -> Result<InstallResult> {
    let discovery = discover_config()?;
    let config_path = discovery
        .writable_config()
        .ok_or_else(|| {
            anyhow!("No Hyprland configuration found; create ~/.config/hypr/hyprland.conf first")
        })?
        .clone();
    let bindings = all_bindings(&discovery);
    let proposed = match key {
        Some(value) => parse_key_string(target, value)?,
        None => ProposedBinding::for_target(target),
    };

    install_from_bindings(target, proposed, config_path, &bindings, dry_run)
}

fn install_from_bindings(
    target: KeybindTarget,
    proposed: ProposedBinding,
    config_path: PathBuf,
    bindings: &[HyprBinding],
    dry_run: bool,
) -> Result<InstallResult> {
    let config_path = target_config_path(target, Some(&config_path), bindings)
        .expect("preferred config path was supplied");
    let already_installed = bindings.iter().any(|binding| {
        binding_target(binding) == Some(target)
            && binding.key.eq_ignore_ascii_case(&proposed.key)
            && binding.modifiers == proposed.modifiers
    });
    let conflicts = bindings
        .iter()
        .filter(|binding| {
            binding.key.eq_ignore_ascii_case(&proposed.key)
                && binding.modifiers == proposed.modifiers
                && binding_target(binding) != Some(target)
        })
        .map(conflict_from_binding)
        .collect::<Vec<_>>();
    let generated_line = proposed.to_hyprland_line();
    let display_key = proposed.display_key();

    if !conflicts.is_empty() {
        return Ok(InstallResult {
            success: false,
            target,
            preview: dry_run,
            changed: false,
            already_installed,
            message: format!(
                "Cannot install {target} shortcut {display_key}: the key is already bound; choose another key or remove the conflicting binding"
            ),
            generated_line,
            display_key,
            config_path,
            backup_path: None,
            conflicts,
        });
    }

    if dry_run || already_installed {
        let message = if already_installed {
            format!("{target} shortcut {display_key} is already installed")
        } else {
            format!("Preview: {target} shortcut {display_key} can be installed")
        };
        return Ok(InstallResult {
            success: true,
            target,
            preview: dry_run,
            changed: false,
            already_installed,
            message,
            generated_line,
            display_key,
            config_path,
            backup_path: None,
            conflicts,
        });
    }

    let backup_manager = BackupManager::new()?;
    let backup_path = backup_manager.create_backup(&config_path)?;
    write_binding(&config_path, target, &proposed)?;

    Ok(InstallResult {
        success: true,
        target,
        preview: false,
        changed: true,
        already_installed: false,
        message: format!("Installed {target} shortcut: {display_key}"),
        generated_line,
        display_key,
        config_path,
        backup_path: Some(backup_path),
        conflicts,
    })
}

/// Remove one managed target without touching the other target's section.
pub fn uninstall(target: KeybindTarget, dry_run: bool) -> Result<UninstallResult> {
    let discovery = discover_config()?;
    let bindings = all_bindings(&discovery);
    let config_path = target_config_path(target, discovery.writable_config(), &bindings)
        .ok_or_else(|| {
            anyhow!("No Hyprland configuration found; create ~/.config/hypr/hyprland.conf first")
        })?;
    let present = writer::has_binding(&config_path, target)?;

    if dry_run || !present {
        return Ok(UninstallResult {
            target,
            preview: dry_run,
            removed: present,
            backup_path: None,
            config_path,
        });
    }

    let backup_manager = BackupManager::new()?;
    let backup_path = backup_manager.create_backup(&config_path)?;
    let removed = remove_binding(&config_path, target)?;

    Ok(UninstallResult {
        target,
        preview: false,
        removed,
        backup_path: Some(backup_path),
        config_path,
    })
}

fn all_bindings(discovery: &ConfigDiscovery) -> Vec<HyprBinding> {
    get_all_config_files(discovery)
        .into_iter()
        .flat_map(|file| parse_bindings(file))
        .collect()
}

/// Existing managed targets stay in the file where they were found. The
/// preferred writable config is only the destination for a new target.
fn target_config_path(
    target: KeybindTarget,
    preferred: Option<&PathBuf>,
    bindings: &[HyprBinding],
) -> Option<PathBuf> {
    bindings
        .iter()
        .find(|binding| binding_target(binding) == Some(target))
        .map(|binding| binding.source.file.clone())
        .or_else(|| preferred.cloned())
}

fn binding_target(binding: &HyprBinding) -> Option<KeybindTarget> {
    [KeybindTarget::Dictation, KeybindTarget::Meeting]
        .into_iter()
        .find(|target| {
            binding.command.trim() == format!("curl -X POST {}", target_endpoint(*target))
        })
}

fn conflict_from_binding(binding: &HyprBinding) -> KeybindConflict {
    KeybindConflict {
        display_key: binding.display_key(),
        command: binding.command.clone(),
        config_path: binding.source.file.clone(),
        line: binding.source.line,
        managed_target: binding_target(binding),
    }
}

/// Parse a key string like `SUPER SHIFT, R` or `SUPER+R` for one target.
pub fn parse_key_string(target: KeybindTarget, value: &str) -> Result<ProposedBinding> {
    let normalized = value.replace(['+', ','], " ");
    let parts = normalized.split_whitespace().collect::<Vec<_>>();

    if parts.is_empty() {
        return Err(anyhow!("Invalid key string: {value}"));
    }

    let key = parts.last().expect("non-empty parts").to_uppercase();
    let modifiers = &parts[..parts.len() - 1];
    if modifiers.is_empty() {
        return Err(anyhow!(
            "No modifiers specified in '{value}'; try SUPER+{key}"
        ));
    }

    if modifiers
        .iter()
        .any(|modifier| Modifier::parse(modifier).is_none())
    {
        return Err(anyhow!(
            "Invalid modifier in '{value}'; supported modifiers are SUPER, SHIFT, CTRL, and ALT"
        ));
    }

    Ok(ProposedBinding::new(target, modifiers, &key))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::keybind::parser::parse_bindings_from_content;

    #[test]
    fn target_defaults_generate_exact_stable_lines() {
        assert_eq!(
            ProposedBinding::for_target(KeybindTarget::Dictation).to_hyprland_line(),
            format!(
                "bindd = SUPER, R, Audetic, exec, curl -X POST {}",
                target_endpoint(KeybindTarget::Dictation)
            )
        );
        assert_eq!(
            ProposedBinding::for_target(KeybindTarget::Meeting).to_hyprland_line(),
            format!(
                "bindd = SUPER SHIFT, R, Audetic Meeting, exec, curl -X POST {}",
                target_endpoint(KeybindTarget::Meeting)
            )
        );
    }

    #[test]
    fn own_binding_is_idempotent_but_other_target_conflicts() {
        let dictation = ProposedBinding::for_target(KeybindTarget::Dictation);
        let bindings = parse_bindings_from_content(
            &dictation.to_hyprland_line(),
            Path::new("/tmp/bindings.conf"),
        );
        let result = install_from_bindings(
            KeybindTarget::Dictation,
            dictation,
            PathBuf::from("/tmp/bindings.conf"),
            &bindings,
            false,
        )
        .unwrap();
        assert!(result.success);
        assert!(result.already_installed);
        assert!(!result.changed);
        assert!(result.backup_path.is_none());

        let meeting_on_same_key = ProposedBinding::new(KeybindTarget::Meeting, &["SUPER"], "R");
        let result = install_from_bindings(
            KeybindTarget::Meeting,
            meeting_on_same_key,
            PathBuf::from("/tmp/bindings.conf"),
            &bindings,
            true,
        )
        .unwrap();
        assert!(!result.success);
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(
            result.conflicts[0].managed_target,
            Some(KeybindTarget::Dictation)
        );
    }

    #[test]
    fn rejects_unknown_modifiers_instead_of_silently_dropping_them() {
        let error = parse_key_string(KeybindTarget::Dictation, "MAGIC+R").unwrap_err();
        assert!(error.to_string().contains("Invalid modifier"));
    }

    #[test]
    fn existing_target_is_updated_in_its_sourced_file_not_the_preferred_file() {
        let directory = tempfile::tempdir().unwrap();
        let preferred = directory.path().join("bindings.conf");
        let sourced = directory.path().join("custom-bindings.conf");
        std::fs::write(&preferred, "# preferred\n").unwrap();
        let original = ProposedBinding::for_target(KeybindTarget::Dictation);
        std::fs::write(
            &sourced,
            format!(
                "{}\n{}\n",
                marker_for_target(KeybindTarget::Dictation),
                original.to_hyprland_line()
            ),
        )
        .unwrap();
        let bindings =
            parse_bindings_from_content(&std::fs::read_to_string(&sourced).unwrap(), &sourced);

        let status =
            status_from_bindings(KeybindTarget::Dictation, Some(preferred.clone()), &bindings);
        assert!(matches!(
            status,
            KeybindStatus::Installed { config_path, .. } if config_path == sourced
        ));

        let selected =
            target_config_path(KeybindTarget::Dictation, Some(&preferred), &bindings).unwrap();
        let replacement = ProposedBinding::new(KeybindTarget::Dictation, &["SUPER", "ALT"], "D");
        write_binding(&selected, KeybindTarget::Dictation, &replacement).unwrap();

        assert_eq!(selected, sourced);
        assert_eq!(
            std::fs::read_to_string(&preferred).unwrap(),
            "# preferred\n"
        );
        assert!(std::fs::read_to_string(&sourced)
            .unwrap()
            .contains("bindd = SUPER ALT, D"));
    }

    #[test]
    fn existing_target_is_removed_from_its_sourced_file_not_the_preferred_file() {
        let directory = tempfile::tempdir().unwrap();
        let preferred = directory.path().join("bindings.conf");
        let sourced = directory.path().join("custom-bindings.conf");
        std::fs::write(&preferred, "# preferred\n").unwrap();
        let binding = ProposedBinding::for_target(KeybindTarget::Meeting);
        std::fs::write(
            &sourced,
            format!(
                "{}\n{}\n",
                marker_for_target(KeybindTarget::Meeting),
                binding.to_hyprland_line()
            ),
        )
        .unwrap();
        let bindings =
            parse_bindings_from_content(&std::fs::read_to_string(&sourced).unwrap(), &sourced);

        let selected =
            target_config_path(KeybindTarget::Meeting, Some(&preferred), &bindings).unwrap();
        assert!(remove_binding(&selected, KeybindTarget::Meeting).unwrap());

        assert_eq!(selected, sourced);
        assert_eq!(
            std::fs::read_to_string(&preferred).unwrap(),
            "# preferred\n"
        );
        assert!(!std::fs::read_to_string(&sourced)
            .unwrap()
            .contains(marker_for_target(KeybindTarget::Meeting)));
    }
}
