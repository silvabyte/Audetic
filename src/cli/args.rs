use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

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
    /// View application and transcription logs
    Logs(LogsCliArgs),
    /// Manage Hyprland keybindings for Audetic
    Keybind(KeybindCliArgs),
    /// Transcribe a local audio or video file
    Transcribe(TranscribeCliArgs),
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
    pub command: Option<ProviderCommand>,
}

#[derive(Subcommand, Debug)]
pub enum ProviderCommand {
    /// Show the current transcription provider configuration
    Show,
    /// Run the interactive provider configuration wizard
    Configure {
        /// Preview changes without saving to config file
        #[arg(long)]
        dry_run: bool,
    },
    /// Test the configured provider with a sample transcription
    Test {
        /// Path to audio file to test with (records brief sample if not provided)
        #[arg(short, long)]
        file: Option<String>,
    },
    /// Show provider status and readiness
    Status,
    /// Reset provider configuration to defaults
    Reset {
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
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

#[derive(ClapArgs, Debug)]
pub struct LogsCliArgs {
    /// Number of log entries to show
    #[arg(short = 'n', long, default_value = "30")]
    pub lines: usize,
}

#[derive(ClapArgs, Debug)]
pub struct KeybindCliArgs {
    #[command(subcommand)]
    pub command: Option<KeybindCommand>,
}

#[derive(Subcommand, Debug)]
pub enum KeybindCommand {
    /// Install Audetic keybinding (default: SUPER+R)
    Install {
        /// Custom keybinding (e.g., "SUPER SHIFT, R" or "SUPER+T")
        #[arg(short, long)]
        key: Option<String>,
        /// Preview changes without applying
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove Audetic keybinding from config
    Uninstall {
        /// Preview changes without applying
        #[arg(long)]
        dry_run: bool,
    },
    /// Show current keybinding status
    Status,
}

/// Transcribe audio or video files to text.
///
/// Files are automatically compressed to opus format before upload.
/// Use --no-compress to send the file in its original format.
#[derive(ClapArgs, Debug)]
pub struct TranscribeCliArgs {
    /// Path to audio or video file to transcribe
    pub file: PathBuf,

    /// Language code (e.g., 'en', 'es', 'auto')
    #[arg(short, long)]
    pub language: Option<String>,

    /// Write transcription to file (default: stdout)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format: text, json, srt
    #[arg(short, long, default_value = "text")]
    pub format: OutputFormat,

    /// Include timestamps in output
    #[arg(long)]
    pub timestamps: bool,

    /// Disable progress indicator
    #[arg(long)]
    pub no_progress: bool,

    /// Copy result to clipboard
    #[arg(short, long)]
    pub copy: bool,

    /// Override transcription API base URL
    #[arg(long)]
    pub api_url: Option<String>,

    /// Skip compression (send file in original format)
    #[arg(long)]
    pub no_compress: bool,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Srt,
}
