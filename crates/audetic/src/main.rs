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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let log_level = if cli.verbose { "debug" } else { "info" };
    let env_filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();

    // Build the async runtime explicitly (rather than `#[tokio::main]`) because
    // on macOS the service runs on a worker thread and the *main* thread is
    // reserved for the global-hotkey CFRunLoop — see `run_service`.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    match cli.command {
        Some(Command::Install { no_launch }) => {
            runtime.block_on(install::run(install::InstallOptions { no_launch }))
        }
        Some(Command::Openapi) => {
            let spec = audetic::api::docs::ApiDoc::openapi();
            println!("{}", spec.to_pretty_json()?);
            Ok(())
        }
        None => run_service(runtime),
    }
}

/// Run the long-lived daemon service. On every platform but macOS this just
/// drives the async service on the main thread.
#[cfg(not(target_os = "macos"))]
fn run_service(runtime: tokio::runtime::Runtime) -> Result<()> {
    runtime.block_on(app::run_service())
}

/// macOS: the async service runs on a dedicated worker thread so the main
/// thread can host the global-hotkey CFRunLoop (Carbon `RegisterEventHotKey`
/// requires the run loop to be pumped on the main thread). The hotkey loop
/// blocks forever; if the service thread exits we bring the process down so
/// launchd's `KeepAlive` restarts us.
#[cfg(target_os = "macos")]
fn run_service(runtime: tokio::runtime::Runtime) -> Result<()> {
    use tracing::error;

    let handle = runtime.handle().clone();
    std::thread::Builder::new()
        .name("audetic-service".into())
        .spawn(move || {
            if let Err(e) = runtime.block_on(app::run_service()) {
                error!("Audetic service exited with error: {e:?}");
            }
            std::process::exit(1);
        })?;

    let initial = audetic::hotkey::initial_hotkey();
    audetic::hotkey::run_main_loop(handle, initial)
}
