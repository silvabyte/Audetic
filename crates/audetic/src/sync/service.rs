//! Thin route-facing composition facade for the Library Sync domain.

use audetic_core::sync::{RecordId, SyncRole, SyncSetupRequest, SyncSetupResult, SyncStatus};

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use super::client::NetworkHubAdapter;
use super::library::HubLibrary;
use super::protocol::{MeetingTitlePatch, RecordKind, SharedMeeting, HUB_LISTENER_ADDRESS};
use super::tailscale::{SystemCommandRunner, Tailscale, TailscaleControl};
use super::transition::RoleCoordinator;
use super::transport::{HubCapabilities, StreamingPayloadResponse};

pub use super::transition::TransitionError as SyncServiceError;

pub enum PayloadSource {
    Local(crate::db::shared_library::LibraryBlobRecord),
    Remote(StreamingPayloadResponse),
}

#[derive(Clone)]
pub struct SyncService {
    #[cfg(test)]
    pub(super) coordinator: RoleCoordinator,
    #[cfg(not(test))]
    coordinator: RoleCoordinator,
}

impl SyncService {
    pub(crate) fn db_path(&self) -> &std::path::Path {
        self.coordinator.db_path()
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
        Self {
            coordinator: RoleCoordinator::new(
                db_path,
                tailscale,
                hub_capabilities,
                hub_bind_address,
            ),
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
        let access = self.coordinator.library_access()?;
        let settings = access.settings.clone();
        let result = super::library_reader::LibraryReader::new(
            access.db_path.clone(),
            access.capabilities.dictations(),
        )
        .read(&settings, params)
        .await
        .map_err(SyncServiceError::Data)?;
        self.coordinator
            .record_library_observation(&access, result.hub_reachable, result.error.as_deref())
            .await?;
        Ok(result.entries)
    }

    pub async fn history_entry(
        &self,
        id: RecordId,
    ) -> Result<Option<crate::history::HistoryEntry>, SyncServiceError> {
        let mut offset = 0usize;
        loop {
            let mut params = crate::history::SearchParams::new().with_limit(100);
            params.offset = offset;
            let page = self.history(&params).await?;
            if let Some(entry) = page.iter().find(|entry| entry.id == id) {
                return Ok(Some(entry.clone()));
            }
            if page.len() < 100 {
                return Ok(None);
            }
            offset = offset.saturating_add(page.len());
        }
    }

    pub async fn meetings(
        &self,
        query: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<super::library_reader::LibraryMeeting>, SyncServiceError> {
        let access = self.coordinator.library_access()?;
        let settings = access.settings.clone();
        let result = super::library_reader::MeetingLibraryReader::new(
            access.db_path.clone(),
            access.capabilities.meetings(),
        )
        .read(&settings, query, offset, limit)
        .await
        .map_err(SyncServiceError::Data)?;
        self.coordinator
            .record_library_observation(&access, result.hub_reachable, result.error.as_deref())
            .await?;
        Ok(result.meetings)
    }

    pub async fn meeting(
        &self,
        id: RecordId,
    ) -> Result<Option<super::library_reader::LibraryMeeting>, SyncServiceError> {
        let mut offset = 0;
        loop {
            let page = self.meetings(None, offset, 100).await?;
            if let Some(value) = page.iter().find(|value| value.id == id) {
                return Ok(Some(value.clone()));
            }
            if page.len() < 100 {
                return Ok(None);
            }
            offset += page.len();
        }
    }

    pub async fn update_shared_meeting_title(
        &self,
        id: RecordId,
        title: String,
        expected_title_version: u64,
        title_source: Option<String>,
    ) -> Result<SharedMeeting, SyncServiceError> {
        let access = self.coordinator.library_access()?;
        let settings = access.settings;
        let patch = MeetingTitlePatch {
            title,
            expected_title_version,
            title_source,
        };
        match settings.role {
            SyncRole::Standalone => Err(SyncServiceError::InvalidRequest(
                "meeting is not shared".into(),
            )),
            SyncRole::HomeHub => HubLibrary::new(access.db_path)
                .update_meeting_title(id, &patch)
                .map_err(SyncServiceError::Data)?
                .ok_or_else(|| SyncServiceError::InvalidRequest("meeting not found".into())),
            SyncRole::ConnectedDevice => access
                .capabilities
                .mutations()
                .update_meeting_title(settings.hub.as_ref().expect("connected hub"), id, patch)
                .await
                .map_err(SyncServiceError::Hub),
        }
    }

    pub async fn delete_shared_record(
        &self,
        id: RecordId,
        kind: RecordKind,
    ) -> Result<(), SyncServiceError> {
        let access = self.coordinator.library_access()?;
        let settings = access.settings;
        match settings.role {
            SyncRole::Standalone => Err(SyncServiceError::InvalidRequest(
                "record is not shared".into(),
            )),
            SyncRole::HomeHub => HubLibrary::new(access.db_path)
                .delete(id, kind)
                .map(|_| ())
                .map_err(|error| SyncServiceError::Data(anyhow::anyhow!(error))),
            SyncRole::ConnectedDevice => access
                .capabilities
                .mutations()
                .delete_record(settings.hub.as_ref().expect("connected hub"), id, kind)
                .await
                .map_err(SyncServiceError::Hub),
        }
    }

    pub async fn payload(
        &self,
        id: RecordId,
        kind: RecordKind,
        range: Option<&str>,
    ) -> Result<Option<PayloadSource>, SyncServiceError> {
        let access = self.coordinator.library_access()?;
        let settings = access.settings;
        match settings.role {
            SyncRole::Standalone => Ok(None),
            SyncRole::HomeHub => HubLibrary::new(access.db_path)
                .payload(id, kind)
                .map(|value| value.map(PayloadSource::Local))
                .map_err(SyncServiceError::Data),
            SyncRole::ConnectedDevice => access
                .capabilities
                .payloads()
                .stream_payload(
                    settings.hub.as_ref().expect("connected hub"),
                    id,
                    kind,
                    range,
                )
                .await
                .map(|value| Some(PayloadSource::Remote(value)))
                .map_err(SyncServiceError::Hub),
        }
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
        Ok(self
            .coordinator
            .configure(request)
            .await?
            .into_setup_result())
    }

    pub async fn shutdown(&self) -> Result<(), SyncServiceError> {
        self.coordinator.shutdown().await
    }
}
