use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tracing::info;

use crate::config::{Config, WhisperConfig};
use crate::normalizer::TranscriptionNormalizer;

mod transcription_service;

pub mod providers;

pub use providers::{
    AssemblyAIProvider, AudeticProvider, OpenAIProvider, OpenAIWhisperCliProvider,
    TranscriptionProvider, WhisperCppProvider,
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
            "audetic-api" => Box::new(AudeticProvider::new(config.api_endpoint)?),
            "assembly-ai" => {
                let api_key = config
                    .api_key
                    .context("api_key is required for AssemblyAI provider")?;

                Box::new(AssemblyAIProvider::new(api_key, config.api_endpoint)?)
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
                "Unknown transcription provider '{}'. Supported providers: audetic-api, assembly-ai, openai-api, openai-cli, whisper-cpp",
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

impl From<&WhisperConfig> for ProviderConfig {
    fn from(whisper: &WhisperConfig) -> Self {
        Self {
            model: whisper.model.clone(),
            model_path: whisper.model_path.clone(),
            language: whisper.language.clone(),
            command_path: whisper.command_path.clone(),
            api_endpoint: whisper.api_endpoint.clone(),
            api_key: whisper.api_key.clone(),
        }
    }
}

// ============================================================================
// Provider status and validation
// ============================================================================

/// Status of the transcription provider
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderStatus {
    /// Provider is configured and ready
    Ready {
        provider: String,
        model: Option<String>,
        language: Option<String>,
    },
    /// Provider is configured but validation failed
    ConfigError { provider: String, error: String },
    /// No provider configured
    NotConfigured,
}

/// Result of testing a provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderTestResult {
    /// Whether the test succeeded
    pub success: bool,
    /// Transcription result (if successful)
    pub transcription: Option<String>,
    /// Error message (if failed)
    pub error: Option<String>,
    /// Time taken in seconds
    pub duration_secs: f64,
}

/// Get the current provider status from config.
pub fn get_provider_status() -> Result<ProviderStatus> {
    let config = Config::load()?;
    get_provider_status_from_config(&config.whisper)
}

/// Get provider status from a WhisperConfig.
pub fn get_provider_status_from_config(whisper: &WhisperConfig) -> Result<ProviderStatus> {
    let provider = match &whisper.provider {
        Some(p) if !p.is_empty() => p.clone(),
        _ => return Ok(ProviderStatus::NotConfigured),
    };

    // Validate configuration based on provider type
    let validation_error = validate_provider_config(&provider, whisper);

    if let Some(error) = validation_error {
        return Ok(ProviderStatus::ConfigError { provider, error });
    }

    // Try to initialize the provider to verify it works
    let provider_config = ProviderConfig::from(whisper);
    match Transcriber::with_provider(&provider, provider_config) {
        Ok(_) => Ok(ProviderStatus::Ready {
            provider,
            model: whisper.model.clone(),
            language: whisper.language.clone(),
        }),
        Err(e) => Ok(ProviderStatus::ConfigError {
            provider,
            error: e.to_string(),
        }),
    }
}

/// Validate provider configuration and return an error message if invalid.
pub fn validate_provider_config(provider: &str, whisper: &WhisperConfig) -> Option<String> {
    match provider {
        "audetic-api" => None, // No additional config required
        "assembly-ai" => {
            if whisper.api_key.is_none() {
                Some("API key required for AssemblyAI".to_string())
            } else {
                None
            }
        }
        "openai-api" => {
            if whisper.api_key.is_none() {
                Some("API key required for OpenAI API".to_string())
            } else {
                None
            }
        }
        "openai-cli" => {
            if whisper.command_path.is_none() {
                Some("Command path required for OpenAI CLI".to_string())
            } else {
                None
            }
        }
        "whisper-cpp" => {
            if whisper.command_path.is_none() {
                Some("Command path required for whisper.cpp".to_string())
            } else if whisper.model_path.is_none() {
                Some("Model path required for whisper.cpp".to_string())
            } else {
                None
            }
        }
        _ => Some(format!("Unknown provider: {}", provider)),
    }
}

/// Test the current provider with an optional audio file.
///
/// If no file is provided, only validates that the provider can be initialized.
pub async fn test_provider(audio_file: Option<&Path>) -> Result<ProviderTestResult> {
    let config = Config::load()?;
    test_provider_with_config(&config.whisper, audio_file).await
}

/// Test a provider with specific config.
pub async fn test_provider_with_config(
    whisper: &WhisperConfig,
    audio_file: Option<&Path>,
) -> Result<ProviderTestResult> {
    let provider_name = whisper
        .provider
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("No transcription provider configured"))?;

    let provider_config = ProviderConfig::from(whisper);

    // Try to initialize
    let transcriber = match Transcriber::with_provider(provider_name, provider_config) {
        Ok(t) => t,
        Err(e) => {
            return Ok(ProviderTestResult {
                success: false,
                transcription: None,
                error: Some(e.to_string()),
                duration_secs: 0.0,
            });
        }
    };

    // If audio file provided, actually transcribe
    if let Some(path) = audio_file {
        let start = std::time::Instant::now();
        match transcriber.transcribe(&path.to_path_buf()).await {
            Ok(text) => Ok(ProviderTestResult {
                success: true,
                transcription: Some(text),
                error: None,
                duration_secs: start.elapsed().as_secs_f64(),
            }),
            Err(e) => Ok(ProviderTestResult {
                success: false,
                transcription: None,
                error: Some(e.to_string()),
                duration_secs: start.elapsed().as_secs_f64(),
            }),
        }
    } else {
        // Just validate initialization
        Ok(ProviderTestResult {
            success: true,
            transcription: None,
            error: None,
            duration_secs: 0.0,
        })
    }
}

/// Get a summary of the current provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
    pub api_endpoint: Option<String>,
    pub has_api_key: bool,
    pub command_path: Option<String>,
    pub model_path: Option<String>,
}

/// Get provider info from config.
pub fn get_provider_info() -> Result<ProviderInfo> {
    let config = Config::load()?;
    Ok(get_provider_info_from_config(&config.whisper))
}

/// Get provider info from a WhisperConfig.
pub fn get_provider_info_from_config(whisper: &WhisperConfig) -> ProviderInfo {
    ProviderInfo {
        provider: whisper.provider.clone(),
        model: whisper.model.clone(),
        language: whisper.language.clone(),
        api_endpoint: whisper.api_endpoint.clone(),
        has_api_key: whisper.api_key.is_some(),
        command_path: whisper.command_path.clone(),
        model_path: whisper.model_path.clone(),
    }
}
