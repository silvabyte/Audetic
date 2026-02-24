//! Transcription job service abstraction.
//!
//! Provides a trait for submitting audio to a remote transcription service
//! and polling for results, decoupled from CLI concerns (no progress bars).

use anyhow::{bail, Result};
use async_trait::async_trait;
use std::path::Path;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

use super::jobs_client::{status, JobsClient, Segment};

/// Result of a completed transcription job.
pub struct TranscriptionJobResult {
    pub text: String,
    pub segments: Option<Vec<Segment>>,
}

/// Trait for submitting audio to a remote transcription service and getting results.
#[async_trait]
pub trait TranscriptionJobService: Send + Sync {
    async fn submit_and_poll(
        &self,
        file_path: &Path,
        language: Option<&str>,
    ) -> Result<TranscriptionJobResult>;
}

/// Implementation that uses the remote jobs API via `JobsClient`.
/// Polls without progress bars — reports progress via `tracing::info!`.
pub struct RemoteTranscriptionJobService {
    client: JobsClient,
    poll_interval: Duration,
    timeout: Duration,
}

impl RemoteTranscriptionJobService {
    /// Create a new service with the given API base URL.
    ///
    /// # Arguments
    /// * `base_url` - Jobs API base URL
    /// * `timeout` - Maximum time to wait for transcription to complete
    pub fn new(base_url: &str, timeout: Duration) -> Self {
        Self {
            client: JobsClient::new(base_url),
            poll_interval: Duration::from_secs(2),
            timeout,
        }
    }
}

#[async_trait]
impl TranscriptionJobService for RemoteTranscriptionJobService {
    async fn submit_and_poll(
        &self,
        file_path: &Path,
        language: Option<&str>,
    ) -> Result<TranscriptionJobResult> {
        info!("Submitting file for transcription: {:?}", file_path);

        // Use streaming upload for large files
        let job_id = self
            .client
            .submit_job_streaming(file_path, language, true)
            .await?;

        info!("Transcription job submitted: {}", job_id);

        let max_attempts = (self.timeout.as_secs() / self.poll_interval.as_secs()).max(1);
        let mut last_status = String::new();

        for attempt in 0..max_attempts {
            let job_status = self.client.get_status(&job_id).await?;

            // Log status changes
            if job_status.status != last_status {
                info!(
                    "Transcription job {} status: {} ({}%)",
                    job_id, job_status.status, job_status.progress
                );
                last_status = job_status.status.clone();
            }

            match job_status.status.as_str() {
                status::COMPLETED => {
                    let job = self.client.get_job(&job_id).await?;
                    let result = job
                        .result
                        .ok_or_else(|| anyhow::anyhow!("Job completed but no result available"))?;

                    info!("Transcription complete: {} chars", result.text.len());
                    return Ok(TranscriptionJobResult {
                        text: result.text,
                        segments: result.segments,
                    });
                }
                status::FAILED => {
                    let job = self.client.get_job(&job_id).await?;
                    bail!(
                        "Transcription failed: {}",
                        job.error.unwrap_or_else(|| "Unknown error".to_string())
                    );
                }
                status::CANCELLED => {
                    bail!("Transcription job was cancelled");
                }
                _ => {
                    if attempt > 0 && attempt % 30 == 0 {
                        let elapsed = attempt * self.poll_interval.as_secs();
                        warn!(
                            "Transcription job {} still running after {}s ({}%)",
                            job_id, elapsed, job_status.progress
                        );
                    }
                    sleep(self.poll_interval).await;
                }
            }
        }

        bail!(
            "Transcription timed out after {} seconds",
            self.timeout.as_secs()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transcription_job_result_creation() {
        let result = TranscriptionJobResult {
            text: "Hello world".to_string(),
            segments: None,
        };
        assert_eq!(result.text, "Hello world");
        assert!(result.segments.is_none());
    }

    #[test]
    fn test_transcription_job_result_with_segments() {
        let result = TranscriptionJobResult {
            text: "Hello world".to_string(),
            segments: Some(vec![Segment {
                start: 0.0,
                end: 1.5,
                text: "Hello world".to_string(),
            }]),
        };
        assert_eq!(result.segments.as_ref().unwrap().len(), 1);
        assert_eq!(result.segments.as_ref().unwrap()[0].start, 0.0);
    }

    #[test]
    fn test_remote_service_creation() {
        let service =
            RemoteTranscriptionJobService::new("https://example.com/api/v1/jobs", Duration::from_secs(7200));
        assert_eq!(service.timeout, Duration::from_secs(7200));
        assert_eq!(service.poll_interval, Duration::from_secs(2));
    }
}
