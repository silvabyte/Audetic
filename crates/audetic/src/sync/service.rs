//! Thin route-facing composition facade for the Library Sync domain.

use anyhow::Context;
use audetic_core::sync::{
    RecordId, SyncDiscoveryFailure, SyncRole, SyncSetupRequest, SyncSetupResult, SyncStatus,
};

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use super::client::NetworkHubAdapter;
use super::library::HubLibrary;
use super::protocol::{
    MeetingTitlePatch, RecordKind, SharedMeeting, HUB_API_MOUNT_PATH, HUB_LISTENER_ADDRESS,
    TAILSCALE_HTTPS_PORT,
};
use super::runtime::RuntimeSet;
use super::serve::ServeManager;
use super::state::InstallationState;
use super::tailscale::{SystemCommandRunner, Tailscale, TailscaleControl};
use super::transition::RoleCoordinator;
use super::transport::{DiscoveryOutcome, HubCapabilities, StreamingPayloadResponse};

pub use super::transition::TransitionError as SyncServiceError;

pub enum PayloadSource {
    Local(crate::db::shared_library::LibraryBlobRecord),
    Remote(StreamingPayloadResponse),
}

#[derive(Clone)]
pub struct SyncService {
    pub(crate) state: InstallationState,
    pub(crate) coordinator: RoleCoordinator,
    serve: ServeManager,
    hub_capabilities: HubCapabilities,
}

impl SyncService {
    pub(crate) fn db_path(&self) -> &std::path::Path {
        self.state.db_path()
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
        let state = InstallationState::new(db_path);
        let serve = ServeManager::new(tailscale);
        let runtime = RuntimeSet::new(state.clone(), hub_capabilities.clone(), hub_bind_address);
        let coordinator = RoleCoordinator::new(
            state.clone(),
            runtime,
            serve.clone(),
            hub_capabilities.clone(),
        );
        Self {
            state,
            coordinator,
            serve,
            hub_capabilities,
        }
    }

    pub async fn initialize(&self) -> Result<SyncStatus, SyncServiceError> {
        self.coordinator.initialize().await?;
        self.status_unlocked().await
    }

    pub async fn status(&self) -> Result<SyncStatus, SyncServiceError> {
        self.status_unlocked().await
    }

    pub async fn history(
        &self,
        params: &crate::history::SearchParams,
    ) -> Result<Vec<crate::history::HistoryEntry>, SyncServiceError> {
        let installation = self.load()?;
        let settings = installation.settings;
        let result = super::library_reader::LibraryReader::new(
            self.state.db_path().to_path_buf(),
            self.hub_capabilities.dictations(),
        )
        .read(&settings, params)
        .await
        .map_err(SyncServiceError::Persistence)?;
        if settings.role != SyncRole::Standalone {
            self.coordinator
                .observe_contact(
                    installation.role_epoch,
                    result.hub_reachable,
                    result.error.as_deref(),
                )
                .await?;
        }
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
        let installation = self.load()?;
        let settings = installation.settings;
        let result = super::library_reader::MeetingLibraryReader::new(
            self.state.db_path().to_path_buf(),
            self.hub_capabilities.meetings(),
        )
        .read(&settings, query, offset, limit)
        .await
        .map_err(SyncServiceError::Persistence)?;
        if settings.role != SyncRole::Standalone {
            self.coordinator
                .observe_contact(
                    installation.role_epoch,
                    result.hub_reachable,
                    result.error.as_deref(),
                )
                .await?;
        }
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
        let installation = self.load()?;
        let settings = installation.settings;
        let patch = MeetingTitlePatch {
            title,
            expected_title_version,
            title_source,
        };
        match settings.role {
            SyncRole::Standalone => Err(SyncServiceError::InvalidRequest(
                "meeting is not shared".into(),
            )),
            SyncRole::HomeHub => HubLibrary::new(self.state.db_path().to_path_buf())
                .update_meeting_title(id, &patch)
                .map_err(SyncServiceError::Persistence)?
                .ok_or_else(|| SyncServiceError::InvalidRequest("meeting not found".into())),
            SyncRole::ConnectedDevice => self
                .hub_capabilities
                .mutations()
                .update_meeting_title(settings.hub.as_ref().expect("connected hub"), id, patch)
                .await
                .map_err(|error| SyncServiceError::HubVerification(error.to_string())),
        }
    }

    pub async fn delete_shared_record(
        &self,
        id: RecordId,
        kind: RecordKind,
    ) -> Result<(), SyncServiceError> {
        let installation = self.load()?;
        let settings = installation.settings;
        match settings.role {
            SyncRole::Standalone => Err(SyncServiceError::InvalidRequest(
                "record is not shared".into(),
            )),
            SyncRole::HomeHub => HubLibrary::new(self.state.db_path().to_path_buf())
                .delete(id, kind)
                .map(|_| ())
                .map_err(|error| SyncServiceError::Persistence(anyhow::anyhow!(error))),
            SyncRole::ConnectedDevice => self
                .hub_capabilities
                .mutations()
                .delete_record(settings.hub.as_ref().expect("connected hub"), id, kind)
                .await
                .map_err(|error| SyncServiceError::HubVerification(error.to_string())),
        }
    }

    pub async fn payload(
        &self,
        id: RecordId,
        kind: RecordKind,
        range: Option<&str>,
    ) -> Result<Option<PayloadSource>, SyncServiceError> {
        let installation = self.load()?;
        let settings = installation.settings;
        match settings.role {
            SyncRole::Standalone => Ok(None),
            SyncRole::HomeHub => HubLibrary::new(self.state.db_path().to_path_buf())
                .payload(id, kind)
                .map(|value| value.map(PayloadSource::Local))
                .map_err(SyncServiceError::Persistence),
            SyncRole::ConnectedDevice => self
                .hub_capabilities
                .payloads()
                .stream_payload(
                    settings.hub.as_ref().expect("connected hub"),
                    id,
                    kind,
                    range,
                )
                .await
                .map(|value| Some(PayloadSource::Remote(value)))
                .map_err(|error| SyncServiceError::HubVerification(error.to_string())),
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
        self.coordinator.ensure_running()?;
        let network = self
            .serve
            .discovery()
            .await
            .map_err(SyncServiceError::from)?;
        let (discovered_hubs, discovery_failures) = match self
            .hub_capabilities
            .probe()
            .discover(network.candidate_base_urls, &network.owner_login)
            .await
        {
            DiscoveryOutcome::None { failures } => (
                Vec::new(),
                failures
                    .into_iter()
                    .map(|failure| SyncDiscoveryFailure {
                        candidate: failure.candidate,
                        reason: failure.reason,
                    })
                    .collect(),
            ),
            DiscoveryOutcome::One(candidate) => (vec![candidate], Vec::new()),
            DiscoveryOutcome::Multiple(candidates) => (candidates, Vec::new()),
        };
        Ok(SyncSetupResult {
            status: self.status_unlocked().await?,
            discovered_hubs,
            discovery_failures,
            setup_command: None,
            serve_preview: None,
        })
    }

    pub async fn configure(
        &self,
        request: SyncSetupRequest,
    ) -> Result<SyncSetupResult, SyncServiceError> {
        let outcome = self.coordinator.configure(request).await?;
        let status = self.status_unlocked().await?;
        let setup_command = if status.role == SyncRole::HomeHub {
            status
                .network
                .dns_name
                .as_deref()
                .zip(status.local_hub_id)
                .map(|(dns_name, hub_id)| connected_setup_command(dns_name, hub_id))
        } else {
            None
        };
        Ok(SyncSetupResult {
            status,
            discovered_hubs: Vec::new(),
            discovery_failures: Vec::new(),
            setup_command,
            serve_preview: outcome.serve_preview,
        })
    }

    pub async fn shutdown(&self) -> Result<(), SyncServiceError> {
        self.coordinator.shutdown().await
    }

    async fn status_unlocked(&self) -> Result<SyncStatus, SyncServiceError> {
        let installation = self.load()?;
        let identity = installation.identity;
        let settings = installation.settings;
        let runtime = self.coordinator.runtime_snapshot().await;
        let runtime_is_current = runtime.role_epoch == Some(installation.role_epoch);
        let reachable = match settings.role {
            SyncRole::Standalone => false,
            SyncRole::HomeHub => {
                runtime_is_current && runtime.hub_listener_running && runtime.hub_reachable
            }
            SyncRole::ConnectedDevice => runtime_is_current && runtime.hub_reachable,
        };
        let connection = crate::db::open_db_at(self.state.db_path())
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        let (pending_items, outbox_error) =
            crate::db::sync_outbox::SyncOutboxRepository::counts(&connection)
                .map_err(SyncServiceError::Persistence)?;
        let pending_bytes =
            crate::db::sync_outbox::SyncOutboxRepository::pending_bytes(&connection)
                .map_err(SyncServiceError::Persistence)?;
        Ok(SyncStatus {
            device_id: identity.device_id,
            role: settings.role,
            device_name: settings.device_name,
            local_hub_id: identity.hub_id,
            hub: settings.hub,
            hub_reachable: reachable,
            last_contact_at: settings.last_contact_at,
            pending_items,
            pending_bytes,
            last_error: outbox_error
                .or(runtime.listener_error)
                .or(settings.last_error),
            upload_recording_payloads: settings.upload_recording_payloads,
            cache_level: settings.cache_level,
            shared_config_enabled: settings.shared_config_enabled,
            applied_shared_config_version: settings.shared_config_version,
            network: self.serve.network_assessment().await,
        })
    }

    fn load(&self) -> Result<super::state::InstallationSnapshot, SyncServiceError> {
        self.state.load().map_err(SyncServiceError::Persistence)
    }
}

fn connected_setup_command(dns_name: &str, hub_id: audetic_core::sync::HubId) -> String {
    format!(
        "audetic setup --sync-role connected-device --hub-url https://{dns_name}:{TAILSCALE_HTTPS_PORT}{HUB_API_MOUNT_PATH} --hub-id {hub_id}"
    )
}
