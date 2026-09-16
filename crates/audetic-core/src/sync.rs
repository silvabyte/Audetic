//! Shared synchronization API contracts.

use serde::{Deserialize, Serialize};

use crate::config::SyncRole;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(utoipa::ToSchema))]
pub struct SyncStatus {
    pub role: SyncRole,
    pub node_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_through_json() {
        let status = SyncStatus {
            role: SyncRole::Standalone,
            node_id: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
        };

        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(serde_json::from_str::<SyncStatus>(&json).unwrap(), status);
        assert!(json.contains("\"role\":\"standalone\""));
    }
}
