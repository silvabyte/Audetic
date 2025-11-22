use crate::db::{self, WorkflowData};
use anyhow::{anyhow, Result};
use arboard::Clipboard;

use super::args::HistoryCliArgs;

pub fn handle_history_command(args: HistoryCliArgs) -> Result<()> {
    let conn = db::init_db()?;

    // If copy flag is provided, copy that specific workflow to clipboard
    if let Some(id) = args.copy {
        let workflows = db::search_workflows(&conn, None, None, None, 1000)?;

        if let Some(workflow) = workflows.iter().find(|w| w.id == Some(id)) {
            let text = match &workflow.data {
                WorkflowData::VoiceToText(data) => &data.text,
            };

            let mut clipboard = Clipboard::new()
                .map_err(|e| anyhow!("Failed to initialize clipboard: {}", e))?;
            clipboard
                .set_text(text)
                .map_err(|e| anyhow!("Failed to copy to clipboard: {}", e))?;

            println!("Copied transcription #{} to clipboard ({} chars)", id, text.len());
            return Ok(());
        } else {
            return Err(anyhow!("Workflow with ID {} not found", id));
        }
    }

    // Otherwise, search and display results
    let workflows = db::search_workflows(
        &conn,
        args.query.as_deref(),
        args.from.as_deref(),
        args.to.as_deref(),
        args.limit,
    )?;

    if workflows.is_empty() {
        println!("No transcriptions found matching your criteria.");
        return Ok(());
    }

    println!("Found {} transcription(s):\n", workflows.len());

    for workflow in workflows {
        let id = workflow.id.unwrap_or(0);
        let created_at = workflow.created_at.as_deref().unwrap_or("Unknown");
        let text = match &workflow.data {
            WorkflowData::VoiceToText(data) => &data.text,
        };

        // Truncate long text for display
        let display_text = if text.len() > 100 {
            format!("{}...", &text[..100])
        } else {
            text.to_string()
        };

        println!("ID: {}", id);
        println!("Date: {}", created_at);
        println!("Text: {}", display_text);
        println!("---");
    }

    println!(
        "\nTo copy a transcription to clipboard, use: audetic history --copy <ID>"
    );

    Ok(())
}
