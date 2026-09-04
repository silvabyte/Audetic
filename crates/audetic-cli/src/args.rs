use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use audetic_core::keybind::KeybindTarget;
use audetic_core::sync::HubId;

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
    /// Print version information
    Version,
    /// Inspect or configure transcription providers
    Provider(ProviderCliArgs),
    /// Assess setup readiness and configure optional Library Sync
    Setup(SetupCliArgs),
    /// Search and view transcription history
    History(HistoryCliArgs),
    /// View application and transcription logs
    Logs(LogsCliArgs),
    /// Manage Hyprland keybindings for Audetic
    Keybind(KeybindCliArgs),
    /// Transcribe a local audio or video file
    Transcribe(TranscribeCliArgs),
    /// Manage on-device transcription models (list, download)
    Models(ModelsCliArgs),
    /// Record and transcribe meetings
    Meeting(MeetingCliArgs),
    /// Manage post-processing jobs (run commands on daemon events)
    PostProcessing(PostProcessingCliArgs),
}

#[derive(ClapArgs, Debug, Default)]
pub struct SetupCliArgs {
    /// Configure the Library Sync role without prompting
    #[arg(long, value_enum)]
    pub sync_role: Option<SyncRoleArg>,
    /// Home Hub base URL from the command printed by the Home Hub
    #[arg(long, requires = "sync_role")]
    pub hub_url: Option<String>,
    /// Home Hub UUID from the command printed by the Home Hub
    #[arg(long, requires = "sync_role")]
    pub hub_id: Option<HubId>,
    /// Optional display name for this Audetic device
    #[arg(long, requires = "sync_role")]
    pub device_name: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum SyncRoleArg {
    Standalone,
    HomeHub,
    ConnectedDevice,
}

#[derive(ClapArgs, Debug)]
pub struct PostProcessingCliArgs {
    #[command(subcommand)]
    pub command: PostProcessingCommand,
}

#[derive(Subcommand, Debug)]
pub enum PostProcessingCommand {
    /// List all configured jobs (optionally filtered by event)
    List {
        /// Filter to a single event kind (e.g. `dictation.completed`)
        #[arg(short, long)]
        event: Option<String>,
    },
    /// Show details of a specific job
    Show {
        /// Job id
        id: i64,
    },
    /// Create a new job
    Add {
        /// Human-readable name
        #[arg(short, long)]
        name: String,
        /// Event to subscribe to (e.g. `dictation.completed`)
        #[arg(short, long)]
        event: String,
        /// Shell command to run
        #[arg(short, long)]
        command: String,
        /// Timeout in seconds (default 3600)
        #[arg(long, default_value = "3600")]
        timeout: u64,
        /// Create the job disabled (won't fire until enabled)
        #[arg(long)]
        disabled: bool,
    },
    /// Update an existing job
    Update {
        /// Job id
        id: i64,
        /// New name
        #[arg(short, long)]
        name: Option<String>,
        /// New event
        #[arg(short, long)]
        event: Option<String>,
        /// New command
        #[arg(short, long)]
        command: Option<String>,
        /// New timeout in seconds
        #[arg(long)]
        timeout: Option<u64>,
        /// Enable the job
        #[arg(long)]
        enable: bool,
        /// Disable the job
        #[arg(long, conflicts_with = "enable")]
        disable: bool,
    },
    /// Delete a job
    Remove {
        /// Job id
        id: i64,
    },
    /// Run a job once with a synthetic payload to verify the command
    Test {
        /// Job id
        id: i64,
    },
    /// List the supported event kinds
    Events,
}

#[derive(ClapArgs, Debug)]
pub struct ModelsCliArgs {
    #[command(subcommand)]
    pub command: ModelsCommand,
}

#[derive(Subcommand, Debug)]
pub enum ModelsCommand {
    /// List available local models and their download status
    List,
    /// Download a model by id (e.g. `parakeet-tdt-0.6b-v3`)
    Download {
        /// Model id from `audetic models list`
        id: String,
    },
}

#[derive(ClapArgs, Debug)]
pub struct MeetingCliArgs {
    #[command(subcommand)]
    pub command: MeetingCommand,
}

#[derive(Subcommand, Debug)]
pub enum MeetingCommand {
    /// Start recording a meeting
    Start {
        /// Optional meeting title
        #[arg(short, long)]
        title: Option<String>,
    },
    /// Stop recording the current meeting (pauses for review before transcribing)
    Stop,
    /// Confirm the recording awaiting review and send it for transcription,
    /// optionally trimming the start/end first. Times accept `SS`, `MM:SS`,
    /// or `HH:MM:SS` (fractional seconds allowed, e.g. `1:05.5`).
    Confirm {
        /// Trim the recording to start at this time (keeps original start if omitted)
        #[arg(long)]
        start: Option<String>,
        /// Trim the recording to end at this time (keeps original end if omitted)
        #[arg(long)]
        end: Option<String>,
    },
    /// Cancel the in-progress or under-review meeting without transcribing
    Cancel,
    /// Show current meeting recording status
    Status,
    /// List recorded meetings
    List {
        /// Maximum number of results to show
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Show details of a specific meeting
    Show {
        /// Meeting ID
        id: i64,
    },
    /// Delete a meeting (hides it from all views; audio stays on disk)
    Delete {
        /// Meeting ID
        id: i64,
    },
    /// Import an existing audio or video file as a new meeting
    Import {
        /// Path to the media file (audio or video) to import
        path: PathBuf,
        /// Optional Manual Title; otherwise generated from the transcript
        #[arg(short, long)]
        title: Option<String>,
    },
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
    /// Validate provider initialization, or transcribe a supplied audio file
    Test {
        /// Path to an audio file; without one, only validates initialization
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
    /// Stable UUID of a specific dictation to copy to clipboard
    #[arg(short, long)]
    pub copy: Option<audetic_core::sync::RecordId>,
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
    /// Install an Audetic keybinding (dictation defaults to SUPER+R)
    Install {
        /// Shortcut action to install: dictation or meeting
        #[arg(short, long, default_value_t)]
        target: KeybindTarget,
        /// Custom keybinding (e.g., "SUPER SHIFT, R" or "SUPER+T")
        #[arg(short, long)]
        key: Option<String>,
        /// Preview changes without applying
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove Audetic keybinding from config
    Uninstall {
        /// Shortcut action to remove: dictation or meeting
        #[arg(short, long, default_value_t)]
        target: KeybindTarget,
        /// Preview changes without applying
        #[arg(long)]
        dry_run: bool,
    },
    /// Show current keybinding status (both targets by default)
    Status {
        /// Show only one shortcut action
        #[arg(short, long)]
        target: Option<KeybindTarget>,
    },
}

/// Transcribe audio or video files to text.
///
/// Files are automatically compressed to mp3 format before upload.
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn keybind_install_defaults_to_dictation() {
        let cli = Cli::try_parse_from(["audetic", "keybind", "install"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Keybind(KeybindCliArgs {
                command: Some(KeybindCommand::Install {
                    target: KeybindTarget::Dictation,
                    ..
                }),
            }))
        ));
    }

    #[test]
    fn keybind_commands_accept_meeting_target() {
        let cli = Cli::try_parse_from([
            "audetic",
            "keybind",
            "install",
            "--target",
            "meeting",
            "--dry-run",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Keybind(KeybindCliArgs {
                command: Some(KeybindCommand::Install {
                    target: KeybindTarget::Meeting,
                    dry_run: true,
                    ..
                }),
            }))
        ));
    }

    #[test]
    fn provider_test_help_describes_initialization_without_claiming_to_record() {
        let mut command = Cli::command();
        let provider = command
            .find_subcommand_mut("provider")
            .unwrap()
            .find_subcommand_mut("test")
            .unwrap();
        let mut help = Vec::new();
        provider.write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();

        assert!(help.contains("validates initialization"));
        assert!(!help.contains("records brief sample"));
    }

    #[test]
    fn setup_accepts_the_generated_connected_device_command() {
        let cli = Cli::try_parse_from([
            "audetic",
            "setup",
            "--sync-role",
            "connected-device",
            "--hub-url",
            "https://home.example.ts.net:8443/audetic/",
            "--hub-id",
            "67e55044-10b1-426f-9247-bb680e5fe0c8",
            "--device-name",
            "Travel Laptop",
        ])
        .unwrap();

        let Some(CliCommand::Setup(args)) = cli.command else {
            panic!("expected setup command");
        };
        assert_eq!(args.sync_role, Some(SyncRoleArg::ConnectedDevice));
        assert_eq!(
            args.hub_url.as_deref(),
            Some("https://home.example.ts.net:8443/audetic/")
        );
        assert_eq!(
            args.hub_id.unwrap().to_string(),
            "67e55044-10b1-426f-9247-bb680e5fe0c8"
        );
        assert_eq!(args.device_name.as_deref(), Some("Travel Laptop"));
    }

    #[test]
    fn setup_role_values_are_stable_kebab_case() {
        for role in ["standalone", "home-hub", "connected-device"] {
            Cli::try_parse_from(["audetic", "setup", "--sync-role", role]).unwrap();
        }
        assert!(
            Cli::try_parse_from(["audetic", "setup", "--sync-role", "connected_device"]).is_err()
        );
    }

    #[test]
    fn setup_connection_fields_require_a_sync_role() {
        assert!(Cli::try_parse_from([
            "audetic",
            "setup",
            "--hub-url",
            "https://home.example.ts.net:8443/audetic/"
        ])
        .is_err());
    }
}

#[derive(Clone, Debug, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Srt,
}
