use async_trait::async_trait;
use audetic_core::sync::{HubCandidate, HubConnection, RecordId};
use bytes::Bytes;
use futures_util::Stream;
use http::HeaderValue;
use thiserror::Error;

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use super::protocol::{
    ChangeCursor, ChangePage, ChangeTarget, DictationPage, MeetingPage, MeetingTitlePatch,
    RecordKind, SharedMeeting, SnapshotBatch, SnapshotBatchResponse,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlobUpload {
    pub record_id: RecordId,
    pub checksum: String,
    pub source_path: PathBuf,
    pub byte_size: u64,
    pub media_type: String,
}

pub type PayloadBody =
    Pin<Box<dyn Stream<Item = Result<Bytes, HubTransferError>> + Send + 'static>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PayloadContentRange {
    Bytes {
        start: u64,
        end: u64,
        complete_length: u64,
    },
    Unsatisfied {
        complete_length: u64,
    },
}

impl PayloadContentRange {
    pub fn byte_length(&self) -> Option<u64> {
        match self {
            Self::Bytes { start, end, .. } if start <= end => Some(end - start + 1),
            Self::Bytes { .. } | Self::Unsatisfied { .. } => None,
        }
    }

    pub fn to_header_value(&self) -> HeaderValue {
        let value = match self {
            Self::Bytes {
                start,
                end,
                complete_length,
            } => format!("bytes {start}-{end}/{complete_length}"),
            Self::Unsatisfied { complete_length } => format!("bytes */{complete_length}"),
        };
        HeaderValue::from_str(&value).expect("validated Content-Range is a valid header value")
    }
}

#[derive(Clone, Debug, Default)]
pub struct PayloadMetadata {
    pub content_type: Option<HeaderValue>,
    pub content_length: Option<u64>,
    pub content_range: Option<PayloadContentRange>,
    pub accept_ranges: Option<HeaderValue>,
}

pub struct StreamingPayloadResponse {
    pub status: u16,
    pub metadata: PayloadMetadata,
    pub body: PayloadBody,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryFailure {
    pub candidate: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryOutcome {
    None { failures: Vec<DiscoveryFailure> },
    One(HubCandidate),
    Multiple(Vec<HubCandidate>),
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum HubTransferError {
    #[error("Home Hub transport failed: {0}")]
    Transport(String),
    #[error("Home Hub returned HTTP {status}: {message}")]
    Http {
        status: u16,
        message: String,
        retry_after: Option<String>,
    },
    #[error("{0}")]
    Retryable(String),
    #[error("{0}")]
    NeedsAttention(String),
}

impl HubTransferError {
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport(_)
                | Self::Retryable(_)
                | Self::Http {
                    status: 408 | 425 | 429 | 500..=599,
                    ..
                }
        )
    }

    pub fn retry_after(&self) -> Option<&str> {
        match self {
            Self::Http { retry_after, .. } => retry_after.as_deref(),
            _ => None,
        }
    }
}

#[async_trait]
pub trait HubProbe: Send + Sync {
    async fn handshake(&self, hub: &HubConnection) -> Result<HubCandidate, HubTransferError>;

    async fn discover(
        &self,
        candidates: Vec<String>,
        expected_owner_login: &str,
    ) -> DiscoveryOutcome;
}

#[async_trait]
pub trait ReplicationTransport: Send + Sync {
    async fn upload_snapshots(
        &self,
        hub: &HubConnection,
        batch: SnapshotBatch,
    ) -> Result<SnapshotBatchResponse, HubTransferError>;

    async fn upload_blob(
        &self,
        hub: &HubConnection,
        blob: BlobUpload,
    ) -> Result<(), HubTransferError>;
}

#[async_trait]
pub trait RemoteDictationLibrary: Send + Sync {
    async fn page_dictations(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DictationPage, HubTransferError>;
}

#[async_trait]
pub trait RemoteMeetingLibrary: Send + Sync {
    async fn page_meetings(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeetingPage, HubTransferError>;

    async fn meeting(
        &self,
        hub: &HubConnection,
        id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError>;
}

#[async_trait]
pub trait RemoteLibraryMutations: Send + Sync {
    async fn update_meeting_title(
        &self,
        hub: &HubConnection,
        id: RecordId,
        patch: MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError>;

    async fn delete_record(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
    ) -> Result<(), HubTransferError>;
}

#[async_trait]
pub trait RemotePayloadSource: Send + Sync {
    async fn stream_payload(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
        range: Option<&str>,
    ) -> Result<StreamingPayloadResponse, HubTransferError>;
}

/// Narrow capability for traversing one immutable authoritative change target.
#[async_trait]
pub trait HubChangeSource: Send + Sync {
    async fn page_changes(
        &self,
        hub: &HubConnection,
        after: ChangeCursor,
        target: Option<ChangeTarget>,
        limit: usize,
    ) -> Result<ChangePage, HubTransferError>;
}

/// Capabilities derived from one adapter instance.
///
/// The fields are private so production callers cannot combine discovery with
/// unrelated replication, library, or payload implementations.
#[derive(Clone)]
pub struct HubCapabilities {
    probe: Arc<dyn HubProbe>,
    replication: Arc<dyn ReplicationTransport>,
    dictations: Arc<dyn RemoteDictationLibrary>,
    meetings: Arc<dyn RemoteMeetingLibrary>,
    mutations: Arc<dyn RemoteLibraryMutations>,
    payloads: Arc<dyn RemotePayloadSource>,
    changes: Arc<dyn HubChangeSource>,
}

impl HubCapabilities {
    pub fn from_adapter<A>(adapter: A) -> Self
    where
        A: HubProbe
            + ReplicationTransport
            + RemoteDictationLibrary
            + RemoteMeetingLibrary
            + RemoteLibraryMutations
            + RemotePayloadSource
            + HubChangeSource
            + 'static,
    {
        let adapter = Arc::new(adapter);
        Self {
            probe: adapter.clone(),
            replication: adapter.clone(),
            dictations: adapter.clone(),
            meetings: adapter.clone(),
            mutations: adapter.clone(),
            payloads: adapter.clone(),
            changes: adapter,
        }
    }

    pub fn probe(&self) -> &dyn HubProbe {
        self.probe.as_ref()
    }

    pub fn replication(&self) -> Arc<dyn ReplicationTransport> {
        Arc::clone(&self.replication)
    }

    pub fn dictations(&self) -> Arc<dyn RemoteDictationLibrary> {
        Arc::clone(&self.dictations)
    }

    pub fn meetings(&self) -> Arc<dyn RemoteMeetingLibrary> {
        Arc::clone(&self.meetings)
    }

    pub fn mutations(&self) -> &dyn RemoteLibraryMutations {
        self.mutations.as_ref()
    }

    pub fn payloads(&self) -> &dyn RemotePayloadSource {
        self.payloads.as_ref()
    }

    pub fn changes(&self) -> Arc<dyn HubChangeSource> {
        Arc::clone(&self.changes)
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        probe: Arc<dyn HubProbe>,
        replication: Arc<dyn ReplicationTransport>,
        dictations: Arc<dyn RemoteDictationLibrary>,
        meetings: Arc<dyn RemoteMeetingLibrary>,
        mutations: Arc<dyn RemoteLibraryMutations>,
        payloads: Arc<dyn RemotePayloadSource>,
        changes: Arc<dyn HubChangeSource>,
    ) -> Self {
        Self {
            probe,
            replication,
            dictations,
            meetings,
            mutations,
            payloads,
            changes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::client::NetworkHubAdapter;

    #[test]
    fn production_capability_views_share_one_adapter_instance() {
        let capabilities = HubCapabilities::from_adapter(NetworkHubAdapter::default());
        let pointers = [
            Arc::as_ptr(&capabilities.probe) as *const (),
            Arc::as_ptr(&capabilities.replication) as *const (),
            Arc::as_ptr(&capabilities.dictations) as *const (),
            Arc::as_ptr(&capabilities.meetings) as *const (),
            Arc::as_ptr(&capabilities.mutations) as *const (),
            Arc::as_ptr(&capabilities.payloads) as *const (),
            Arc::as_ptr(&capabilities.changes) as *const (),
        ];

        assert!(pointers.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn transient_http_statuses_are_retryable() {
        for status in [408, 425, 429, 500, 503, 599] {
            assert!(HubTransferError::Http {
                status,
                message: "try later".to_owned(),
                retry_after: Some("30".to_owned()),
            }
            .is_retryable());
        }
        for status in [400, 401, 404, 409, 422, 600] {
            assert!(!HubTransferError::Http {
                status,
                message: "do not retry".to_owned(),
                retry_after: None,
            }
            .is_retryable());
        }
    }
}
