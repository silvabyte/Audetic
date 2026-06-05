//TODO:AGENT: why is this needed? can drop a comment explaining this
#![allow(clippy::arc_with_non_send_sync)]

//! `audeticd` — the Audetic daemon.
//!
//! With no subcommand it runs the long-lived service (audio capture, the HTTP
//! API on 127.0.0.1:3737, and the bundled web UI). The only subcommand is
//! `install`, which bootstraps the platform service (systemd user unit on
//! Linux, LaunchAgent on macOS) and places the standalone `audetic` CLI on
//! PATH. `install` deliberately lives here rather than in the slim CLI because
//! on macOS it must run from inside the `Audetic.app` bundle so TCC permission
//! attribution lands on the bundle's cdhash.
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
        Some(Command::Openapi) => {
            let spec = audetic::api::docs::ApiDoc::openapi();
            println!("{}", spec.to_pretty_json()?);
            Ok(())
        }
        None => app::run_service().await,
    }
}
