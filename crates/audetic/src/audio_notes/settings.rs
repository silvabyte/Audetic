//! Persisted capture preferences use the shared Config format and an explicit
//! file path. Tests never redirect process-global HOME/config directories.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

use super::AudioNoteCaptureSource;
use crate::config::{AutoPastePreferences, Config};

pub fn load_config(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(content) => toml::from_str(&content).context("Failed to parse Audetic configuration"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(error).context("Failed to read Audetic configuration"),
    }
}

/// Saved automatic paste for this capture source.
///
/// A new `AudioNoteCaptureSource` variant must read its own preference field.
/// That field defaults to off when a saved table does not mention it.
pub fn auto_paste_enabled(
    preferences: AutoPastePreferences,
    source: AudioNoteCaptureSource,
) -> bool {
    match source {
        AudioNoteCaptureSource::Microphone => preferences.microphone,
        AudioNoteCaptureSource::MicrophoneAndSystem => preferences.microphone_and_system,
    }
}

/// Preserve all other settings and atomically replace the configuration so
/// concurrent readers cannot observe a half-written TOML document.
pub fn save_auto_paste(path: &Path, auto_paste: AutoPastePreferences) -> Result<()> {
    let mut config = load_config(path)?;
    config.behavior.auto_paste = auto_paste;
    let parent = path.parent().context("Configuration path has no parent")?;
    std::fs::create_dir_all(parent).context("Failed to create configuration directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(toml::to_string_pretty(&config)?.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .context("Failed to save Audetic configuration")?;
    Ok(())
}
