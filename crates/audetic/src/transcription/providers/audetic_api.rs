use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio::fs;
use tracing::{debug, error, info};

use super::TranscriptionProvider;
use crate::normalizer::TranscriptionNormalizer;

async fn encode_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path).await?;
    Ok(BASE64.encode(&bytes))
}

// struct TranscriptionPayload {
#[derive(Debug, Serialize)]
struct TranscriptionPayload {
    content: String, //base64 string
    language: String,
    timestamps: bool,
}

#[derive(Debug, Deserialize)]
struct TranscriptionResponse {
    result: TranscriptionResult,
}

#[derive(Debug, Deserialize)]
struct TranscriptionResult {
    text: String,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    message: String,
    r#type: Option<String>,
    code: Option<String>,
}

pub struct AudeticProvider {
    client: reqwest::Client,
    endpoint: String,
}

impl AudeticProvider {
    pub fn new(endpoint: Option<String>) -> Result<Self> {
        let client = reqwest::Client::new();
        let endpoint = endpoint
            .unwrap_or_else(|| "https://audio.audetic.link/api/v1/transcriptions".to_string());

        info!("Initialized Audetic provider with endpoint: {}", endpoint);

        Ok(Self { client, endpoint })
    }
}

impl TranscriptionProvider for AudeticProvider {
    fn name(&self) -> &'static str {
        "Audetic API"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn transcribe<'a>(
        &'a self,
        audio_path: &'a Path,
        language: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            info!("Transcribing audio file via Audetic API: {:?}", audio_path);

            let content = encode_file(audio_path).await?;

            let body = TranscriptionPayload {
                content,
                language: language.to_string(),
                timestamps: false,
            };

            debug!("Sending request to Audetic API with model");

            let response = self
                .client
                .post(&self.endpoint)
                .json(&body)
                .send()
                .await
                .context("Failed to send request to Audetic API")?;

            let status = response.status();
            let response_text = response
                .text()
                .await
                .context("Failed to read response body")?;

            if !status.is_success() {
                error!(
                    "Audetic API request failed with status {}: {}",
                    status, response_text
                );

                if let Ok(error_response) = serde_json::from_str::<ErrorResponse>(&response_text) {
                    return Err(anyhow::anyhow!(
                        "Audetic API error: {} (type: {:?}, code: {:?})",
                        error_response.error.message,
                        error_response.error.r#type,
                        error_response.error.code
                    ));
                }

                return Err(anyhow::anyhow!(
                    "Audetic API request failed with status {}: {}",
                    status,
                    response_text
                ));
            }

            let transcription: TranscriptionResponse = serde_json::from_str(&response_text)
                .context("Failed to parse transcription response")?;

            let text = transcription.result.text.trim().to_string();
            info!("Transcription complete: {} chars", text.len());
            debug!("Raw transcription: {}", text);

            Ok(text)
        })
    }

    fn normalizer(&self) -> Result<Box<dyn TranscriptionNormalizer>> {
        Ok(Box::new(AudeticWhisperNormalizer::new()))
    }
}

struct AudeticWhisperNormalizer;

impl AudeticWhisperNormalizer {
    fn new() -> Self {
        Self
    }
}

impl TranscriptionNormalizer for AudeticWhisperNormalizer {
    fn normalize(&self, raw_output: &str) -> String {
        raw_output.trim().to_string()
    }

    fn name(&self) -> &'static str {
        "AudeticWhisperNormalizer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_whisper_normalizer() {
        let normalizer = AudeticWhisperNormalizer::new();

        let input = "  This is clean text  ";
        let expected = "This is clean text";

        assert_eq!(normalizer.normalize(input), expected);
    }
}
