//! Persisted capture preferences use the shared Config format and an explicit
//! file path. Tests never redirect process-global HOME/config directories.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::Path;

use crate::config::Config;

pub fn load_config(path: &Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(content) => toml::from_str(&content).context("Failed to parse Audetic configuration"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(error).context("Failed to read Audetic configuration"),
    }
}

/// Preserve all other settings and atomically replace the configuration so
/// concurrent readers cannot observe a half-written TOML document.
pub fn save_auto_paste(path: &Path, auto_paste: bool) -> Result<()> {
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
