//! `audetic` — the standalone Audetic command-line client.
//!
//! This binary is independent of the `audeticd` daemon: it links no audio,
//! transcription-provider, or web-UI code. Commands that need daemon state talk
//! to the daemon over its local REST API (127.0.0.1:3737); `transcribe` runs
//! standalone against the external jobs API and needs no daemon at all.
//!
//! Service installation lives in `audeticd install`, not here — on macOS it
//! must run from inside the app bundle for TCC attribution.

mod args;
mod client;
mod history;
mod keybind;
mod logs;
mod meeting;
mod post_processing;
mod provider;
mod transcribe;
mod update;

use anyhow::Result;
use args::{Cli, CliCommand};
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let log_level = if cli.verbose { "debug" } else { "info" };
    let env_filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Some(CliCommand::Version) => {
            println!("Audetic {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(CliCommand::Update(args)) => update::handle_update_command(args).await,
        Some(CliCommand::Provider(args)) => provider::handle_provider_command(args).await,
        Some(CliCommand::History(args)) => history::handle_history_command(args).await,
        Some(CliCommand::Logs(args)) => logs::handle_logs_command(args).await,
        Some(CliCommand::Keybind(args)) => keybind::handle_keybind_command(args).await,
        Some(CliCommand::Transcribe(args)) => transcribe::handle_transcribe_command(args).await,
        Some(CliCommand::Meeting(args)) => meeting::handle_meeting_command(args).await,
        Some(CliCommand::PostProcessing(args)) => {
            post_processing::handle_post_processing_command(args).await
        }
        None => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}
