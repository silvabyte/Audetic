use audetic_core::sync::HubId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const HUB_LISTENER_PORT: u16 = 3738;
pub const HUB_LISTENER_ADDRESS: &str = "127.0.0.1:3738";
pub const HUB_LOOPBACK_BASE_URL: &str = "http://127.0.0.1:3738";
pub const TAILSCALE_HTTPS_PORT: u16 = 8443;
pub const HUB_API_MOUNT_PATH: &str = "/audetic/";
pub const HUB_INFO_ROUTE: &str = "/v1/info";
pub const HUB_INFO_PATH: &str = "v1/info";

pub const PROTOCOL_VERSION: u16 = 1;
pub const MIN_PROTOCOL_VERSION: u16 = 1;

pub const HUB_ID_HEADER: &str = "x-audetic-hub-id";
pub const PROTOCOL_VERSION_HEADER: &str = "x-audetic-protocol-version";
pub const TAILSCALE_FUNNEL_REQUEST_HEADER: &str = "tailscale-funnel-request";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct ProtocolRange {
    pub current: u16,
    pub minimum: u16,
}

impl ProtocolRange {
    pub const fn supported() -> Self {
        Self {
            current: PROTOCOL_VERSION,
            minimum: MIN_PROTOCOL_VERSION,
        }
    }

    pub const fn accepts(&self, version: u16) -> bool {
        version >= self.minimum && version <= self.current
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct HubInfo {
    /// Stable UUID identifying this Home Hub installation.
    pub hub_id: HubId,
    /// Exact login extracted from Tailscale's identity header during setup.
    pub owner_login: String,
    pub device_name: Option<String>,
    pub protocol: ProtocolRange,
    pub audetic_version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct HubApiError {
    pub code: String,
    pub message: String,
}

impl HubApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_protocol_range_accepts_only_slice_one_version() {
        let range = ProtocolRange::supported();

        assert!(range.accepts(PROTOCOL_VERSION));
        assert!(!range.accepts(PROTOCOL_VERSION - 1));
        assert!(!range.accepts(PROTOCOL_VERSION + 1));
    }

    #[test]
    fn mounted_paths_are_relative_to_the_canonical_base() {
        assert_eq!(HUB_API_MOUNT_PATH, "/audetic/");
        assert!(!HUB_INFO_PATH.starts_with('/'));
        assert_eq!(HUB_INFO_ROUTE.strip_prefix('/'), Some(HUB_INFO_PATH));
        assert_eq!(
            HUB_LISTENER_ADDRESS,
            format!("127.0.0.1:{HUB_LISTENER_PORT}")
        );
        assert_eq!(
            HUB_LOOPBACK_BASE_URL,
            format!("http://{HUB_LISTENER_ADDRESS}")
        );
    }
}
