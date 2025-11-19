use crate::normalizer::TranscriptionNormalizer;

/// Normalizer for OpenAI Whisper output format
pub struct OpenAIWhisperNormalizer;

impl Default for OpenAIWhisperNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAIWhisperNormalizer {
    pub fn new() -> Self {
        Self
    }
}

impl TranscriptionNormalizer for OpenAIWhisperNormalizer {
    fn normalize(&self, raw_output: &str) -> String {
        // OpenAI Whisper typically outputs clean text without timestamps
        // Just trim whitespace
        raw_output.trim().to_string()
    }

    fn name(&self) -> &'static str {
        "OpenAIWhisperNormalizer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_whisper_normalizer() {
        let normalizer = OpenAIWhisperNormalizer::new();

        let input = "  This is clean text  ";
        let expected = "This is clean text";

        assert_eq!(normalizer.normalize(input), expected);
    }
}
