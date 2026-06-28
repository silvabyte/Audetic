//! Local transcription model management: list, download, track progress.
//!
//! The daemon owns the models directory (`<data_dir>/models/`), so downloads
//! happen here and the CLI / web UI drive them over HTTP. Files come straight
//! from HuggingFace (see [`audetic_core::local_models`]) — Audetic has no model
//! CDN. Downloads stream to a `.partial` file with HTTP range resume, then get
//! renamed into place on completion.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};
use utoipa::ToSchema;

use audetic_core::global;
use audetic_core::local_models::{self, ModelFile, ModelInfo};

/// Public, serializable view of a catalog model plus its on-disk + download
/// state. This is what `GET /models` returns.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelDescriptor {
    pub id: String,
    pub label: String,
    pub description: String,
    /// "parakeet" or "whisper".
    pub engine: String,
    pub multilingual: bool,
    /// Whether an explicit language can be set (Whisper) vs auto-detect (Parakeet).
    pub supports_language_selection: bool,
    pub recommended: bool,
    /// Total download size across all files, in bytes.
    pub size_bytes: u64,
    /// Whether the model is fully present on disk.
    pub installed: bool,
    /// In-flight or most-recent download status, if any.
    pub download: Option<DownloadProgress>,
}

/// Progress of a model download.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DownloadProgress {
    /// Bytes are streaming in. `total_bytes` is the catalog's expected total.
    Downloading {
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    /// All files present and verified.
    Completed,
    /// Download failed; `message` is surfaced to the UI.
    Error { message: String },
}

/// Process-wide download status, keyed by model id.
fn download_states() -> &'static Mutex<HashMap<String, DownloadProgress>> {
    static STATES: OnceLock<Mutex<HashMap<String, DownloadProgress>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn set_state(id: &str, progress: DownloadProgress) {
    if let Ok(mut states) = download_states().lock() {
        states.insert(id.to_string(), progress);
    }
}

fn current_state(id: &str) -> Option<DownloadProgress> {
    download_states().lock().ok()?.get(id).cloned()
}

fn build_descriptor(model: &ModelInfo, data_dir: &Path) -> ModelDescriptor {
    let installed = local_models::is_installed(data_dir, model);
    // Once installed, report Completed even if no download ran this session.
    let download = match current_state(model.id) {
        Some(p) => Some(p),
        None if installed => Some(DownloadProgress::Completed),
        None => None,
    };
    ModelDescriptor {
        id: model.id.to_string(),
        label: model.label.to_string(),
        description: model.description.to_string(),
        engine: model.engine.as_str().to_string(),
        multilingual: model.multilingual,
        supports_language_selection: model.supports_language_selection,
        recommended: model.recommended,
        size_bytes: model.total_size_bytes(),
        installed,
        download,
    }
}

/// List every catalog model with its install + download state.
pub fn list() -> Result<Vec<ModelDescriptor>> {
    let data_dir = global::data_dir()?;
    Ok(local_models::catalog()
        .iter()
        .map(|m| build_descriptor(m, &data_dir))
        .collect())
}

/// Describe a single model, or `None` if the id is unknown.
pub fn describe(id: &str) -> Result<Option<ModelDescriptor>> {
    let data_dir = global::data_dir()?;
    Ok(local_models::find(id).map(|m| build_descriptor(m, &data_dir)))
}

/// Begin downloading a model in the background. Returns immediately:
///
/// - already installed → marks Completed, no-op
/// - already downloading → no-op (idempotent)
/// - otherwise → spawns a task that fetches each missing file
///
/// Progress is observable via [`describe`] / [`list`].
pub fn start_download(id: &str) -> Result<()> {
    let model = local_models::find(id)
        .ok_or_else(|| anyhow!("Unknown model '{id}'. Run `audetic models list`."))?;
    let data_dir = global::data_dir()?;

    if local_models::is_installed(&data_dir, model) {
        set_state(model.id, DownloadProgress::Completed);
        return Ok(());
    }

    // Don't start a second download for the same model.
    if matches!(
        current_state(model.id),
        Some(DownloadProgress::Downloading { .. })
    ) {
        return Ok(());
    }

    let total = model.total_size_bytes();
    set_state(
        model.id,
        DownloadProgress::Downloading {
            downloaded_bytes: 0,
            total_bytes: total,
        },
    );

    let model_id = model.id;
    tokio::spawn(async move {
        match download_model_files(model, &data_dir).await {
            Ok(()) => {
                info!("Model '{model_id}' download complete");
                set_state(model_id, DownloadProgress::Completed);
            }
            Err(e) => {
                warn!("Model '{model_id}' download failed: {e:#}");
                set_state(
                    model_id,
                    DownloadProgress::Error {
                        message: format!("{e:#}"),
                    },
                );
            }
        }
    });

    Ok(())
}

/// Download all of a model's files into its directory, updating progress as
/// cumulative bytes across files.
async fn download_model_files(model: &'static ModelInfo, data_dir: &Path) -> Result<()> {
    let dir = local_models::model_dir(data_dir, model.id);
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("Failed to create model dir {dir:?}"))?;

    let total = model.total_size_bytes();
    let mut completed_bytes: u64 = 0;

    for file in model.files {
        let final_path = dir.join(file.name);

        // Skip files that are already fully present.
        if let Ok(meta) = tokio::fs::metadata(&final_path).await {
            if meta.len() >= (file.size_bytes as f64 * 0.9) as u64 {
                completed_bytes += file.size_bytes;
                set_progress(model.id, completed_bytes, total);
                continue;
            }
        }

        download_one_file(model.id, file, &final_path, completed_bytes, total).await?;
        completed_bytes += file.size_bytes;
        set_progress(model.id, completed_bytes, total);
    }

    if !local_models::is_installed(data_dir, model) {
        bail!("Download finished but model files failed validation");
    }
    Ok(())
}

fn set_progress(id: &str, downloaded: u64, total: u64) {
    set_state(
        id,
        DownloadProgress::Downloading {
            downloaded_bytes: downloaded.min(total),
            total_bytes: total,
        },
    );
}

/// Download a single file with `.partial` staging and HTTP range resume.
async fn download_one_file(
    model_id: &str,
    file: &ModelFile,
    final_path: &Path,
    base_completed: u64,
    total: u64,
) -> Result<()> {
    let partial_path = final_path.with_extension("partial");

    // Resume from an existing partial, if any.
    let existing = tokio::fs::metadata(&partial_path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let client = reqwest::Client::new();
    let mut request = client.get(file.url);
    if existing > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }

    let mut response = request
        .send()
        .await
        .with_context(|| format!("Failed to GET {}", file.url))?;

    // If the server ignored the range (200 instead of 206), restart the file.
    let resuming = existing > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if !response.status().is_success() {
        bail!(
            "Download of {} failed: HTTP {}",
            file.url,
            response.status()
        );
    }

    let mut out = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resuming)
        .truncate(!resuming)
        .open(&partial_path)
        .await
        .with_context(|| format!("Failed to open {partial_path:?}"))?;

    let mut written = if resuming { existing } else { 0 };
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("Network error downloading {}", file.url))?
    {
        out.write_all(&chunk)
            .await
            .context("Failed to write chunk")?;
        written += chunk.len() as u64;
        set_progress(model_id, base_completed + written, total);
    }
    out.flush().await.ok();
    drop(out);

    tokio::fs::rename(&partial_path, final_path)
        .await
        .with_context(|| format!("Failed to finalize {final_path:?}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_returns_full_catalog() {
        let models = list().expect("list should succeed");
        assert_eq!(models.len(), local_models::catalog().len());
        assert!(models.iter().any(|m| m.recommended));
    }

    #[test]
    fn describe_unknown_is_none() {
        assert!(describe("nope").unwrap().is_none());
    }
}
