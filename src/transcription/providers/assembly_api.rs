use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;
use tracing::{debug, error, info};

use super::TranscriptionProvider;
use crate::normalizer::TranscriptionNormalizer;

/// Response from the upload endpoint
#[derive(Debug, Deserialize)]
struct UploadResponse {
    upload_url: String,
}

/// Request body for creating a transcript
#[derive(Debug, Serialize)]
struct TranscriptRequest {
    audio_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_code: Option<String>,
}

/// Response from transcript creation and polling
#[derive(Debug, Deserialize)]
struct TranscriptResponse {
    id: String,
    status: TranscriptStatus,
    text: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum TranscriptStatus {
    Queued,
    Processing,
    Completed,
    Error,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
}

pub struct AssemblyAIProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl AssemblyAIProvider {
    pub fn new(api_key: String, endpoint: Option<String>) -> Result<Self> {
        let client = reqwest::Client::new();
        let base_url = endpoint.unwrap_or_else(|| "https://api.assemblyai.com/v2".to_string());

        info!(
            "Initialized AssemblyAI provider with base URL: {}",
            base_url
        );

        Ok(Self {
            client,
            api_key,
            base_url,
        })
    }

    /// Upload audio file to AssemblyAI and get a URL
    async fn upload_audio(&self, audio_path: &Path) -> Result<String> {
        let upload_url = format!("{}/upload", self.base_url);

        debug!("Uploading audio file to AssemblyAI: {:?}", audio_path);

        let audio_data = tokio::fs::read(audio_path)
            .await
            .context("Failed to read audio file")?;

        let response = self
            .client
            .post(&upload_url)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/octet-stream")
            .body(audio_data)
            .send()
            .await
            .context("Failed to upload audio to AssemblyAI")?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .context("Failed to read upload response body")?;

        if !status.is_success() {
            error!(
                "AssemblyAI upload failed with status {}: {}",
                status, response_text
            );
            return Err(anyhow::anyhow!(
                "AssemblyAI upload failed with status {}: {}",
                status,
                response_text
            ));
        }

        let upload_response: UploadResponse =
            serde_json::from_str(&response_text).context("Failed to parse upload response")?;

        debug!(
            "Audio uploaded successfully: {}",
            upload_response.upload_url
        );
        Ok(upload_response.upload_url)
    }

    /// Submit transcription request
    async fn submit_transcription(&self, audio_url: String, language: &str) -> Result<String> {
        let transcript_url = format!("{}/transcript", self.base_url);

        let language_code = if language.is_empty() || language == "auto" {
            None
        } else {
            Some(language.to_string())
        };

        let request_body = TranscriptRequest {
            audio_url,
            language_code,
        };

        debug!("Submitting transcription request to AssemblyAI");

        let response = self
            .client
            .post(&transcript_url)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .context("Failed to submit transcription request")?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .context("Failed to read transcription response body")?;

        if !status.is_success() {
            error!(
                "AssemblyAI transcription request failed with status {}: {}",
                status, response_text
            );

            if let Ok(error_response) = serde_json::from_str::<ErrorResponse>(&response_text) {
                return Err(anyhow::anyhow!(
                    "AssemblyAI API error: {}",
                    error_response.error
                ));
            }

            return Err(anyhow::anyhow!(
                "AssemblyAI transcription request failed with status {}: {}",
                status,
                response_text
            ));
        }

        let transcript_response: TranscriptResponse = serde_json::from_str(&response_text)
            .context("Failed to parse transcription response")?;

        debug!(
            "Transcription submitted with ID: {}",
            transcript_response.id
        );
        Ok(transcript_response.id)
    }

    /// Poll for transcription completion
    async fn poll_transcription(&self, transcript_id: &str) -> Result<String> {
        let poll_url = format!("{}/transcript/{}", self.base_url, transcript_id);
        let poll_interval = Duration::from_secs(3);
        // lets make this 6 minutes
        let max_attempts = 120; // 6 minutes max

        for attempt in 1..=max_attempts {
            debug!(
                "Polling transcription status (attempt {}/{}): {}",
                attempt, max_attempts, transcript_id
            );

            let response = self
                .client
                .get(&poll_url)
                .header("Authorization", &self.api_key)
                .send()
                .await
                .context("Failed to poll transcription status")?;

            let status = response.status();
            let response_text = response
                .text()
                .await
                .context("Failed to read poll response body")?;

            if !status.is_success() {
                error!(
                    "AssemblyAI poll request failed with status {}: {}",
                    status, response_text
                );
                return Err(anyhow::anyhow!(
                    "AssemblyAI poll request failed with status {}: {}",
                    status,
                    response_text
                ));
            }

            let transcript_response: TranscriptResponse =
                serde_json::from_str(&response_text).context("Failed to parse poll response")?;

            match transcript_response.status {
                TranscriptStatus::Completed => {
                    let text = transcript_response
                        .text
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    info!("Transcription complete: {} chars", text.len());
                    return Ok(text);
                }
                TranscriptStatus::Error => {
                    let error_msg = transcript_response
                        .error
                        .unwrap_or_else(|| "Unknown error".to_string());
                    error!("Transcription failed: {}", error_msg);
                    return Err(anyhow::anyhow!("Transcription failed: {}", error_msg));
                }
                TranscriptStatus::Queued | TranscriptStatus::Processing => {
                    debug!("Transcription still processing, waiting...");
                    tokio::time::sleep(poll_interval).await;
                }
            }
        }

        Err(anyhow::anyhow!(
            "Transcription timed out after {} attempts",
            max_attempts
        ))
    }
}

impl TranscriptionProvider for AssemblyAIProvider {
    fn name(&self) -> &'static str {
        "AssemblyAI API"
    }

    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn transcribe<'a>(
        &'a self,
        audio_path: &'a Path,
        language: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            info!(
                "Transcribing audio file via AssemblyAI API: {:?}",
                audio_path
            );

            // Step 1: Upload the audio file
            let audio_url = self.upload_audio(audio_path).await?;

            // Step 2: Submit transcription request
            let transcript_id = self.submit_transcription(audio_url, language).await?;

            // Step 3: Poll for completion
            let text = self.poll_transcription(&transcript_id).await?;

            debug!("Raw transcription: {}", text);
            Ok(text)
        })
    }

    fn normalizer(&self) -> Result<Box<dyn TranscriptionNormalizer>> {
        Ok(Box::new(AssemblyAINormalizer::new()))
    }
}

struct AssemblyAINormalizer;

impl AssemblyAINormalizer {
    fn new() -> Self {
        Self
    }
}

impl TranscriptionNormalizer for AssemblyAINormalizer {
    fn normalize(&self, raw_output: &str) -> String {
        raw_output.trim().to_string()
    }

    fn name(&self) -> &'static str {
        "AssemblyAINormalizer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assembly_ai_normalizer() {
        let normalizer = AssemblyAINormalizer::new();

        let input = "  This is clean text  ";
        let expected = "This is clean text";

        assert_eq!(normalizer.normalize(input), expected);
    }
}
