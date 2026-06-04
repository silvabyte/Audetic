//! CLI handler for transcription provider management.
//!
//! The interactive wizard runs locally, but all reads/writes/tests go through
//! the daemon's REST API (`GET`/`PUT /api/provider/config`,
//! `POST /api/provider/reset`, `POST /api/provider/test`,
//! `GET /api/provider/status`). The daemon owns `config.toml` (and its backups),
//! so there is a single writer.

use crate::args::{ProviderCliArgs, ProviderCommand};
use crate::client::{base_url, json_or_error, CONNECT_HINT};
use anyhow::{Context, Result};
use audetic_core::config::WhisperConfig;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};
use serde_json::json;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
use which::which;

pub async fn handle_provider_command(args: ProviderCliArgs) -> Result<()> {
    match args.command {
        Some(ProviderCommand::Show) => handle_show().await,
        Some(ProviderCommand::Configure { dry_run }) => handle_configure(dry_run).await,
        Some(ProviderCommand::Test { file }) => handle_test(file).await,
        Some(ProviderCommand::Status) => handle_status().await,
        Some(ProviderCommand::Reset { force }) => handle_reset(force).await,
        None => handle_interactive().await,
    }
}

// ============================================================================
// REST helpers
// ============================================================================

/// Fetch the raw provider config from the daemon.
async fn fetch_config() -> Result<WhisperConfig> {
    let response = reqwest::Client::new()
        .get(format!("{}/provider/config", base_url()))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "get provider config").await?;
    serde_json::from_value(body).context("Failed to parse provider config")
}

/// Persist a provider config via the daemon (it backs up `config.toml` first).
async fn save_config(whisper: &WhisperConfig) -> Result<()> {
    let response = reqwest::Client::new()
        .put(format!("{}/provider/config", base_url()))
        .json(whisper)
        .send()
        .await
        .context(CONNECT_HINT)?;
    json_or_error(response, "save provider config").await?;
    Ok(())
}

// ============================================================================
// Commands
// ============================================================================

async fn handle_interactive() -> Result<()> {
    if !io::stdin().is_terminal() {
        eprintln!("Non-interactive session. Use 'audetic provider configure' for automated setup.");
        return Ok(());
    }

    let theme = ColorfulTheme::default();

    println!();
    println!("Audetic Provider Setup");
    println!("======================");
    println!();

    let whisper = fetch_config().await?;
    println!(
        "Current provider: {}",
        whisper.provider.as_deref().unwrap_or("<not set>")
    );
    println!();

    let options = vec![
        "Configure provider",
        "Test current provider",
        "Show full configuration",
        "Reset to defaults",
        "Exit",
    ];

    let selection = Select::with_theme(&theme)
        .with_prompt("What would you like to do?")
        .items(&options)
        .default(0)
        .interact()?;

    match selection {
        0 => handle_configure(false).await,
        1 => handle_test(None).await,
        2 => handle_show().await,
        3 => handle_reset(false).await,
        _ => {
            println!("Exiting provider setup.");
            Ok(())
        }
    }
}

async fn handle_show() -> Result<()> {
    let whisper = fetch_config().await?;

    println!();
    println!("Provider Configuration");
    println!("======================");
    println!();
    println!(
        "Provider:     {}",
        whisper.provider.as_deref().unwrap_or("<not set>")
    );
    println!(
        "Model:        {}",
        whisper.model.as_deref().unwrap_or("<default>")
    );
    println!(
        "Language:     {}",
        whisper.language.as_deref().unwrap_or("<default>")
    );
    println!();
    println!("API Settings:");
    println!("  Key:        {}", mask_secret(&whisper.api_key));
    println!("  Endpoint:   {}", display_value(&whisper.api_endpoint));
    println!();
    println!("Local Binary Settings:");
    println!("  Command:    {}", display_value(&whisper.command_path));
    println!("  Model Path: {}", display_value(&whisper.model_path));

    Ok(())
}

async fn handle_configure(dry_run: bool) -> Result<()> {
    if !io::stdin().is_terminal() {
        eprintln!("Non-interactive session detected. Run `audetic provider configure` from a terminal to change providers.");
        return Ok(());
    }

    let theme = ColorfulTheme::default();
    let mut whisper = fetch_config().await?;
    let old_config = whisper.clone();

    println!();
    println!("Provider Configuration");
    println!("======================");
    println!();
    println!(
        "Current provider: {}",
        whisper.provider.as_deref().unwrap_or("<not set>")
    );
    println!();

    let selection = prompt_provider_selection(&theme, whisper.provider.as_deref())?;
    whisper.provider = Some(selection.as_str().to_string());

    match selection {
        ProviderSelection::AudeticApi => configure_audetic_api(&theme, &mut whisper)?,
        ProviderSelection::AssemblyAi => configure_assembly_ai(&theme, &mut whisper)?,
        ProviderSelection::OpenAiApi => configure_openai_api(&theme, &mut whisper)?,
        ProviderSelection::OpenAiCli => configure_openai_cli(&theme, &mut whisper)?,
        ProviderSelection::WhisperCpp => configure_whisper_cpp(&theme, &mut whisper)?,
    }

    println!();
    println!("Configuration Changes");
    println!("---------------------");
    print_config_diff(&old_config, &whisper);

    if dry_run {
        println!();
        println!("Dry run mode - no changes saved.");
        println!("Remove --dry-run to apply these changes.");
        return Ok(());
    }

    println!();
    let proceed = Confirm::with_theme(&theme)
        .with_prompt("Save these changes?")
        .default(true)
        .interact()?;
    if !proceed {
        println!("Configuration cancelled.");
        return Ok(());
    }

    save_config(&whisper).await?;
    println!();
    println!(
        "Provider updated to '{}'.",
        whisper.provider.as_deref().unwrap_or_default()
    );
    println!();
    println!("Next steps:");
    println!("  audetic provider test    - Verify the provider works");
    println!("  Restart the Audetic daemon to apply changes to the running service");

    Ok(())
}

async fn handle_test(file: Option<String>) -> Result<()> {
    println!();
    println!("Provider Test");
    println!("=============");
    println!();

    if let Some(f) = &file {
        println!("Audio file: {f}");
    }
    print!("Testing... ");

    let response = reqwest::Client::new()
        .post(format!("{}/provider/test", base_url()))
        .json(&json!({ "file": file }))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "test provider").await?;

    let success = body
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    println!("{}", if success { "OK" } else { "failed" });
    println!();

    if let Some(err) = body.get("error").and_then(|v| v.as_str()) {
        if !err.is_empty() {
            println!("Error: {err}");
            return Ok(());
        }
    }

    if let Some(text) = body.get("transcription").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            let duration = body
                .get("duration_secs")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            println!("Result ({duration:.2}s):");
            println!("  \"{text}\"");
            println!();
        }
    }

    if success {
        println!("Provider is working correctly.");
    }
    Ok(())
}

async fn handle_status() -> Result<()> {
    let response = reqwest::Client::new()
        .get(format!("{}/provider/status", base_url()))
        .send()
        .await
        .context(CONNECT_HINT)?;
    let body = json_or_error(response, "get provider status").await?;

    println!();
    println!("Audetic Provider Status");
    println!("=======================");
    println!();

    match body.get("status").and_then(|v| v.as_str()) {
        Some("ready") => {
            println!("Status: READY");
            println!();
            if let Some(p) = body.get("provider").and_then(|v| v.as_str()) {
                println!("Provider:  {p}");
            }
            println!(
                "Model:     {}",
                body.get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<default>")
            );
            println!(
                "Language:  {}",
                body.get("language")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<default>")
            );
            println!();
            println!("Health: Ready for transcription");
        }
        Some("config_error") => {
            println!("Status: CONFIGURATION ERROR");
            println!();
            if let Some(p) = body.get("provider").and_then(|v| v.as_str()) {
                println!("Provider:  {p}");
            }
            if let Some(e) = body.get("error").and_then(|v| v.as_str()) {
                println!();
                println!("Error: {e}");
            }
            println!();
            println!("Run 'audetic provider configure' to fix the configuration.");
        }
        _ => {
            println!("Status: NOT CONFIGURED");
            println!();
            println!("No transcription provider has been set up.");
            println!();
            println!("Run 'audetic provider' to configure a provider.");
        }
    }
    Ok(())
}

async fn handle_reset(force: bool) -> Result<()> {
    let whisper = fetch_config().await?;
    let current_provider = whisper.provider.as_deref().unwrap_or("<not set>");

    println!();
    println!("Reset Provider Configuration");
    println!("============================");
    println!();
    println!("Current provider: {current_provider}");
    println!();
    println!("This will reset to:");
    println!("  Provider: audetic-api (default)");
    println!("  Model:    base");
    println!("  Language: en");
    println!("  All API keys and custom paths will be cleared.");
    println!();

    if !force {
        if !io::stdin().is_terminal() {
            println!("Non-interactive session. Use --force to reset without confirmation.");
            return Ok(());
        }
        let theme = ColorfulTheme::default();
        let proceed = Confirm::with_theme(&theme)
            .with_prompt("Proceed with reset?")
            .default(false)
            .interact()?;
        if !proceed {
            println!("Reset cancelled.");
            return Ok(());
        }
    }

    let response = reqwest::Client::new()
        .post(format!("{}/provider/reset", base_url()))
        .send()
        .await
        .context(CONNECT_HINT)?;
    json_or_error(response, "reset provider config").await?;

    println!();
    println!("Provider configuration reset to defaults.");
    println!();
    println!("Next steps:");
    println!("  audetic provider           - Configure a new provider");
    println!("  Restart the Audetic daemon to apply changes");

    Ok(())
}

// ============================================================================
// Configuration diff display
// ============================================================================

fn print_config_diff(old: &WhisperConfig, new: &WhisperConfig) {
    print_field_diff("Provider", &old.provider, &new.provider);
    print_field_diff("Model", &old.model, &new.model);
    print_field_diff("Language", &old.language, &new.language);
    print_field_diff("API Endpoint", &old.api_endpoint, &new.api_endpoint);
    print_secret_diff("API Key", &old.api_key, &new.api_key);
    print_field_diff("Command Path", &old.command_path, &new.command_path);
    print_field_diff("Model Path", &old.model_path, &new.model_path);
}

fn print_field_diff(name: &str, old: &Option<String>, new: &Option<String>) {
    if old != new {
        let old_display = old.as_deref().unwrap_or("<not set>");
        let new_display = new.as_deref().unwrap_or("<not set>");
        println!("  {name}: {old_display} -> {new_display}");
    }
}

fn print_secret_diff(name: &str, old: &Option<String>, new: &Option<String>) {
    if old != new {
        println!("  {name}: {} -> {}", mask_secret(old), mask_secret(new));
    }
}

// ============================================================================
// Provider configuration wizards
// ============================================================================

fn configure_audetic_api(theme: &ColorfulTheme, whisper: &mut WhisperConfig) -> Result<()> {
    whisper.command_path = None;
    whisper.model_path = None;
    whisper.api_key = None;

    let endpoint_default = whisper
        .api_endpoint
        .clone()
        .unwrap_or_else(|| "https://audio.audetic.link/api/v1/transcriptions".to_string());
    whisper.api_endpoint = Some(prompt_string_with_default(
        theme,
        "API endpoint",
        &endpoint_default,
    )?);

    let model_default = whisper.model.clone().unwrap_or_else(|| "base".to_string());
    whisper.model = Some(prompt_string_with_default(
        theme,
        "Model (base, small, medium, large-v3, ...)",
        &model_default,
    )?);

    prompt_language_choice(theme, whisper, "en")?;
    Ok(())
}

fn configure_assembly_ai(theme: &ColorfulTheme, whisper: &mut WhisperConfig) -> Result<()> {
    whisper.command_path = None;
    whisper.model_path = None;

    let api_key = prompt_secret(theme, "AssemblyAI API key", whisper.api_key.as_ref())?;
    whisper.api_key = Some(api_key);

    let endpoint_default = whisper
        .api_endpoint
        .clone()
        .unwrap_or_else(|| "https://api.assemblyai.com/v2".to_string());
    whisper.api_endpoint = Some(prompt_string_with_default(
        theme,
        "API base URL",
        &endpoint_default,
    )?);

    whisper.model = None;
    prompt_language_choice(theme, whisper, "en")?;
    Ok(())
}

fn configure_openai_api(theme: &ColorfulTheme, whisper: &mut WhisperConfig) -> Result<()> {
    whisper.command_path = None;
    whisper.model_path = None;

    let api_key = prompt_secret(theme, "OpenAI API key (sk-...)", whisper.api_key.as_ref())?;
    whisper.api_key = Some(api_key);

    let endpoint_default = whisper
        .api_endpoint
        .clone()
        .unwrap_or_else(|| "https://api.openai.com/v1/audio/transcriptions".to_string());
    whisper.api_endpoint = Some(prompt_string_with_default(
        theme,
        "API endpoint",
        &endpoint_default,
    )?);

    let model_default = whisper
        .model
        .clone()
        .unwrap_or_else(|| "whisper-1".to_string());
    whisper.model = Some(prompt_string_with_default(
        theme,
        "Model (whisper-1)",
        &model_default,
    )?);

    prompt_language_choice(theme, whisper, "en")?;
    Ok(())
}

fn configure_openai_cli(theme: &ColorfulTheme, whisper: &mut WhisperConfig) -> Result<()> {
    whisper.api_key = None;
    whisper.api_endpoint = None;
    whisper.model_path = None;

    let default_path = whisper
        .command_path
        .clone()
        .or_else(|| detect_default_binary("whisper"));
    whisper.command_path = Some(prompt_required_path(
        theme,
        "Path to `whisper` CLI binary",
        default_path,
        true,
    )?);

    let model_default = whisper.model.clone().unwrap_or_else(|| "base".to_string());
    whisper.model = Some(prompt_string_with_default(
        theme,
        "Model (tiny, base, small, medium, large-v3, ...)",
        &model_default,
    )?);

    prompt_language_choice(theme, whisper, "en")?;
    Ok(())
}

fn configure_whisper_cpp(theme: &ColorfulTheme, whisper: &mut WhisperConfig) -> Result<()> {
    whisper.api_key = None;
    whisper.api_endpoint = None;

    let command_default = whisper.command_path.clone();
    whisper.command_path = Some(prompt_required_path(
        theme,
        "Path to whisper.cpp binary",
        command_default,
        true,
    )?);

    let model_path_default = whisper.model_path.clone();
    whisper.model_path = Some(prompt_required_path(
        theme,
        "Path to GGML/GGUF model file",
        model_path_default,
        true,
    )?);

    let model_default = whisper.model.clone().unwrap_or_else(|| "base".to_string());
    whisper.model = Some(prompt_string_with_default(
        theme,
        "Model size label (tiny, base, small, medium, large)",
        &model_default,
    )?);

    prompt_language_choice(theme, whisper, "en")?;
    Ok(())
}

// ============================================================================
// Input prompt helpers
// ============================================================================

fn prompt_provider_selection(
    theme: &ColorfulTheme,
    current: Option<&str>,
) -> Result<ProviderSelection> {
    const OPTIONS: &[(&str, &str)] = &[
        (
            "audetic-api",
            "Audetic Cloud API (default, no setup required)",
        ),
        ("assembly-ai", "AssemblyAI API (requires API key)"),
        ("openai-api", "OpenAI Whisper API (requires API key)"),
        (
            "openai-cli",
            "Local OpenAI Whisper CLI (requires local install)",
        ),
        (
            "whisper-cpp",
            "Local whisper.cpp binary (requires local install)",
        ),
    ];

    let items: Vec<String> = OPTIONS
        .iter()
        .map(|(name, desc)| format!("{name:<12} - {desc}"))
        .collect();

    let default_index = current
        .and_then(|value| OPTIONS.iter().position(|(name, _)| *name == value))
        .unwrap_or(0);

    let selection = Select::with_theme(theme)
        .with_prompt("Select a transcription provider")
        .items(&items)
        .default(default_index)
        .interact()?;

    Ok(ProviderSelection::from_index(selection))
}

fn prompt_secret(theme: &ColorfulTheme, prompt: &str, current: Option<&String>) -> Result<String> {
    if let Some(existing) = current {
        let keep = Confirm::with_theme(theme)
            .with_prompt(format!("Keep existing {prompt}?"))
            .default(true)
            .interact()?;
        if keep {
            return Ok(existing.clone());
        }
    }

    loop {
        let value = Password::new().with_prompt(prompt).interact()?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            println!("{prompt} cannot be empty.");
            continue;
        }
        return Ok(trimmed.to_string());
    }
}

fn prompt_string_with_default(theme: &ColorfulTheme, label: &str, current: &str) -> Result<String> {
    let value: String = Input::with_theme(theme)
        .with_prompt(format!("{label} [{current}]"))
        .allow_empty(true)
        .interact_text()?;

    let trimmed = value.trim();
    if trimmed.is_empty() {
        Ok(current.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn prompt_language_choice(
    theme: &ColorfulTheme,
    whisper: &mut WhisperConfig,
    fallback: &str,
) -> Result<()> {
    let current = whisper
        .language
        .clone()
        .unwrap_or_else(|| fallback.to_string());

    let value: String = Input::with_theme(theme)
        .with_prompt(format!(
            "Language code (ISO 639-1, e.g. en, es, auto) [{current}]"
        ))
        .allow_empty(true)
        .interact_text()?;

    let trimmed = value.trim();
    whisper.language = Some(if trimmed.is_empty() {
        current
    } else {
        trimmed.to_string()
    });
    Ok(())
}

fn prompt_required_path(
    theme: &ColorfulTheme,
    label: &str,
    default: Option<String>,
    require_file: bool,
) -> Result<String> {
    loop {
        let prompt = match &default {
            Some(value) => format!("{label} [{value}]"),
            None => label.to_string(),
        };

        let value: String = Input::with_theme(theme)
            .with_prompt(prompt)
            .allow_empty(default.is_some())
            .interact_text()?;

        let candidate = if value.trim().is_empty() {
            match &default {
                Some(def) => def.clone(),
                None => {
                    println!("Value cannot be empty.");
                    continue;
                }
            }
        } else {
            value.trim().to_string()
        };

        if validate_path(&candidate, require_file) {
            return Ok(candidate);
        }
        println!("Path '{candidate}' does not exist or is not accessible. Please try again.");
    }
}

fn validate_path(path: &str, require_file: bool) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => {
            if require_file {
                metadata.is_file()
            } else {
                true
            }
        }
        Err(_) => Path::new(path).exists(),
    }
}

fn detect_default_binary(program: &str) -> Option<String> {
    which(program)
        .ok()
        .map(|path| path.to_string_lossy().to_string())
}

// ============================================================================
// Display helpers
// ============================================================================

fn display_value(value: &Option<String>) -> String {
    value
        .as_deref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| "<not set>".to_string())
}

fn mask_secret(value: &Option<String>) -> String {
    match value {
        Some(secret) if secret.len() > 8 => {
            let prefix = &secret[..4];
            let suffix = &secret[secret.len() - 2..];
            format!("{prefix}****{suffix}")
        }
        Some(secret) if !secret.is_empty() => "*".repeat(secret.len()),
        _ => "<not set>".to_string(),
    }
}

// ============================================================================
// Provider selection enum
// ============================================================================

#[derive(Debug, Clone, Copy)]
enum ProviderSelection {
    AudeticApi,
    AssemblyAi,
    OpenAiApi,
    OpenAiCli,
    WhisperCpp,
}

impl ProviderSelection {
    fn as_str(&self) -> &'static str {
        match self {
            ProviderSelection::AudeticApi => "audetic-api",
            ProviderSelection::AssemblyAi => "assembly-ai",
            ProviderSelection::OpenAiApi => "openai-api",
            ProviderSelection::OpenAiCli => "openai-cli",
            ProviderSelection::WhisperCpp => "whisper-cpp",
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            0 => ProviderSelection::AudeticApi,
            1 => ProviderSelection::AssemblyAi,
            2 => ProviderSelection::OpenAiApi,
            3 => ProviderSelection::OpenAiCli,
            _ => ProviderSelection::WhisperCpp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_secret() {
        assert_eq!(mask_secret(&None), "<not set>");
        assert_eq!(mask_secret(&Some("".to_string())), "<not set>");
        assert_eq!(mask_secret(&Some("short".to_string())), "*****");
        assert_eq!(
            mask_secret(&Some("sk-1234567890abcdef".to_string())),
            "sk-1****ef"
        );
    }
}
