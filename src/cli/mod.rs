mod args;
mod history;
mod keybind;
mod logs;
pub mod provider;
mod update;

// Re-export public API
pub use args::{
    Cli, CliCommand, HistoryCliArgs, KeybindCliArgs, KeybindCommand, LogsCliArgs, ProviderCliArgs,
    ProviderCommand, UpdateCliArgs,
};
pub use history::handle_history_command;
pub use keybind::handle_keybind_command;
pub use logs::handle_logs_command;
pub use provider::handle_provider_command;
pub use update::handle_update_command;
