use crate::db::{self, WorkflowData};
use anyhow::{Context, Result};
use std::process::Command;

use super::args::LogsCliArgs;

pub fn handle_logs_command(args: LogsCliArgs) -> Result<()> {
    println!("=== Application Logs (last {} entries) ===\n", args.lines);

    // Fetch application logs from systemd journal
    let journal_output = Command::new("journalctl")
        .arg("--user")
        .arg("-u")
        .arg("audetic.service")
        .arg("-n")
        .arg(args.lines.to_string())
        .arg("--output=short-iso")
        .arg("--no-pager")
        .output()
        .context("Failed to execute journalctl. Is the service running?")?;

    if journal_output.status.success() {
        let logs = String::from_utf8_lossy(&journal_output.stdout);
        if logs.trim().is_empty() {
            println!("No application logs found.");
        } else {
            println!("{}", logs);
        }
    } else {
        let error = String::from_utf8_lossy(&journal_output.stderr);
        println!("Could not fetch application logs: {}", error);
    }

    println!("\n=== Transcription History (last {} entries) ===\n", args.lines);

    // Fetch transcription history from database
    let conn = db::init_db()?;
    let workflows = db::get_recent_workflows(&conn, args.lines)?;

    if workflows.is_empty() {
        println!("No transcriptions found in history.");
    } else {
        for workflow in workflows {
            let id = workflow.id.unwrap_or(0);
            let created_at = workflow.created_at.as_deref().unwrap_or("Unknown");
            let workflow_type = &workflow.workflow_type;
            let text = match &workflow.data {
                WorkflowData::VoiceToText(data) => &data.text,
            };

            // Truncate long text for display
            let display_text = if text.len() > 80 {
                format!("{}...", &text[..80])
            } else {
                text.to_string()
            };

            println!("[{}] {} | {:?} | \"{}\"", id, created_at, workflow_type, display_text);
        }
    }

    Ok(())
}
