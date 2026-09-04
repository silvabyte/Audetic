use audetic_core::sync::{DeviceId, HubId, RecordId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const HUB_LISTENER_PORT: u16 = 3738;
pub const HUB_LISTENER_ADDRESS: &str = "127.0.0.1:3738";
pub const HUB_LOOPBACK_BASE_URL: &str = "http://127.0.0.1:3738";
pub const TAILSCALE_HTTPS_PORT: u16 = 8443;
pub const HUB_API_MOUNT_PATH: &str = "/audetic/";
pub const HUB_INFO_ROUTE: &str = "/v1/info";
pub const HUB_INFO_PATH: &str = "v1/info";
pub const HUB_SNAPSHOTS_ROUTE: &str = "/v1/snapshots";
pub const HUB_SNAPSHOTS_PATH: &str = "v1/snapshots";
pub const HUB_DICTATIONS_ROUTE: &str = "/v1/dictations";
pub const HUB_DICTATIONS_PATH: &str = "v1/dictations";
pub const HUB_MEETINGS_ROUTE: &str = "/v1/meetings";
pub const HUB_MEETINGS_PATH: &str = "v1/meetings";
pub const HUB_ARTIFACTS_ROUTE: &str = "/v1/artifacts";
pub const MAX_SNAPSHOT_BATCH: usize = 25;
pub const MAX_DICTATION_PAGE: usize = 100;
pub const MAX_MEETING_PAGE: usize = 100;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MIN_PROTOCOL_VERSION: u16 = 1;

pub const HUB_ID_HEADER: &str = "x-audetic-hub-id";
pub const PROTOCOL_VERSION_HEADER: &str = "x-audetic-protocol-version";
pub const TAILSCALE_FUNNEL_REQUEST_HEADER: &str = "tailscale-funnel-request";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Dictation,
    Meeting,
    Artifact,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct DictationPayload {
    pub text: String,
}

/// Portable origin snapshot. It intentionally contains no local row ID or
/// filesystem path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct DictationSnapshot {
    pub kind: RecordKind,
    pub schema_version: u16,
    pub record_id: RecordId,
    pub origin_device_id: DeviceId,
    pub local_version: u64,
    pub created_at: String,
    pub updated_at: String,
    pub payload: DictationPayload,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct MeetingPayload {
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub title_version: u64,
    pub source_filename: Option<String>,
    pub transcript_text: String,
    pub transcript_segments: Option<Vec<audetic_core::jobs_client::Segment>>,
    pub duration_seconds: u64,
    pub status: String,
    pub completed_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct MeetingSnapshot {
    pub kind: RecordKind,
    pub schema_version: u16,
    pub record_id: RecordId,
    pub origin_device_id: DeviceId,
    pub local_version: u64,
    pub created_at: String,
    pub updated_at: String,
    pub payload: MeetingPayload,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct CompletedArtifactPayload {
    pub artifact_kind: String,
    pub title: String,
    pub template_id: Option<String>,
    pub agent_profile_name: Option<String>,
    pub content_markdown: String,
    pub completed_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct CompletedArtifactSnapshot {
    pub kind: RecordKind,
    pub schema_version: u16,
    pub record_id: RecordId,
    pub parent_record_id: RecordId,
    pub origin_device_id: DeviceId,
    pub local_version: u64,
    pub created_at: String,
    pub updated_at: String,
    pub payload: CompletedArtifactPayload,
}

/// A bounded upload item. The untagged representation preserves the domain
/// envelope on the wire: every variant carries and validates its own `kind`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(untagged)]
pub enum Snapshot {
    Dictation(DictationSnapshot),
    Meeting(MeetingSnapshot),
    Artifact(CompletedArtifactSnapshot),
}

impl Snapshot {
    pub const fn record_id(&self) -> RecordId {
        match self {
            Self::Dictation(value) => value.record_id,
            Self::Meeting(value) => value.record_id,
            Self::Artifact(value) => value.record_id,
        }
    }

    pub const fn kind(&self) -> RecordKind {
        match self {
            Self::Dictation(_) => RecordKind::Dictation,
            Self::Meeting(_) => RecordKind::Meeting,
            Self::Artifact(_) => RecordKind::Artifact,
        }
    }

    pub const fn local_version(&self) -> u64 {
        match self {
            Self::Dictation(value) => value.local_version,
            Self::Meeting(value) => value.local_version,
            Self::Artifact(value) => value.local_version,
        }
    }
}

impl From<DictationSnapshot> for Snapshot {
    fn from(value: DictationSnapshot) -> Self {
        Self::Dictation(value)
    }
}

impl From<MeetingSnapshot> for Snapshot {
    fn from(value: MeetingSnapshot) -> Self {
        Self::Meeting(value)
    }
}

impl From<CompletedArtifactSnapshot> for Snapshot {
    fn from(value: CompletedArtifactSnapshot) -> Self {
        Self::Artifact(value)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct SnapshotBatch {
    pub snapshots: Vec<Snapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotDisposition {
    Accepted,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SnapshotResult {
    pub record_id: RecordId,
    pub disposition: SnapshotDisposition,
    pub authoritative_revision: Option<u64>,
    pub error_code: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SnapshotBatchResponse {
    pub results: Vec<SnapshotResult>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SharedDictation {
    pub record_id: RecordId,
    pub origin_device_id: DeviceId,
    pub text: String,
    pub created_at: String,
    pub updated_at: String,
    pub local_version: u64,
    pub authoritative_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct DictationPage {
    pub items: Vec<SharedDictation>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct SharedMeeting {
    pub record_id: RecordId,
    pub origin_device_id: DeviceId,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub title_version: u64,
    pub source_filename: Option<String>,
    pub transcript_text: String,
    pub transcript_segments: Option<Vec<audetic_core::jobs_client::Segment>>,
    pub duration_seconds: u64,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: String,
    pub local_version: u64,
    pub authoritative_revision: u64,
    pub artifacts: Vec<SharedArtifact>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct SharedArtifact {
    pub record_id: RecordId,
    pub parent_record_id: RecordId,
    pub origin_device_id: DeviceId,
    pub artifact_kind: String,
    pub title: String,
    pub template_id: Option<String>,
    pub agent_profile_name: Option<String>,
    pub content_markdown: String,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: String,
    pub local_version: u64,
    pub authoritative_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct MeetingPage {
    pub items: Vec<SharedMeeting>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
pub struct MeetingTitlePatch {
    pub title: String,
    pub expected_title_version: u64,
    #[serde(default)]
    pub title_source: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOperation {
    Upsert,
    Delete,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct ChangeEnvelope {
    pub cursor: Option<u64>,
    pub operation: ChangeOperation,
    pub kind: RecordKind,
    pub record_id: RecordId,
    pub origin_device_id: Option<DeviceId>,
    pub authoritative_revision: u64,
    pub snapshot: Option<Snapshot>,
    pub changed_at: String,
}

impl ChangeEnvelope {
    pub fn upsert(snapshot: DictationSnapshot, authoritative_revision: u64) -> Self {
        Self {
            cursor: None,
            operation: ChangeOperation::Upsert,
            kind: RecordKind::Dictation,
            record_id: snapshot.record_id,
            origin_device_id: Some(snapshot.origin_device_id),
            authoritative_revision,
            changed_at: chrono::Utc::now().to_rfc3339(),
            snapshot: Some(Snapshot::Dictation(snapshot)),
        }
    }
}

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

    #[test]
    fn dictation_snapshot_is_portable_and_has_no_filesystem_path() {
        let snapshot = DictationSnapshot {
            kind: RecordKind::Dictation,
            schema_version: 1,
            record_id: RecordId::new(),
            origin_device_id: DeviceId::new(),
            local_version: 1,
            created_at: "2026-09-04T10:00:00Z".into(),
            updated_at: "2026-09-04T10:00:00Z".into(),
            payload: DictationPayload {
                text: "hello".into(),
            },
        };
        let json = serde_json::to_value(snapshot).unwrap();
        assert!(json.get("audio_path").is_none());
        assert!(json.get("transcript_path").is_none());
        assert_eq!(json["kind"], "dictation");
    }

    #[test]
    fn meeting_and_artifact_snapshots_are_portable_and_uuid_linked() {
        let meeting_id = RecordId::new();
        let origin = DeviceId::new();
        let meeting = MeetingSnapshot {
            kind: RecordKind::Meeting,
            schema_version: 1,
            record_id: meeting_id,
            origin_device_id: origin,
            local_version: 1,
            created_at: "2026-09-04T10:00:00Z".into(),
            updated_at: "2026-09-04T10:01:00Z".into(),
            payload: MeetingPayload {
                title: Some("Portable meeting".into()),
                title_source: Some("manual".into()),
                title_version: 1,
                source_filename: Some("capture.wav".into()),
                transcript_text: "portable transcript".into(),
                transcript_segments: None,
                duration_seconds: 60,
                status: "completed".into(),
                completed_at: "2026-09-04T10:01:00Z".into(),
            },
        };
        let artifact = CompletedArtifactSnapshot {
            kind: RecordKind::Artifact,
            schema_version: 1,
            record_id: RecordId::new(),
            parent_record_id: meeting_id,
            origin_device_id: origin,
            local_version: 1,
            created_at: "2026-09-04T10:02:00Z".into(),
            updated_at: "2026-09-04T10:02:00Z".into(),
            payload: CompletedArtifactPayload {
                artifact_kind: "summary".into(),
                title: "Summary".into(),
                template_id: Some("standard_meeting".into()),
                agent_profile_name: Some("Local agent".into()),
                content_markdown: "# Summary".into(),
                completed_at: "2026-09-04T10:02:00Z".into(),
            },
        };
        let meeting_json = serde_json::to_value(meeting).unwrap();
        let artifact_json = serde_json::to_value(artifact).unwrap();
        for json in [&meeting_json, &artifact_json] {
            assert!(json.get("audio_path").is_none());
            assert!(json.get("transcript_path").is_none());
            assert!(json.get("meeting_id").is_none());
        }
        assert_eq!(artifact_json["parent_record_id"], meeting_id.to_string());
    }
}
