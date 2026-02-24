mod args;
pub mod compression;
mod history;
mod keybind;
mod logs;
pub mod meeting;
pub mod provider;
mod transcribe;
mod update;

// Re-export public API
pub use args::{
    Cli, CliCommand, HistoryCliArgs, KeybindCliArgs, KeybindCommand, LogsCliArgs,
    MeetingCliArgs, OutputFormat, ProviderCliArgs, ProviderCommand, TranscribeCliArgs,
    UpdateCliArgs,
};
pub use history::handle_history_command;
pub use keybind::handle_keybind_command;
pub use logs::handle_logs_command;
pub use meeting::handle_meeting_command;
pub use provider::handle_provider_command;
pub use transcribe::handle_transcribe_command;
pub use update::handle_update_command;
