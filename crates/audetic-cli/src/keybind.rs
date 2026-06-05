//! CLI handler for keybinding management.
//!
//! Talks to the daemon's REST API (`GET /api/keybind/status`,
//! `POST /api/keybind/install`, `DELETE /api/keybind`). The daemon owns the
//! Hyprland config (conflict detection, backups), so it is the single writer.

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, Input};
use serde_json::{json, Value};
use std::io::{self, IsTerminal};

use crate::args::{KeybindCliArgs, KeybindCommand};
use crate::client::{base_url, json_or_error, CONNECT_HINT};

pub async fn handle_keybind_command(args: KeybindCliArgs) -> Result<()> {
    match args.command {
        Some(KeybindCommand::Install { key, dry_run }) => install(key, dry_run).await,
        Some(KeybindCommand::Uninstall { dry_run }) => uninstall(dry_run).await,
        Some(KeybindCommand::Status) => status().await,
        None => interactive().await,
    }
}

async fn status() -> Result<()> {
    let response = reqwest::Client::new()
        .get(format!("{}/keybind/status", base_url()))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "get keybind status").await?;

    println!();
    println!("Audetic Keybinding Status");
    println!("=========================");
    println!();

    let is_macos = body.get("platform").and_then(|v| v.as_str()) == Some("macos");

    match body.get("status").and_then(|v| v.as_str()) {
        Some("installed") => {
            println!("Status: INSTALLED");
            println!();
            if let Some(display_key) = body.get("display_key").and_then(|v| v.as_str()) {
                println!("Keybinding: {display_key}");
            }
            if let Some(config_path) = body.get("config_path").and_then(|v| v.as_str()) {
                println!("Location: {config_path}");
            }
            if let Some(command) = body.get("command").and_then(|v| v.as_str()) {
                println!("Command: {command}");
            }
        }
        // macOS: the daemon owns a system-wide global hotkey that's turned off.
        Some("disabled") => {
            println!("Status: DISABLED");
            println!();
            println!("The global hotkey is turned off.");
            println!("Run 'audetic keybind install' to enable it (default ⌘R).");
        }
        Some("not_installed") => {
            println!("Status: NOT INSTALLED");
            println!();
            if let Some(path) = body.get("config_path").and_then(|v| v.as_str()) {
                println!("Config file: {path}");
                println!();
                println!("Run 'audetic keybind install' to install.");
            } else {
                println!("No Hyprland config found.");
            }
        }
        _ if is_macos => {
            println!("Status: DISABLED");
            println!();
            println!("Run 'audetic keybind install' to enable the global hotkey.");
        }
        _ => {
            println!("Status: NO CONFIG");
            println!();
            println!("No Hyprland configuration found.");
            println!("Please create ~/.config/hypr/hyprland.conf first.");
        }
    }
    Ok(())
}

async fn install(key: Option<String>, dry_run: bool) -> Result<()> {
    if dry_run {
        println!(
            "Dry-run preview isn't available from the CLI — the daemon applies keybind \
             changes directly (with a backup). Run without --dry-run to install."
        );
        return Ok(());
    }

    let response = reqwest::Client::new()
        .post(format!("{}/keybind/install", base_url()))
        .json(&json!({ "key": key }))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "install keybinding").await?;
    print_install_result(&body);
    Ok(())
}

async fn uninstall(dry_run: bool) -> Result<()> {
    if dry_run {
        println!(
            "Dry-run preview isn't available from the CLI — the daemon applies keybind \
             changes directly (with a backup). Run without --dry-run to uninstall."
        );
        return Ok(());
    }

    let response = reqwest::Client::new()
        .delete(format!("{}/keybind", base_url()))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "uninstall keybinding").await?;

    if let Some(backup) = body.get("backup_path").and_then(|v| v.as_str()) {
        println!("Backup: {backup}");
    }
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Done");
    println!("{message}");
    if body
        .get("removed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        println!("Run 'hyprctl reload' to apply changes.");
    }
    Ok(())
}

/// Minimal interactive flow: show current status, then offer to install the
/// default (or a custom) binding. Conflict detection and config-file editing
/// happen server-side in the daemon.
async fn interactive() -> Result<()> {
    if !io::stdin().is_terminal() {
        eprintln!("Non-interactive session. Use 'audetic keybind install' for automated setup.");
        return Ok(());
    }

    status().await?;
    println!();

    let theme = ColorfulTheme::default();
    let proceed = Confirm::with_theme(&theme)
        .with_prompt("Install or update the Audetic keybinding now?")
        .default(true)
        .interact()?;
    if !proceed {
        println!("No changes made.");
        return Ok(());
    }

    let key: String = Input::with_theme(&theme)
        .with_prompt("Keybinding (e.g. \"SUPER, R\" or \"SUPER SHIFT, T\")")
        .default("SUPER, R".to_string())
        .interact_text()?;

    install(Some(key), false).await
}

fn print_install_result(body: &Value) {
    if let Some(backup) = body.get("backup_path").and_then(|v| v.as_str()) {
        println!("Backup: {backup}");
    }
    if let Some(display_key) = body.get("display_key").and_then(|v| v.as_str()) {
        println!("Installed keybinding: {display_key}");
    } else if let Some(message) = body.get("message").and_then(|v| v.as_str()) {
        println!("{message}");
    }
    println!("Run 'hyprctl reload' to apply changes.");
}
