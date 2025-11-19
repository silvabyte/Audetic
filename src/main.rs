#![allow(clippy::arc_with_non_send_sync)]

use anyhow::{anyhow, Result};
use audetic::app;
use audetic::update::{UpdateConfig, UpdateEngine, UpdateOptions};
use clap::{Args as ClapArgs, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "audetic")]
#[command(about = "Voice to text for Hyprland", long_about = None)]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    /// Manage updates (manual install/check/enable/disable)
    Update(UpdateCliArgs),
    /// Print version information
    Version,
}

#[derive(ClapArgs, Debug)]
struct UpdateCliArgs {
    /// Only check for updates, do not download/install
    #[arg(long)]
    check: bool,
    /// Force installation even if versions appear identical
    #[arg(long)]
    force: bool,
    /// Override release channel (default: stable)
    #[arg(long)]
    channel: Option<String>,
    /// Enable automatic background updates
    #[arg(long)]
    enable: bool,
    /// Disable automatic background updates
    #[arg(long)]
    disable: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let log_level = if cli.verbose { "debug" } else { "info" };
    let env_filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    match cli.command {
        Some(CliCommand::Version) => {
            println!("Audetic {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(CliCommand::Update(args)) => {
            handle_update_command(args).await?;
            return Ok(());
        }
        None => {}
    }

    app::run_service().await
}

async fn handle_update_command(args: UpdateCliArgs) -> Result<()> {
    if args.enable && args.disable {
        return Err(anyhow!(
            "Cannot enable and disable auto-update at the same time"
        ));
    }

    let config = UpdateConfig::detect(args.channel.clone())?;
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

    Ok(())
}
