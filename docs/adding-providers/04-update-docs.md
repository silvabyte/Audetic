# Step 4: Update Documentation

Help users discover and configure your provider.

## 4.1 Update example_config.toml

Add a commented example to `example_config.toml`:

```toml
# =============================================================================
# YourProvider API
# =============================================================================
# provider = "your-provider"
# api_key = "your-api-key-here"
# api_endpoint = "https://api.yourprovider.com/v1/transcribe"  # optional
# model = "default"  # optional
# language = "en"
```

## 4.2 Update docs/configuration.md

Add your provider to the provider list:

````markdown
## Available Providers

### YourProvider API (`your-provider`)

Cloud-based transcription service.

**Requirements:**

- API key from [yourprovider.com](https://yourprovider.com)
- Internet connection

**Configuration:**

```toml
[whisper]
provider = "your-provider"
api_key = "your-api-key"
model = "default"        # optional: default, premium, etc.
language = "en"          # optional: ISO 639-1 code or "auto"
```
````

**Pricing:** ~$X.XX per minute of audio

````

## 4.3 Update the Provider List in README (if applicable)

If the main README lists supported providers, add yours:

```markdown
## Supported Providers

- **Audetic API** - Zero-config cloud transcription (default)
- **OpenAI API** - OpenAI's Whisper API
- **AssemblyAI** - AssemblyAI transcription service
- **YourProvider** - Your provider description  <!-- Add this -->
- **whisper.cpp** - Local transcription with whisper.cpp
- **OpenAI CLI** - Local OpenAI Whisper CLI
````

## Final Testing

Run through the complete flow:

```bash
# 1. Build
cargo build

# 2. Configure your provider
./target/debug/audetic provider configure
# Select your provider, enter credentials

# 3. Check status
./target/debug/audetic provider status
# Should show "READY"

# 4. Test with audio (if you have a test file)
./target/debug/audetic provider test --file /path/to/test.wav

# 5. Run all tests
cargo test

# 6. Run lints
cargo clippy --all-targets --all-features -- -D warnings
```

## Checklist

- [ ] Added example to `example_config.toml`
- [ ] Added section to `docs/configuration.md`
- [ ] Updated README provider list (if applicable)
- [ ] Full flow tested: configure → status → test
- [ ] `cargo test` passes
- [ ] `cargo clippy` passes

## You're Done!

Your provider is ready. Consider:

1. **Opening a PR** if contributing upstream
2. **Adding integration tests** with real API calls (gated behind a feature flag)
3. **Documenting edge cases** you discovered

## Submitting a PR

If contributing to Audetic:

```bash
# Create feature branch
git checkout -b add-your-provider

# Commit your changes
git add -A
git commit -m "feat: add YourProvider transcription support"

# Push and create PR
git push -u origin add-your-provider
```

Include in your PR description:

- What the provider does
- Link to provider's API docs
- Any special setup requirements
- Test results
