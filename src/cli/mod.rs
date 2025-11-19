use crate::update::{UpdateConfig, UpdateEngine, UpdateOptions};
use anyhow::{anyhow, Result};
use clap::{Args as ClapArgs, Parser, Subcommand};
use std::io;
use std::process::Command;

#[derive(Parser, Debug)]
#[command(name = "audetic")]
#[command(about = "Voice to text for Hyprland", long_about = None)]
pub struct Cli {
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Manage updates (manual install/check/enable/disable)
    Update(UpdateCliArgs),
    /// Print version information
    Version,
}

#[derive(ClapArgs, Debug)]
pub struct UpdateCliArgs {
    /// Only check for updates, do not download/install
    #[arg(long)]
    pub check: bool,
    /// Force installation even if versions appear identical
    #[arg(long)]
    pub force: bool,
    /// Override release channel (default: stable)
    #[arg(long)]
    pub channel: Option<String>,
    /// Enable automatic background updates
    #[arg(long)]
    pub enable: bool,
    /// Disable automatic background updates
    #[arg(long)]
    pub disable: bool,
}

pub async fn handle_update_command(args: UpdateCliArgs) -> Result<()> {
    if args.enable && args.disable {
        return Err(anyhow!(
            "Cannot enable and disable auto-update at the same time"
        ));
    }

    let mut config = UpdateConfig::detect(args.channel.clone())?;
    config.restart_on_success = false;
    let engine = UpdateEngine::new(config)?;
    let report = engine
        .run_manual(UpdateOptions {
            channel: args.channel,
            check_only: args.check,
            force: args.force,
            enable_auto_update: args.enable,
            disable_auto_update: args.disable,
        })
        .await?;

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
            Ok(()) => {
                println!("Audetic service restarted via systemd user service.");
            }
            Err(err) => {
                eprintln!("Failed to restart Audetic automatically: {err}");
                println!(
                    "Please restart the Audetic service manually (e.g. `systemctl --user restart audetic.service`) to begin running {}.",
                    remote
                );
            }
        }
    }

    Ok(())
}

fn restart_user_service() -> Result<()> {
    match Command::new("systemctl")
        .arg("--user")
        .arg("restart")
        .arg("audetic.service")
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(anyhow!(
            "systemctl reported failure restarting audetic.service (exit status: {status})"
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Err(anyhow!(
            "systemctl binary not found in PATH, cannot restart audetic.service automatically"
        )),
        Err(err) => Err(anyhow!(
            "Failed to invoke systemctl --user restart audetic.service: {err}"
        )),
    }
}
