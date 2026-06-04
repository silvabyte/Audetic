//! CLI handler for viewing logs.
//!
//! Talks to the daemon's REST API (`GET /api/logs`).

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::args::LogsCliArgs;
use crate::client::{base_url, json_or_error, CONNECT_HINT};

#[derive(Debug, Deserialize)]
struct LogsResult {
    #[serde(default)]
    app_logs: Vec<String>,
    #[serde(default)]
    transcriptions: Vec<TranscriptionEntry>,
}

#[derive(Debug, Deserialize)]
struct TranscriptionEntry {
    id: i64,
    created_at: String,
    text: String,
}

pub async fn handle_logs_command(args: LogsCliArgs) -> Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/logs", base_url()))
        .query(&[("lines", args.lines.to_string())])
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "get logs").await?;
    let result: LogsResult = serde_json::from_value(body).context("Failed to parse logs")?;

    println!("=== Application Logs (last {} entries) ===\n", args.lines);
    if result.app_logs.is_empty() {
        println!("No application logs found.");
    } else {
        for line in &result.app_logs {
            println!("{}", line);
        }
    }

    println!(
        "\n=== Transcription History (last {} entries) ===\n",
        args.lines
    );
    if result.transcriptions.is_empty() {
        println!("No transcriptions found in history.");
    } else {
        for entry in &result.transcriptions {
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
