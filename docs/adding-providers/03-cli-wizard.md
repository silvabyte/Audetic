# Step 3: Add CLI Wizard

Let users configure your provider via `audetic provider configure`.

Edit `src/cli/provider.rs` - you'll make changes in **6 places**.

## 3.1 Add to ProviderSelection Enum

Find the `ProviderSelection` enum near the bottom of the file:

```rust
#[derive(Debug, Clone, Copy)]
enum ProviderSelection {
    AudeticApi,
    AssemblyAi,
    OpenAiApi,
    OpenAiCli,
    WhisperCpp,
    YourProvider,  // <-- Add this
}
```

## 3.2 Add to OPTIONS List

Find `prompt_provider_selection()` and add to the OPTIONS array:

```rust
const OPTIONS: &[(&str, &str)] = &[
    ("audetic-api", "Audetic Cloud API (default, no setup required)"),
    ("assembly-ai", "AssemblyAI API (requires API key)"),
    ("openai-api", "OpenAI Whisper API (requires API key)"),
    ("openai-cli", "Local OpenAI Whisper CLI (requires local install)"),
    ("whisper-cpp", "Local whisper.cpp binary (requires local install)"),
    ("your-provider", "YourProvider API (requires API key)"),  // <-- Add this
];
```

## 3.3 Add as_str() Mapping

Find `impl ProviderSelection` and add to `as_str()`:

```rust
fn as_str(&self) -> &'static str {
    match self {
        ProviderSelection::AudeticApi => "audetic-api",
        ProviderSelection::AssemblyAi => "assembly-ai",
        ProviderSelection::OpenAiApi => "openai-api",
        ProviderSelection::OpenAiCli => "openai-cli",
        ProviderSelection::WhisperCpp => "whisper-cpp",
        ProviderSelection::YourProvider => "your-provider",  // <-- Add this
    }
}
```

## 3.4 Add from_index() Mapping

Update `from_index()` to handle the new menu index:

```rust
fn from_index(index: usize) -> Self {
    match index {
        0 => ProviderSelection::AudeticApi,
        1 => ProviderSelection::AssemblyAi,
        2 => ProviderSelection::OpenAiApi,
        3 => ProviderSelection::OpenAiCli,
        4 => ProviderSelection::WhisperCpp,
        _ => ProviderSelection::YourProvider,  // <-- Add this (catches index 5+)
    }
}
```

## 3.5 Add to handle_configure Match

Find `handle_configure()` and add your provider to the match:

```rust
match selection {
    ProviderSelection::AudeticApi => configure_audetic_api(&theme, &mut config.whisper)?,
    ProviderSelection::AssemblyAi => configure_assembly_ai(&theme, &mut config.whisper)?,
    ProviderSelection::OpenAiApi => configure_openai_api(&theme, &mut config.whisper)?,
    ProviderSelection::OpenAiCli => configure_openai_cli(&theme, &mut config.whisper)?,
    ProviderSelection::WhisperCpp => configure_whisper_cpp(&theme, &mut config.whisper)?,
    ProviderSelection::YourProvider => configure_your_provider(&theme, &mut config.whisper)?,  // <-- Add
}
```

## 3.6 Create Configuration Function

Add a new function to handle the configuration wizard:

```rust
fn configure_your_provider(theme: &ColorfulTheme, whisper: &mut WhisperConfig) -> Result<()> {
    // Clear fields not used by this provider
    whisper.command_path = None;
    whisper.model_path = None;

    // Prompt for API key (required)
    let api_key = prompt_secret(theme, "YourProvider API key", whisper.api_key.as_ref())?;
    whisper.api_key = Some(api_key);

    // Prompt for endpoint (optional, with default)
    let endpoint_default = whisper
        .api_endpoint
        .clone()
        .unwrap_or_else(|| "https://api.yourprovider.com/v1/transcribe".to_string());
    whisper.api_endpoint = Some(prompt_string_with_default(
        theme,
        "API endpoint",
        &endpoint_default,
    )?);

    // Prompt for model (optional, with default)
    let model_default = whisper.model.clone().unwrap_or_else(|| "default".to_string());
    whisper.model = Some(prompt_string_with_default(
        theme,
        "Model (e.g., default, premium)",
        &model_default,
    )?);

    // Prompt for language
    prompt_language_choice(theme, whisper, "en")?;

    Ok(())
}
```

## 3.7 Add to handle_status Display (Optional)

Find `handle_status()` and add provider-specific status display:

```rust
match provider.as_str() {
    "audetic-api" => {
        // existing...
    }
    // ... other providers ...

    "your-provider" => {
        println!("API Key:   {}", mask_secret(&whisper.api_key));
        println!("Endpoint:  {}", whisper.api_endpoint.as_deref().unwrap_or("<default>"));
    }
    _ => {}
}
```

## Test the CLI

```bash
# Build
cargo build

# Test the wizard
./target/debug/audetic provider configure

# Check status
./target/debug/audetic provider status
```

## Checklist

- [ ] Added variant to `ProviderSelection` enum
- [ ] Added to `OPTIONS` array
- [ ] Added `as_str()` mapping
- [ ] Added `from_index()` mapping
- [ ] Added match arm in `handle_configure()`
- [ ] Created `configure_your_provider()` function
- [ ] Added status display in `handle_status()` (optional)
- [ ] Tested with `audetic provider configure`

## Next Step

[Step 4: Update Documentation →](./04-update-docs.md)
