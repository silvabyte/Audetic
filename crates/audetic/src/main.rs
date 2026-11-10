//TODO:AGENT: why is this needed? can drop a comment explaining this
#![allow(clippy::arc_with_non_send_sync)]

//! `audeticd` — the Audetic daemon.
//!
//! With no subcommand it runs the long-lived service (audio capture, the HTTP
//! API on 127.0.0.1:3737, and the bundled web UI). `install` bootstraps the
//! platform service (systemd user unit on Linux, LaunchAgent on macOS) and
//! places the standalone `audetic` CLI on PATH; `uninstall` reverses it.
//! Both live here rather than in the slim CLI because on macOS `install` must
//! run from inside the `Audetic.app` bundle so TCC permission attribution
//! lands on the bundle's cdhash.
//!
//! Neither is meant to be typed by hand — `make install` / `make uninstall`
//! build the right artifact for the platform and then call these.
//!
//! Day-to-day commands (meeting, history, transcribe, provider, …) live in the
//! separate `audetic` binary, which talks to this daemon over its REST API.

use anyhow::Result;
use audetic::{app, install};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use utoipa::OpenApi;

#[derive(Parser)]
#[command(name = "audeticd", version, about = "The Audetic voice-to-text daemon")]
struct Cli {
    /// Enable verbose (debug) logging.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Install audetic as a background service and put the `audetic` CLI on PATH.
    Install {
        /// Don't open the web UI in a browser after install.
        #[arg(long)]
        no_launch: bool,
    },
    /// Stop the background service and remove what `install` put on disk.
    Uninstall {
        /// Skip the confirmation prompt.
        #[arg(short, long)]
        yes: bool,
        /// Show what would be removed without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Preserve ~/.config/audetic/config.toml.
        #[arg(long)]
        keep_config: bool,
        /// Preserve the transcription history database.
        #[arg(long)]
        keep_database: bool,
    },
    /// Print the OpenAPI spec (JSON) to stdout and exit. Lets the web UI run
    /// `codegen` against a freshly built daemon without starting the service
    /// or contending for port 3737.
    Openapi,
}

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
        Some(Command::Install { no_launch }) => {
            install::run(install::InstallOptions { no_launch }).await
        }
        Some(Command::Uninstall {
            yes,
            dry_run,
            keep_config,
            keep_database,
        }) => install::uninstall(install::UninstallOptions {
            yes,
            dry_run,
            keep_config,
            keep_database,
        }),
        Some(Command::Openapi) => {
            let spec = audetic::api::docs::ApiDoc::openapi();
            println!("{}", spec.to_pretty_json()?);
            Ok(())
        }
        None => app::run_service().await,
    }
}
