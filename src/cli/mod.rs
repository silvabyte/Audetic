use crate::db::{self, WorkflowData};
use crate::update::{UpdateConfig, UpdateEngine, UpdateOptions};
use anyhow::{anyhow, Result};
use arboard::Clipboard;
use clap::{Args as ClapArgs, Parser, Subcommand};
use std::io;
use std::process::Command;

pub mod provider;

pub use provider::handle_provider_command;

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
    /// Inspect or configure transcription providers
    Provider(ProviderCliArgs),
    /// Search and view transcription history
    History(HistoryCliArgs),
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

#[derive(ClapArgs, Debug)]
pub struct ProviderCliArgs {
    #[command(subcommand)]
    pub command: ProviderCommand,
}

#[derive(Subcommand, Debug)]
pub enum ProviderCommand {
    /// Show the current transcription provider configuration
    Show,
    /// Run the interactive provider configuration wizard
    Configure,
    /// Validate the configured provider without recording audio
    Test,
}

#[derive(ClapArgs, Debug)]
pub struct HistoryCliArgs {
    /// Search query to filter transcriptions by text content
    #[arg(short, long)]
    pub query: Option<String>,
    /// Filter by start date (YYYY-MM-DD format)
    #[arg(long)]
    pub from: Option<String>,
    /// Filter by end date (YYYY-MM-DD format)
    #[arg(long)]
    pub to: Option<String>,
    /// Maximum number of results to show
    #[arg(short, long, default_value = "20")]
    pub limit: usize,
    /// ID of specific workflow to copy to clipboard
    #[arg(short, long)]
    pub copy: Option<i64>,
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

pub fn handle_history_command(args: HistoryCliArgs) -> Result<()> {
    let conn = db::init_db()?;

    // If copy flag is provided, copy that specific workflow to clipboard
    if let Some(id) = args.copy {
        let workflows = db::search_workflows(&conn, None, None, None, 1000)?;

        if let Some(workflow) = workflows.iter().find(|w| w.id == Some(id)) {
            let text = match &workflow.data {
                WorkflowData::VoiceToText(data) => &data.text,
            };

            let mut clipboard = Clipboard::new()
                .map_err(|e| anyhow!("Failed to initialize clipboard: {}", e))?;
            clipboard
                .set_text(text)
                .map_err(|e| anyhow!("Failed to copy to clipboard: {}", e))?;

            println!("Copied transcription #{} to clipboard ({} chars)", id, text.len());
            return Ok(());
        } else {
            return Err(anyhow!("Workflow with ID {} not found", id));
        }
    }

    // Otherwise, search and display results
    let workflows = db::search_workflows(
        &conn,
        args.query.as_deref(),
        args.from.as_deref(),
        args.to.as_deref(),
        args.limit,
    )?;

    if workflows.is_empty() {
        println!("No transcriptions found matching your criteria.");
        return Ok(());
    }

    println!("Found {} transcription(s):\n", workflows.len());

    for workflow in workflows {
        let id = workflow.id.unwrap_or(0);
        let created_at = workflow.created_at.as_deref().unwrap_or("Unknown");
        let text = match &workflow.data {
            WorkflowData::VoiceToText(data) => &data.text,
        };

        // Truncate long text for display
        let display_text = if text.len() > 100 {
            format!("{}...", &text[..100])
        } else {
            text.to_string()
        };

        println!("ID: {}", id);
        println!("Date: {}", created_at);
        println!("Text: {}", display_text);
        println!("---");
    }

    println!(
        "\nTo copy a transcription to clipboard, use: audetic history --copy <ID>"
    );

    Ok(())
}
