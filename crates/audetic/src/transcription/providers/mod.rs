use anyhow::Result;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use crate::normalizer::TranscriptionNormalizer;

pub mod assembly_api;
pub mod audetic_api;
pub mod openai_api;
pub mod openai_cli;
pub mod whisper_cpp;

pub use assembly_api::AssemblyAIProvider;
pub use audetic_api::AudeticProvider;
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
}
