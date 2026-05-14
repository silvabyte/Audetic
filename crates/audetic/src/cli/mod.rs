mod args;
pub mod compression;
mod history;
mod install;
mod keybind;
mod logs;
pub mod meeting;
mod post_processing;
pub mod provider;
mod transcribe;
mod update;

// Re-export public API
pub use args::{
    Cli, CliCommand, HistoryCliArgs, InstallCliArgs, KeybindCliArgs, KeybindCommand, LogsCliArgs,
    MeetingCliArgs, OutputFormat, PostProcessingCliArgs, PostProcessingCommand, ProviderCliArgs,
    ProviderCommand, TranscribeCliArgs, UpdateCliArgs,
};
pub use history::handle_history_command;
pub use install::handle_install_command;
pub use keybind::handle_keybind_command;
pub use logs::handle_logs_command;
pub use meeting::handle_meeting_command;
pub use post_processing::handle_post_processing_command;
pub use provider::handle_provider_command;
pub use transcribe::handle_transcribe_command;
pub use update::handle_update_command;
