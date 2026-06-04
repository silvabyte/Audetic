//! CLI handler for update management.
//!
//! Talks to the daemon's REST API (`GET /api/update/check`,
//! `POST /api/update/install`, `PUT /api/update/auto`). The daemon owns the
//! update engine; the CLI just drives it and reports results.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::json;
use std::io;
use std::process::Command;

use crate::args::UpdateCliArgs;
use crate::client::{base_url, json_or_error, CONNECT_HINT};

const SERVICE_NAME: &str = "audeticd.service";

#[derive(Debug, Deserialize)]
struct UpdateReport {
    message: String,
    current_version: String,
    #[serde(default)]
    remote_version: Option<String>,
    #[serde(default)]
    restart_required: bool,
}

pub async fn handle_update_command(args: UpdateCliArgs) -> Result<()> {
    if args.enable && args.disable {
        return Err(anyhow!(
            "Cannot enable and disable auto-update at the same time"
        ));
    }

    // Toggling auto-update is its own action.
    if args.enable || args.disable {
        return set_auto_update(args.enable).await;
    }

    let report = if args.check {
        check_update().await?
    } else {
        install_update(args.channel, args.force).await?
    };

    println!("{}", report.message);
    if let Some(remote) = report.remote_version.as_deref() {
        println!("Current: {} | Remote: {}", report.current_version, remote);
    } else {
        println!("Current: {}", report.current_version);
    }

    if report.restart_required {
        let remote = report
            .remote_version
            .as_deref()
            .unwrap_or("the newly installed version");
        match restart_user_service() {
            Ok(()) => println!("Audetic service restarted via systemd user service."),
            Err(err) => {
                eprintln!("Failed to restart Audetic automatically: {err}");
                println!(
                    "Please restart the Audetic service manually (e.g. `systemctl --user restart {SERVICE_NAME}`) to begin running {remote}."
                );
            }
        }
    }

    Ok(())
}

async fn check_update() -> Result<UpdateReport> {
    let response = reqwest::Client::new()
        .get(format!("{}/update/check", base_url()))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "check for updates").await?;
    serde_json::from_value(body).context("Failed to parse update report")
}

async fn install_update(channel: Option<String>, force: bool) -> Result<UpdateReport> {
    let response = reqwest::Client::new()
        .post(format!("{}/update/install", base_url()))
        .json(&json!({ "channel": channel, "force": force }))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "install update").await?;
    serde_json::from_value(body).context("Failed to parse update report")
}

async fn set_auto_update(enabled: bool) -> Result<()> {
    let response = reqwest::Client::new()
        .put(format!("{}/update/auto", base_url()))
        .json(&json!({ "enabled": enabled }))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "set auto-update").await?;
    let message = body
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or(if enabled {
            "Auto-update enabled"
        } else {
            "Auto-update disabled"
        });
    println!("{message}");
    Ok(())
}

fn restart_user_service() -> Result<()> {
    match Command::new("systemctl")
        .arg("--user")
        .arg("restart")
        .arg(SERVICE_NAME)
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(anyhow!(
            "systemctl reported failure restarting {SERVICE_NAME} (exit status: {status})"
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Err(anyhow!(
            "systemctl binary not found in PATH, cannot restart {SERVICE_NAME} automatically"
        )),
        Err(err) => Err(anyhow!(
            "Failed to invoke systemctl --user restart {SERVICE_NAME}: {err}"
        )),
    }
}
