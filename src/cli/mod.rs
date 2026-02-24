mod args;
pub mod compression;
mod history;
mod jobs_client;
mod keybind;
mod logs;
pub mod provider;
mod transcribe;
mod update;

// Re-export public API
pub use args::{
    Cli, CliCommand, HistoryCliArgs, KeybindCliArgs, KeybindCommand, LogsCliArgs, OutputFormat,
    ProviderCliArgs, ProviderCommand, TranscribeCliArgs, UpdateCliArgs,
};
pub use history::handle_history_command;
pub use keybind::handle_keybind_command;
pub use logs::handle_logs_command;
pub use provider::handle_provider_command;
pub use transcribe::handle_transcribe_command;
pub use update::handle_update_command;
