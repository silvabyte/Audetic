# Adding a Transcription Provider

This guide walks you through adding a new transcription provider to Audetic.

## Quick Overview

Adding a provider requires changes in **4 areas**:

| Step                                              | File(s)                                                          | What You'll Do                             |
| ------------------------------------------------- | ---------------------------------------------------------------- | ------------------------------------------ |
| [1. Create Provider](./01-create-provider.md)     | `src/transcription/providers/your_provider.rs`                   | Implement the provider + normalizer        |
| [2. Register Provider](./02-register-provider.md) | `src/transcription/providers/mod.rs`, `src/transcription/mod.rs` | Export and wire up the factory             |
| [3. Add CLI Wizard](./03-cli-wizard.md)           | `src/cli/provider.rs`                                            | Let users configure via `audetic provider` |
| [4. Update Docs](./04-update-docs.md)             | `docs/`, `example_config.toml`                                   | Document the new provider                  |

## Before You Start

1. **Pick a provider name** - Use kebab-case: `superspeech-api`, `google-stt`, `amazon-transcribe`
2. **Understand the API** - Have the provider's API docs handy
3. **Get test credentials** - You'll need real credentials to test

## Time Estimate

- Simple API provider: ~1-2 hours
- CLI-based provider: ~2-3 hours
- Complex provider (polling, retries): ~3-4 hours

## Reference Implementations

Study these existing providers:

| Provider          | Type      | Complexity | Good For Learning                 |
| ----------------- | --------- | ---------- | --------------------------------- |
| `audetic_api.rs`  | HTTP API  | Simple     | Basic structure, JSON payload     |
| `openai_api.rs`   | HTTP API  | Medium     | Multipart uploads, error handling |
| `assembly_api.rs` | HTTP API  | Complex    | Polling, async workflows          |
| `whisper_cpp.rs`  | Local CLI | Medium     | Binary detection, fallbacks       |

## The Provider Trait

Every provider implements this trait:

```rust
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
```

## Ready?

Start with [Step 1: Create the Provider →](./01-create-provider.md)
