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

/// Automatic paste for each capture source.
///
/// Keys match `AudioNoteCaptureSource` (`microphone`, `microphone_and_system`).
/// A source that is missing — including one added later — stays off.
/// A legacy boolean loads as that value for every source and is rewritten as
/// this table the next time the file is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(default, deny_unknown_fields)]
pub struct AutoPastePreferences {
    /// Microphone-only notes (dictation).
    pub microphone: bool,
    /// Microphone plus system-audio notes.
    pub microphone_and_system: bool,
}

impl AutoPastePreferences {
    /// Legacy `auto_paste = true|false` applies this value to every source.
    /// A newly added source must be set here so a legacy file still covers it.
    pub fn for_every_source(enabled: bool) -> Self {
        Self {
            microphone: enabled,
            microphone_and_system: enabled,
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AutoPasteConfig {
    Legacy(bool),
    PerSource(AutoPastePreferences),
}

fn deserialize_auto_paste<'de, D>(deserializer: D) -> Result<AutoPastePreferences, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match AutoPasteConfig::deserialize(deserializer)? {
        AutoPasteConfig::Legacy(enabled) => Ok(AutoPastePreferences::for_every_source(enabled)),
        AutoPasteConfig::PerSource(preferences) => Ok(preferences),
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    /// Per capture source. Legacy boolean values still load; see [`AutoPastePreferences`].
    #[serde(default, deserialize_with = "deserialize_auto_paste")]
    pub auto_paste: AutoPastePreferences,
    pub preserve_clipboard: bool,
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
            auto_paste: AutoPastePreferences::default(),
            preserve_clipboard: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_auto_paste_bool_applies_to_every_capture_source() {
        assert_eq!(
            Config::default().behavior.auto_paste,
            AutoPastePreferences::default()
        );
        let empty: Config = toml::from_str("").unwrap();
        assert_eq!(empty.behavior.auto_paste, AutoPastePreferences::default());
        let untouched: Config = toml::from_str("[behavior]\npreserve_clipboard = true\n").unwrap();
        assert_eq!(
            untouched.behavior.auto_paste,
            AutoPastePreferences::default()
        );
        assert!(untouched.behavior.preserve_clipboard);

        for (literal, enabled) in [("true", true), ("false", false)] {
            let legacy: Config = toml::from_str(&format!(
                "[behavior]\nauto_paste = {literal}\ndelete_audio_files = true\n"
            ))
            .unwrap();
            assert_eq!(
                legacy.behavior.auto_paste,
                AutoPastePreferences::for_every_source(enabled)
            );
            let written = toml::to_string(&legacy).unwrap();
            assert!(!written.contains("delete_audio_files"));
            assert!(!written.contains("auto_paste = true"));
            assert!(!written.contains("auto_paste = false"));
            assert!(written.contains("microphone"));
            assert!(written.contains("microphone_and_system"));
            let reloaded: Config = toml::from_str(&written).unwrap();
            assert_eq!(reloaded.behavior.auto_paste, legacy.behavior.auto_paste);
        }
    }

    #[test]
    fn auto_paste_preferences_round_trip_per_capture_source() {
        let mixed: Config = toml::from_str(
            "[behavior.auto_paste]\nmicrophone = true\nmicrophone_and_system = false\n",
        )
        .unwrap();
        assert!(mixed.behavior.auto_paste.microphone);
        assert!(!mixed.behavior.auto_paste.microphone_and_system);
        let reloaded: Config = toml::from_str(&toml::to_string(&mixed).unwrap()).unwrap();
        assert_eq!(reloaded.behavior.auto_paste, mixed.behavior.auto_paste);

        // A source omitted from the table stays off, including sources added later.
        let microphone_only: Config =
            toml::from_str("[behavior.auto_paste]\nmicrophone = true\n").unwrap();
        assert!(microphone_only.behavior.auto_paste.microphone);
        assert!(!microphone_only.behavior.auto_paste.microphone_and_system);

        assert!(toml::from_str::<Config>("[behavior.auto_paste]\ndictation = true\n").is_err());
    }
}
