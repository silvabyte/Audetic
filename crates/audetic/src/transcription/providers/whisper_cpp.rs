use anyhow::{Context, Result};
use regex::Regex;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Command, Stdio};
use tracing::{debug, error, info, warn};
use which::which;

use super::TranscriptionProvider;
use crate::normalizer::TranscriptionNormalizer;

pub struct WhisperCppProvider {
    command_path: PathBuf,
    model_path: Option<String>,
    model: String,
}

impl WhisperCppProvider {
    pub fn new(
        command_path: Option<String>,
        model: String,
        model_path: Option<String>,
    ) -> Result<Self> {
        let command_path = if let Some(path) = command_path {
            let custom_path = PathBuf::from(path);
            if custom_path.exists() {
                info!("Using custom whisper.cpp path: {:?}", custom_path);
                custom_path
            } else {
                return Err(anyhow::anyhow!(
                    "Custom whisper path does not exist: {:?}",
                    custom_path
                ));
            }
        } else {
            // Try to find whisper-cli first (as built by our install script), then whisper
            which("whisper-cli")
                .or_else(|_| which("whisper"))
                .context("Whisper CLI not found. Please install whisper.cpp (whisper-cli or whisper command)")?
        };

        info!("Found whisper.cpp at: {:?}", command_path);

        Ok(Self {
            command_path,
            model_path,
            model,
        })
    }
}

impl TranscriptionProvider for WhisperCppProvider {
    fn name(&self) -> &'static str {
        "whisper.cpp"
    }

    fn is_available(&self) -> bool {
        self.command_path.exists()
    }

    fn transcribe<'a>(
        &'a self,
        audio_path: &'a Path,
        language: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        let audio_path = audio_path.to_path_buf();
        let language = language.to_string();
        let command_path = self.command_path.clone();
        let model = self.model.clone();
        let model_path = self.model_path.clone();

        Box::pin(async move {
            info!("Using whisper.cpp to transcribe: {:?}", audio_path);
            warn!("whisper.cpp integration is experimental - consider using OpenAI whisper");

            let model_arg = if let Some(mp) = &model_path {
                info!("Using custom model path: {}", mp);
                mp.clone()
            } else {
                format!("models/ggml-{model}.bin")
            };

            let mut cmd = Command::new(&command_path);
            cmd.arg("-f")
                .arg(&audio_path)
                .arg("-m")
                .arg(&model_arg)
                .arg("-l")
                .arg(&language)
                .arg("-nt")
                .arg("-np")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::null());

            let output = cmd
                .output()
                .context("Failed to execute whisper.cpp command")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!("Whisper.cpp failed: {}", stderr);

                warn!("Trying fallback whisper.cpp command");
                let mut cmd = Command::new(&command_path);
                cmd.arg("-f").arg(&audio_path);

                if let Some(mp) = &model_path {
                    cmd.arg("-m").arg(mp);
                }

                let output = cmd
                    .output()
                    .context("Failed to execute fallback whisper.cpp command")?;

                if !output.status.success() {
                    return Err(anyhow::anyhow!("Whisper.cpp transcription failed"));
                }

                let transcription = String::from_utf8_lossy(&output.stdout);
                return Ok(transcription.trim().to_string());
            }

            let transcription = String::from_utf8_lossy(&output.stdout);
            let transcription = transcription.trim().to_string();

            info!("Transcription complete: {} chars", transcription.len());

            Ok(transcription)
        })
    }

    fn normalizer(&self) -> Result<Box<dyn TranscriptionNormalizer>> {
        Ok(Box::new(WhisperCppNormalizer::new()?))
    }
}

struct WhisperCppNormalizer {
    timestamp_regex: Regex,
}

impl WhisperCppNormalizer {
    fn new() -> Result<Self> {
        let timestamp_regex =
            Regex::new(r"\[\d{2}:\d{2}:\d{2}[:.]\d{3}\s*-->\s*\d{2}:\d{2}:\d{2}[:.]\d{3}\]\s*")?;

        Ok(Self { timestamp_regex })
    }
}

impl TranscriptionNormalizer for WhisperCppNormalizer {
    fn normalize(&self, raw_output: &str) -> String {
        debug!("Normalizing whisper.cpp output");

        let mut cleaned = String::new();

        for line in raw_output.lines() {
            let line_cleaned = self.timestamp_regex.replace_all(line, "");
            let line_trimmed = line_cleaned.trim();

            if !line_trimmed.is_empty() {
                if !cleaned.is_empty() {
                    cleaned.push(' ');
                }
                cleaned.push_str(line_trimmed);
            }
        }

        let result = cleaned.trim().to_string();
        debug!(
            "Normalized {} chars to {} chars",
            raw_output.len(),
            result.len()
        );

        result
    }

    fn name(&self) -> &'static str {
        "WhisperCppNormalizer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_whisper_cpp_normalizer() {
        let normalizer = WhisperCppNormalizer::new().unwrap();

        let input = "[00:00:00.000 --> 00:00:03.280] This is me talking\n[00:00:03.280 --> 00:00:05.000] And more text";
        let expected = "This is me talking And more text";

        assert_eq!(normalizer.normalize(input), expected);
    }

    #[test]
    fn test_whisper_cpp_normalizer_with_colons() {
        let normalizer = WhisperCppNormalizer::new().unwrap();

        let input = "[00:00:00:000 --> 00:00:03:280] This is me talking";
        let expected = "This is me talking";

        assert_eq!(normalizer.normalize(input), expected);
    }
}
