use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use audetic_core::keybind::KeybindTarget;

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
    /// Assess audio note capture readiness
    Setup,
    /// View application and transcription logs
    Logs(LogsCliArgs),
    /// Manage Hyprland keybindings for Audetic
    Keybind(KeybindCliArgs),
    /// Manage on-device transcription models (list, download)
    Models(ModelsCliArgs),
    /// Capture, import, and manage durable audio notes
    Notes(NotesCliArgs),
    /// Manage post-processing jobs (run commands on daemon events)
    PostProcessing(PostProcessingCliArgs),
    /// Manage external audio integrations
    Integrations(IntegrationsCliArgs),
}

#[derive(ClapArgs, Debug)]
pub struct IntegrationsCliArgs {
    #[command(subcommand)]
    pub command: IntegrationsCommand,
}

#[derive(Subcommand, Debug)]
pub enum IntegrationsCommand {
    /// Show external ingress endpoints
    Status,
    /// Manage one-time ingress access keys
    Keys(IntegrationKeysArgs),
    /// Inspect recent external imports
    Imports {
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Configure and run Plaud synchronization
    Plaud(PlaudArgs),
}

#[derive(ClapArgs, Debug)]
pub struct IntegrationKeysArgs {
    #[command(subcommand)]
    pub command: IntegrationKeysCommand,
}

#[derive(Subcommand, Debug)]
pub enum IntegrationKeysCommand {
    /// Create a key; its plaintext is displayed exactly once
    Add {
        #[arg(long)]
        name: String,
        #[arg(long, value_enum)]
        scope: IntegrationKeyScope,
    },
    /// List key metadata without secrets or hashes
    List,
    /// Revoke a key immediately
    Revoke { id: String },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum IntegrationKeyScope {
    Index,
    Generic,
}

impl IntegrationKeyScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Generic => "generic",
        }
    }
}

#[derive(ClapArgs, Debug)]
pub struct PlaudArgs {
    #[command(subcommand)]
    pub command: PlaudCommand,
}

#[derive(Subcommand, Debug)]
pub enum PlaudCommand {
    /// Show CLI readiness and synchronization state
    Status,
    /// Enable periodic synchronization for new recordings
    Enable {
        #[arg(long, default_value = "15")]
        interval_minutes: i64,
    },
    /// Disable periodic synchronization
    Disable,
    /// Schedule an incremental synchronization
    Sync,
    /// Schedule a historical backfill
    Backfill,
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
        /// Filter to a single event kind (e.g. `audio_note.completed`)
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
        /// Event to subscribe to (e.g. `audio_note.completed`)
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
pub struct NotesCliArgs {
    #[command(subcommand)]
    pub command: NotesCommand,
}

#[derive(ClapArgs, Debug)]
pub struct CaptureArgs {
    #[arg(short, long)]
    pub title: Option<String>,
    #[arg(long, value_enum, default_value = "microphone")]
    pub capture_source: CaptureSource,
    #[arg(long)]
    pub review_before_processing: bool,
    /// Override the configured paste preference (off by default)
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub auto_paste: Option<bool>,
    #[arg(long)]
    pub copy_to_clipboard: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CaptureSource {
    Microphone,
    #[value(name = "microphone_and_system", alias = "microphone-and-system")]
    MicrophoneAndSystem,
}

impl CaptureSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::MicrophoneAndSystem => "microphone_and_system",
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum NotesCommand {
    /// Start capturing an audio note
    Start(CaptureArgs),
    /// Start or stop the current capture
    Toggle(CaptureArgs),
    /// Stop capturing (process immediately unless review was requested)
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
    /// Cancel the in-progress or under-review audio note
    Cancel,
    /// Show current audio note capture status
    Status,
    /// Search and filter audio notes
    List {
        /// Maximum number of results to show
        #[arg(short, long, default_value = "20")]
        limit: usize,
        #[arg(long, default_value = "0")]
        offset: usize,
        #[arg(short, long)]
        query: Option<String>,
        #[arg(long)]
        kind: Option<String>,
    },
    /// Show details and transcript of an audio note
    Show {
        /// Audio note ID
        id: i64,
    },
    /// Hide an audio note (audio stays on disk)
    Delete {
        /// Audio note ID
        id: i64,
    },
    /// Import audio or video through the daemon's durable transcription pipeline
    Import {
        /// Path to the media file (audio or video) to import
        path: PathBuf,
        /// Optional Manual Title; otherwise generated from the transcript
        #[arg(short, long)]
        title: Option<String>,
    },
    /// Retry transcription from retained audio
    Retry { id: i64 },
    /// Run classification and enrichment
    Process { id: i64 },
    /// Copy the persisted raw transcript
    Copy { id: i64 },
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
    /// Install an Audetic keybinding (microphone defaults to SUPER+R)
    Install {
        /// Capture source shortcut: note or system-note
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
        /// Capture source shortcut: note or system-note
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn keybind_install_defaults_to_note() {
        let cli = Cli::try_parse_from(["audetic", "keybind", "install"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Keybind(KeybindCliArgs {
                command: Some(KeybindCommand::Install {
                    target: KeybindTarget::Note,
                    ..
                }),
            }))
        ));
    }

    #[test]
    fn keybind_commands_accept_system_note_target() {
        let cli = Cli::try_parse_from([
            "audetic",
            "keybind",
            "install",
            "--target",
            "system-note",
            "--dry-run",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(CliCommand::Keybind(KeybindCliArgs {
                command: Some(KeybindCommand::Install {
                    target: KeybindTarget::SystemNote,
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
}
