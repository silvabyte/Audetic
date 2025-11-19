/// Trait for normalizing transcription output from various whisper implementations
pub trait TranscriptionNormalizer: Send + Sync {
    /// Normalize the raw transcription output
    fn normalize(&self, raw_output: &str) -> String;

    /// Get the name of this normalizer for logging
    fn name(&self) -> &'static str;
}
