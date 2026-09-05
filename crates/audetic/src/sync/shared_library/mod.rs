//! Intent-level access to the operational and authoritative meeting library.
//!
//! Routes depend on [`SharedLibrary`], not repositories, paths, or the sync
//! lifecycle coordinator. The private module family owns source selection,
//! merge semantics, persistence, payload resolution, and workflows.

mod mutations;
mod payload;
mod queries;
mod workflows;

use audetic_core::sync::{DeviceId, PayloadAvailability, RecordId, UploadState};
use thiserror::Error;

use std::path::PathBuf;

use super::transition::{LibraryContext, LibraryObservation, LibraryRole, RoleCoordinator};
use super::transport::{PayloadBody, PayloadMetadata, StreamingPayloadResponse};

pub type LibraryResult<T> = Result<T, LibraryError>;

/// Stable failures exposed by the route-facing Shared Library interface.
#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Invalid(String),
    #[error("Shared Library operation failed")]
    Internal {
        operation: &'static str,
        #[source]
        source: anyhow::Error,
    },
}

impl LibraryError {
    pub(super) fn internal(operation: &'static str, source: impl Into<anyhow::Error>) -> Self {
        Self::Internal {
            operation,
            source: source.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MeetingPageRequest {
    pub query: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct PayloadRequest {
    pub id: RecordId,
    pub kind: super::protocol::RecordKind,
    pub range: Option<String>,
}

pub struct LibraryPayload {
    pub status: u16,
    pub metadata: PayloadMetadata,
    pub body: PayloadBody,
}

impl From<StreamingPayloadResponse> for LibraryPayload {
    fn from(value: StreamingPayloadResponse) -> Self {
        Self {
            status: value.status,
            metadata: value.metadata,
            body: value.body,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MeetingTitleResult {
    pub meeting_id: RecordId,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub local_id: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeleteMeetingResult {
    pub local_id: Option<i64>,
}

#[derive(Debug)]
pub struct PreparedMeetingRetry {
    pub local_id: i64,
    pub record_id: RecordId,
    pub audio_path: PathBuf,
    pub duration_seconds: i64,
}

#[derive(Clone, Debug)]
pub struct LibraryMeeting {
    pub id: RecordId,
    pub local_id: Option<i64>,
    pub origin_device_id: DeviceId,
    pub title: Option<String>,
    pub title_source: Option<String>,
    pub title_version: u64,
    pub source_filename: Option<String>,
    pub status: String,
    pub transcript_text: Option<String>,
    pub transcript_segments: Option<Vec<audetic_core::jobs_client::Segment>>,
    pub duration_seconds: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub upload_state: Option<UploadState>,
    pub payload_availability: PayloadAvailability,
    pub access: LibraryItemAccess,
    pub artifacts: Vec<super::protocol::SharedArtifact>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LibraryItemAccess {
    Local,
    Shared,
    LocalOffline,
    LocalOfflineReadOnly,
}

impl LibraryItemAccess {
    pub const fn source(self) -> &'static str {
        match self {
            Self::Local | Self::LocalOffline | Self::LocalOfflineReadOnly => "local",
            Self::Shared => "shared",
        }
    }

    pub const fn offline(self) -> bool {
        matches!(self, Self::LocalOffline | Self::LocalOfflineReadOnly)
    }

    pub const fn read_only(self) -> bool {
        matches!(self, Self::LocalOfflineReadOnly)
    }
}

/// Deep route-facing module for Shared Library user intents.
#[derive(Clone)]
pub struct SharedLibrary {
    pub(super) coordinator: RoleCoordinator,
    standalone_only: bool,
}

impl SharedLibrary {
    pub(super) fn new(coordinator: RoleCoordinator) -> Self {
        Self {
            coordinator,
            standalone_only: false,
        }
    }

    pub(super) fn standalone(coordinator: RoleCoordinator) -> Self {
        Self {
            coordinator,
            standalone_only: true,
        }
    }

    pub(super) fn context(&self) -> LibraryResult<LibraryContext> {
        let mut context = self
            .coordinator
            .library_context()
            .map_err(|error| LibraryError::internal("selecting library role", error))?;
        if self.standalone_only {
            context.role = LibraryRole::Standalone;
        }
        Ok(context)
    }

    pub(super) async fn observe(
        &self,
        context: &LibraryContext,
        reachable: bool,
        error: Option<&str>,
    ) -> LibraryResult<()> {
        let observation = if reachable {
            LibraryObservation::Reachable
        } else {
            LibraryObservation::Unreachable(error.unwrap_or("Home Hub unavailable").to_owned())
        };
        self.coordinator
            .record_library_observation(context, observation)
            .await
            .map_err(|error| LibraryError::internal("recording Home Hub reachability", error))
    }
}

#[cfg(test)]
mod tests;
