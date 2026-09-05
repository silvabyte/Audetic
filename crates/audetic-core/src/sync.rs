//! Portable Library Sync identities, enums, and local API data transfer types.
//!
//! This module deliberately contains no daemon runtime, SQLite, HTTP-server,
//! or Tailscale process behavior. It is shared by local API consumers and the
//! daemon-owned sync domain.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

macro_rules! uuid_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
        #[cfg_attr(feature = "schema", schema(value_type = String, format = "uuid"))]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                self.0.hyphenated().fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let uuid = Uuid::parse_str(value)
                    .map_err(|error| format!("invalid {}: {error}", stringify!($name)))?;
                if value != uuid.hyphenated().to_string() {
                    return Err(format!(
                        "invalid {}: expected a lowercase hyphenated UUID",
                        stringify!($name)
                    ));
                }
                Ok(Self(uuid))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

uuid_newtype!(RecordId);
uuid_newtype!(DeviceId);
uuid_newtype!(HubId);
uuid_newtype!(AgentProfileId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SyncRole {
    Standalone,
    HomeHub,
    ConnectedDevice,
}

impl SyncRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::HomeHub => "home_hub",
            Self::ConnectedDevice => "connected_device",
        }
    }
}

impl FromStr for SyncRole {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "standalone" => Ok(Self::Standalone),
            "home_hub" => Ok(Self::HomeHub),
            "connected_device" => Ok(Self::ConnectedDevice),
            _ => Err(format!("invalid sync role: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum CacheLevel {
    LiveOnly,
    TextForOfflineUse,
    TextAndAvailableAudio,
}

impl CacheLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LiveOnly => "live_only",
            Self::TextForOfflineUse => "text_for_offline_use",
            Self::TextAndAvailableAudio => "text_and_available_audio",
        }
    }
}

impl FromStr for CacheLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "live_only" => Ok(Self::LiveOnly),
            "text_for_offline_use" => Ok(Self::TextForOfflineUse),
            "text_and_available_audio" => Ok(Self::TextAndAvailableAudio),
            _ => Err(format!("invalid cache level: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum UploadState {
    Pending,
    Uploading,
    Synced,
    NeedsAttention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PayloadAvailability {
    Available,
    Pending,
    Unavailable,
    NeedsAttention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ServeMappingState {
    Vacant,
    Audetic,
    Collision,
}

/// Current Tailscale and Serve readiness as observed by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncNetworkAssessment {
    pub ready: bool,
    pub backend_state: Option<String>,
    pub dns_name: Option<String>,
    pub owner_login: Option<String>,
    pub serve_mapping: Option<ServeMappingState>,
    pub funnel_enabled: Option<bool>,
    pub serve_preview: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct HubConnection {
    /// Canonical base URL ending in `/audetic/`.
    pub base_url: String,
    pub hub_id: HubId,
    pub owner_login: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct HubCandidate {
    pub connection: HubConnection,
    pub device_name: Option<String>,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncDiscoveryFailure {
    pub candidate: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncSetupRequest {
    pub role: SyncRole,
    pub device_name: Option<String>,
    pub hub: Option<HubConnection>,
    pub upload_recording_payloads: bool,
    pub cache_level: CacheLevel,
    pub shared_config_enabled: bool,
    /// Home Hub activation changes Tailscale Serve only when explicitly true.
    #[serde(default)]
    pub confirm_serve_change: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncSetupResult {
    pub status: SyncStatus,
    pub discovered_hubs: Vec<HubCandidate>,
    pub discovery_failures: Vec<SyncDiscoveryFailure>,
    pub setup_command: Option<String>,
    pub serve_preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncPayloadPolicyRequest {
    pub upload_recording_payloads: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncPayloadPolicyResponse {
    pub upload_recording_payloads: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncStatus {
    pub device_id: DeviceId,
    pub role: SyncRole,
    pub device_name: Option<String>,
    pub local_hub_id: Option<HubId>,
    pub hub: Option<HubConnection>,
    pub hub_reachable: bool,
    pub last_contact_at: Option<String>,
    pub pending_items: u64,
    pub pending_bytes: u64,
    pub last_error: Option<String>,
    pub upload_recording_payloads: bool,
    pub cache_level: CacheLevel,
    pub shared_config_enabled: bool,
    pub applied_shared_config_version: Option<u64>,
    pub network: SyncNetworkAssessment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SharedConfigDocument {
    pub schema_version: u32,
    pub document_version: u64,
    pub behavior: SharedBehaviorConfig,
    pub appearance: SharedAppearanceConfig,
    pub transcription: SharedTranscriptionConfig,
    pub agent_profiles: Vec<SharedAgentProfile>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SharedBehaviorConfig {
    pub auto_paste: bool,
    pub preserve_clipboard: bool,
    pub audio_feedback: bool,
    pub retain_recording_payloads: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SharedAppearanceConfig {
    pub theme: Option<String>,
    pub notification_color: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SharedTranscriptionConfig {
    pub provider: Option<String>,
    pub language: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SharedAgentProfile {
    pub id: AgentProfileId,
    pub name: String,
    pub kind: String,
    pub arguments: Vec<String>,
    pub prompt_mode: String,
    pub enabled: bool,
    pub preferred_default: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_use_one_strict_canonical_wire_format() {
        let canonical = "67e55044-10b1-426f-9247-bb680e5fe0c8";
        let id: RecordId = canonical.parse().unwrap();
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            format!("\"{canonical}\"")
        );
        assert_eq!(
            serde_json::from_str::<RecordId>(&format!("\"{canonical}\"")).unwrap(),
            id
        );

        assert!("67E55044-10B1-426F-9247-BB680E5FE0C8"
            .parse::<RecordId>()
            .is_err());
        assert!("67e5504410b1426f9247bb680e5fe0c8"
            .parse::<RecordId>()
            .is_err());
    }

    #[test]
    fn sync_enums_have_stable_wire_names() {
        assert_eq!(
            serde_json::to_string(&SyncRole::HomeHub).unwrap(),
            "\"home_hub\""
        );
        assert_eq!(
            serde_json::to_string(&CacheLevel::TextForOfflineUse).unwrap(),
            "\"text_for_offline_use\""
        );
        assert_eq!(
            serde_json::to_string(&UploadState::NeedsAttention).unwrap(),
            "\"needs_attention\""
        );
    }
}
