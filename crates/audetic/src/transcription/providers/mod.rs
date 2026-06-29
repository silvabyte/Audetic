use anyhow::Result;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use crate::normalizer::TranscriptionNormalizer;
use audetic_core::jobs_client::Segment;

/// Transcription output with optional timing. `segments` is empty for providers
/// that don't surface per-segment timestamps; consumers that only need text use
/// [`TranscriptionProvider::transcribe`].
pub struct TranscriptionOutput {
    pub text: String,
    pub segments: Vec<Segment>,
}

pub mod assembly_api;
pub mod audetic_api;
pub mod local_engine;
pub mod openai_api;
pub mod openai_cli;
pub mod whisper_cpp;

pub use assembly_api::AssemblyAIProvider;
pub use audetic_api::AudeticProvider;
pub use local_engine::LocalEngineProvider;
pub use openai_api::OpenAIProvider;
pub use openai_cli::OpenAIWhisperCliProvider;
pub use whisper_cpp::WhisperCppProvider;

pub trait TranscriptionProvider: Send + Sync {
    fn name(&self) -> &'static str;

    fn is_available(&self) -> bool;

    fn normalizer(&self) -> Result<Box<dyn TranscriptionNormalizer>>;

    fn transcribe<'a>(
        &'a self,
        audio_path: &'a Path,
        language: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>>;

    /// Transcribe and also return per-segment timestamps when the engine
    /// produces them. The default delegates to [`transcribe`](Self::transcribe)
    /// and returns no segments — providers that have timing (e.g. the local
    /// engine) override this.
    fn transcribe_detailed<'a>(
        &'a self,
        audio_path: &'a Path,
        language: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<TranscriptionOutput>> + Send + 'a>> {
        Box::pin(async move {
            let text = self.transcribe(audio_path, language).await?;
            Ok(TranscriptionOutput {
                text,
                segments: Vec::new(),
            })
        })
    }
}
