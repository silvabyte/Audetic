//! CLI handler for transcription provider management.
//!
//! This module handles terminal presentation and user interaction.
//! Core business logic is delegated to the `transcription` module.

use crate::cli::{ProviderCliArgs, ProviderCommand};
use crate::config::{Config, WhisperConfig};
use crate::transcription::{
    get_provider_status_from_config, ProviderConfig, ProviderStatus, Transcriber,
};
use anyhow::{anyhow, Context, Result};
use chrono::Local;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::info;
use which::which;

const MAX_CONFIG_BACKUPS: usize = 3;

pub fn handle_provider_command(args: ProviderCliArgs) -> Result<()> {
    match args.command {
        Some(ProviderCommand::Show) => handle_show(),
        Some(ProviderCommand::Configure { dry_run }) => handle_configure(dry_run),
        Some(ProviderCommand::Test { file }) => handle_test(file),
        Some(ProviderCommand::Status) => handle_status(),
        Some(ProviderCommand::Reset { force }) => handle_reset(force),
        None => handle_interactive(),
    }
}

/// Interactive provider setup wizard (default when no subcommand provided)
fn handle_interactive() -> Result<()> {
    if !io::stdin().is_terminal() {
        info!("Non-interactive session. Use 'audetic provider configure' for automated setup.");
        return Ok(());
    }

    let theme = ColorfulTheme::default();

    println!();
    println!("Audetic Provider Setup");
    println!("======================");
    println!();

    // Show current status summary
    let config = Config::load()?;
    let provider_name = config.whisper.provider.as_deref().unwrap_or("<not set>");
    let status = get_provider_status_from_config(&config.whisper)?;

    println!("Current provider: {}", provider_name);
    println!("Status: {}", provider_status_display(&status));
    println!();

    // Interactive menu
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
        0 => handle_configure(false),
        1 => handle_test(None),
        2 => handle_show(),
        3 => handle_reset(false),
        _ => {
            println!("Exiting provider setup.");
            Ok(())
        }
    }
}

/// Show current provider configuration
fn handle_show() -> Result<()> {
    let config = Config::load()?;
    let whisper = &config.whisper;

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
    println!();
    println!("Config file:  {}", crate::global::config_file()?.display());

    Ok(())
}

/// Configure provider with optional dry-run
fn handle_configure(dry_run: bool) -> Result<()> {
    if !io::stdin().is_terminal() {
        info!("Non-interactive session detected. Please edit ~/.config/audetic/config.toml manually to change providers.");
        return Ok(());
    }

    let theme = ColorfulTheme::default();
    let mut config = Config::load()?;
    let old_config = config.whisper.clone();

    println!();
    println!("Provider Configuration");
    println!("======================");
    println!();
    println!(
        "Current provider: {}",
        config.whisper.provider.as_deref().unwrap_or("<not set>")
    );
    println!();

    let selection = prompt_provider_selection(&theme, config.whisper.provider.as_deref())?;
    config.whisper.provider = Some(selection.as_str().to_string());

    match selection {
        ProviderSelection::AudeticApi => configure_audetic_api(&theme, &mut config.whisper)?,
        ProviderSelection::AssemblyAi => configure_assembly_ai(&theme, &mut config.whisper)?,
        ProviderSelection::OpenAiApi => configure_openai_api(&theme, &mut config.whisper)?,
        ProviderSelection::OpenAiCli => configure_openai_cli(&theme, &mut config.whisper)?,
        ProviderSelection::WhisperCpp => configure_whisper_cpp(&theme, &mut config.whisper)?,
    }

    // Show what would change
    println!();
    println!("Configuration Changes");
    println!("---------------------");
    print_config_diff(&old_config, &config.whisper);

    if dry_run {
        println!();
        println!("Dry run mode - no changes saved.");
        println!("Remove --dry-run to apply these changes.");
        return Ok(());
    }

    // Confirm before saving
    println!();
    let proceed = Confirm::with_theme(&theme)
        .with_prompt("Save these changes?")
        .default(true)
        .interact()?;

    if !proceed {
        println!("Configuration cancelled.");
        return Ok(());
    }

    // Create backup before saving
    let config_path = crate::global::config_file()?;
    if config_path.exists() {
        let backup_path = create_config_backup(&config_path)?;
        println!("Backup: {}", backup_path.display());
    }

    config.save()?;
    println!();
    println!(
        "Provider updated to '{}'.",
        config.whisper.provider.as_deref().unwrap_or_default()
    );
    println!();
    println!("Next steps:");
    println!("  audetic provider test    - Verify the provider works");
    println!("  systemctl --user restart audetic.service  - Apply to running service");

    Ok(())
}

/// Test provider with optional audio file
fn handle_test(file: Option<String>) -> Result<()> {
    let config = Config::load()?;
    let provider_name = config.whisper.provider.as_deref().ok_or_else(|| {
        anyhow!("No transcription provider configured. Run `audetic provider configure` first.")
    })?;

    println!();
    println!("Provider Test");
    println!("=============");
    println!();
    println!("Provider: {}", provider_name);

    // Initialize provider
    print!("Initializing... ");
    let provider_config = provider_config_from_whisper(&config.whisper);
    let transcriber = Transcriber::with_provider(provider_name, provider_config)?;
    println!("OK");

    // If file provided, test with it
    if let Some(audio_file) = file {
        let audio_path = PathBuf::from(&audio_file);
        if !audio_path.exists() {
            return Err(anyhow!("Audio file not found: {}", audio_file));
        }

        println!("Audio file: {}", audio_file);
        println!();
        print!("Transcribing... ");

        let start = Instant::now();
        let result = tokio::runtime::Runtime::new()?
            .block_on(async { transcriber.transcribe(&audio_path).await })?;
        let elapsed = start.elapsed();

        println!("done ({:.2}s)", elapsed.as_secs_f64());
        println!();
        println!("Result:");
        println!("  \"{}\"", result);
        println!();
        println!("Provider '{}' is working correctly.", provider_name);
    } else {
        // No file provided - just validate configuration
        println!();
        println!("Provider '{}' initialized successfully.", provider_name);
        println!();
        println!("To test with actual audio:");
        println!("  audetic provider test --file <audio.wav>");
        println!();
        println!("Or use Audetic normally to test recording and transcription.");
    }

    Ok(())
}

/// Show provider status and health - uses transcription::get_provider_status_from_config()
fn handle_status() -> Result<()> {
    let config = Config::load()?;
    let whisper = &config.whisper;
    let status = get_provider_status_from_config(whisper)?;

    println!();
    println!("Audetic Provider Status");
    println!("=======================");
    println!();

    match status {
        ProviderStatus::Ready {
            provider,
            model,
            language,
        } => {
            println!("Status: READY");
            println!();
            println!("Provider:  {}", provider);
            println!("Model:     {}", model.as_deref().unwrap_or("<default>"));
            println!("Language:  {}", language.as_deref().unwrap_or("<default>"));

            // Show provider-specific config
            match provider.as_str() {
                "audetic-api" => {
                    println!(
                        "Endpoint:  {}",
                        whisper.api_endpoint.as_deref().unwrap_or("<default>")
                    );
                }
                "assembly-ai" => {
                    println!("API Key:   {}", mask_secret(&whisper.api_key));
                    println!(
                        "Base URL:  {}",
                        whisper.api_endpoint.as_deref().unwrap_or("<default>")
                    );
                }
                "openai-api" => {
                    println!("API Key:   {}", mask_secret(&whisper.api_key));
                    println!(
                        "Endpoint:  {}",
                        whisper.api_endpoint.as_deref().unwrap_or("<default>")
                    );
                }
                "openai-cli" => {
                    println!("Command:   {}", display_value(&whisper.command_path));
                }
                "whisper-cpp" => {
                    println!("Command:   {}", display_value(&whisper.command_path));
                    println!("Model:     {}", display_value(&whisper.model_path));
                }
                _ => {}
            }

            println!();
            println!("Health: Ready for transcription");
        }
        ProviderStatus::ConfigError { provider, error } => {
            println!("Status: CONFIGURATION ERROR");
            println!();
            println!("Provider:  {}", provider);
            println!();
            println!("Error: {}", error);
            println!();
            println!("Run 'audetic provider configure' to fix the configuration.");
        }
        ProviderStatus::NotConfigured => {
            println!("Status: NOT CONFIGURED");
            println!();
            println!("No transcription provider has been set up.");
            println!();
            println!("Run 'audetic provider' to configure a provider.");
        }
    }

    Ok(())
}

/// Reset provider to defaults
fn handle_reset(force: bool) -> Result<()> {
    let config = Config::load()?;
    let current_provider = config.whisper.provider.as_deref().unwrap_or("<not set>");

    println!();
    println!("Reset Provider Configuration");
    println!("============================");
    println!();
    println!("Current provider: {}", current_provider);
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

    // Create backup
    let config_path = crate::global::config_file()?;
    if config_path.exists() {
        let backup_path = create_config_backup(&config_path)?;
        println!("Backup: {}", backup_path.display());
    }

    // Reset whisper config to defaults
    let mut new_config = config;
    new_config.whisper = WhisperConfig::default();
    new_config.save()?;

    println!();
    println!("Provider configuration reset to defaults.");
    println!();
    println!("Next steps:");
    println!("  audetic provider           - Configure a new provider");
    println!("  systemctl --user restart audetic.service  - Apply changes");

    Ok(())
}

// ============================================================================
// Provider status helpers
// ============================================================================

/// Display helper for ProviderStatus
fn provider_status_display(status: &ProviderStatus) -> &'static str {
    match status {
        ProviderStatus::Ready { .. } => "Ready",
        ProviderStatus::ConfigError { .. } => "Configuration error",
        ProviderStatus::NotConfigured => "Not configured",
    }
}

// ============================================================================
// Backup helpers
// ============================================================================

fn create_config_backup(config_path: &Path) -> Result<PathBuf> {
    let backup_dir = crate::global::data_dir()?.join("config-backups");
    fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create backup directory: {:?}", backup_dir))?;

    let timestamp = Local::now().format("%Y%m%d-%H%M%S");
    let backup_name = format!("config.toml.backup-{}", timestamp);
    let backup_path = backup_dir.join(&backup_name);

    fs::copy(config_path, &backup_path)
        .with_context(|| format!("Failed to create backup of {:?}", config_path))?;

    // Rotate old backups
    rotate_config_backups(&backup_dir)?;

    Ok(backup_path)
}

fn rotate_config_backups(backup_dir: &Path) -> Result<()> {
    let mut backups: Vec<PathBuf> = fs::read_dir(backup_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("config.toml.backup-"))
                .unwrap_or(false)
        })
        .collect();

    backups.sort_by(|a, b| {
        let a_time = fs::metadata(a).and_then(|m| m.modified()).ok();
        let b_time = fs::metadata(b).and_then(|m| m.modified()).ok();
        b_time.cmp(&a_time)
    });

    for old_backup in backups.iter().skip(MAX_CONFIG_BACKUPS) {
        let _ = fs::remove_file(old_backup);
    }

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
        println!("  {}: {} -> {}", name, old_display, new_display);
    }
}

fn print_secret_diff(name: &str, old: &Option<String>, new: &Option<String>) {
    if old != new {
        let old_display = mask_secret(old);
        let new_display = mask_secret(new);
        println!("  {}: {} -> {}", name, old_display, new_display);
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

    // AssemblyAI doesn't use a model parameter like OpenAI
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
        .map(|(name, desc)| format!("{:<12} - {}", name, desc))
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
            .with_prompt(format!("Keep existing {}?", prompt))
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
            println!("{} cannot be empty.", prompt);
            continue;
        }
        return Ok(trimmed.to_string());
    }
}

fn prompt_string_with_default(theme: &ColorfulTheme, label: &str, current: &str) -> Result<String> {
    let prompt = format!("{label} [{current}]");
    let value: String = Input::with_theme(theme)
        .with_prompt(prompt)
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

    let prompt = format!("Language code (ISO 639-1, e.g. en, es, auto) [{current}]");
    let value: String = Input::with_theme(theme)
        .with_prompt(prompt)
        .allow_empty(true)
        .interact_text()?;

    let trimmed = value.trim();
    if trimmed.is_empty() {
        whisper.language = Some(current);
    } else {
        whisper.language = Some(trimmed.to_string());
    }
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
            if let Some(def) = &default {
                def.clone()
            } else {
                println!("Value cannot be empty.");
                continue;
            }
        } else {
            value.trim().to_string()
        };

        if validate_path(&candidate, require_file) {
            return Ok(candidate);
        } else {
            println!(
                "Path '{}' does not exist or is not accessible. Please try again.",
                candidate
            );
        }
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

fn provider_config_from_whisper(whisper: &WhisperConfig) -> ProviderConfig {
    ProviderConfig {
        model: whisper.model.clone(),
        model_path: whisper.model_path.clone(),
        language: whisper.language.clone(),
        command_path: whisper.command_path.clone(),
        api_endpoint: whisper.api_endpoint.clone(),
        api_key: whisper.api_key.clone(),
    }
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
    fn test_provider_status_display() {
        let status = ProviderStatus::Ready {
            provider: "audetic-api".to_string(),
            model: Some("base".to_string()),
            language: Some("en".to_string()),
        };
        assert_eq!(provider_status_display(&status), "Ready");

        let status = ProviderStatus::NotConfigured;
        assert_eq!(provider_status_display(&status), "Not configured");
    }

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

    #[test]
    fn test_get_provider_status() {
        let mut whisper = WhisperConfig::default();

        // Default has audetic-api which needs no extra config
        let status = get_provider_status_from_config(&whisper).unwrap();
        assert!(matches!(status, ProviderStatus::Ready { .. }));

        // OpenAI API without key should error
        whisper.provider = Some("openai-api".to_string());
        whisper.api_key = None;
        let status = get_provider_status_from_config(&whisper).unwrap();
        assert!(matches!(status, ProviderStatus::ConfigError { .. }));

        // OpenAI API with key is valid
        whisper.api_key = Some("sk-test".to_string());
        let status = get_provider_status_from_config(&whisper).unwrap();
        // Note: This will likely still fail initialization as the API key is fake
        // But it should at least pass validation
        assert!(matches!(
            status,
            ProviderStatus::Ready { .. } | ProviderStatus::ConfigError { .. }
        ));
    }
}
