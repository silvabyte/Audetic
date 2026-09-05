use async_trait::async_trait;
use audetic_core::sync::{HubCandidate, HubConnection, RecordId};
use bytes::Bytes;
use futures_util::Stream;
use thiserror::Error;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::pin::Pin;

use super::protocol::{
    DictationPage, MeetingPage, MeetingTitlePatch, RecordKind, SharedMeeting, SnapshotBatch,
    SnapshotBatchResponse,
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

pub struct StreamingPayloadResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
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
    Http { status: u16, message: String },
    #[error("{0}")]
    Retryable(String),
    #[error("{0}")]
    NeedsAttention(String),
}

impl HubTransferError {
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Transport(_) | Self::Retryable(_) | Self::Http { status: 500.., .. }
        )
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
        _hub: &HubConnection,
        _batch: SnapshotBatch,
    ) -> Result<SnapshotBatchResponse, HubTransferError> {
        Err(HubTransferError::NeedsAttention(
            "snapshot upload is unavailable".to_owned(),
        ))
    }

    async fn upload_blob(
        &self,
        _hub: &HubConnection,
        _blob: BlobUpload,
    ) -> Result<(), HubTransferError> {
        Err(HubTransferError::NeedsAttention(
            "Recording Payload upload is unavailable".to_owned(),
        ))
    }
}

#[async_trait]
pub trait RemoteLibrary: Send + Sync {
    async fn page_dictations(
        &self,
        _hub: &HubConnection,
        _query: Option<&str>,
        _from: Option<&str>,
        _to: Option<&str>,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<DictationPage, HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub is unavailable".to_owned(),
        ))
    }

    async fn page_meetings(
        &self,
        _hub: &HubConnection,
        _query: Option<&str>,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<MeetingPage, HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub is unavailable".to_owned(),
        ))
    }

    async fn meeting(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub is unavailable".to_owned(),
        ))
    }

    async fn update_meeting_title(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _patch: MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub is unavailable".to_owned(),
        ))
    }

    async fn delete_record(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _kind: RecordKind,
    ) -> Result<(), HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub is unavailable".to_owned(),
        ))
    }
}

#[async_trait]
pub trait RemotePayloadSource: Send + Sync {
    async fn stream_payload(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _kind: RecordKind,
        _range: Option<&str>,
    ) -> Result<StreamingPayloadResponse, HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub Recording Payload is unavailable".to_owned(),
        ))
    }
}
