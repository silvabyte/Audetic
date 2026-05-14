//! CLI handler for meeting commands.
//!
//! All commands communicate via the HTTP API (same pattern as other CLI commands).

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::PathBuf;

use crate::api::url::{api_url, paths};
use crate::cli::args::{MeetingCliArgs, MeetingCommand};

/// Daemon API base — single derived value so we never inline
/// `http://127.0.0.1:3737/api/...` in this module.
fn base_url() -> String {
    let mut url = api_url("");
    if url.ends_with('/') {
        url.pop();
    }
    url
}

pub async fn handle_meeting_command(args: MeetingCliArgs) -> Result<()> {
    match args.command {
        MeetingCommand::Start { title } => start_meeting(title).await,
        MeetingCommand::Stop => stop_meeting().await,
        MeetingCommand::Cancel => cancel_meeting().await,
        MeetingCommand::Status => show_status().await,
        MeetingCommand::List { limit } => list_meetings(limit).await,
        MeetingCommand::Show { id } => show_meeting(id).await,
        MeetingCommand::Import { path, title } => import_meeting(path, title).await,
    }
}

/// Decode the API response body, turning non-2xx status codes into a
/// friendly `anyhow::Error`. Extracts `.message` from a JSON error body if
/// present, otherwise falls back to a generic HTTP status message.
///
/// Fixes the class of bugs where the previous handlers called
/// `response.json().await?` on a 404/empty body and crashed with
/// `EOF while parsing a value at line 1 column 0`.
async fn json_or_error(response: reqwest::Response, op: &str) -> Result<Value> {
    let status = response.status();
    let text = response
        .text()
        .await
        .with_context(|| format!("{} response read failed", op))?;

    if !status.is_success() {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or_else(|| format!("{} failed (HTTP {})", op, status));
        bail!(msg);
    }

    serde_json::from_str(&text).with_context(|| format!("{} response parse error", op))
}

async fn start_meeting(title: Option<String>) -> Result<()> {
    let client = reqwest::Client::new();
    let mut body = serde_json::Map::new();
    if let Some(t) = &title {
        body.insert("title".to_string(), Value::String(t.clone()));
    }

    let response = client
        .post(format!("{}/meetings/start", base_url()))
        .json(&body)
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let json = json_or_error(response, "start meeting").await?;

    let meeting_id = json.get("meeting_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let capture = json
        .get("capture_state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    println!(
        "Meeting recording started (id: {}, {})",
        meeting_id, capture
    );

    if let Some(path) = json.get("audio_path").and_then(|v| v.as_str()) {
        println!("Audio: {}", path);
    }

    Ok(())
}

async fn stop_meeting() -> Result<()> {
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{}/meetings/stop", base_url()))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let json = json_or_error(response, "stop meeting").await?;

    println!(
        "Meeting stopped (id: {}, duration: {}s)",
        json.get("meeting_id").and_then(|v| v.as_i64()).unwrap_or(0),
        json.get("duration_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );
    println!(
        "Transcription running in background. Run 'audetic meeting status' to watch progress."
    );

    Ok(())
}

async fn cancel_meeting() -> Result<()> {
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{}/meetings/cancel", base_url()))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let json = json_or_error(response, "cancel meeting").await?;

    println!(
        "Meeting cancelled (id: {}, duration: {}s)",
        json.get("meeting_id").and_then(|v| v.as_i64()).unwrap_or(0),
        json.get("duration_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    );

    Ok(())
}

async fn show_status() -> Result<()> {
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/meetings/status", base_url()))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let json = json_or_error(response, "get meeting status").await?;

    let phase = json
        .get("phase")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let meeting_id = json.get("meeting_id").and_then(|v| v.as_i64());
    let title = json
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled");
    let duration = json
        .get("duration_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let audio_path = json.get("audio_path").and_then(|v| v.as_str());
    let last_error = json.get("last_error").and_then(|v| v.as_str());

    match phase {
        "idle" => {
            println!("No meeting in progress");
        }
        "recording" => {
            let minutes = duration / 60;
            let seconds = duration % 60;
            println!(
                "Meeting: {} (recording, {:02}:{:02})",
                title, minutes, seconds
            );
            if let Some(path) = audio_path {
                println!("Audio: {}", path);
            }
        }
        "compressing" => {
            println!("Meeting #{}: compressing audio...", meeting_id.unwrap_or(0));
        }
        "transcribing" => {
            println!("Meeting #{}: transcribing...", meeting_id.unwrap_or(0));
        }
        "completed" => {
            println!("Meeting #{}: completed", meeting_id.unwrap_or(0));
        }
        "cancelled" => {
            println!("Meeting #{}: cancelled", meeting_id.unwrap_or(0));
        }
        "error" => {
            let err = last_error.unwrap_or("unknown error");
            println!("Meeting #{}: error — {}", meeting_id.unwrap_or(0), err);
        }
        other => {
            println!("Meeting status: {}", other);
        }
    }

    Ok(())
}

async fn list_meetings(limit: usize) -> Result<()> {
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/meetings?limit={}", base_url(), limit))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let json = json_or_error(response, "list meetings").await?;

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
        .get(format!("{}/meetings/{}", base_url(), id))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let json = json_or_error(response, "show meeting").await?;

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

    if let Some(err) = json.get("error").and_then(|v| v.as_str()) {
        if !err.is_empty() {
            println!("Error: {}", err);
        }
    }

    if let Some(transcript) = json.get("transcript_text").and_then(|v| v.as_str()) {
        if !transcript.is_empty() {
            println!("\n--- Transcript ---\n{}", transcript);
        }
    }

    Ok(())
}

async fn import_meeting(path: PathBuf, title: Option<String>) -> Result<()> {
    if !path.exists() {
        bail!("File does not exist: {}", path.display());
    }
    let metadata =
        std::fs::metadata(&path).with_context(|| format!("Failed to stat {}", path.display()))?;
    if !metadata.is_file() {
        bail!("Not a regular file: {}", path.display());
    }

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "upload".to_string());

    let file_size = metadata.len();
    let mime_type = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(crate::transcription::jobs_client::mime_type_for_extension)
        .unwrap_or("application/octet-stream");

    let file = tokio::fs::File::open(&path)
        .await
        .with_context(|| format!("Failed to open {}", path.display()))?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let body = reqwest::Body::wrap_stream(stream);

    let part = reqwest::multipart::Part::stream_with_length(body, file_size)
        .file_name(filename.clone())
        .mime_str(mime_type)?;

    let mut form = reqwest::multipart::Form::new().part("file", part);
    if let Some(t) = title.as_deref() {
        form = form.text("title", t.to_string());
    }

    let client = reqwest::Client::new();
    let response = client
        .post(api_url(paths::MEETINGS_IMPORT))
        .multipart(form)
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;

    let json = json_or_error(response, "import meeting").await?;
    let meeting_id = json.get("meeting_id").and_then(|v| v.as_i64()).unwrap_or(0);

    println!("Imported {} as meeting #{}", filename, meeting_id);
    println!(
        "Transcription running in background. Run 'audetic meeting show {}' to check progress.",
        meeting_id
    );

    Ok(())
}
