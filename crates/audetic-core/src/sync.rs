//! Shared synchronization API contracts.

use serde::{Deserialize, Serialize};

use std::fmt;

use crate::config::SyncRole;

pub const SYNC_PROTOCOL_VERSION: u16 = 1;

/// The Slice 1 status shape retained for existing consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncStatus {
    pub role: SyncRole,
    pub node_id: String,
}

/// Non-secret information about the Hub paired with this Client Node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct PairedHub {
    pub hub_url: String,
    pub hub_node_id: String,
    pub device_id: String,
    pub protocol_version: u16,
    pub paired_at: String,
}

/// Extended local status returned by Slice 2 synchronization APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncStatusResponse {
    pub role: SyncRole,
    pub node_id: String,
    #[serde(default)]
    pub paired_hub: Option<PairedHub>,
}

impl SyncStatusResponse {
    pub fn unpaired(status: SyncStatus) -> Self {
        Self {
            role: status.role,
            node_id: status.node_id,
            paired_hub: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct HubEnableResponse {
    pub role: SyncRole,
    pub restart_required: bool,
}

/// Public Hub-side device projection. It never contains a credential or hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncDevice {
    pub device_id: String,
    pub name: String,
    pub client_node_id: Option<String>,
    pub paired: bool,
    pub revoked: bool,
    pub created_at: String,
    pub paired_at: Option<String>,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct DeviceAddRequest {
    pub name: String,
}

/// The only Hub administration response that contains a plaintext credential.
/// Its custom `Debug` representation intentionally redacts that credential.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct DeviceAddResponse {
    pub device: SyncDevice,
    pub credential: String,
}

impl fmt::Debug for DeviceAddResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceAddResponse")
            .field("device", &self.device)
            .field("credential", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct DeviceListResponse {
    pub devices: Vec<SyncDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct DeviceRevokeResponse {
    pub device: SyncDevice,
}

/// Local request asking this daemon to pair with a remote Hub.
/// Its custom `Debug` representation intentionally redacts the credential.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct LocalPairRequest {
    pub hub_url: String,
    pub credential: String,
}

impl fmt::Debug for LocalPairRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalPairRequest")
            .field("hub_url", &self.hub_url)
            .field("credential", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct LocalPairResponse {
    pub role: SyncRole,
    pub paired_hub: PairedHub,
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct LocalUnpairResponse {
    pub role: SyncRole,
    pub restart_required: bool,
    pub hub_credential_revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct TransportPairRequest {
    pub client_node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct TransportPairResponse {
    pub protocol_version: u16,
    pub hub_node_id: String,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct TransportStatusResponse {
    pub protocol_version: u16,
    pub hub_node_id: String,
    pub device_id: String,
    pub client_node_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> SyncDevice {
        SyncDevice {
            device_id: "257dc5a6-d8d7-463b-8f2f-f95f616c3a15".to_string(),
            name: "Work laptop".to_string(),
            client_node_id: None,
            paired: false,
            revoked: false,
            created_at: "2026-09-16 18:00:00".to_string(),
            paired_at: None,
            revoked_at: None,
        }
    }

    #[test]
    fn status_round_trips_through_json() {
        let status = SyncStatusResponse {
            role: SyncRole::Client,
            node_id: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
            paired_hub: Some(PairedHub {
                hub_url: "https://sync.example.com".to_string(),
                hub_node_id: "350e8400-e29b-41d4-a716-446655440000".to_string(),
                device_id: "257dc5a6-d8d7-463b-8f2f-f95f616c3a15".to_string(),
                protocol_version: SYNC_PROTOCOL_VERSION,
                paired_at: "2026-09-16 18:00:00".to_string(),
            }),
        };

        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(
            serde_json::from_str::<SyncStatusResponse>(&json).unwrap(),
            status
        );
        assert!(json.contains("\"role\":\"client\""));
        assert!(json.contains("\"paired_hub\""));
    }

    #[test]
    fn secret_bearing_contracts_redact_debug_output() {
        let credential = "audetic_sync_secret-value";
        let add = DeviceAddResponse {
            device: device(),
            credential: credential.to_string(),
        };
        let pair = LocalPairRequest {
            hub_url: "https://sync.example.com".to_string(),
            credential: credential.to_string(),
        };

        for debug in [format!("{add:?}"), format!("{pair:?}")] {
            assert!(debug.contains("[REDACTED]"));
            assert!(!debug.contains(credential));
        }
    }

    #[test]
    fn public_device_projection_has_no_secret_fields() {
        let json = serde_json::to_value(device()).unwrap();
        let object = json.as_object().unwrap();

        assert!(!object.contains_key("credential"));
        assert!(!object.contains_key("credential_hash"));
        assert!(!object.contains_key("bearer_credential"));
    }
}
