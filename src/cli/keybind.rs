//! CLI handler for keybinding management.

use crate::cli::{KeybindCliArgs, KeybindCommand};
use crate::keybind::{
    check_conflicts, discover_config, find_audetic_bindings, parse_bindings,
    write_binding, BackupManager, KeybindStatus, Modifiers, ProposedBinding,
    AUDETIC_SECTION_MARKER, DEFAULT_KEY, FALLBACK_MODIFIERS,
};
use crate::keybind::discovery::get_all_config_files;
use crate::keybind::writer::remove_binding;
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
            println!("  Location: {}:{}", binding.source.file.display(), binding.source.line);
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
            let desc = conflict.description.as_deref().unwrap_or("(no description)");
            println!(
                "  {} is already bound to: {}",
                conflict.display_key(),
                desc
            );
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
            vec![
                "Enter custom keybinding",
                "Skip (configure manually later)",
            ]
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

/// Handle the install subcommand
fn handle_install(key: Option<String>, dry_run: bool) -> Result<()> {
    let discovery = discover_config()?;
    let config_path = discovery.writable_config().ok_or_else(|| {
        anyhow!("No Hyprland configuration found")
    })?;

    // Parse the key if provided, otherwise use default
    let proposed = if let Some(key_str) = key {
        parse_key_string(&key_str)?
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
        println!("Conflict detected:");
        for conflict in &conflict_result.conflicts {
            println!(
                "  {} is already bound in {}:{}",
                conflict.display_key(),
                conflict.source.file.display(),
                conflict.source.line
            );
        }
        return Err(anyhow!(
            "Keybinding {} conflicts with existing binding. Use --key to specify a different key.",
            proposed.display_key()
        ));
    }

    if dry_run {
        println!("Dry run - would add to {}:", config_path.display());
        println!("  {}", AUDETIC_SECTION_MARKER);
        println!("  {}", proposed.to_hyprland_line());
        return Ok(());
    }

    // Create backup and write
    let backup_manager = BackupManager::new()?;
    let backup_path = backup_manager.create_backup(config_path)?;
    println!("Backup: {}", backup_path.display());

    write_binding(config_path, &proposed)?;
    println!("Installed keybinding: {}", proposed.display_key());
    println!("Run 'hyprctl reload' to apply changes.");

    Ok(())
}

/// Handle the uninstall subcommand
fn handle_uninstall(dry_run: bool) -> Result<()> {
    let discovery = discover_config()?;
    let config_path = discovery.writable_config().ok_or_else(|| {
        anyhow!("No Hyprland configuration found")
    })?;

    if dry_run {
        println!("Dry run - would remove Audetic keybinding from {}", config_path.display());
        return Ok(());
    }

    let backup_manager = BackupManager::new()?;
    let backup_path = backup_manager.create_backup(config_path)?;
    println!("Backup: {}", backup_path.display());

    let removed = remove_binding(config_path)?;

    if removed {
        println!("Removed Audetic keybinding from {}", config_path.display());
        println!("Run 'hyprctl reload' to apply changes.");
    } else {
        println!("No Audetic keybinding found in {}", config_path.display());
    }

    Ok(())
}

/// Handle the status subcommand
fn handle_status() -> Result<()> {
    let status = get_keybind_status()?;

    println!();
    println!("Audetic Keybinding Status");
    println!("=========================");
    println!();

    match status {
        KeybindStatus::Installed { binding, config_path } => {
            println!("Status: INSTALLED");
            println!();
            println!("Keybinding: {}", binding.display_key());
            if let Some(ref desc) = binding.description {
                println!("Description: {}", desc);
            }
            println!("Command: {}", binding.command);
            println!("Location: {}:{}", config_path.display(), binding.source.line);
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

/// Get the current keybinding status
fn get_keybind_status() -> Result<KeybindStatus> {
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
            binding: binding.clone(),
            config_path,
        })
    } else {
        Ok(KeybindStatus::NotInstalled {
            config_path: Some(config_path),
        })
    }
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

    let modifiers = Modifiers::from_str(&modifiers_str);
    let key = key.trim().to_uppercase();

    Ok(ProposedBinding {
        modifiers,
        key,
        ..Default::default()
    })
}

/// Parse a key string like "SUPER SHIFT, R" or "SUPER+R"
fn parse_key_string(s: &str) -> Result<ProposedBinding> {
    // Handle formats:
    // "SUPER SHIFT, R"
    // "SUPER+R"
    // "SUPER, R"

    let normalized = s.replace('+', " ").replace(',', " ");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_string() {
        let binding = parse_key_string("SUPER SHIFT, R").unwrap();
        assert_eq!(binding.key, "R");
        assert!(binding.modifiers.0.len() == 2);

        let binding = parse_key_string("SUPER+R").unwrap();
        assert_eq!(binding.key, "R");

        let binding = parse_key_string("SUPER, T").unwrap();
        assert_eq!(binding.key, "T");
    }
}
