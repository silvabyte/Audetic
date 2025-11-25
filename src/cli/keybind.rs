//! CLI handler for keybinding management.
//!
//! This module handles terminal presentation and user interaction.
//! Core business logic is delegated to the `keybind` module.

use crate::cli::{KeybindCliArgs, KeybindCommand};
use crate::keybind::discovery::get_all_config_files;
use crate::keybind::{
    self, check_conflicts, discover_config, find_audetic_bindings, parse_bindings, write_binding,
    BackupManager, KeybindStatus, Modifiers, ProposedBinding, AUDETIC_SECTION_MARKER, DEFAULT_KEY,
    FALLBACK_MODIFIERS,
};
use anyhow::{anyhow, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use std::io::{self, IsTerminal};
use tracing::info;

pub fn handle_keybind_command(args: KeybindCliArgs) -> Result<()> {
    match args.command {
        Some(KeybindCommand::Install { key, dry_run }) => handle_install(key, dry_run),
        Some(KeybindCommand::Uninstall { dry_run }) => handle_uninstall(dry_run),
        Some(KeybindCommand::Status) => handle_status(),
        None => handle_interactive(),
    }
}

/// Interactive keybinding setup wizard
fn handle_interactive() -> Result<()> {
    if !io::stdin().is_terminal() {
        info!("Non-interactive session. Use 'audetic keybind install' for automated setup.");
        return Ok(());
    }

    let theme = ColorfulTheme::default();

    println!();
    println!("Audetic Keybinding Setup");
    println!("========================");
    println!();

    // Discover config
    let discovery = discover_config()?;
    let config_path = discovery.writable_config().ok_or_else(|| {
        anyhow!(
            "No Hyprland configuration found.\n\
             Please create ~/.config/hypr/hyprland.conf or ~/.config/hypr/bindings.conf first."
        )
    })?;

    println!("Found config: {}", config_path.display());
    println!();

    // Parse existing bindings from all config files
    let all_files = get_all_config_files(&discovery);
    let mut all_bindings = Vec::new();
    for file in all_files {
        all_bindings.extend(parse_bindings(file));
    }

    // Check for existing Audetic bindings
    let existing = find_audetic_bindings(&all_bindings);
    if !existing.is_empty() {
        println!("Existing Audetic keybinding found:");
        for binding in &existing {
            println!("  {} -> {}", binding.display_key(), binding.command);
            println!(
                "  Location: {}:{}",
                binding.source.file.display(),
                binding.source.line
            );
        }
        println!();

        let reconfigure = Confirm::with_theme(&theme)
            .with_prompt("Reconfigure keybinding?")
            .default(false)
            .interact()?;

        if !reconfigure {
            println!("Keeping existing configuration.");
            return Ok(());
        }
    }

    // Propose default binding
    let mut proposed = ProposedBinding::default();

    println!("Proposed keybinding:");
    println!("  {} -> Toggle Audetic recording", proposed.display_key());
    println!();

    // Check for conflicts
    let conflict_result = check_conflicts(&proposed, &all_bindings);

    if conflict_result.has_conflicts() {
        println!("Conflict detected!");
        for conflict in &conflict_result.conflicts {
            let desc = conflict
                .description
                .as_deref()
                .unwrap_or("(no description)");
            println!("  {} is already bound to: {}", conflict.display_key(), desc);
            println!("    Command: {}", conflict.command);
            println!(
                "    Location: {}:{}",
                conflict.source.file.display(),
                conflict.source.line
            );
        }
        println!();

        // Offer alternatives
        let fallback = ProposedBinding::new(FALLBACK_MODIFIERS, DEFAULT_KEY);
        let fallback_conflicts = check_conflicts(&fallback, &all_bindings);

        let options = if fallback_conflicts.has_conflicts() {
            vec!["Enter custom keybinding", "Skip (configure manually later)"]
        } else {
            vec![
                format!("Use alternative: {}", fallback.display_key()).leak(),
                "Enter custom keybinding",
                "Skip (configure manually later)",
            ]
        };

        let selection = Select::with_theme(&theme)
            .with_prompt("Choose an option")
            .items(&options)
            .default(0)
            .interact()?;

        match (fallback_conflicts.has_conflicts(), selection) {
            (false, 0) => {
                // Use fallback
                proposed = fallback;
            }
            (false, 1) | (true, 0) => {
                // Custom keybinding
                proposed = prompt_custom_keybinding(&theme)?;
            }
            _ => {
                println!("Skipping keybinding configuration.");
                println!("You can manually add to your Hyprland config:");
                println!("  {}", proposed.to_hyprland_line());
                return Ok(());
            }
        }
    } else {
        // No conflicts, confirm the default
        let confirm = Confirm::with_theme(&theme)
            .with_prompt(format!("Install keybinding {} ?", proposed.display_key()))
            .default(true)
            .interact()?;

        if !confirm {
            // Offer custom keybinding
            let custom = Confirm::with_theme(&theme)
                .with_prompt("Would you like to use a custom keybinding?")
                .default(false)
                .interact()?;

            if custom {
                proposed = prompt_custom_keybinding(&theme)?;
            } else {
                println!("Skipping keybinding configuration.");
                return Ok(());
            }
        }
    }

    // Final confirmation with preview
    println!();
    println!("Will add to {}:", config_path.display());
    println!("  {}", AUDETIC_SECTION_MARKER);
    println!("  {}", proposed.to_hyprland_line());
    println!();

    let proceed = Confirm::with_theme(&theme)
        .with_prompt("Proceed with installation?")
        .default(true)
        .interact()?;

    if !proceed {
        println!("Cancelled.");
        return Ok(());
    }

    // Create backup and write
    let backup_manager = BackupManager::new()?;
    let backup_path = backup_manager.create_backup(config_path)?;
    println!("Backup created: {}", backup_path.display());

    write_binding(config_path, &proposed)?;
    println!("Keybinding installed!");
    println!();
    println!("Reload Hyprland config with:");
    println!("  hyprctl reload");
    println!();
    println!("Or press {} to test (after reload)", proposed.display_key());

    Ok(())
}

/// Handle the install subcommand - uses keybind::install()
fn handle_install(key: Option<String>, dry_run: bool) -> Result<()> {
    if dry_run {
        // For dry run, we need to show what would be added
        let discovery = discover_config()?;
        let config_path = discovery
            .writable_config()
            .ok_or_else(|| anyhow!("No Hyprland configuration found"))?;

        let proposed = if let Some(ref key_str) = key {
            keybind::parse_key_string(key_str)?
        } else {
            ProposedBinding::default()
        };

        println!("Dry run - would add to {}:", config_path.display());
        println!("  {}", AUDETIC_SECTION_MARKER);
        println!("  {}", proposed.to_hyprland_line());
        return Ok(());
    }

    match keybind::install(key.as_deref(), dry_run)? {
        Some(result) => {
            println!("Backup: {}", result.backup_path.display());
            println!("Installed keybinding: {}", result.display_key);
            println!("Run 'hyprctl reload' to apply changes.");
        }
        None => {
            // dry_run case handled above
        }
    }

    Ok(())
}

/// Handle the uninstall subcommand - uses keybind::uninstall()
fn handle_uninstall(dry_run: bool) -> Result<()> {
    if dry_run {
        let discovery = discover_config()?;
        let config_path = discovery
            .writable_config()
            .ok_or_else(|| anyhow!("No Hyprland configuration found"))?;

        println!(
            "Dry run - would remove Audetic keybinding from {}",
            config_path.display()
        );
        return Ok(());
    }

    match keybind::uninstall(dry_run)? {
        Some(result) => {
            if let Some(backup) = result.backup_path {
                println!("Backup: {}", backup.display());
            }
            if result.removed {
                println!(
                    "Removed Audetic keybinding from {}",
                    result.config_path.display()
                );
                println!("Run 'hyprctl reload' to apply changes.");
            } else {
                println!(
                    "No Audetic keybinding found in {}",
                    result.config_path.display()
                );
            }
        }
        None => {
            // dry_run case handled above
        }
    }

    Ok(())
}

/// Handle the status subcommand - uses keybind::get_status()
fn handle_status() -> Result<()> {
    let status = keybind::get_status()?;

    println!();
    println!("Audetic Keybinding Status");
    println!("=========================");
    println!();

    match status {
        KeybindStatus::Installed {
            binding,
            config_path,
            display_key,
            command,
        } => {
            println!("Status: INSTALLED");
            println!();
            println!("Keybinding: {}", display_key);
            if let Some(ref b) = *binding {
                if let Some(ref desc) = b.description {
                    println!("Description: {}", desc);
                }
                println!("Location: {}:{}", config_path.display(), b.source.line);
            }
            println!("Command: {}", command);
        }
        KeybindStatus::NotInstalled { config_path } => {
            println!("Status: NOT INSTALLED");
            println!();
            if let Some(path) = config_path {
                println!("Config file: {}", path.display());
                println!();
                println!("Run 'audetic keybind' to install.");
            } else {
                println!("No Hyprland config found.");
            }
        }
        KeybindStatus::NoConfig => {
            println!("Status: NO CONFIG");
            println!();
            println!("No Hyprland configuration found.");
            println!("Please create ~/.config/hypr/hyprland.conf first.");
        }
    }

    Ok(())
}

/// Prompt user for a custom keybinding
fn prompt_custom_keybinding(theme: &ColorfulTheme) -> Result<ProposedBinding> {
    println!();
    println!("Enter custom keybinding:");
    println!("  Modifiers: SUPER, SHIFT, CTRL, ALT (space-separated)");
    println!("  Key: Single key like R, T, F1, etc.");
    println!();

    let modifiers_str: String = Input::with_theme(theme)
        .with_prompt("Modifiers")
        .default("SUPER".to_string())
        .interact_text()?;

    let key: String = Input::with_theme(theme)
        .with_prompt("Key")
        .default("R".to_string())
        .interact_text()?;

    let modifiers = Modifiers::parse(&modifiers_str);
    let key = key.trim().to_uppercase();

    Ok(ProposedBinding {
        modifiers,
        key,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_string() {
        let binding = keybind::parse_key_string("SUPER SHIFT, R").unwrap();
        assert_eq!(binding.key, "R");
        assert!(binding.modifiers.0.len() == 2);

        let binding = keybind::parse_key_string("SUPER+R").unwrap();
        assert_eq!(binding.key, "R");

        let binding = keybind::parse_key_string("SUPER, T").unwrap();
        assert_eq!(binding.key, "T");
    }
}
