//! HTTP client for the transcription-manager jobs API.
//!
//! Provides methods for submitting files for transcription, polling status,
//! and retrieving results.

use anyhow::{Context, Result};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;

/// Client for interacting with the jobs API.
pub struct JobsClient {
    client: reqwest::Client,
    base_url: String,
}

/// Response from submitting a new transcription job.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct SubmitJobResponse {
    pub success: bool,
    #[serde(rename = "jobId")]
    pub job_id: String,
    pub status: String,
    pub message: String,
}

/// Lightweight status response for polling.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct JobStatusResponse {
    pub success: bool,
    #[serde(rename = "jobId")]
    pub job_id: String,
    pub status: String,
    pub progress: u8,
    #[serde(rename = "progressMessage")]
    pub progress_message: Option<String>,
}

/// Full job response including result.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct JobResponse {
    pub success: bool,
    pub job: Job,
}

/// Full job details.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Job {
    pub id: String,
    pub status: String,
    pub progress: u8,
    pub result: Option<TranscriptionResult>,
    pub error: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "completedAt")]
    pub completed_at: Option<String>,
}

/// Transcription result with text and optional segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub text: String,
    pub segments: Option<Vec<Segment>>,
}

/// A segment of transcription with timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

impl JobsClient {
    /// Create a new client with the given base URL.
    pub fn new(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Submit a file for transcription, returns the job ID.
    pub async fn submit_job(
        &self,
        file_path: &Path,
        language: Option<&str>,
        timestamps: bool,
    ) -> Result<String> {
        let file_data = fs::read(file_path).await.context("Failed to read file")?;

        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio")
            .to_string();

        // Determine MIME type from extension
        let mime_type = match file_path.extension().and_then(|e| e.to_str()) {
            Some("wav") => "audio/wav",
            Some("mp3") => "audio/mpeg",
            Some("m4a") => "audio/mp4",
            Some("flac") => "audio/flac",
            Some("ogg") => "audio/ogg",
            Some("opus") => "audio/opus",
            Some("mp4") => "video/mp4",
            Some("mkv") => "video/x-matroska",
            Some("webm") => "video/webm",
            Some("avi") => "video/x-msvideo",
            Some("mov") => "video/quicktime",
            _ => "application/octet-stream",
        };

        let mut form = Form::new().part(
            "file",
            Part::bytes(file_data)
                .file_name(filename)
                .mime_str(mime_type)?,
        );

        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }
        form = form.text("timestamps", timestamps.to_string());

        let response = self
            .client
            .post(&self.base_url)
            .multipart(form)
            .send()
            .await
            .context("Failed to submit job")?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "Job submission failed ({}): {}",
                status,
                body
            ));
        }

        let result: SubmitJobResponse =
            serde_json::from_str(&body).context("Failed to parse job submission response")?;

        Ok(result.job_id)
    }

    /// Get job status (lightweight polling endpoint).
    pub async fn get_status(&self, job_id: &str) -> Result<JobStatusResponse> {
        let url = format!("{}/{}/status", self.base_url, job_id);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to get job status")?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "Failed to get status ({}): {}",
                status,
                body
            ));
        }

        serde_json::from_str(&body).context("Failed to parse status response")
    }

    /// Get full job details including result.
    pub async fn get_job(&self, job_id: &str) -> Result<Job> {
        let url = format!("{}/{}", self.base_url, job_id);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to get job")?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(anyhow::anyhow!("Failed to get job ({}): {}", status, body));
        }

        let result: JobResponse =
            serde_json::from_str(&body).context("Failed to parse job response")?;

        Ok(result.job)
    }
}
