#![allow(clippy::arc_with_non_send_sync)]

use anyhow::Result;
use audetic::{
    app,
    cli::{
        handle_history_command, handle_logs_command, handle_provider_command, handle_update_command, Cli, CliCommand,
    },
};
use clap::Parser;
use tracing_subscriber::EnvFilter;

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
        Some(CliCommand::Provider(args)) => {
            handle_provider_command(args)?;
            return Ok(());
        }
        Some(CliCommand::History(args)) => {
            handle_history_command(args)?;
            return Ok(());
        }
        Some(CliCommand::Logs(args)) => {
            handle_logs_command(args)?;
            return Ok(());
        }
        None => {}
    }

    app::run_service().await
}
