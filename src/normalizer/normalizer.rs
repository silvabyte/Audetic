use anyhow::Result;
use tracing::{debug, info};

use crate::normalizer::{
    OpenAIWhisperNormalizer, TranscriptionNormalizer, WhisperCppNormalizer,
};

/// Enum to hold different normalizer types
pub enum Normalizer {
    WhisperCpp(WhisperCppNormalizer),
    OpenAIWhisper(OpenAIWhisperNormalizer),
}

impl Normalizer {
    /// Create a normalizer based on whether this is OpenAI whisper or whisper.cpp
    pub fn create(is_openai_whisper: bool) -> Result<Self> {
        if is_openai_whisper {
            info!("Creating OpenAI Whisper normalizer");
            Ok(Normalizer::OpenAIWhisper(OpenAIWhisperNormalizer::new()))
        } else {
            info!("Creating whisper.cpp normalizer");
            Ok(Normalizer::WhisperCpp(WhisperCppNormalizer::new()?))
        }
    }

    /// Run normalization using the appropriate normalizer
    pub fn run(&self, raw_output: &str) -> String {
        match self {
            Normalizer::WhisperCpp(n) => {
                debug!("Running {}", n.name());
                n.normalize(raw_output)
            }
            Normalizer::OpenAIWhisper(n) => {
                debug!("Running {}", n.name());
                n.normalize(raw_output)
            }
        }
    }
}
