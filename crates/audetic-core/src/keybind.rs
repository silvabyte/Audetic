//! Capture-source shortcuts shared by the daemon and its consumers.
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

use crate::url::paths;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum KeybindTarget {
    #[default]
    Note,
    SystemNote,
}

impl KeybindTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::SystemNote => "system-note",
        }
    }

    pub const fn endpoint_path(self) -> &'static str {
        paths::AUDIO_NOTES_TOGGLE
    }

    pub const fn capture_source(self) -> &'static str {
        match self {
            Self::Note => "microphone",
            Self::SystemNote => "microphone_and_system",
        }
    }
}

impl fmt::Display for KeybindTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for KeybindTarget {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "note" => Ok(Self::Note),
            "system-note" => Ok(Self::SystemNote),
            _ => Err(format!(
                "unknown keybind target '{value}'; expected note or system-note"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_shortcuts_share_endpoint_but_not_source() {
        for target in [KeybindTarget::Note, KeybindTarget::SystemNote] {
            assert_eq!(target.endpoint_path(), paths::AUDIO_NOTES_TOGGLE);
            assert_eq!(target.as_str().parse::<KeybindTarget>().unwrap(), target);
            assert_eq!(serde_json::to_value(target).unwrap(), target.as_str());
        }
        assert_eq!(KeybindTarget::Note.capture_source(), "microphone");
        assert_eq!(
            KeybindTarget::SystemNote.capture_source(),
            "microphone_and_system"
        );
        assert!("meeting".parse::<KeybindTarget>().is_err());
    }
}
