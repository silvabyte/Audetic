use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use tracing::info;

use crate::normalizer::TranscriptionNormalizer;

mod transcription_service;

pub mod providers;

pub use providers::{
    AudeticProvider, OpenAIProvider, OpenAIWhisperCliProvider, TranscriptionProvider,
    WhisperCppProvider,
};

pub use transcription_service::TranscriptionService;

pub struct Transcriber {
    provider: Box<dyn TranscriptionProvider>,
    language: String,
}

impl Transcriber {
    pub fn with_provider(provider_name: &str, config: ProviderConfig) -> Result<Self> {
        let language = config.language.clone().unwrap_or_else(|| "en".to_string());

        let provider: Box<dyn TranscriptionProvider> = match provider_name {
            "audetic-api" => {

                Box::new(AudeticProvider::new(config.api_endpoint)?)
            }
            "openai-api" => {
                let api_key = config
                    .api_key
                    .context("api_key is required for OpenAI API provider")?;

                let model = config.model.unwrap_or_else(|| "whisper-1".to_string());
                Box::new(OpenAIProvider::new(api_key, config.api_endpoint, model)?)
            }
            "openai-cli" => {
                let model = config.model.unwrap_or_else(|| "base".to_string());
                Box::new(OpenAIWhisperCliProvider::new(config.command_path, model)?)
            }
            "whisper-cpp" => {
                let model = config.model.unwrap_or_else(|| "base".to_string());
                Box::new(WhisperCppProvider::new(
                    config.command_path,
                    model,
                    config.model_path,
                )?)
            }
            _ => bail!(
                "Unknown transcription provider '{}'. Supported providers: audetic-api, openai-api, openai-cli, whisper-cpp",
                provider_name
            ),
        };

        info!("Using {} for transcription", provider.name());

        Ok(Self { provider, language })
    }

    pub async fn transcribe(&self, audio_path: &PathBuf) -> Result<String> {
        info!(
            "Transcribing audio file: {:?} with {}",
            audio_path,
            self.provider.name()
        );
        self.provider
            .transcribe(audio_path.as_path(), &self.language)
            .await
    }

    pub fn normalizer(&self) -> Result<Box<dyn TranscriptionNormalizer>> {
        self.provider.normalizer()
    }
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub model: Option<String>,
    pub model_path: Option<String>,
    pub language: Option<String>,
    pub command_path: Option<String>,
    pub api_endpoint: Option<String>,
    pub api_key: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            model: None,
            model_path: None,
            language: Some("en".to_string()),
            command_path: None,
            api_endpoint: None,
            api_key: None,
        }
    }
}
