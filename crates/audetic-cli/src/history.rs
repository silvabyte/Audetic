//! CLI handler for transcription history.
//!
//! Talks to the daemon's REST API (`GET /api/history`, `GET /api/history/{id}`).
//! Clipboard copy happens client-side.

use anyhow::{Context, Result};
use audetic_core::clipboard::copy_to_clipboard_sync;
use dialoguer::FuzzySelect;
use serde::Deserialize;

use crate::args::HistoryCliArgs;
use crate::client::{base_url, json_or_error, CONNECT_HINT};

#[derive(Debug, Deserialize)]
struct HistoryEntry {
    id: i64,
    created_at: String,
    text: String,
}

pub async fn handle_history_command(args: HistoryCliArgs) -> Result<()> {
    if let Some(id) = args.copy {
        return handle_copy_by_id(id).await;
    }

    let no_filters = args.query.is_none() && args.from.is_none() && args.to.is_none();
    if no_filters {
        handle_interactive_mode(args.limit).await
    } else {
        handle_search_mode(&args).await
    }
}

/// Fetch history entries from the daemon, applying the given filters.
async fn fetch_history(args: &HistoryCliArgs) -> Result<Vec<HistoryEntry>> {
    let client = reqwest::Client::new();
    let mut req = client
        .get(format!("{}/history", base_url()))
        .query(&[("limit", args.limit.to_string())]);
    if let Some(q) = &args.query {
        req = req.query(&[("q", q)]);
    }
    if let Some(from) = &args.from {
        req = req.query(&[("from", from)]);
    }
    if let Some(to) = &args.to {
        req = req.query(&[("to", to)]);
    }

    let response = req.send().await.context(CONNECT_HINT)?;
    let body = json_or_error(response, "list history").await?;
    serde_json::from_value(body).context("Failed to parse history entries")
}

/// Copy a specific transcription to clipboard by ID.
async fn handle_copy_by_id(id: i64) -> Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/history/{}", base_url(), id))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "get transcription").await?;
    let entry: HistoryEntry =
        serde_json::from_value(body).context("Failed to parse transcription")?;

    copy_to_clipboard_sync(&entry.text)?;
    println!(
        "Copied transcription #{} to clipboard ({} chars)",
        entry.id,
        entry.text.len()
    );
    Ok(())
}

/// Interactive mode with fuzzy selection.
async fn handle_interactive_mode(limit: usize) -> Result<()> {
    let args = HistoryCliArgs {
        query: None,
        from: None,
        to: None,
        limit,
        copy: None,
    };
    let entries = fetch_history(&args).await?;

    if entries.is_empty() {
        println!("No transcriptions found in history.");
        return Ok(());
    }

    let items: Vec<String> = entries
        .iter()
        .map(|entry| {
            let display_text = if entry.text.len() > 80 {
                format!("{}...", &entry.text[..80])
            } else {
                entry.text.clone()
            };
            format!("[{}] {} - {}", entry.id, entry.created_at, display_text)
        })
        .collect();

    let selection = FuzzySelect::new()
        .with_prompt("Search and select a transcription to copy")
        .items(&items)
        .default(0)
        .interact_opt()?;

    if let Some(index) = selection {
        let entry = &entries[index];
        copy_to_clipboard_sync(&entry.text)?;
        println!("\n✓ Copied to clipboard ({} chars)", entry.text.len());
        println!("\nFull text:");
        println!("{}", entry.text);
    } else {
        println!("Selection cancelled.");
    }

    Ok(())
}

/// Search mode with filters - displays results without interaction.
async fn handle_search_mode(args: &HistoryCliArgs) -> Result<()> {
    let entries = fetch_history(args).await?;

    if entries.is_empty() {
        println!("No transcriptions found matching your criteria.");
        return Ok(());
    }

    println!("Found {} transcription(s):\n", entries.len());

    for entry in entries {
        let display_text = if entry.text.len() > 100 {
            format!("{}...", &entry.text[..100])
        } else {
            entry.text.clone()
        };

        println!("ID: {}", entry.id);
        println!("Date: {}", entry.created_at);
        println!("Text: {}", display_text);
        println!("---");
    }

    println!("\nTo copy a transcription to clipboard, use: audetic history --copy <ID>");
    Ok(())
}
