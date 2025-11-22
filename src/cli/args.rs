use clap::{Args as ClapArgs, Parser, Subcommand};

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
