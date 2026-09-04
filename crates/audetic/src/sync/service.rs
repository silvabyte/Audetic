use anyhow::Context;
use async_trait::async_trait;
use audetic_core::sync::{
    HubCandidate, HubConnection, RecordId, ServeMappingState, SyncDiscoveryFailure,
    SyncNetworkAssessment, SyncRole, SyncSetupRequest, SyncSetupResult, SyncStatus,
};
use thiserror::Error;
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::db::sync_identity::{SyncIdentity, SyncIdentityRepository};
use crate::db::sync_serve::{SyncServeOwnership, SyncServeRepository};
use crate::db::sync_settings::{SyncSettings, SyncSettingsRepository};
use crate::sync::is_exact_audetic_serve_ownership;

use super::client::{
    canonicalize_base_url, discover_hubs, DiscoveryOutcome, HandshakeExpectation, HubClient,
    ReqwestHubTransport,
};
use super::library::HubLibrary;
use super::outbox::OutboxWorker;
use super::protocol::{DictationPage, SnapshotBatch, SnapshotBatchResponse};
use super::protocol::{
    HUB_API_MOUNT_PATH, HUB_LISTENER_ADDRESS, HUB_LOOPBACK_BASE_URL, TAILSCALE_HTTPS_PORT,
};
use super::server::{HubServer, HubServerConfig};
use super::tailscale::{
    MappingState, ServeAssessment, SystemCommandRunner, Tailscale, TailscaleControl,
    TailscaleError, TailscaleStatus,
};

#[async_trait]
pub trait HubAccess: Send + Sync {
    async fn handshake(&self, hub: &HubConnection) -> Result<HubCandidate, String>;
    async fn discover(
        &self,
        candidates: Vec<String>,
        expected_owner_login: &str,
    ) -> DiscoveryOutcome;

    async fn upload_snapshots(
        &self,
        _hub: &HubConnection,
        _batch: SnapshotBatch,
    ) -> Result<SnapshotBatchResponse, HubTransferError> {
        Err(HubTransferError::NeedsAttention(
            "snapshot upload is unavailable".to_owned(),
        ))
    }

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
}

#[derive(Clone, Debug, Error)]
pub enum HubTransferError {
    #[error("{0}")]
    Retryable(String),
    #[error("{0}")]
    NeedsAttention(String),
}

#[derive(Default)]
struct NetworkHubAccess;

#[async_trait]
impl HubAccess for NetworkHubAccess {
    async fn handshake(&self, hub: &HubConnection) -> Result<HubCandidate, String> {
        HubClient::new(&hub.base_url)
            .map_err(|error| error.to_string())?
            .handshake(HandshakeExpectation {
                hub_id: Some(hub.hub_id),
                owner_login: Some(&hub.owner_login),
            })
            .await
            .map_err(|error| error.to_string())
    }

    async fn discover(
        &self,
        candidates: Vec<String>,
        expected_owner_login: &str,
    ) -> DiscoveryOutcome {
        let transport = match ReqwestHubTransport::new() {
            Ok(transport) => transport,
            Err(error) => {
                return DiscoveryOutcome::None {
                    failures: vec![super::client::DiscoveryFailure {
                        candidate: "Tailscale peers".to_owned(),
                        reason: error.to_string(),
                    }],
                };
            }
        };
        discover_hubs(transport, candidates, expected_owner_login).await
    }

    async fn upload_snapshots(
        &self,
        hub: &HubConnection,
        batch: SnapshotBatch,
    ) -> Result<SnapshotBatchResponse, HubTransferError> {
        let client = HubClient::new(&hub.base_url)
            .map_err(|error| HubTransferError::NeedsAttention(error.to_string()))?;
        client
            .upload_snapshots(hub.hub_id, &batch)
            .await
            .map_err(classify_client_error)
    }

    async fn page_dictations(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DictationPage, HubTransferError> {
        let client = HubClient::new(&hub.base_url)
            .map_err(|error| HubTransferError::NeedsAttention(error.to_string()))?;
        client
            .page_dictations(hub.hub_id, query, from, to, cursor, limit)
            .await
            .map_err(classify_client_error)
    }
}

fn classify_client_error(error: super::client::HubClientError) -> HubTransferError {
    use super::client::HubClientError;
    match &error {
        HubClientError::Transport(_) => HubTransferError::Retryable(error.to_string()),
        HubClientError::Http { status, .. } if *status >= 500 => {
            HubTransferError::Retryable(error.to_string())
        }
        _ => HubTransferError::NeedsAttention(error.to_string()),
    }
}

#[derive(Debug, Error)]
pub enum SyncServiceError {
    #[error("invalid sync request: {0}")]
    InvalidRequest(String),
    #[error("invalid sync role transition: {0}")]
    InvalidTransition(String),
    #[error("Tailscale is not ready: {0}")]
    Tailscale(#[from] TailscaleError),
    #[error("Home Hub verification failed: {0}")]
    HubVerification(String),
    #[error("Home Hub listener failed: {0}")]
    Listener(String),
    #[error("sync runtime task failed: {0}")]
    RuntimeTask(String),
    #[error("sync persistence failed: {0}")]
    Persistence(#[source] anyhow::Error),
    #[error("sync service has shut down")]
    Shutdown,
    #[error("activation failed ({source_error}); rollback also failed ({rollback_error})")]
    Rollback {
        source_error: String,
        rollback_error: String,
    },
}

impl SyncServiceError {
    pub const fn is_request_error(&self) -> bool {
        matches!(self, Self::InvalidRequest(_) | Self::InvalidTransition(_))
    }

    pub const fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::Tailscale(TailscaleError::ServeCollision)
                | Self::Tailscale(TailscaleError::FunnelEnabled)
        )
    }

    pub const fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Tailscale(_)
                | Self::HubVerification(_)
                | Self::Listener(_)
                | Self::RuntimeTask(_)
                | Self::Shutdown
                | Self::Rollback { .. }
        )
    }
}

struct HubRuntime {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<Result<(), super::server::HubServerError>>,
}

struct OutboxRuntime {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct PreparedOutboxRuntime {
    start: oneshot::Sender<()>,
    runtime: OutboxRuntime,
}

/// Single owner of durable sync settings and all role-dependent runtime tasks.
///
/// The transition mutex covers preview/verify/apply/commit/rollback as one
/// serialized operation. SQLite transactions are kept short and never cross an
/// await point.
pub struct SyncService {
    db_path: PathBuf,
    tailscale: Arc<dyn TailscaleControl>,
    hubs: Arc<dyn HubAccess>,
    hub_bind_address: SocketAddr,
    transition: Mutex<()>,
    hub_runtime: Mutex<Option<HubRuntime>>,
    outbox_runtime: Mutex<Option<OutboxRuntime>>,
    hub_reachable: RwLock<bool>,
    shut_down: AtomicBool,
}

impl SyncService {
    pub fn production(db_path: PathBuf) -> Self {
        Self::with_dependencies(
            db_path,
            Arc::new(Tailscale::new(SystemCommandRunner)),
            Arc::new(NetworkHubAccess),
            HUB_LISTENER_ADDRESS
                .parse()
                .expect("valid listener address"),
        )
    }

    pub fn with_dependencies(
        db_path: PathBuf,
        tailscale: Arc<dyn TailscaleControl>,
        hubs: Arc<dyn HubAccess>,
        hub_bind_address: SocketAddr,
    ) -> Self {
        Self {
            db_path,
            tailscale,
            hubs,
            hub_bind_address,
            transition: Mutex::new(()),
            hub_runtime: Mutex::new(None),
            outbox_runtime: Mutex::new(None),
            hub_reachable: RwLock::new(false),
            shut_down: AtomicBool::new(false),
        }
    }

    /// Reconstruct the persisted role. Network/listener failures are recorded
    /// as degraded status and deliberately do not fail daemon startup.
    pub async fn initialize(&self) -> Result<SyncStatus, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
        let (identity, mut settings) = self.load()?;
        let startup_error = match settings.role {
            SyncRole::Standalone => None,
            SyncRole::HomeHub => self.reconstruct_home_hub(&identity, &settings).await.err(),
            SyncRole::ConnectedDevice => self.verify_connected_device(&settings).await.err(),
        };

        settings.last_error = startup_error.as_ref().map(ToString::to_string);
        if startup_error.is_none() && settings.role != SyncRole::Standalone {
            settings.last_contact_at = Some(now());
        }
        self.save_settings(&settings)?;
        if settings.role != SyncRole::Standalone {
            self.activate_dictation_transfer(&settings).await?;
        }
        self.status_unlocked().await
    }

    pub async fn status(&self) -> Result<SyncStatus, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.status_unlocked().await
    }

    pub async fn history(
        &self,
        params: &crate::history::SearchParams,
    ) -> Result<Vec<crate::history::HistoryEntry>, SyncServiceError> {
        let _transition = self.transition.lock().await;
        let (_, settings) = self.load()?;
        let result =
            super::library_reader::LibraryReader::new(self.db_path.clone(), Arc::clone(&self.hubs))
                .read(&settings, params)
                .await
                .map_err(SyncServiceError::Persistence)?;
        *self.hub_reachable.write().await = result.hub_reachable;
        if settings.role != SyncRole::Standalone {
            let mut contact = settings;
            if result.hub_reachable {
                contact.last_contact_at = Some(now());
                contact.last_error = None;
            } else if let Some(error) = result.error {
                contact.last_error = Some(error);
            }
            self.save_settings(&contact)?;
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
                break;
            }
            offset = offset.saturating_add(page.len());
        }
        Ok(None)
    }

    pub async fn retry(&self) -> Result<u64, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
        let connection = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        crate::db::sync_outbox::SyncOutboxRepository::retry_all(&connection)
            .map(|count| count as u64)
            .map_err(SyncServiceError::Persistence)
    }

    pub async fn discover(&self) -> Result<SyncSetupResult, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
        let status = self.tailscale_status().await?;
        require_running(&status)?;
        let candidates = status
            .discoverable_peers()
            .map(|peer| peer.audetic_base_url())
            .collect();
        let (discovered_hubs, discovery_failures) =
            match self.hubs.discover(candidates, &status.owner_login).await {
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
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
        let (_, current) = self.load()?;
        let serve_preview =
            (request.role == SyncRole::HomeHub).then(|| self.tailscale.serve_preview());

        match request.role {
            SyncRole::Standalone => self.configure_standalone(request, current).await?,
            SyncRole::HomeHub => {
                if current.role == SyncRole::ConnectedDevice {
                    return Err(SyncServiceError::InvalidTransition(
                        "demote the Connected Device to Standalone before promotion".into(),
                    ));
                }
                if current.role == SyncRole::Standalone && !request.confirm_serve_change {
                    self.assess_home_hub_ready().await?;
                    return Ok(SyncSetupResult {
                        status: self.status_unlocked().await?,
                        discovered_hubs: Vec::new(),
                        discovery_failures: Vec::new(),
                        setup_command: None,
                        serve_preview,
                    });
                }
                self.configure_home_hub(request, current).await?;
            }
            SyncRole::ConnectedDevice => {
                if current.role == SyncRole::HomeHub {
                    return Err(SyncServiceError::InvalidTransition(
                        "demote the Home Hub to Standalone before connecting to another hub".into(),
                    ));
                }
                self.configure_connected_device(request).await?;
            }
        }

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
            serve_preview,
        })
    }

    pub async fn shutdown(&self) -> Result<(), SyncServiceError> {
        let _transition = self.transition.lock().await;
        if self.shut_down.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        *self.hub_reachable.write().await = false;
        self.stop_outbox_runtime().await;
        self.stop_hub_runtime().await
    }

    async fn configure_standalone(
        &self,
        request: SyncSetupRequest,
        current: SyncSettings,
    ) -> Result<(), SyncServiceError> {
        if request.hub.is_some() {
            return Err(SyncServiceError::InvalidRequest(
                "Standalone settings cannot contain a Home Hub connection".into(),
            ));
        }

        let ownership = self.load_ownership()?;
        let removed_mapping = if current.role == SyncRole::HomeHub {
            if ownership
                .as_ref()
                .is_some_and(is_exact_audetic_serve_ownership)
            {
                self.tailscale_remove().await?
            } else {
                false
            }
        } else {
            false
        };

        let settings = settings_from_request(request, None);
        if let Err(error) = self.commit_settings_and_ownership(&settings, None, true) {
            if removed_mapping {
                if let Err(rollback) = self.tailscale_apply().await {
                    return Err(SyncServiceError::Rollback {
                        source_error: error.to_string(),
                        rollback_error: rollback.to_string(),
                    });
                }
            }
            return Err(error);
        }
        if current.role == SyncRole::HomeHub {
            self.stop_hub_runtime().await?;
        }
        self.stop_outbox_runtime().await;
        *self.hub_reachable.write().await = false;
        Ok(())
    }

    async fn configure_home_hub(
        &self,
        request: SyncSetupRequest,
        current: SyncSettings,
    ) -> Result<(), SyncServiceError> {
        if request.hub.is_some() {
            return Err(SyncServiceError::InvalidRequest(
                "Home Hub settings cannot contain another hub connection".into(),
            ));
        }
        let tailscale_status = self.assess_home_hub_ready().await?;
        let identity = self.load()?.0;
        let hub_id = identity.hub_id.unwrap_or_default();
        let owner_login = tailscale_status.owner_login.clone();

        if current.role == SyncRole::HomeHub
            && identity.owner_login.as_deref() != Some(owner_login.as_str())
        {
            return Err(SyncServiceError::InvalidRequest(
                "the current Tailscale owner differs from the persisted Home Hub owner; demote explicitly before changing ownership".into(),
            ));
        }

        let runtime_was_running = self.hub_runtime_running().await;
        let was_reachable = *self.hub_reachable.read().await;
        self.start_hub_runtime(hub_id, &owner_login, request.device_name.as_deref())
            .await?;
        let mapping_created = match self
            .verify_home_hub_network(&tailscale_status, hub_id, &owner_login)
            .await
        {
            Ok(mapping_created) => mapping_created,
            Err((error, mapping_created)) => {
                return Err(self
                    .rollback_home_hub_activation(
                        None,
                        runtime_was_running,
                        was_reachable,
                        mapping_created,
                        error,
                    )
                    .await)
            }
        };

        let mut settings = settings_from_request(request, None);
        settings.shared_config_enabled = true;
        settings.last_contact_at = Some(now());
        let prepared = match self.prepare_dictation_transfer(&settings).await {
            Ok(prepared) => prepared,
            Err(error) => {
                return Err(self
                    .rollback_home_hub_activation(
                        None,
                        runtime_was_running,
                        was_reachable,
                        mapping_created,
                        error,
                    )
                    .await)
            }
        };
        if let Err(error) = self.commit_home_hub(&settings, hub_id, &owner_login) {
            return Err(self
                .rollback_home_hub_activation(
                    Some(prepared),
                    runtime_was_running,
                    was_reachable,
                    mapping_created,
                    error,
                )
                .await);
        }
        self.activate_prepared_outbox(prepared).await;
        *self.hub_reachable.write().await = true;
        Ok(())
    }

    async fn verify_home_hub_network(
        &self,
        tailscale_status: &TailscaleStatus,
        hub_id: audetic_core::sync::HubId,
        owner_login: &str,
    ) -> Result<bool, (SyncServiceError, bool)> {
        let mapping_created = self
            .tailscale_apply()
            .await
            .map_err(|error| (error, false))?;
        let assessment = self
            .tailscale_serve_assessment()
            .await
            .map_err(|error| (error, mapping_created))?;
        if assessment.mapping != MappingState::OwnedByAudetic {
            return Err((
                SyncServiceError::HubVerification(
                    "Tailscale Serve did not retain the exact Audetic mapping".into(),
                ),
                mapping_created,
            ));
        }
        if assessment.funnel_enabled {
            return Err((
                SyncServiceError::Tailscale(TailscaleError::FunnelEnabled),
                mapping_created,
            ));
        }

        let connection = HubConnection {
            base_url: format!(
                "https://{}:{TAILSCALE_HTTPS_PORT}{HUB_API_MOUNT_PATH}",
                tailscale_status.self_dns_name.trim_end_matches('.')
            ),
            hub_id,
            owner_login: owner_login.to_owned(),
        };
        self.hubs
            .handshake(&connection)
            .await
            .map_err(|error| (SyncServiceError::HubVerification(error), mapping_created))?;
        Ok(mapping_created)
    }

    async fn configure_connected_device(
        &self,
        request: SyncSetupRequest,
    ) -> Result<(), SyncServiceError> {
        let mut requested_hub = request.hub.clone().ok_or_else(|| {
            SyncServiceError::InvalidRequest(
                "Connected Device settings require a Home Hub connection".into(),
            )
        })?;
        requested_hub.base_url = canonicalize_base_url(&requested_hub.base_url)
            .map_err(|error| SyncServiceError::InvalidRequest(error.to_string()))?
            .to_string();
        let tailscale = self.tailscale_status().await?;
        require_running(&tailscale)?;
        if tailscale.owner_login != requested_hub.owner_login {
            return Err(SyncServiceError::InvalidRequest(format!(
                "the local Tailscale owner {:?} does not match the Home Hub owner {:?}",
                tailscale.owner_login, requested_hub.owner_login
            )));
        }
        let candidate = self
            .hubs
            .handshake(&requested_hub)
            .await
            .map_err(SyncServiceError::HubVerification)?;
        if candidate.connection.hub_id != requested_hub.hub_id
            || candidate.connection.owner_login != requested_hub.owner_login
        {
            return Err(SyncServiceError::HubVerification(
                "Home Hub identity changed during verification".into(),
            ));
        }
        let mut settings = settings_from_request(request, Some(candidate.connection));
        settings.last_contact_at = Some(now());
        settings.last_error = None;
        let was_reachable = *self.hub_reachable.read().await;
        let prepared = self.prepare_dictation_transfer(&settings).await?;
        if let Err(error) = self.commit_connected_device(&settings) {
            self.cancel_prepared_outbox(prepared).await;
            *self.hub_reachable.write().await = was_reachable;
            return Err(error);
        }
        self.activate_prepared_outbox(prepared).await;
        *self.hub_reachable.write().await = true;
        Ok(())
    }

    async fn reconstruct_home_hub(
        &self,
        identity: &SyncIdentity,
        settings: &SyncSettings,
    ) -> Result<(), SyncServiceError> {
        let hub_id = identity.hub_id.ok_or_else(|| {
            SyncServiceError::Persistence(anyhow::anyhow!("persisted Home Hub role has no Hub ID"))
        })?;
        let owner_login = identity.owner_login.as_deref().ok_or_else(|| {
            SyncServiceError::Persistence(anyhow::anyhow!(
                "persisted Home Hub role has no owner login"
            ))
        })?;
        self.start_hub_runtime(hub_id, owner_login, settings.device_name.as_deref())
            .await?;

        let status = self.assess_home_hub_ready().await?;
        let assessment = self.tailscale_serve_assessment().await?;
        if assessment.mapping != MappingState::OwnedByAudetic || assessment.funnel_enabled {
            return Err(SyncServiceError::HubVerification(
                "persisted Home Hub Serve mapping is missing, changed, or exposed by Funnel".into(),
            ));
        }
        if !self
            .load_ownership()?
            .as_ref()
            .is_some_and(is_exact_audetic_serve_ownership)
        {
            return Err(SyncServiceError::HubVerification(
                "persisted Home Hub has no exact Audetic Serve ownership record".into(),
            ));
        }
        let connection = HubConnection {
            base_url: format!(
                "https://{}:{TAILSCALE_HTTPS_PORT}{HUB_API_MOUNT_PATH}",
                status.self_dns_name.trim_end_matches('.')
            ),
            hub_id,
            owner_login: owner_login.to_owned(),
        };
        self.hubs
            .handshake(&connection)
            .await
            .map_err(SyncServiceError::HubVerification)?;
        *self.hub_reachable.write().await = true;
        Ok(())
    }

    async fn verify_connected_device(
        &self,
        settings: &SyncSettings,
    ) -> Result<(), SyncServiceError> {
        let hub = settings.hub.as_ref().ok_or_else(|| {
            SyncServiceError::Persistence(anyhow::anyhow!(
                "persisted Connected Device role has no Home Hub"
            ))
        })?;
        self.hubs
            .handshake(hub)
            .await
            .map_err(SyncServiceError::HubVerification)?;
        *self.hub_reachable.write().await = true;
        Ok(())
    }

    async fn assess_home_hub_ready(&self) -> Result<TailscaleStatus, SyncServiceError> {
        let status = self.tailscale_status().await?;
        require_running(&status)?;
        let assessment = self.tailscale_serve_assessment().await?;
        if assessment.funnel_enabled {
            return Err(TailscaleError::FunnelEnabled.into());
        }
        if assessment.mapping == MappingState::Collision {
            return Err(TailscaleError::ServeCollision.into());
        }
        Ok(status)
    }

    async fn status_unlocked(&self) -> Result<SyncStatus, SyncServiceError> {
        let (identity, settings) = self.load()?;
        let runtime_running = self.hub_runtime_running().await;
        let reachable = match settings.role {
            SyncRole::Standalone => false,
            SyncRole::HomeHub => runtime_running && *self.hub_reachable.read().await,
            SyncRole::ConnectedDevice => *self.hub_reachable.read().await,
        };
        let connection = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        let (pending_items, outbox_error) =
            crate::db::sync_outbox::SyncOutboxRepository::counts(&connection)
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
            pending_bytes: 0,
            last_error: outbox_error.or(settings.last_error),
            upload_recording_payloads: settings.upload_recording_payloads,
            cache_level: settings.cache_level,
            shared_config_enabled: settings.shared_config_enabled,
            applied_shared_config_version: settings.shared_config_version,
            network: self.network_assessment().await,
        })
    }

    async fn network_assessment(&self) -> SyncNetworkAssessment {
        let preview = self.tailscale.serve_preview();
        let status = match self.tailscale_status().await {
            Ok(status) => status,
            Err(error) => return failed_network_assessment(preview, error.to_string()),
        };
        let assessment = match self.tailscale_serve_assessment().await {
            Ok(assessment) => assessment,
            Err(error) => {
                return SyncNetworkAssessment {
                    ready: false,
                    backend_state: Some(status.backend_state),
                    dns_name: Some(status.self_dns_name.trim_end_matches('.').to_owned()),
                    owner_login: Some(status.owner_login),
                    serve_mapping: None,
                    funnel_enabled: None,
                    serve_preview: preview,
                    error: Some(error.to_string()),
                };
            }
        };
        SyncNetworkAssessment {
            ready: status.backend_state == "Running"
                && assessment.mapping != MappingState::Collision
                && !assessment.funnel_enabled,
            backend_state: Some(status.backend_state),
            dns_name: Some(status.self_dns_name.trim_end_matches('.').to_owned()),
            owner_login: Some(status.owner_login),
            serve_mapping: Some(mapping_state(assessment.mapping)),
            funnel_enabled: Some(assessment.funnel_enabled),
            serve_preview: preview,
            error: None,
        }
    }

    async fn tailscale_status(&self) -> Result<TailscaleStatus, SyncServiceError> {
        let tailscale = Arc::clone(&self.tailscale);
        tokio::task::spawn_blocking(move || tailscale.status())
            .await
            .map_err(|error| SyncServiceError::RuntimeTask(error.to_string()))?
            .map_err(SyncServiceError::Tailscale)
    }

    async fn tailscale_serve_assessment(&self) -> Result<ServeAssessment, SyncServiceError> {
        let tailscale = Arc::clone(&self.tailscale);
        tokio::task::spawn_blocking(move || tailscale.serve_assessment())
            .await
            .map_err(|error| SyncServiceError::RuntimeTask(error.to_string()))?
            .map_err(SyncServiceError::Tailscale)
    }

    async fn tailscale_apply(&self) -> Result<bool, SyncServiceError> {
        let tailscale = Arc::clone(&self.tailscale);
        tokio::task::spawn_blocking(move || tailscale.apply_audetic_serve())
            .await
            .map_err(|error| SyncServiceError::RuntimeTask(error.to_string()))?
            .map_err(SyncServiceError::Tailscale)
    }

    async fn tailscale_remove(&self) -> Result<bool, SyncServiceError> {
        let tailscale = Arc::clone(&self.tailscale);
        tokio::task::spawn_blocking(move || tailscale.remove_audetic_serve())
            .await
            .map_err(|error| SyncServiceError::RuntimeTask(error.to_string()))?
            .map_err(SyncServiceError::Tailscale)
    }

    async fn start_hub_runtime(
        &self,
        hub_id: audetic_core::sync::HubId,
        owner_login: &str,
        device_name: Option<&str>,
    ) -> Result<(), SyncServiceError> {
        if self.hub_runtime_running().await {
            return Ok(());
        }
        if !self.hub_bind_address.ip().is_loopback() {
            return Err(SyncServiceError::Listener(format!(
                "non-loopback bind address {}",
                self.hub_bind_address
            )));
        }
        let listener = tokio::net::TcpListener::bind(self.hub_bind_address)
            .await
            .map_err(|error| SyncServiceError::Listener(error.to_string()))?;
        let mut config = HubServerConfig::new(hub_id, owner_login)
            .map_err(|error| SyncServiceError::Listener(error.to_string()))?
            .with_library(HubLibrary::new(self.db_path.clone()));
        if let Some(device_name) = device_name {
            config = config.with_device_name(device_name);
        }
        let server = HubServer::new(config);
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            server
                .serve_with_shutdown(listener, async move {
                    let _ = receiver.await;
                })
                .await
        });
        *self.hub_runtime.lock().await = Some(HubRuntime { shutdown, task });
        Ok(())
    }

    async fn stop_hub_runtime(&self) -> Result<(), SyncServiceError> {
        let Some(runtime) = self.hub_runtime.lock().await.take() else {
            return Ok(());
        };
        let _ = runtime.shutdown.send(());
        runtime
            .task
            .await
            .map_err(|error| SyncServiceError::Listener(error.to_string()))?
            .map_err(|error| SyncServiceError::Listener(error.to_string()))
    }

    async fn hub_runtime_running(&self) -> bool {
        self.hub_runtime
            .lock()
            .await
            .as_ref()
            .is_some_and(|runtime| !runtime.task.is_finished())
    }

    async fn activate_dictation_transfer(
        &self,
        settings: &SyncSettings,
    ) -> Result<(), SyncServiceError> {
        let connection = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        crate::db::backfill_visible_dictations(&connection, settings.role)
            .map_err(SyncServiceError::Persistence)?;
        let prepared = self.prepare_dictation_transfer(settings).await?;
        self.activate_prepared_outbox(prepared).await;
        Ok(())
    }

    async fn prepare_dictation_transfer(
        &self,
        settings: &SyncSettings,
    ) -> Result<PreparedOutboxRuntime, SyncServiceError> {
        let worker = OutboxWorker::new(
            self.db_path.clone(),
            settings.role,
            settings.hub.clone(),
            Arc::clone(&self.hubs),
        );
        let (start, start_receiver) = oneshot::channel();
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            if start_receiver.await.is_ok() {
                worker.run(receiver).await;
            }
        });
        Ok(PreparedOutboxRuntime {
            start,
            runtime: OutboxRuntime { shutdown, task },
        })
    }

    async fn activate_prepared_outbox(&self, prepared: PreparedOutboxRuntime) {
        self.stop_outbox_runtime().await;
        let PreparedOutboxRuntime { start, runtime } = prepared;
        let _ = start.send(());
        *self.outbox_runtime.lock().await = Some(runtime);
    }

    async fn cancel_prepared_outbox(&self, prepared: PreparedOutboxRuntime) {
        let PreparedOutboxRuntime { start, runtime } = prepared;
        drop(start);
        let _ = runtime.shutdown.send(());
        let _ = runtime.task.await;
    }

    async fn rollback_home_hub_activation(
        &self,
        prepared: Option<PreparedOutboxRuntime>,
        runtime_was_running: bool,
        was_reachable: bool,
        mapping_created: bool,
        source_error: SyncServiceError,
    ) -> SyncServiceError {
        if let Some(prepared) = prepared {
            self.cancel_prepared_outbox(prepared).await;
        }
        let mut rollback_errors = Vec::new();
        if mapping_created {
            if let Err(error) = self.tailscale_remove().await {
                rollback_errors.push(error.to_string());
            }
        }
        if !runtime_was_running {
            if let Err(error) = self.stop_hub_runtime().await {
                rollback_errors.push(error.to_string());
            }
        }
        *self.hub_reachable.write().await = was_reachable;
        if rollback_errors.is_empty() {
            source_error
        } else {
            SyncServiceError::Rollback {
                source_error: source_error.to_string(),
                rollback_error: rollback_errors.join("; "),
            }
        }
    }

    async fn stop_outbox_runtime(&self) {
        let Some(runtime) = self.outbox_runtime.lock().await.take() else {
            return;
        };
        let _ = runtime.shutdown.send(());
        let _ = runtime.task.await;
    }

    fn load(&self) -> Result<(SyncIdentity, SyncSettings), SyncServiceError> {
        let conn = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        let identity = SyncIdentityRepository::get_or_create_device(&conn)
            .map_err(SyncServiceError::Persistence)?;
        let settings = SyncSettingsRepository::get(&conn).map_err(SyncServiceError::Persistence)?;
        Ok((identity, settings))
    }

    fn load_ownership(&self) -> Result<Option<SyncServeOwnership>, SyncServiceError> {
        let conn = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        SyncServeRepository::get(&conn).map_err(SyncServiceError::Persistence)
    }

    fn save_settings(&self, settings: &SyncSettings) -> Result<(), SyncServiceError> {
        let conn = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        SyncSettingsRepository::save(&conn, settings).map_err(SyncServiceError::Persistence)
    }

    fn commit_home_hub(
        &self,
        settings: &SyncSettings,
        hub_id: audetic_core::sync::HubId,
        owner_login: &str,
    ) -> Result<(), SyncServiceError> {
        let mut conn = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        let transaction = conn
            .transaction()
            .context("starting Home Hub settings transaction")
            .map_err(SyncServiceError::Persistence)?;
        SyncIdentityRepository::save_hub(&transaction, hub_id, owner_login)
            .map_err(SyncServiceError::Persistence)?;
        SyncSettingsRepository::save(&transaction, settings)
            .map_err(SyncServiceError::Persistence)?;
        SyncServeRepository::save(&transaction, &expected_ownership())
            .map_err(SyncServiceError::Persistence)?;
        crate::db::backfill_visible_dictations_in_transaction(&transaction, settings.role)
            .map_err(SyncServiceError::Persistence)?;
        transaction
            .commit()
            .context("committing Home Hub settings transaction")
            .map_err(SyncServiceError::Persistence)
    }

    fn commit_connected_device(&self, settings: &SyncSettings) -> Result<(), SyncServiceError> {
        let mut conn = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        let transaction = conn
            .transaction()
            .context("starting Connected Device settings transaction")
            .map_err(SyncServiceError::Persistence)?;
        SyncSettingsRepository::save(&transaction, settings)
            .map_err(SyncServiceError::Persistence)?;
        crate::db::backfill_visible_dictations_in_transaction(&transaction, settings.role)
            .map_err(SyncServiceError::Persistence)?;
        transaction
            .commit()
            .context("committing Connected Device settings transaction")
            .map_err(SyncServiceError::Persistence)
    }

    fn commit_settings_and_ownership(
        &self,
        settings: &SyncSettings,
        ownership: Option<&SyncServeOwnership>,
        clear_ownership: bool,
    ) -> Result<(), SyncServiceError> {
        let mut conn = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        let transaction = conn
            .transaction()
            .context("starting sync settings transaction")
            .map_err(SyncServiceError::Persistence)?;
        SyncSettingsRepository::save(&transaction, settings)
            .map_err(SyncServiceError::Persistence)?;
        if clear_ownership {
            SyncServeRepository::clear(&transaction).map_err(SyncServiceError::Persistence)?;
        } else if let Some(ownership) = ownership {
            SyncServeRepository::save(&transaction, ownership)
                .map_err(SyncServiceError::Persistence)?;
        }
        transaction
            .commit()
            .context("committing sync settings transaction")
            .map_err(SyncServiceError::Persistence)
    }

    fn ensure_running(&self) -> Result<(), SyncServiceError> {
        if self.shut_down.load(Ordering::SeqCst) {
            Err(SyncServiceError::Shutdown)
        } else {
            Ok(())
        }
    }
}

fn settings_from_request(request: SyncSetupRequest, hub: Option<HubConnection>) -> SyncSettings {
    SyncSettings {
        role: request.role,
        device_name: request.device_name,
        hub,
        upload_recording_payloads: request.upload_recording_payloads,
        cache_level: request.cache_level,
        shared_config_enabled: request.shared_config_enabled,
        ..SyncSettings::default()
    }
}

fn require_running(status: &TailscaleStatus) -> Result<(), SyncServiceError> {
    if status.backend_state == "Running" {
        Ok(())
    } else {
        Err(SyncServiceError::InvalidRequest(format!(
            "Tailscale backend is {:?}, expected Running",
            status.backend_state
        )))
    }
}

fn expected_ownership() -> SyncServeOwnership {
    SyncServeOwnership {
        https_port: TAILSCALE_HTTPS_PORT,
        mount_path: HUB_API_MOUNT_PATH.trim_end_matches('/').to_owned(),
        proxy_url: HUB_LOOPBACK_BASE_URL.to_owned(),
    }
}

fn mapping_state(mapping: MappingState) -> ServeMappingState {
    match mapping {
        MappingState::Vacant => ServeMappingState::Vacant,
        MappingState::OwnedByAudetic => ServeMappingState::Audetic,
        MappingState::Collision => ServeMappingState::Collision,
    }
}

fn failed_network_assessment(preview: String, error: String) -> SyncNetworkAssessment {
    SyncNetworkAssessment {
        ready: false,
        backend_state: None,
        dns_name: None,
        owner_login: None,
        serve_mapping: None,
        funnel_enabled: None,
        serve_preview: preview,
        error: Some(error),
    }
}

fn connected_setup_command(dns_name: &str, hub_id: audetic_core::sync::HubId) -> String {
    format!(
        "audetic setup --sync-role connected-device --hub-url https://{dns_name}:{TAILSCALE_HTTPS_PORT}{HUB_API_MOUNT_PATH} --hub-id {hub_id}"
    )
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use audetic_core::sync::{CacheLevel, DeviceId, HubId};
    use semver::Version;
    use std::sync::Mutex as StdMutex;

    use crate::sync::tailscale::ServeAssessment;

    struct FakeTailscale {
        mapping: StdMutex<MappingState>,
        fail_apply: AtomicBool,
        fail_status: AtomicBool,
        apply_calls: std::sync::atomic::AtomicUsize,
        remove_calls: std::sync::atomic::AtomicUsize,
    }

    impl Default for FakeTailscale {
        fn default() -> Self {
            Self {
                mapping: StdMutex::new(MappingState::Vacant),
                fail_apply: AtomicBool::new(false),
                fail_status: AtomicBool::new(false),
                apply_calls: std::sync::atomic::AtomicUsize::new(0),
                remove_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl FakeTailscale {
        fn status_value() -> TailscaleStatus {
            TailscaleStatus {
                version: Version::parse("1.80.0").unwrap(),
                backend_state: "Running".into(),
                self_dns_name: "home.example.ts.net.".into(),
                owner_login: "owner@example.com".into(),
                self_is_tagged: false,
                peers: Vec::new(),
            }
        }
    }

    impl TailscaleControl for FakeTailscale {
        fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
            if self.fail_status.load(Ordering::SeqCst) {
                Err(TailscaleError::MissingStatusField("Self"))
            } else {
                Ok(Self::status_value())
            }
        }

        fn serve_assessment(&self) -> Result<ServeAssessment, TailscaleError> {
            Ok(ServeAssessment {
                mapping: *self.mapping.lock().unwrap(),
                funnel_enabled: false,
            })
        }

        fn apply_audetic_serve(&self) -> Result<bool, TailscaleError> {
            self.apply_calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_apply.load(Ordering::SeqCst) {
                return Err(TailscaleError::ServeCollision);
            }
            let mut mapping = self.mapping.lock().unwrap();
            let created = *mapping == MappingState::Vacant;
            *mapping = MappingState::OwnedByAudetic;
            Ok(created)
        }

        fn remove_audetic_serve(&self) -> Result<bool, TailscaleError> {
            self.remove_calls.fetch_add(1, Ordering::SeqCst);
            let mut mapping = self.mapping.lock().unwrap();
            let removed = *mapping == MappingState::OwnedByAudetic;
            if removed {
                *mapping = MappingState::Vacant;
            }
            Ok(removed)
        }

        fn serve_preview(&self) -> String {
            "tailscale serve --bg --https=8443 --set-path=/audetic http://127.0.0.1:3738".into()
        }
    }

    #[derive(Default)]
    struct FakeHubs {
        fail: AtomicBool,
    }

    #[async_trait]
    impl HubAccess for FakeHubs {
        async fn handshake(&self, hub: &HubConnection) -> Result<HubCandidate, String> {
            if self.fail.load(Ordering::SeqCst) {
                Err("hub offline".into())
            } else {
                Ok(HubCandidate {
                    connection: hub.clone(),
                    device_name: Some("Hub".into()),
                    protocol_version: 1,
                })
            }
        }

        async fn discover(
            &self,
            _candidates: Vec<String>,
            _expected_owner_login: &str,
        ) -> DiscoveryOutcome {
            DiscoveryOutcome::None { failures: vec![] }
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        service: SyncService,
        tailscale: Arc<FakeTailscale>,
        hubs: Arc<FakeHubs>,
        path: PathBuf,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        crate::db::migrate_db_at(&path).unwrap();
        let tailscale = Arc::new(FakeTailscale::default());
        let hubs = Arc::new(FakeHubs::default());
        let service = SyncService::with_dependencies(
            path.clone(),
            tailscale.clone(),
            hubs.clone(),
            "127.0.0.1:0".parse().unwrap(),
        );
        Fixture {
            _temp: temp,
            service,
            tailscale,
            hubs,
            path,
        }
    }

    fn request(role: SyncRole) -> SyncSetupRequest {
        SyncSetupRequest {
            role,
            device_name: Some("Device".into()),
            hub: None,
            upload_recording_payloads: false,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: false,
            confirm_serve_change: true,
        }
    }

    fn connected_request(hub_id: HubId) -> SyncSetupRequest {
        SyncSetupRequest {
            hub: Some(HubConnection {
                base_url: "https://home.example.ts.net:8443/audetic/".into(),
                hub_id,
                owner_login: "owner@example.com".into(),
            }),
            ..request(SyncRole::ConnectedDevice)
        }
    }

    #[tokio::test]
    async fn home_hub_transition_verifies_then_commits_identity_settings_and_ownership() {
        let fixture = fixture();
        let result = fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();

        assert_eq!(result.status.role, SyncRole::HomeHub);
        assert!(result.status.hub_reachable);
        assert!(result.status.local_hub_id.is_some());
        let expected_command = format!(
            "audetic setup --sync-role connected-device --hub-url https://home.example.ts.net:8443/audetic/ --hub-id {}",
            result.status.local_hub_id.unwrap()
        );
        assert_eq!(
            result.setup_command.as_deref(),
            Some(expected_command.as_str())
        );
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::HomeHub
        );
        assert!(SyncServeRepository::get(&conn).unwrap().is_some());
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn home_hub_preview_has_no_runtime_or_persistence_side_effects() {
        let fixture = fixture();
        let mut preview = request(SyncRole::HomeHub);
        preview.confirm_serve_change = false;

        let result = fixture.service.configure(preview).await.unwrap();

        assert_eq!(result.status.role, SyncRole::Standalone);
        assert!(result.serve_preview.is_some());
        assert_eq!(fixture.tailscale.apply_calls.load(Ordering::SeqCst), 0);
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::Standalone
        );
        assert!(SyncServeRepository::get(&conn).unwrap().is_none());
    }

    #[tokio::test]
    async fn failed_activation_rolls_back_listener_mapping_and_persisted_role() {
        let fixture = fixture();
        fixture.hubs.fail.store(true, Ordering::SeqCst);

        let error = fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap_err();

        assert!(matches!(error, SyncServiceError::HubVerification(_)));
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::Vacant
        );
        assert_eq!(fixture.tailscale.remove_calls.load(Ordering::SeqCst), 1);
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::Standalone
        );
        assert!(SyncServeRepository::get(&conn).unwrap().is_none());
    }

    fn insert_dictation_with_invalid_backfill_timestamp(path: &std::path::Path) {
        let conn = crate::db::open_db_at(path).unwrap();
        crate::db::insert_workflow_record(
            &conn,
            &crate::db::Workflow::new(
                crate::db::WorkflowType::VoiceToText,
                crate::db::WorkflowData::VoiceToText(crate::db::VoiceToTextData {
                    text: "valid before rollback".into(),
                    audio_path: "/missing".into(),
                }),
            ),
        )
        .unwrap();
        let (_, record_id) = crate::db::insert_workflow_record(
            &conn,
            &crate::db::Workflow::new(
                crate::db::WorkflowType::VoiceToText,
                crate::db::WorkflowData::VoiceToText(crate::db::VoiceToTextData {
                    text: "cannot backfill".into(),
                    audio_path: "/missing".into(),
                }),
            ),
        )
        .unwrap();
        conn.execute(
            "UPDATE workflows SET created_at = 'not-a-timestamp' WHERE sync_id = ?1",
            [record_id.to_string()],
        )
        .unwrap();
    }

    fn outbox_count(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM sync_outbox_items", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[tokio::test]
    async fn home_hub_backfill_failure_rolls_back_role_listener_mapping_and_worker() {
        let fixture = fixture();
        insert_dictation_with_invalid_backfill_timestamp(&fixture.path);

        let error = fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap_err();

        assert!(matches!(error, SyncServiceError::Persistence(_)));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::Standalone
        );
        assert!(SyncServeRepository::get(&conn).unwrap().is_none());
        assert_eq!(outbox_count(&conn), 0);
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::Vacant
        );
        assert!(!fixture.service.hub_runtime_running().await);
        assert!(fixture.service.outbox_runtime.lock().await.is_none());
    }

    #[tokio::test]
    async fn home_hub_commit_failure_cancels_prepared_worker_and_rolls_back_runtime() {
        let fixture = fixture();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        SyncSettingsRepository::get(&conn).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_home_hub_role
             BEFORE UPDATE OF role ON sync_settings
             WHEN NEW.role = 'home_hub'
             BEGIN SELECT RAISE(ABORT, 'simulated settings commit failure'); END;",
        )
        .unwrap();
        drop(conn);

        let error = fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap_err();

        assert!(matches!(error, SyncServiceError::Persistence(_)));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::Standalone
        );
        assert_eq!(outbox_count(&conn), 0);
        assert!(SyncServeRepository::get(&conn).unwrap().is_none());
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::Vacant
        );
        assert!(!fixture.service.hub_runtime_running().await);
        assert!(fixture.service.outbox_runtime.lock().await.is_none());
    }

    #[tokio::test]
    async fn connected_device_backfill_failure_leaves_standalone_without_a_worker() {
        let fixture = fixture();
        insert_dictation_with_invalid_backfill_timestamp(&fixture.path);

        let error = fixture
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap_err();

        assert!(matches!(error, SyncServiceError::Persistence(_)));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::Standalone
        );
        assert_eq!(outbox_count(&conn), 0);
        assert!(fixture.service.outbox_runtime.lock().await.is_none());
        assert!(!*fixture.service.hub_reachable.read().await);
    }

    #[tokio::test]
    async fn connected_startup_failure_keeps_role_and_reports_degraded_status() {
        let fixture = fixture();
        let hub_id = HubId::new();
        fixture
            .service
            .configure(connected_request(hub_id))
            .await
            .unwrap();
        fixture.hubs.fail.store(true, Ordering::SeqCst);

        let restarted = SyncService::with_dependencies(
            fixture.path.clone(),
            fixture.tailscale.clone(),
            fixture.hubs.clone(),
            "127.0.0.1:0".parse().unwrap(),
        );
        let status = restarted.initialize().await.unwrap();

        assert_eq!(status.role, SyncRole::ConnectedDevice);
        assert!(!status.hub_reachable);
        assert!(status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("offline")));
    }

    #[tokio::test]
    async fn demotion_removes_only_the_exact_persisted_owned_mapping() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        fixture
            .service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap();

        assert_eq!(fixture.tailscale.remove_calls.load(Ordering::SeqCst), 1);
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert!(SyncServeRepository::get(&conn).unwrap().is_none());
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::Standalone
        );
    }

    #[tokio::test]
    async fn demotion_never_removes_a_mapping_when_ownership_metadata_drifted() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        conn.execute(
            "UPDATE sync_serve_ownership SET proxy_url = 'http://127.0.0.1:9999'",
            [],
        )
        .unwrap();

        fixture
            .service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap();

        assert_eq!(fixture.tailscale.remove_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
    }

    #[test]
    fn device_identity_is_not_replaced_by_role_transitions() {
        let fixture = fixture();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        let first: DeviceId = SyncIdentityRepository::get_or_create_device(&conn)
            .unwrap()
            .device_id;
        let second = SyncIdentityRepository::get_or_create_device(&conn)
            .unwrap()
            .device_id;
        assert_eq!(first, second);
    }
}
