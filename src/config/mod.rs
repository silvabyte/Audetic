use crate::global;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub whisper: WhisperConfig,
    pub ui: UiConfig,
    pub wayland: WaylandConfig,
    pub behavior: BehaviorConfig,
    pub meeting: MeetingConfig,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MeetingConfig {
    /// Shell command to run after meeting transcription completes.
    /// Receives transcript text via stdin.
    /// Env vars: AUDETIC_MEETING_ID, AUDETIC_MEETING_TITLE,
    /// AUDETIC_AUDIO_PATH, AUDETIC_TRANSCRIPT_PATH, AUDETIC_DURATION_SECONDS
    pub post_command: String,
    /// Timeout in seconds for the post_command (default: 3600 = 1 hour)
    pub post_command_timeout_seconds: u64,
}

impl Default for MeetingConfig {
    fn default() -> Self {
        Self {
            post_command: String::new(),
            post_command_timeout_seconds: 3600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        if !config_path.exists() {
            info!(
                "Config file not found, creating default at {:?}",
                config_path
            );
            let config = Self::default();
            config.save()?;
            return Ok(config);
        }

        let content =
            std::fs::read_to_string(&config_path).context("Failed to read config file")?;

        let config: Self = toml::from_str(&content).context("Failed to parse config file")?;

        info!("Loaded config from {:?}", config_path);
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create config directory")?;
        }

        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        std::fs::write(&config_path, content).context("Failed to write config file")?;

        Ok(())
    }

    fn config_path() -> Result<PathBuf> {
        global::config_file()
    }
}
