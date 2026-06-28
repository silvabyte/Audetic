//! CLI handler for on-device transcription models.
//!
//! All operations go through the daemon (it owns the models directory):
//! `GET /models` to list, `POST /models/{id}/download` to fetch, and
//! `GET /models/{id}` polled for progress. Models download from HuggingFace.

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::Value;
use std::time::Duration;
use tokio::time::sleep;

use crate::args::{ModelsCliArgs, ModelsCommand};
use crate::client::{json_or_error, CONNECT_HINT};
use audetic_core::url::{api_url, model_download_path, model_path, paths};

pub async fn handle_models_command(args: ModelsCliArgs) -> Result<()> {
    match args.command {
        ModelsCommand::List => handle_list().await,
        ModelsCommand::Download { id } => ensure_downloaded(&id).await,
    }
}

async fn handle_list() -> Result<()> {
    let response = reqwest::Client::new()
        .get(api_url(paths::MODELS))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "list models").await?;

    let models = body
        .get("models")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();

    println!();
    println!("On-device transcription models");
    println!("==============================");
    println!();
    for model in &models {
        let id = model.get("id").and_then(Value::as_str).unwrap_or("?");
        let label = model.get("label").and_then(Value::as_str).unwrap_or("");
        let installed = model
            .get("installed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let recommended = model
            .get("recommended")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let size_gb =
            model.get("size_bytes").and_then(Value::as_u64).unwrap_or(0) as f64 / 1_000_000_000.0;
        let status = if installed {
            "installed".to_string()
        } else {
            describe_download(model.get("download"))
        };
        let star = if recommended { " (recommended)" } else { "" };
        println!("  {id}");
        println!("    {label}{star}");
        println!("    {size_gb:.2} GB — {status}");
        if let Some(desc) = model.get("description").and_then(Value::as_str) {
            println!("    {desc}");
        }
        println!();
    }
    println!("Download one with: audetic models download <id>");
    Ok(())
}

fn describe_download(download: Option<&Value>) -> String {
    match download.and_then(|d| d.get("state").and_then(Value::as_str)) {
        Some("downloading") => "downloading...".to_string(),
        Some("completed") => "installed".to_string(),
        Some("error") => "error".to_string(),
        _ => "not downloaded".to_string(),
    }
}

/// Trigger a download and poll until the model is installed (or fails),
/// rendering a progress bar.
pub async fn ensure_downloaded(id: &str) -> Result<()> {
    let client = reqwest::Client::new();

    let response = client
        .post(api_url(&model_download_path(id)))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "start model download").await?;

    if body
        .get("installed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        println!("Model '{id}' is already installed.");
        return Ok(());
    }

    let pb = ProgressBar::new(100);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {percent}% {msg}")
            .unwrap()
            .progress_chars("━╸━"),
    );
    pb.enable_steady_tick(Duration::from_millis(120));
    pb.set_message(format!("Downloading {id}..."));

    loop {
        let response = client
            .get(api_url(&model_path(id)))
            .send()
            .await
            .context(CONNECT_HINT)?;
        let model = json_or_error(response, "poll model status").await?;

        if model
            .get("installed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            pb.set_position(100);
            pb.finish_with_message(format!("Downloaded {id}"));
            return Ok(());
        }

        if let Some(d) = model.get("download") {
            match d.get("state").and_then(Value::as_str) {
                Some("downloading") => {
                    let done = d
                        .get("downloaded_bytes")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let total = d
                        .get("total_bytes")
                        .and_then(Value::as_u64)
                        .unwrap_or(1)
                        .max(1);
                    pb.set_position((done * 100 / total).min(100));
                }
                Some("error") => {
                    let msg = d
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error");
                    pb.abandon_with_message("Download failed");
                    bail!("Model download failed: {msg}");
                }
                _ => {}
            }
        }

        sleep(Duration::from_millis(1000)).await;
    }
}
