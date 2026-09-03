//! CLI consumer for daemon-owned Hyprland keybinding management.

use std::io::{self, IsTerminal};

use anyhow::{Context, Result};
use audetic_core::keybind::KeybindTarget;
use audetic_core::url::{api_url, paths};
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use serde_json::{json, Value};

use crate::args::{KeybindCliArgs, KeybindCommand};
use crate::client::{json_or_error, CONNECT_HINT};

pub async fn handle_keybind_command(args: KeybindCliArgs) -> Result<()> {
    match args.command {
        Some(KeybindCommand::Install {
            target,
            key,
            dry_run,
        }) => install(target, key, dry_run).await,
        Some(KeybindCommand::Uninstall { target, dry_run }) => uninstall(target, dry_run).await,
        Some(KeybindCommand::Status { target }) => status(target).await,
        None => interactive().await,
    }
}

async fn status(target: Option<KeybindTarget>) -> Result<()> {
    let response = reqwest::Client::new()
        .get(api_url(paths::KEYBIND_STATUS))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "get keybind status").await?;

    println!();
    println!("Audetic Keybinding Status");
    println!("=========================");

    let targets = target
        .map(|target| vec![target])
        .unwrap_or_else(|| vec![KeybindTarget::Dictation, KeybindTarget::Meeting]);
    for target in targets {
        println!();
        print_target_status(target, &body[target.as_str()]);
    }
    Ok(())
}

fn print_target_status(target: KeybindTarget, status: &Value) {
    let label = title(target);
    match status.get("status").and_then(Value::as_str) {
        Some("installed") => {
            println!("{label}: INSTALLED");
            if let Some(display_key) = status.get("display_key").and_then(Value::as_str) {
                println!("  Keybinding: {display_key}");
            }
            if let Some(config_path) = status.get("config_path").and_then(Value::as_str) {
                println!("  Location: {config_path}");
            }
            if let Some(command) = status.get("command").and_then(Value::as_str) {
                println!("  Command: {command}");
            }
        }
        Some("not_installed") => {
            println!("{label}: NOT INSTALLED");
            if let Some(path) = status.get("config_path").and_then(Value::as_str) {
                println!("  Config file: {path}");
                println!("  Run 'audetic keybind install --target {target}'.");
            }
        }
        _ => {
            println!("{label}: NO CONFIG");
            println!("  Create ~/.config/hypr/hyprland.conf first.");
        }
    }
}

async fn install(target: KeybindTarget, key: Option<String>, dry_run: bool) -> Result<()> {
    let response = reqwest::Client::new()
        .post(api_url(paths::KEYBIND_INSTALL))
        .json(&json!({
            "target": target,
            "key": key,
            "dry_run": dry_run,
        }))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "install keybinding").await?;
    print_install_result(&body);
    Ok(())
}

async fn uninstall(target: KeybindTarget, dry_run: bool) -> Result<()> {
    let response = reqwest::Client::new()
        .delete(api_url(paths::KEYBIND))
        .query(&[("target", target.as_str()), ("dry_run", bool_str(dry_run))])
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "uninstall keybinding").await?;

    let removed = body
        .get("removed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if dry_run {
        if removed {
            println!("Preview: the {target} keybinding would be removed.");
        } else {
            println!("Preview: no managed {target} keybinding was found.");
        }
        return Ok(());
    }

    if let Some(backup) = body.get("backup_path").and_then(Value::as_str) {
        println!("Backup: {backup}");
    }
    if removed {
        println!("Removed {target} keybinding.");
        println!("Run 'hyprctl reload' to apply changes.");
    } else {
        println!("No managed {target} keybinding found to remove.");
    }
    Ok(())
}

async fn interactive() -> Result<()> {
    if !io::stdin().is_terminal() {
        eprintln!(
            "Non-interactive session. Use 'audetic keybind install [--target meeting]' for automated setup."
        );
        return Ok(());
    }

    status(None).await?;
    println!();

    let theme = ColorfulTheme::default();
    let selected = Select::with_theme(&theme)
        .with_prompt("Shortcut to install or update")
        .items(&["Dictation (SUPER+R)", "Meeting (SUPER+SHIFT+R)"])
        .default(0)
        .interact()?;
    let target = if selected == 0 {
        KeybindTarget::Dictation
    } else {
        KeybindTarget::Meeting
    };
    let proceed = Confirm::with_theme(&theme)
        .with_prompt(format!("Install or update the {target} keybinding now?"))
        .default(true)
        .interact()?;
    if !proceed {
        println!("No changes made.");
        return Ok(());
    }

    let default_key = match target {
        KeybindTarget::Dictation => "SUPER, R",
        KeybindTarget::Meeting => "SUPER SHIFT, R",
    };
    let key: String = Input::with_theme(&theme)
        .with_prompt("Keybinding (e.g. \"SUPER, R\" or \"SUPER SHIFT, T\")")
        .default(default_key.to_string())
        .interact_text()?;

    install(target, Some(key), false).await
}

fn print_install_result(body: &Value) {
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Keybind operation completed");
    println!("{message}");

    if let Some(line) = body.get("generated_line").and_then(Value::as_str) {
        println!("Generated line: {line}");
    }
    if let Some(path) = body.get("config_path").and_then(Value::as_str) {
        println!("Config: {path}");
    }
    if let Some(conflicts) = body.get("conflicts").and_then(Value::as_array) {
        for conflict in conflicts {
            let command = conflict
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("unknown command");
            let path = conflict
                .get("config_path")
                .and_then(Value::as_str)
                .unwrap_or("unknown file");
            let line = conflict.get("line").and_then(Value::as_u64).unwrap_or(0);
            println!("Conflict: {path}:{line}: {command}");
        }
    }
    if let Some(backup) = body.get("backup_path").and_then(Value::as_str) {
        println!("Backup: {backup}");
    }
    if body.get("changed").and_then(Value::as_bool) == Some(true) {
        println!("Run 'hyprctl reload' to apply changes.");
    }
}

fn title(target: KeybindTarget) -> &'static str {
    match target {
        KeybindTarget::Dictation => "Dictation",
        KeybindTarget::Meeting => "Meeting",
    }
}

fn bool_str(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}
