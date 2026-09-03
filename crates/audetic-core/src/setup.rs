//! Shared DTOs for the daemon's read-only setup assessment.
//!
//! Capability IDs and states are serialized enums rather than display strings
//! so CLI and UI consumers can depend on a stable wire format.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    Ready,
    NeedsAction,
    Unavailable,
    NotApplicable,
}

impl SetupState {
    pub fn is_ready(self) -> bool {
        matches!(self, Self::Ready | Self::NotApplicable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SetupCapabilityId {
    Omarchy,
    HyprlandSession,
    HyprlandConfig,
    TranscriptionProvider,
    TextDelivery,
    ClipboardFallback,
    DictationKeybind,
    MeetingKeybind,
    Ffmpeg,
    MeetingAudio,
}

impl SetupCapabilityId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Omarchy => "omarchy",
            Self::HyprlandSession => "hyprland_session",
            Self::HyprlandConfig => "hyprland_config",
            Self::TranscriptionProvider => "transcription_provider",
            Self::TextDelivery => "text_delivery",
            Self::ClipboardFallback => "clipboard_fallback",
            Self::DictationKeybind => "dictation_keybind",
            Self::MeetingKeybind => "meeting_keybind",
            Self::Ffmpeg => "ffmpeg",
            Self::MeetingAudio => "meeting_audio",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct ToolReadiness {
    /// Stable executable name, for example `wtype` or `pw-cat`.
    pub id: String,
    pub available: bool,
    pub path: Option<String>,
    /// Arch package that supplies this executable, when known.
    pub arch_package: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SetupCapability {
    pub id: SetupCapabilityId,
    pub state: SetupState,
    /// Whether this capability participates in basic dictation readiness.
    pub required_for_dictation: bool,
    /// Whether this capability participates in meeting readiness.
    pub required_for_meetings: bool,
    pub summary: String,
    pub detail: Option<String>,
    pub action: Option<String>,
    pub tools: Vec<ToolReadiness>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct PlatformInfo {
    pub os: String,
    pub architecture: String,
    pub distribution: Option<String>,
    pub arch_linux: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct WorkflowReadiness {
    pub dictation: SetupState,
    pub meetings: SetupState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SetupAssessment {
    pub state: SetupState,
    /// The provider config on disk differs from the config used by this daemon
    /// process. A daemon restart is required before the persisted provider is
    /// active.
    pub restart_required: bool,
    pub platform: PlatformInfo,
    pub workflows: WorkflowReadiness,
    pub capabilities: Vec<SetupCapability>,
    /// Deduplicated packages needed for a complete setup on Arch Linux.
    pub missing_arch_packages: Vec<String>,
    /// A copyable command only. The daemon never executes this command or sudo.
    pub arch_package_command: Option<String>,
}

impl SetupAssessment {
    pub fn capability(&self, id: SetupCapabilityId) -> Option<&SetupCapability> {
        self.capabilities
            .iter()
            .find(|capability| capability.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_ids_have_stable_wire_names() {
        assert_eq!(
            serde_json::to_string(&SetupCapabilityId::DictationKeybind).unwrap(),
            "\"dictation_keybind\""
        );
        assert_eq!(
            serde_json::to_string(&SetupState::NeedsAction).unwrap(),
            "\"needs_action\""
        );
        assert_eq!(
            serde_json::to_string(&SetupCapabilityId::MeetingKeybind).unwrap(),
            "\"meeting_keybind\""
        );
    }
}
