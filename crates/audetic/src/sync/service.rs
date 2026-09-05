//! Thin route-facing composition facade for the Library Sync domain.

use audetic_core::sync::{RecordId, SyncSetupRequest, SyncSetupResult, SyncStatus};

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use super::client::NetworkHubAdapter;
use super::protocol::{RecordKind, SharedMeeting, HUB_LISTENER_ADDRESS};
use super::shared_library::{
    ArtifactDeleteResult, DeleteResult, LibraryMeeting, LibraryPayload, MeetingPageRequest,
    MeetingTitleResult, PayloadRequest, RetryMeetingResult, SharedLibraryService,
};
use super::tailscale::{SystemCommandRunner, Tailscale, TailscaleControl};
use super::transition::RoleCoordinator;
use super::transport::HubCapabilities;

pub use super::transition::TransitionError as SyncServiceError;

#[derive(Clone)]
pub struct SyncService {
    #[cfg(test)]
    pub(super) coordinator: RoleCoordinator,
    #[cfg(not(test))]
    coordinator: RoleCoordinator,
    library: SharedLibraryService,
}

impl SyncService {
    pub(crate) fn default_local_library() -> anyhow::Result<Self> {
        Ok(Self::local_library(crate::global::db_file()?))
    }

    pub(crate) fn local_library(db_path: PathBuf) -> Self {
        let mut service = Self::with_dependencies(
            db_path,
            Arc::new(Tailscale::new(SystemCommandRunner)),
            HubCapabilities::from_adapter(NetworkHubAdapter::default()),
            HUB_LISTENER_ADDRESS
                .parse()
                .expect("valid listener address"),
        );
        service.library = SharedLibraryService::standalone(service.coordinator.clone());
        service
    }

    pub fn production(db_path: PathBuf) -> Self {
        Self::with_dependencies(
            db_path,
            Arc::new(Tailscale::new(SystemCommandRunner)),
            HubCapabilities::from_adapter(NetworkHubAdapter::default()),
            HUB_LISTENER_ADDRESS
                .parse()
                .expect("valid listener address"),
        )
    }

    pub fn with_dependencies(
        db_path: PathBuf,
        tailscale: Arc<dyn TailscaleControl>,
        hub_capabilities: HubCapabilities,
        hub_bind_address: SocketAddr,
    ) -> Self {
        let coordinator =
            RoleCoordinator::new(db_path, tailscale, hub_capabilities, hub_bind_address);
        let library = SharedLibraryService::new(coordinator.clone());
        Self {
            coordinator,
            library,
        }
    }

    pub async fn initialize(&self) -> Result<SyncStatus, SyncServiceError> {
        self.coordinator.initialize().await?;
        self.coordinator.status().await
    }

    pub async fn status(&self) -> Result<SyncStatus, SyncServiceError> {
        self.coordinator.status().await
    }

    pub async fn history(
        &self,
        params: &crate::history::SearchParams,
    ) -> Result<Vec<crate::history::HistoryEntry>, SyncServiceError> {
        self.library
            .dictations(params)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn history_entry(
        &self,
        id: RecordId,
    ) -> Result<Option<crate::history::HistoryEntry>, SyncServiceError> {
        self.library
            .dictation(id)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn meetings(
        &self,
        query: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<LibraryMeeting>, SyncServiceError> {
        self.library
            .meetings(MeetingPageRequest {
                query: query.map(str::to_owned),
                offset,
                limit,
            })
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn meeting(&self, id: RecordId) -> Result<Option<LibraryMeeting>, SyncServiceError> {
        self.library
            .meeting(id)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn update_shared_meeting_title(
        &self,
        id: RecordId,
        title: String,
        expected_title_version: u64,
        title_source: Option<String>,
    ) -> Result<SharedMeeting, SyncServiceError> {
        self.library
            .update_shared_title(id, title, expected_title_version, title_source)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn update_meeting_title(
        &self,
        id: RecordId,
        title: String,
    ) -> Result<MeetingTitleResult, SyncServiceError> {
        self.library
            .update_meeting_title(id, title)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn regenerate_meeting_title(
        &self,
        id: RecordId,
    ) -> Result<Option<i64>, SyncServiceError> {
        self.library
            .regenerate_meeting_title(id)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub fn public_meeting_id(&self, local_id: i64) -> Result<RecordId, SyncServiceError> {
        self.library
            .public_meeting_id(local_id)
            .map_err(SyncServiceError::Data)
    }

    pub fn recent_meeting_titles(&self, limit: usize) -> Result<Vec<String>, SyncServiceError> {
        self.library
            .recent_meeting_titles(limit)
            .map_err(SyncServiceError::Data)
    }

    pub async fn prepare_meeting_retry(
        &self,
        id: RecordId,
    ) -> Result<RetryMeetingResult, SyncServiceError> {
        self.library
            .prepare_meeting_retry(id)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn meeting_artifacts(
        &self,
        meeting_id: RecordId,
    ) -> Result<Vec<crate::db::meeting_artifacts::MeetingArtifact>, SyncServiceError> {
        self.library
            .artifacts(meeting_id)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn meeting_artifact(
        &self,
        meeting_id: RecordId,
        artifact_id: RecordId,
    ) -> Result<Option<crate::db::meeting_artifacts::MeetingArtifact>, SyncServiceError> {
        self.library
            .artifact(meeting_id, artifact_id)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn generate_meeting_artifact(
        &self,
        meeting_id: RecordId,
        request: crate::meeting_artifacts::GenerateArtifactRequest,
    ) -> Result<crate::db::meeting_artifacts::MeetingArtifact, SyncServiceError> {
        self.library
            .generate_artifact(meeting_id, request)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn delete_meeting_artifact(
        &self,
        meeting_id: RecordId,
        artifact_id: RecordId,
    ) -> Result<ArtifactDeleteResult, SyncServiceError> {
        self.library
            .delete_artifact(meeting_id, artifact_id)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn delete_shared_record(
        &self,
        id: RecordId,
        kind: RecordKind,
    ) -> Result<(), SyncServiceError> {
        self.library
            .delete_shared_record(id, kind)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn delete_meeting(&self, id: RecordId) -> Result<DeleteResult, SyncServiceError> {
        self.library
            .delete_meeting(id)
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn payload(
        &self,
        id: RecordId,
        kind: RecordKind,
        range: Option<&str>,
    ) -> Result<Option<LibraryPayload>, SyncServiceError> {
        self.library
            .payload(PayloadRequest {
                id,
                kind,
                range: range.map(str::to_owned),
            })
            .await
            .map_err(SyncServiceError::Data)
    }

    pub async fn retry(&self) -> Result<u64, SyncServiceError> {
        self.coordinator.retry().await
    }

    pub async fn update_recording_payload_policy(
        &self,
        enabled: bool,
    ) -> Result<bool, SyncServiceError> {
        self.coordinator
            .update_recording_payload_policy(enabled)
            .await
    }

    pub async fn discover(&self) -> Result<SyncSetupResult, SyncServiceError> {
        self.coordinator.discover().await
    }

    pub async fn configure(
        &self,
        request: SyncSetupRequest,
    ) -> Result<SyncSetupResult, SyncServiceError> {
        let receipt = self.coordinator.configure(request).await?;
        Ok(self.coordinator.enrich_configure_receipt(receipt).await)
    }

    pub async fn shutdown(&self) -> Result<(), SyncServiceError> {
        self.coordinator.shutdown().await
    }
}
