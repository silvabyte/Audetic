//! CLI handler for transcription history.
//!
//! This module handles terminal presentation and user interaction.
//! Core business logic is delegated to the `history` module.

use crate::history::{self, SearchParams};
use anyhow::{anyhow, Result};
use arboard::Clipboard;
use dialoguer::FuzzySelect;

use super::args::HistoryCliArgs;

pub fn handle_history_command(args: HistoryCliArgs) -> Result<()> {
    // If copy flag is provided, copy that specific workflow to clipboard
    if let Some(id) = args.copy {
        return handle_copy_by_id(id);
    }

    // Check if no filters are specified (use interactive mode)
    let no_filters = args.query.is_none() && args.from.is_none() && args.to.is_none();

    if no_filters {
        handle_interactive_mode(args.limit)
    } else {
        handle_search_mode(&args)
    }
}

/// Copy a specific transcription to clipboard by ID.
fn handle_copy_by_id(id: i64) -> Result<()> {
    let text = history::get_text_by_id(id)?;

    let mut clipboard =
        Clipboard::new().map_err(|e| anyhow!("Failed to initialize clipboard: {}", e))?;
    clipboard
        .set_text(&text)
        .map_err(|e| anyhow!("Failed to copy to clipboard: {}", e))?;

    println!(
        "Copied transcription #{} to clipboard ({} chars)",
        id,
        text.len()
    );
    Ok(())
}

/// Interactive mode with fuzzy selection.
fn handle_interactive_mode(limit: usize) -> Result<()> {
    let entries = history::get_recent(limit)?;

    if entries.is_empty() {
        println!("No transcriptions found in history.");
        return Ok(());
    }

    // Create display items for FuzzySelect
    let items: Vec<String> = entries
        .iter()
        .map(|entry| {
            // Truncate long text for display
            let display_text = if entry.text.len() > 80 {
                format!("{}...", &entry.text[..80])
            } else {
                entry.text.clone()
            };

            format!("[{}] {} - {}", entry.id, entry.created_at, display_text)
        })
        .collect();

    // Show fuzzy select
    let selection = FuzzySelect::new()
        .with_prompt("Search and select a transcription to copy")
        .items(&items)
        .default(0)
        .interact_opt()?;

    // Handle selection
    if let Some(index) = selection {
        let entry = &entries[index];

        let mut clipboard =
            Clipboard::new().map_err(|e| anyhow!("Failed to initialize clipboard: {}", e))?;
        clipboard
            .set_text(&entry.text)
            .map_err(|e| anyhow!("Failed to copy to clipboard: {}", e))?;

        println!("\n✓ Copied to clipboard ({} chars)", entry.text.len());
        println!("\nFull text:");
        println!("{}", entry.text);
    } else {
        println!("Selection cancelled.");
    }

    Ok(())
}

/// Search mode with filters - displays results without interaction.
fn handle_search_mode(args: &HistoryCliArgs) -> Result<()> {
    let params = SearchParams {
        query: args.query.clone(),
        from: args.from.clone(),
        to: args.to.clone(),
        limit: args.limit,
    };

    let entries = history::search(&params)?;

    if entries.is_empty() {
        println!("No transcriptions found matching your criteria.");
        return Ok(());
    }

    println!("Found {} transcription(s):\n", entries.len());

    for entry in entries {
        // Truncate long text for display
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
