//! Shared keybind target identifiers used by daemon and CLI consumers.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::url::paths;

/// Stable Audetic actions that can be installed as Hyprland shortcuts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum KeybindTarget {
    #[default]
    Dictation,
    Meeting,
}

impl KeybindTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dictation => "dictation",
            Self::Meeting => "meeting",
        }
    }

    pub const fn endpoint_path(self) -> &'static str {
        match self {
            Self::Dictation => paths::TOGGLE,
            Self::Meeting => paths::MEETINGS_TOGGLE,
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
            "dictation" => Ok(Self::Dictation),
            "meeting" => Ok(Self::Meeting),
            _ => Err(format!(
                "unknown keybind target '{value}'; expected dictation or meeting"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_have_stable_wire_names_and_paths() {
        assert_eq!(
            serde_json::to_string(&KeybindTarget::Dictation).unwrap(),
            "\"dictation\""
        );
        assert_eq!(
            serde_json::to_string(&KeybindTarget::Meeting).unwrap(),
            "\"meeting\""
        );
        assert_eq!(KeybindTarget::Dictation.endpoint_path(), paths::TOGGLE);
        assert_eq!(
            KeybindTarget::Meeting.endpoint_path(),
            paths::MEETINGS_TOGGLE
        );
    }
}
