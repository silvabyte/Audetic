//! CLI handler for post-processing job management.
//!
//! Talks to the daemon's REST API (`/api/post-processing/*`) — never
//! reads/writes the SQLite table directly. Mirrors the `meeting` CLI
//! shape so output is consistent across subcommands.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::api::url::{api_url, paths, post_processing_job_path, post_processing_job_test_path};
use crate::cli::args::{PostProcessingCliArgs, PostProcessingCommand};

pub async fn handle_post_processing_command(args: PostProcessingCliArgs) -> Result<()> {
    match args.command {
        PostProcessingCommand::List { event } => list_jobs(event).await,
        PostProcessingCommand::Show { id } => show_job(id).await,
        PostProcessingCommand::Add {
            name,
            event,
            command,
            timeout,
            disabled,
        } => add_job(name, event, command, timeout, !disabled).await,
        PostProcessingCommand::Update {
            id,
            name,
            event,
            command,
            timeout,
            enable,
            disable,
        } => {
            let enabled = if enable {
                Some(true)
            } else if disable {
                Some(false)
            } else {
                None
            };
            update_job(id, name, event, command, timeout, enabled).await
        }
        PostProcessingCommand::Remove { id } => remove_job(id).await,
        PostProcessingCommand::Test { id } => test_job(id).await,
        PostProcessingCommand::Events => list_events().await,
    }
}

/// Decode the API response body; turn non-2xx into a friendly error
/// (extract `.message` from the JSON error envelope if present).
async fn json_or_error(response: reqwest::Response, op: &str) -> Result<Value> {
    let status = response.status();
    let text = response
        .text()
        .await
        .with_context(|| format!("{op} response read failed"))?;

    if !status.is_success() {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or_else(|| format!("{op} failed (HTTP {status})"));
        bail!(msg);
    }

    if text.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(&text).with_context(|| format!("{op} response parse error"))
}

async fn list_jobs(event: Option<String>) -> Result<()> {
    let client = reqwest::Client::new();
    let mut url = api_url(paths::POST_PROCESSING_JOBS);
    if let Some(e) = &event {
        url.push_str("?event=");
        url.push_str(&urlencoding(e));
    }
    let response = client
        .get(&url)
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;
    let body = json_or_error(response, "list jobs").await?;

    let jobs = body
        .get("jobs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if jobs.is_empty() {
        println!("No post-processing jobs configured.");
        return Ok(());
    }

    for job in &jobs {
        print_job_summary(job);
    }
    Ok(())
}

async fn show_job(id: i64) -> Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .get(api_url(&post_processing_job_path(id)))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;
    let job = json_or_error(response, "show job").await?;
    print_job_detail(&job);
    Ok(())
}

async fn add_job(
    name: String,
    event: String,
    command: String,
    timeout: u64,
    enabled: bool,
) -> Result<()> {
    let body = json!({
        "name": name,
        "event": event,
        "action": {
            "type": "command",
            "command": command,
            "timeout_seconds": timeout,
        },
        "enabled": enabled,
    });
    let client = reqwest::Client::new();
    let response = client
        .post(api_url(paths::POST_PROCESSING_JOBS))
        .json(&body)
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;
    let job = json_or_error(response, "add job").await?;
    println!(
        "Created job #{}",
        job.get("id").and_then(|v| v.as_i64()).unwrap_or(0)
    );
    print_job_detail(&job);
    Ok(())
}

async fn update_job(
    id: i64,
    name: Option<String>,
    event: Option<String>,
    command: Option<String>,
    timeout: Option<u64>,
    enabled: Option<bool>,
) -> Result<()> {
    let mut patch = serde_json::Map::new();
    if let Some(n) = name {
        patch.insert("name".to_string(), Value::String(n));
    }
    if let Some(e) = event {
        patch.insert("event".to_string(), Value::String(e));
    }
    if command.is_some() || timeout.is_some() {
        // Action must be supplied as a whole — fetch existing so we can
        // overlay only the fields the user changed.
        let existing = reqwest::Client::new()
            .get(api_url(&post_processing_job_path(id)))
            .send()
            .await
            .context("Failed to connect to Audetic service. Is it running?")?;
        let job = json_or_error(existing, "fetch job for update").await?;
        let cur_command = job
            .get("action")
            .and_then(|a| a.get("command"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cur_timeout = job
            .get("action")
            .and_then(|a| a.get("timeout_seconds"))
            .and_then(|v| v.as_u64())
            .unwrap_or(3600);
        patch.insert(
            "action".to_string(),
            json!({
                "type": "command",
                "command": command.unwrap_or(cur_command),
                "timeout_seconds": timeout.unwrap_or(cur_timeout),
            }),
        );
    }
    if let Some(e) = enabled {
        patch.insert("enabled".to_string(), Value::Bool(e));
    }
    if patch.is_empty() {
        println!("Nothing to update.");
        return Ok(());
    }

    let response = reqwest::Client::new()
        .patch(api_url(&post_processing_job_path(id)))
        .json(&Value::Object(patch))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;
    let job = json_or_error(response, "update job").await?;
    println!("Updated job #{}", id);
    print_job_detail(&job);
    Ok(())
}

async fn remove_job(id: i64) -> Result<()> {
    let response = reqwest::Client::new()
        .delete(api_url(&post_processing_job_path(id)))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;
    let _ = json_or_error(response, "remove job").await?;
    println!("Removed job #{}", id);
    Ok(())
}

async fn test_job(id: i64) -> Result<()> {
    let response = reqwest::Client::new()
        .post(api_url(&post_processing_job_test_path(id)))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;
    let result = json_or_error(response, "test job").await?;

    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let exit_code = result.get("exit_code").and_then(|v| v.as_i64());
    let stdout = result.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    let stderr = result.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
    let timed_out = result
        .get("timed_out")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    println!(
        "Result: {}{}",
        if success { "success" } else { "failed" },
        match exit_code {
            Some(code) => format!(" (exit {})", code),
            None => String::new(),
        }
    );
    if timed_out {
        println!("Timed out");
    }
    if !stdout.is_empty() {
        println!("\n--- stdout ---\n{}", stdout.trim_end());
    }
    if !stderr.is_empty() {
        println!("\n--- stderr ---\n{}", stderr.trim_end());
    }
    Ok(())
}

async fn list_events() -> Result<()> {
    let response = reqwest::Client::new()
        .get(api_url(paths::POST_PROCESSING_EVENTS))
        .send()
        .await
        .context("Failed to connect to Audetic service. Is it running?")?;
    let body = json_or_error(response, "list events").await?;
    let events = body
        .get("events")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for e in &events {
        let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let label = e.get("label").and_then(|v| v.as_str()).unwrap_or("");
        println!("{name}  — {label}");
    }
    Ok(())
}

fn print_job_summary(job: &Value) {
    let id = job.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let name = job
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("(no name)");
    let event = job.get("event").and_then(|v| v.as_str()).unwrap_or("?");
    let enabled = job
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let command = job
        .get("action")
        .and_then(|a| a.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let short_cmd = if command.chars().count() > 60 {
        format!("{}…", command.chars().take(57).collect::<String>())
    } else {
        command.to_string()
    };
    println!(
        "#{id} [{event}] {name} {} — {short_cmd}",
        if enabled { "(enabled)" } else { "(disabled)" }
    );
}

fn print_job_detail(job: &Value) {
    let id = job.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let name = job
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("(no name)");
    let event = job.get("event").and_then(|v| v.as_str()).unwrap_or("?");
    let enabled = job
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let action_type = job
        .get("action")
        .and_then(|a| a.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let command = job
        .get("action")
        .and_then(|a| a.get("command"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let timeout = job
        .get("action")
        .and_then(|a| a.get("timeout_seconds"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    println!("Job #{id}: {name}");
    println!("  Event:   {event}");
    println!("  Enabled: {enabled}");
    println!("  Action:  {action_type}");
    println!("  Command: {command}");
    println!("  Timeout: {timeout}s");
}

/// Minimal percent-encoder for the `event` query string. The set of
/// legal event names is a known-safe alphanumeric-plus-dot grammar, but
/// we encode anyway to defend against future renames.
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}
