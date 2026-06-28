use anyhow::Result;
use std::path::PathBuf;
use tracing::{debug, info};

use super::{Transcriber, TranscriptionOutput};
use crate::normalizer::TranscriptionNormalizer;

/// Service that orchestrates transcription and normalization
pub struct TranscriptionService {
    transcriber: Transcriber,
    normalizer: Box<dyn TranscriptionNormalizer>,
}

impl TranscriptionService {
    /// Create a new transcription service with the provided transcriber
    pub fn new(transcriber: Transcriber) -> Result<Self> {
        let normalizer = transcriber.normalizer()?;

        Ok(Self {
            transcriber,
            normalizer,
        })
    }

    /// Transcribe audio file and return normalized text
    pub async fn transcribe(&self, audio_path: &PathBuf) -> Result<String> {
        info!("Starting transcription pipeline for: {:?}", audio_path);

        // Step 1: Get raw transcription
        debug!("Getting raw transcription");
        let raw_transcription = self.transcriber.transcribe(audio_path).await?;

        // Step 2: Normalize the transcription
        debug!("Normalizing transcription output");
        let normalized = self.normalizer.normalize(&raw_transcription);

        info!(
            "Transcription pipeline complete: {} chars -> {} chars",
            raw_transcription.len(),
            normalized.len()
        );

        Ok(normalized)
    }

    /// Transcribe and return normalized text plus per-segment timestamps (empty
    /// when the provider doesn't surface them).
    pub async fn transcribe_detailed(&self, audio_path: &PathBuf) -> Result<TranscriptionOutput> {
        info!(
            "Starting detailed transcription pipeline for: {:?}",
            audio_path
        );
        let raw = self.transcriber.transcribe_detailed(audio_path).await?;
        let text = self.normalizer.normalize(&raw.text);
        Ok(TranscriptionOutput {
            text,
            segments: raw.segments,
        })
    }
}

#[cfg(test)]
mod tests {
    // use super::*;

    #[tokio::test]
    async fn test_transcription_service_creation() {
        //TODO: implement this
        // NOTE:: This would require mocking Transcriber
    }
}
