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

/// Map a lowercase file extension to its MIME type.
/// Returns None for unsupported/unknown formats.
pub fn mime_type_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "wav" => Some("audio/wav"),
        "mp3" => Some("audio/mpeg"),
        "m4a" => Some("audio/mp4"),
        "flac" => Some("audio/flac"),
        "ogg" => Some("audio/ogg"),
        "opus" => Some("audio/opus"),
        "mp4" => Some("video/mp4"),
        "mkv" => Some("video/x-matroska"),
        "webm" => Some("video/webm"),
        "avi" => Some("video/x-msvideo"),
        "mov" => Some("video/quicktime"),
        _ => None,
    }
}

/// Known job status values returned by the API.
pub mod status {
    pub const PENDING: &str = "pending";
    pub const EXTRACTING_AUDIO: &str = "extracting_audio";
    pub const TRANSCRIBING: &str = "transcribing";
    pub const COMPLETED: &str = "completed";
    pub const FAILED: &str = "failed";
    pub const CANCELLED: &str = "cancelled";
}

impl JobsClient {
    /// Create a new client with the given base URL.
    pub fn new(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Create with a custom reqwest client (for testing, proxy config, timeouts).
    #[cfg(test)]
    pub fn with_client(client: reqwest::Client, base_url: &str) -> Self {
        Self {
            client,
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
        let mime_type = file_path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(mime_type_for_extension)
            .unwrap_or("application/octet-stream");

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

#[cfg(test)]
mod tests {
    use super::*;

    // MIME type mapping tests
    #[test]
    fn test_mime_type_for_known_audio_extensions() {
        assert_eq!(mime_type_for_extension("wav"), Some("audio/wav"));
        assert_eq!(mime_type_for_extension("mp3"), Some("audio/mpeg"));
        assert_eq!(mime_type_for_extension("m4a"), Some("audio/mp4"));
        assert_eq!(mime_type_for_extension("flac"), Some("audio/flac"));
        assert_eq!(mime_type_for_extension("ogg"), Some("audio/ogg"));
        assert_eq!(mime_type_for_extension("opus"), Some("audio/opus"));
    }

    #[test]
    fn test_mime_type_for_known_video_extensions() {
        assert_eq!(mime_type_for_extension("mp4"), Some("video/mp4"));
        assert_eq!(mime_type_for_extension("mkv"), Some("video/x-matroska"));
        assert_eq!(mime_type_for_extension("webm"), Some("video/webm"));
        assert_eq!(mime_type_for_extension("avi"), Some("video/x-msvideo"));
        assert_eq!(mime_type_for_extension("mov"), Some("video/quicktime"));
    }

    #[test]
    fn test_mime_type_for_unknown_extension() {
        assert_eq!(mime_type_for_extension("xyz"), None);
        assert_eq!(mime_type_for_extension(""), None);
        assert_eq!(mime_type_for_extension("pdf"), None);
    }

    // URL construction tests
    #[test]
    fn test_base_url_trailing_slash_stripped() {
        let client = JobsClient::new("https://example.com/api/v1/jobs/");
        assert_eq!(client.base_url, "https://example.com/api/v1/jobs");
    }

    #[test]
    fn test_base_url_no_trailing_slash() {
        let client = JobsClient::new("https://example.com/api/v1/jobs");
        assert_eq!(client.base_url, "https://example.com/api/v1/jobs");
    }

    // Deserialization tests
    #[test]
    fn test_deserialize_submit_response() {
        let json = r#"{"success":true,"jobId":"job-123","status":"pending","message":"Job created"}"#;
        let resp: SubmitJobResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.job_id, "job-123");
        assert_eq!(resp.status, "pending");
        assert!(resp.success);
    }

    #[test]
    fn test_deserialize_status_response_with_message() {
        let json = r#"{"success":true,"jobId":"job-123","status":"transcribing","progress":45,"progressMessage":"Processing..."}"#;
        let resp: JobStatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.progress, 45);
        assert_eq!(resp.progress_message, Some("Processing...".to_string()));
    }

    #[test]
    fn test_deserialize_status_response_without_message() {
        let json = r#"{"success":true,"jobId":"job-123","status":"pending","progress":0}"#;
        let resp: JobStatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.progress, 0);
        assert_eq!(resp.progress_message, None);
    }

    #[test]
    fn test_deserialize_completed_job_with_result() {
        let json = r#"{
            "success": true,
            "job": {
                "id": "job-123",
                "status": "completed",
                "progress": 100,
                "result": {"text": "Hello world", "segments": []},
                "error": null,
                "createdAt": "2024-01-01T00:00:00Z",
                "completedAt": "2024-01-01T00:01:00Z"
            }
        }"#;
        let resp: JobResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.job.status, "completed");
        assert_eq!(resp.job.progress, 100);
        let result = resp.job.result.unwrap();
        assert_eq!(result.text, "Hello world");
    }

    #[test]
    fn test_deserialize_failed_job() {
        let json = r#"{
            "success": true,
            "job": {
                "id": "job-456",
                "status": "failed",
                "progress": 30,
                "result": null,
                "error": "Transcription engine error",
                "createdAt": "2024-01-01T00:00:00Z",
                "completedAt": "2024-01-01T00:02:00Z"
            }
        }"#;
        let resp: JobResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.job.status, "failed");
        assert!(resp.job.result.is_none());
        assert_eq!(resp.job.error, Some("Transcription engine error".to_string()));
    }

    #[test]
    fn test_with_client_constructor() {
        let client = reqwest::Client::new();
        let jobs_client = JobsClient::with_client(client, "https://example.com/api/");
        assert_eq!(jobs_client.base_url, "https://example.com/api");
    }
}
