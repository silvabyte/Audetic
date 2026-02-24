//! CLI handler for meeting commands.
//!
//! All commands communicate via the HTTP API (same pattern as other CLI commands).

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::cli::args::MeetingCliArgs;

const BASE_URL: &str = "http://127.0.0.1:3737";

pub async fn handle_meeting_command(args: MeetingCliArgs) -> Result<()> {
    match args.command {
        MeetingCommand::Start { title } => start_meeting(title).await,
        MeetingCommand::Stop => stop_meeting().await,
        MeetingCommand::Status => show_status().await,
        MeetingCommand::List { limit } => list_meetings(limit).await,
        MeetingCommand::Show { id } => show_meeting(id).await,
    }
}

use crate::cli::args::MeetingCommand;

async fn start_meeting(title: Option<String>) -> Result<()> {
    let client = reqwest::Client::new();
    let mut body = serde_json::Map::new();
    if let Some(t) = &title {
        body.insert("title".to_string(), Value::String(t.clone()));
    }

    let response = client
        .post(format!("{}/meetings/start", BASE_URL))
        .json(&body)
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let status = response.status();
    let json: Value = response.json().await?;

    if !status.is_success() {
        bail!(
            "Failed to start meeting: {}",
            json.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error")
        );
    }

    println!(
        "Meeting recording started (id: {})",
        json.get("meeting_id").and_then(|v| v.as_i64()).unwrap_or(0)
    );

    if let Some(path) = json.get("audio_path").and_then(|v| v.as_str()) {
        println!("Audio: {}", path);
    }

    Ok(())
}

async fn stop_meeting() -> Result<()> {
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{}/meetings/stop", BASE_URL))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let status = response.status();
    let json: Value = response.json().await?;

    if !status.is_success() {
        bail!(
            "Failed to stop meeting: {}",
            json.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error")
        );
    }

    println!(
        "Meeting stopped (id: {}, duration: {}s)",
        json.get("meeting_id").and_then(|v| v.as_i64()).unwrap_or(0),
        json.get("duration_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );
    println!("Transcription started in background.");

    Ok(())
}

async fn show_status() -> Result<()> {
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/meetings/status", BASE_URL))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let json: Value = response.json().await?;

    let phase = json
        .get("phase")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let active = json.get("active").and_then(|v| v.as_bool()).unwrap_or(false);

    if active {
        let duration = json
            .get("duration_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let title = json
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled");

        let minutes = duration / 60;
        let seconds = duration % 60;

        println!("Meeting: {} ({})", title, phase);
        println!("Duration: {:02}:{:02}", minutes, seconds);

        if let Some(path) = json.get("audio_path").and_then(|v| v.as_str()) {
            println!("Audio: {}", path);
        }
    } else {
        println!("No meeting in progress (status: {})", phase);
    }

    Ok(())
}

async fn list_meetings(limit: usize) -> Result<()> {
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/meetings?limit={}", BASE_URL, limit))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let json: Value = response.json().await?;

    if let Some(meetings) = json.get("meetings").and_then(|v| v.as_array()) {
        if meetings.is_empty() {
            println!("No meetings recorded yet.");
            return Ok(());
        }

        for meeting in meetings {
            let id = meeting.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            let title = meeting
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled");
            let status = meeting
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let duration = meeting
                .get("duration_seconds")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let started = meeting
                .get("started_at")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let minutes = duration / 60;
            let seconds = duration % 60;

            println!(
                "#{} {} [{}] {:02}:{:02} - {}",
                id, title, status, minutes, seconds, started
            );
        }
    }

    Ok(())
}

async fn show_meeting(id: i64) -> Result<()> {
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/meetings/{}", BASE_URL, id))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let status = response.status();
    let json: Value = response.json().await?;

    if !status.is_success() {
        bail!(
            "Meeting not found: {}",
            json.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error")
        );
    }

    let title = json
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled");
    let meeting_status = json
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let duration = json
        .get("duration_seconds")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    println!("Meeting #{}: {}", id, title);
    println!("Status: {}", meeting_status);
    println!("Duration: {:02}:{:02}", duration / 60, duration % 60);

    if let Some(started) = json.get("started_at").and_then(|v| v.as_str()) {
        println!("Started: {}", started);
    }

    if let Some(audio) = json.get("audio_path").and_then(|v| v.as_str()) {
        println!("Audio: {}", audio);
    }

    if let Some(transcript) = json.get("transcript_text").and_then(|v| v.as_str()) {
        println!("\n--- Transcript ---\n{}", transcript);
    }

    Ok(())
}
