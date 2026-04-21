//! CLI handler for viewing logs.
//!
//! This module handles terminal presentation.
//! Core business logic is delegated to the `logs` module.

use crate::logs::{self, LogsOptions};
use anyhow::Result;

use super::args::LogsCliArgs;

pub fn handle_logs_command(args: LogsCliArgs) -> Result<()> {
    let options = LogsOptions::new(args.lines);
    let result = logs::get_logs(&options)?;

    // Display application logs
    println!("=== Application Logs (last {} entries) ===\n", args.lines);

    if result.app_logs.is_empty() {
        println!("No application logs found.");
    } else {
        for line in &result.app_logs {
            println!("{}", line);
        }
    }

    // Display transcription history
    println!(
        "\n=== Transcription History (last {} entries) ===\n",
        args.lines
    );

    if result.transcriptions.is_empty() {
        println!("No transcriptions found in history.");
    } else {
        for entry in &result.transcriptions {
            // Truncate long text for display
            let display_text = if entry.text.len() > 80 {
                format!("{}...", &entry.text[..80])
            } else {
                entry.text.clone()
            };

            println!("[{}] {} | \"{}\"", entry.id, entry.created_at, display_text);
        }
    }

    Ok(())
}
