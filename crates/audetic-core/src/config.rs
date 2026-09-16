use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::info;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::global;

static CONFIG_UPDATE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub whisper: WhisperConfig,
    pub ui: UiConfig,
    pub wayland: WaylandConfig,
    pub behavior: BehaviorConfig,
    pub sync: SyncConfig,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SyncRole {
    #[default]
    Standalone,
    Hub,
    Client,
}

impl SyncRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Hub => "hub",
            Self::Client => "client",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct SyncConfig {
    pub role: SyncRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct WhisperConfig {
    pub model: Option<String>,
    pub language: Option<String>,
    pub command_path: Option<String>,
    pub model_path: Option<String>,
    pub api_endpoint: Option<String>,
    pub provider: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub notification_color: String,
    pub waybar: WaybarConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WaybarConfig {
    pub idle_text: String,
    pub recording_text: String,
    pub idle_tooltip: String,
    pub recording_tooltip: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct WaylandConfig {
    pub input_method: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    pub auto_paste: bool,
    pub preserve_clipboard: bool,
    pub delete_audio_files: bool,
    #[serde(default = "default_audio_feedback")]
    pub audio_feedback: bool,
}

fn default_audio_feedback() -> bool {
    true
}

impl Default for WhisperConfig {
    fn default() -> Self {
        Self {
            model: Some("base".to_string()),
            language: Some("en".to_string()),
            command_path: None,
            model_path: None,
            api_endpoint: None,
            provider: Some("audetic-api".to_string()),
            api_key: None,
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            notification_color: "rgb(ff1744)".to_string(),
            waybar: WaybarConfig::default(),
        }
    }
}

impl Default for WaybarConfig {
    fn default() -> Self {
        Self {
            idle_text: "󰑊".to_string(),      // Nerd Font circle with dot (idle)
            recording_text: "󰻃".to_string(), // Nerd Font record button (recording)
            idle_tooltip: "Press Super+R to record".to_string(),
            recording_tooltip: "Recording... Press Super+R to stop".to_string(),
        }
    }
}

impl Default for WaylandConfig {
    fn default() -> Self {
        Self {
            input_method: "wtype".to_string(),
        }
    }
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            auto_paste: true,
            preserve_clipboard: false,
            delete_audio_files: true,
            audio_feedback: true,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;
        Self::load_from(&config_path)
    }

    pub fn load_from(config_path: &Path) -> Result<Self> {
        if !config_path.exists() {
            info!(
                "Config file not found, creating default at {:?}",
                config_path
            );
            let config = Self::default();
            config.save_to(config_path)?;
            return Ok(config);
        }

        let content = std::fs::read_to_string(config_path).context("Failed to read config file")?;

        let config: Self = toml::from_str(&content).context("Failed to parse config file")?;

        info!("Loaded config from {:?}", config_path);
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;
        self.save_to(&config_path)
    }

    pub fn save_to(&self, config_path: &Path) -> Result<()> {
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create config directory")?;
        }

        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        std::fs::write(config_path, content).context("Failed to write config file")?;

        Ok(())
    }

    pub fn update<R>(mutator: impl FnOnce(&mut Self) -> R) -> Result<R> {
        let config_path = Self::config_path()?;
        Self::update_at(&config_path, mutator)
    }

    pub fn update_at<R>(config_path: &Path, mutator: impl FnOnce(&mut Self) -> R) -> Result<R> {
        let _guard = CONFIG_UPDATE_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("Config update lock is poisoned"))?;
        let mut config = Self::load_from(config_path)?;
        let result = mutator(&mut config);
        config.save_to(config_path)?;
        Ok(result)
    }

    fn config_path() -> Result<PathBuf> {
        global::config_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_config_defaults_to_standalone_sync() {
        let config: Config = toml::from_str("").unwrap();

        assert_eq!(config.sync, SyncConfig::default());
        assert_eq!(config.sync.role, SyncRole::Standalone);
    }

    #[test]
    fn sync_roles_have_stable_serialized_values() {
        for (role, expected) in [
            (SyncRole::Standalone, "standalone"),
            (SyncRole::Hub, "hub"),
            (SyncRole::Client, "client"),
        ] {
            let serialized = toml::to_string(&SyncConfig { role }).unwrap();
            assert_eq!(serialized, format!("role = \"{expected}\"\n"));

            let parsed: SyncConfig = toml::from_str(&serialized).unwrap();
            assert_eq!(parsed.role, role);
            assert_eq!(role.as_str(), expected);
        }
    }
}
