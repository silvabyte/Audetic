use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

const APP_DIR: &str = "audetic";

pub fn config_dir() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|dir| dir.join(APP_DIR))
        .context("Unable to determine config directory")
}

pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = dirs::data_dir() {
        return Ok(dir.join(APP_DIR));
    }
    if let Some(home) = dirs::home_dir() {
        return Ok(home.join(".local").join("share").join(APP_DIR));
    }
    Err(anyhow!("Unable to determine data directory"))
}

pub fn updates_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join("updates"))
}

pub fn update_state_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("update_state.json"))
}

pub fn update_lock_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("update.lock"))
}

pub fn db_file() -> Result<PathBuf> {
    Ok(data_dir()?.join("audetic.db"))
}
