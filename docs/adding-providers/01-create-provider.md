# Step 1: Create the Provider

Create a new file for your provider at `src/transcription/providers/your_provider.rs`.

## File Structure

Your provider file contains **three things**:

1. **Response structs** - For deserializing API responses
2. **Provider struct + impl** - The main provider logic
3. **Normalizer struct + impl** - Cleans up raw transcription output

## Starter Template

Copy this template and customize for your provider:

```rust
use anyhow::{Context, Result};
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tracing::{debug, error, info};

use crate::normalizer::TranscriptionNormalizer;
use crate::transcription::providers::TranscriptionProvider;

// =============================================================================
// Response types
// =============================================================================

#[derive(Debug, Deserialize)]
struct YourProviderResponse {
    text: String,
    // Add other fields your API returns
}

#[derive(Debug, Deserialize)]
struct YourProviderError {
    error: String,
    // Add other error fields
}

// =============================================================================
// Provider
// =============================================================================

pub struct YourProvider {
    client: reqwest::Client,
    api_key: String,
    endpoint: String,
    model: String,
}

impl YourProvider {
    pub fn new(api_key: String, endpoint: Option<String>, model: String) -> Result<Self> {
        let client = reqwest::Client::new();
        let endpoint = endpoint.unwrap_or_else(|| {
            "https://api.yourprovider.com/v1/transcribe".to_string()
        });

        info!("Initialized YourProvider with endpoint: {}", endpoint);

        Ok(Self {
            client,
            api_key,
            endpoint,
            model,
        })
    }
}

impl TranscriptionProvider for YourProvider {
    fn name(&self) -> &'static str {
        "YourProvider API"
    }

    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn normalizer(&self) -> Result<Box<dyn TranscriptionNormalizer>> {
        Ok(Box::new(YourProviderNormalizer))
    }

    fn transcribe<'a>(
        &'a self,
        audio_path: &'a Path,
        language: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async move {
            info!("Transcribing via YourProvider: {:?}", audio_path);

            // 1. Read audio file
            let audio_data = tokio::fs::read(audio_path)
                .await
                .context("Failed to read audio file")?;

            let filename = audio_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("audio.wav");

            // 2. Build request (multipart example)
            let audio_part = Part::bytes(audio_data)
                .file_name(filename.to_string())
                .mime_str("audio/wav")
                .context("Failed to set MIME type")?;

            let mut form = Form::new()
                .part("audio", audio_part)
                .text("model", self.model.clone());

            if !language.is_empty() && language != "auto" {
                form = form.text("language", language.to_string());
            }

            // 3. Send request
            let response = self
                .client
                .post(&self.endpoint)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .multipart(form)
                .send()
                .await
                .context("Failed to send request")?;

            let status = response.status();
            let response_text = response.text().await
                .context("Failed to read response")?;

            // 4. Handle errors
            if !status.is_success() {
                error!("API request failed: {} - {}", status, response_text);

                if let Ok(err) = serde_json::from_str::<YourProviderError>(&response_text) {
                    return Err(anyhow::anyhow!("API error: {}", err.error));
                }
                return Err(anyhow::anyhow!("Request failed: {}", status));
            }

            // 5. Parse response
            let result: YourProviderResponse = serde_json::from_str(&response_text)
                .context("Failed to parse response")?;

            info!("Transcription complete: {} chars", result.text.len());
            Ok(result.text)
        })
    }
}

// =============================================================================
// Normalizer (keep in same file!)
// =============================================================================

struct YourProviderNormalizer;

impl TranscriptionNormalizer for YourProviderNormalizer {
    fn normalize(&self, raw_output: &str) -> String {
        // Clean up provider-specific quirks
        raw_output.trim().to_string()
    }

    fn name(&self) -> &'static str {
        "YourProviderNormalizer"
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_available_with_key() {
        let provider = YourProvider::new(
            "test-key".to_string(),
            None,
            "base".to_string(),
        ).unwrap();

        assert!(provider.is_available());
    }

    #[test]
    fn test_provider_unavailable_without_key() {
        let provider = YourProvider::new(
            "".to_string(),
            None,
            "base".to_string(),
        ).unwrap();

        assert!(!provider.is_available());
    }

    #[test]
    fn test_normalizer() {
        let normalizer = YourProviderNormalizer;
        assert_eq!(normalizer.normalize("  hello world  "), "hello world");
    }
}
```

## Key Points

### The Normalizer Lives Here

The normalizer struct and impl go in the **same file** as the provider. This keeps related code together and makes maintenance easier.

### Async Pattern

The `transcribe` method uses a manual async pattern:

```rust
fn transcribe<'a>(...) -> Pin<Box<dyn Future<Output = Result<String>> + Send + 'a>> {
    Box::pin(async move {
        // async code here
    })
}
```

This is required because async traits aren't fully stable yet.

### Error Handling

Use `anyhow::Context` to add context to errors:

```rust
.context("Failed to read audio file")?
```

This produces helpful error messages like:

```
Failed to read audio file: No such file or directory
```

### Logging Levels

- `info!` - Provider init, transcription start/complete
- `debug!` - Request details, raw responses
- `error!` - API failures

## Common Provider Patterns

### API with Polling (like AssemblyAI)

```rust
// 1. Upload audio, get upload URL
// 2. Submit transcription job
// 3. Poll until complete
loop {
    let status = check_status(&job_id).await?;
    match status.as_str() {
        "completed" => return Ok(status.text),
        "error" => return Err(anyhow!("Transcription failed")),
        _ => tokio::time::sleep(Duration::from_secs(3)).await,
    }
}
```

### CLI-Based Provider

```rust
use std::process::Command;
use which::which;

let binary = which("whisper").context("whisper not found in PATH")?;

let output = Command::new(&binary)
    .arg("--model").arg(&self.model)
    .arg("--output-format").arg("txt")
    .arg(audio_path)
    .output()
    .context("Failed to run whisper")?;

String::from_utf8_lossy(&output.stdout).trim().to_string()
```

## Checklist

- [ ] Created `src/transcription/providers/your_provider.rs`
- [ ] Implemented `TranscriptionProvider` trait
- [ ] Implemented normalizer in the same file
- [ ] Added basic tests
- [ ] Code compiles: `cargo check`

## Next Step

[Step 2: Register the Provider →](./02-register-provider.md)
