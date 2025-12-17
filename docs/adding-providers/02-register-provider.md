# Step 2: Register the Provider

Now wire up your provider so Audetic can use it.

## 2.1 Export from Providers Module

Edit `src/transcription/providers/mod.rs`:

```rust
// Add your module
mod audetic_api;
mod assembly_api;
mod openai_api;
mod openai_cli;
mod whisper_cpp;
mod your_provider;  // <-- Add this

// Re-export your provider
pub use audetic_api::AudeticProvider;
pub use assembly_api::AssemblyAIProvider;
pub use openai_api::OpenAIProvider;
pub use openai_cli::OpenAIWhisperCliProvider;
pub use whisper_cpp::WhisperCppProvider;
pub use your_provider::YourProvider;  // <-- Add this
```

## 2.2 Register in Factory

Edit `src/transcription/mod.rs` and add your provider to the `with_provider()` match:

```rust
pub fn with_provider(provider_name: &str, config: ProviderConfig) -> Result<Self> {
    let language = config.language.clone().unwrap_or_else(|| "en".to_string());

    let provider: Box<dyn TranscriptionProvider> = match provider_name {
        "audetic-api" => {
            // existing...
        }
        "assembly-ai" => {
            // existing...
        }
        "openai-api" => {
            // existing...
        }
        // ... other providers ...

        // Add your provider here:
        "your-provider" => {
            let api_key = config.api_key
                .context("api_key is required for YourProvider")?;
            let model = config.model.unwrap_or_else(|| "default".to_string());

            Box::new(YourProvider::new(api_key, config.api_endpoint, model)?)
        }

        _ => bail!("Unknown transcription provider: '{}'", provider_name),
    };

    Ok(Self { provider, language })
}
```

## 2.3 Add Config Validation

In the same file, find `validate_provider_config()` and add validation for your provider:

```rust
pub fn validate_provider_config(provider: &str, config: &ProviderConfig) -> Option<String> {
    match provider {
        "audetic-api" => None,  // No extra config needed

        "openai-api" | "assembly-ai" => {
            if config.api_key.is_none() {
                return Some(format!("{} requires an API key", provider));
            }
            None
        }

        // Add your provider:
        "your-provider" => {
            if config.api_key.is_none() {
                return Some("your-provider requires an API key".to_string());
            }
            None
        }

        _ => Some(format!("Unknown provider: {}", provider)),
    }
}
```

## Verify It Works

```bash
# Should compile without errors
cargo check

# Run tests
cargo test
```

## Checklist

- [ ] Added `mod your_provider;` to `providers/mod.rs`
- [ ] Added `pub use your_provider::YourProvider;` to `providers/mod.rs`
- [ ] Added match arm in `Transcriber::with_provider()`
- [ ] Added validation in `validate_provider_config()`
- [ ] `cargo check` passes

## Next Step

[Step 3: Add CLI Wizard →](./03-cli-wizard.md)
