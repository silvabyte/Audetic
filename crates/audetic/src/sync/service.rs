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
use crate::db::sync_outbox::OutboxBlob;
use crate::db::sync_serve::{SyncServeOwnership, SyncServeRepository};
use crate::db::sync_settings::{SyncSettings, SyncSettingsRepository};
use crate::sync::is_exact_audetic_serve_ownership;

use super::client::{
    canonicalize_base_url, discover_hubs, DiscoveryOutcome, HandshakeExpectation, HubClient,
    ReqwestHubTransport,
};
use super::library::HubLibrary;
use super::outbox::OutboxWorker;
use super::protocol::{
    DictationPage, MeetingPage, MeetingTitlePatch, RecordKind, SharedMeeting, SnapshotBatch,
    SnapshotBatchResponse,
};
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

    async fn page_meetings(
        &self,
        _hub: &HubConnection,
        _query: Option<&str>,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<MeetingPage, HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub is unavailable".into(),
        ))
    }
    async fn meeting(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub is unavailable".into(),
        ))
    }
    async fn update_meeting_title(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _patch: MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub is unavailable".into(),
        ))
    }
    async fn delete_record(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _kind: RecordKind,
    ) -> Result<(), HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub is unavailable".into(),
        ))
    }

    async fn upload_blob(
        &self,
        _hub: &HubConnection,
        _blob: &OutboxBlob,
    ) -> Result<(), HubTransferError> {
        Err(HubTransferError::NeedsAttention(
            "Recording Payload upload is unavailable".into(),
        ))
    }

    async fn stream_payload(
        &self,
        _hub: &HubConnection,
        _id: RecordId,
        _kind: RecordKind,
        _range: Option<&str>,
    ) -> Result<super::client::StreamingPayloadResponse, HubTransferError> {
        Err(HubTransferError::Retryable(
            "Home Hub Recording Payload is unavailable".into(),
        ))
    }
}

pub enum PayloadSource {
    Local(crate::db::shared_library::LibraryBlobRecord),
    Remote(super::client::StreamingPayloadResponse),
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

    async fn page_meetings(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeetingPage, HubTransferError> {
        HubClient::new(&hub.base_url)
            .map_err(|error| HubTransferError::NeedsAttention(error.to_string()))?
            .page_meetings(hub.hub_id, query, cursor, limit)
            .await
            .map_err(classify_client_error)
    }
    async fn meeting(
        &self,
        hub: &HubConnection,
        id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError> {
        HubClient::new(&hub.base_url)
            .map_err(|error| HubTransferError::NeedsAttention(error.to_string()))?
            .meeting(hub.hub_id, id)
            .await
            .map_err(classify_client_error)
    }
    async fn update_meeting_title(
        &self,
        hub: &HubConnection,
        id: RecordId,
        patch: MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError> {
        HubClient::new(&hub.base_url)
            .map_err(|error| HubTransferError::NeedsAttention(error.to_string()))?
            .update_meeting_title(hub.hub_id, id, &patch)
            .await
            .map_err(classify_client_error)
    }
    async fn delete_record(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
    ) -> Result<(), HubTransferError> {
        HubClient::new(&hub.base_url)
            .map_err(|error| HubTransferError::NeedsAttention(error.to_string()))?
            .delete_record(hub.hub_id, id, kind)
            .await
            .map_err(classify_client_error)
    }

    async fn upload_blob(
        &self,
        hub: &HubConnection,
        blob: &OutboxBlob,
    ) -> Result<(), HubTransferError> {
        HubClient::new(&hub.base_url)
            .map_err(|error| HubTransferError::NeedsAttention(error.to_string()))?
            .upload_blob(
                hub.hub_id,
                &blob.checksum,
                &blob.staged_path,
                blob.byte_size,
                &blob.media_type,
            )
            .await
            .map_err(classify_client_error)
    }

    async fn stream_payload(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
        range: Option<&str>,
    ) -> Result<super::client::StreamingPayloadResponse, HubTransferError> {
        HubClient::new(&hub.base_url)
            .map_err(|error| HubTransferError::NeedsAttention(error.to_string()))?
            .stream_payload(hub.hub_id, kind, id, range)
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
    cancellation: tokio_util::sync::CancellationToken,
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
    pub(crate) fn db_path(&self) -> &std::path::Path {
        &self.db_path
    }

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

    pub async fn meetings(
        &self,
        query: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<super::library_reader::LibraryMeeting>, SyncServiceError> {
        let _transition = self.transition.lock().await;
        let (_, settings) = self.load()?;
        let result = super::library_reader::MeetingLibraryReader::new(
            self.db_path.clone(),
            Arc::clone(&self.hubs),
        )
        .read(&settings, query, offset, limit)
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
        let _transition = self.transition.lock().await;
        let (_, settings) = self.load()?;
        let patch = MeetingTitlePatch {
            title,
            expected_title_version,
            title_source,
        };
        match settings.role {
            SyncRole::Standalone => Err(SyncServiceError::InvalidRequest(
                "meeting is not shared".into(),
            )),
            SyncRole::HomeHub => HubLibrary::new(self.db_path.clone())
                .update_meeting_title(id, &patch)
                .map_err(SyncServiceError::Persistence)?
                .ok_or_else(|| SyncServiceError::InvalidRequest("meeting not found".into())),
            SyncRole::ConnectedDevice => self
                .hubs
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
        let _transition = self.transition.lock().await;
        let (_, settings) = self.load()?;
        match settings.role {
            SyncRole::Standalone => Err(SyncServiceError::InvalidRequest(
                "record is not shared".into(),
            )),
            SyncRole::HomeHub => HubLibrary::new(self.db_path.clone())
                .delete(id, kind)
                .map(|_| ())
                .map_err(|error| SyncServiceError::Persistence(anyhow::anyhow!(error))),
            SyncRole::ConnectedDevice => self
                .hubs
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
        let _transition = self.transition.lock().await;
        let (_, settings) = self.load()?;
        match settings.role {
            SyncRole::Standalone => Ok(None),
            SyncRole::HomeHub => HubLibrary::new(self.db_path.clone())
                .payload(id, kind)
                .map(|value| value.map(PayloadSource::Local))
                .map_err(SyncServiceError::Persistence),
            SyncRole::ConnectedDevice => self
                .hubs
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
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
        let (_, settings) = self.load()?;
        let prepared = if settings.role == SyncRole::Standalone {
            None
        } else {
            Some(self.prepare_dictation_transfer(&settings).await?)
        };
        if prepared.is_some() {
            self.stop_outbox_runtime().await;
        }
        let result = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .and_then(|connection| {
                crate::db::sync_outbox::SyncOutboxRepository::retry_all(&connection)
                    .map(|count| count as u64)
            })
            .map_err(SyncServiceError::Persistence);
        if let Some(prepared) = prepared {
            self.activate_prepared_outbox(prepared).await;
        }
        result
    }

    pub async fn update_recording_payload_policy(
        &self,
        enabled: bool,
    ) -> Result<bool, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
        let (_, mut settings) = self.load()?;
        if settings.role == SyncRole::Standalone {
            return Err(SyncServiceError::InvalidRequest(
                "Recording Payload upload policy requires an active Shared Library role".into(),
            ));
        }
        if settings.upload_recording_payloads == enabled {
            return Ok(enabled);
        }

        let previous_settings = settings.clone();
        let prepared = self
            .prepare_dictation_transfer(&SyncSettings {
                upload_recording_payloads: enabled,
                ..settings.clone()
            })
            .await?;
        self.stop_outbox_runtime().await;
        settings.upload_recording_payloads = enabled;
        let persistence = (|| -> Result<(), SyncServiceError> {
            let mut connection = crate::db::open_db_at(&self.db_path)
                .context("opening sync database")
                .map_err(SyncServiceError::Persistence)?;
            let transaction = connection
                .transaction()
                .context("starting Recording Payload policy transaction")
                .map_err(SyncServiceError::Persistence)?;
            SyncSettingsRepository::save(&transaction, &settings)
                .map_err(SyncServiceError::Persistence)?;
            if !enabled {
                crate::db::sync_outbox::SyncOutboxRepository::pause_blob_uploads(&transaction)
                    .map_err(SyncServiceError::Persistence)?;
            } else {
                crate::db::sync_outbox::SyncOutboxRepository::reset_restageable_for_backfill(
                    &transaction,
                )
                .map_err(SyncServiceError::Persistence)?;
            }
            transaction
                .commit()
                .context("committing Recording Payload policy")
                .map_err(SyncServiceError::Persistence)
        })();
        if let Err(error) = persistence {
            self.cancel_prepared_outbox(prepared).await;
            return Err(
                match self.restore_outbox_runtime(&previous_settings).await {
                    Ok(()) => error,
                    Err(rollback) => SyncServiceError::Rollback {
                        source_error: error.to_string(),
                        rollback_error: rollback.to_string(),
                    },
                },
            );
        }
        self.activate_prepared_outbox(prepared).await;
        Ok(enabled)
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
        let (identity, current) = self.load()?;
        if current.role != SyncRole::Standalone
            && request.role != SyncRole::Standalone
            && request.upload_recording_payloads != current.upload_recording_payloads
        {
            return Err(SyncServiceError::InvalidRequest(
                "Recording Payload upload policy for an active Shared Library role must be changed with PUT /sync/payload-policy".into(),
            ));
        }
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
                self.configure_home_hub(request, current, identity.device_id)
                    .await?;
            }
            SyncRole::ConnectedDevice => {
                if current.role == SyncRole::HomeHub {
                    return Err(SyncServiceError::InvalidTransition(
                        "demote the Home Hub to Standalone before connecting to another hub".into(),
                    ));
                }
                self.configure_connected_device(request, current, identity.device_id)
                    .await?;
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
        if current.role != SyncRole::Standalone {
            self.stop_outbox_runtime().await;
        }
        let removed_mapping = if current.role == SyncRole::HomeHub {
            if ownership
                .as_ref()
                .is_some_and(is_exact_audetic_serve_ownership)
            {
                match self.tailscale_remove().await {
                    Ok(removed) => removed,
                    Err(error) => {
                        return Err(match self.restore_outbox_runtime(&current).await {
                            Ok(()) => error,
                            Err(rollback) => SyncServiceError::Rollback {
                                source_error: error.to_string(),
                                rollback_error: rollback.to_string(),
                            },
                        });
                    }
                }
            } else {
                false
            }
        } else {
            false
        };

        let settings = settings_from_request(request, None);
        if let Err(error) = self.commit_settings_and_ownership(&settings, None, true) {
            let mut rollback_errors = Vec::new();
            if removed_mapping {
                if let Err(rollback) = self.tailscale_apply().await {
                    rollback_errors.push(rollback.to_string());
                }
            }
            if let Err(rollback) = self.restore_outbox_runtime(&current).await {
                rollback_errors.push(rollback.to_string());
            }
            return Err(if rollback_errors.is_empty() {
                error
            } else {
                SyncServiceError::Rollback {
                    source_error: error.to_string(),
                    rollback_error: rollback_errors.join("; "),
                }
            });
        }
        if current.role == SyncRole::HomeHub {
            self.stop_hub_runtime().await?;
        }
        *self.hub_reachable.write().await = false;
        Ok(())
    }

    async fn configure_home_hub(
        &self,
        request: SyncSetupRequest,
        current: SyncSettings,
        local_device_id: audetic_core::sync::DeviceId,
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
        let obsolete_staged_paths = match self.commit_home_hub(
            &settings,
            hub_id,
            &owner_login,
            local_device_id,
            current.role == SyncRole::Standalone,
        ) {
            Ok(paths) => paths,
            Err(error) => {
                return Err(self
                    .rollback_home_hub_activation(
                        Some(prepared),
                        runtime_was_running,
                        was_reachable,
                        mapping_created,
                        error,
                    )
                    .await)
            }
        };
        self.reclaim_obsolete_staged_paths(&obsolete_staged_paths, "Home Hub activation");
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
        current: SyncSettings,
        local_device_id: audetic_core::sync::DeviceId,
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
        let destination_changed = current.role == SyncRole::Standalone
            || (current.role == SyncRole::ConnectedDevice
                && current.hub.as_ref().map(|hub| hub.hub_id)
                    != settings.hub.as_ref().map(|hub| hub.hub_id));
        let was_reachable = *self.hub_reachable.read().await;
        let runtime_was_running = self
            .outbox_runtime
            .lock()
            .await
            .as_ref()
            .is_some_and(|runtime| !runtime.task.is_finished());
        let prepared = self.prepare_dictation_transfer(&settings).await?;
        if current.role != SyncRole::Standalone {
            self.stop_outbox_runtime().await;
        }
        let obsolete_staged_paths =
            match self.commit_connected_device(&settings, local_device_id, destination_changed) {
                Ok(paths) => paths,
                Err(error) => {
                    self.cancel_prepared_outbox(prepared).await;
                    *self.hub_reachable.write().await = was_reachable;
                    return Err(if runtime_was_running {
                        match self.restore_outbox_runtime(&current).await {
                            Ok(()) => error,
                            Err(rollback) => SyncServiceError::Rollback {
                                source_error: error.to_string(),
                                rollback_error: rollback.to_string(),
                            },
                        }
                    } else {
                        error
                    });
                }
            };
        self.reclaim_obsolete_staged_paths(&obsolete_staged_paths, "Connected Device activation");
        self.activate_prepared_outbox(prepared).await;
        *self.hub_reachable.write().await = true;
        Ok(())
    }

    fn commit_connected_device(
        &self,
        settings: &SyncSettings,
        local_device_id: audetic_core::sync::DeviceId,
        destination_changed: bool,
    ) -> Result<Vec<std::path::PathBuf>, SyncServiceError> {
        let mut conn = crate::db::open_db_at(&self.db_path)
            .context("opening sync database")
            .map_err(SyncServiceError::Persistence)?;
        let transaction = conn
            .transaction()
            .context("starting Connected Device settings transaction")
            .map_err(SyncServiceError::Persistence)?;
        SyncSettingsRepository::save(&transaction, settings)
            .map_err(SyncServiceError::Persistence)?;
        let obsolete_staged_paths = if destination_changed {
            crate::db::sync_outbox::SyncOutboxRepository::reset_for_new_destination(
                &transaction,
                local_device_id,
            )
            .map_err(SyncServiceError::Persistence)?
        } else {
            Vec::new()
        };
        transaction
            .commit()
            .context("committing Connected Device settings transaction")
            .map_err(SyncServiceError::Persistence)?;
        Ok(obsolete_staged_paths)
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
        )
        .with_payload_uploads(settings.upload_recording_payloads);
        let (start, start_receiver) = oneshot::channel();
        let cancellation = tokio_util::sync::CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            if start_receiver.await.is_ok() {
                worker.run(worker_cancellation).await;
            }
        });
        Ok(PreparedOutboxRuntime {
            start,
            runtime: OutboxRuntime { cancellation, task },
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
        runtime.cancellation.cancel();
        let _ = runtime.task.await;
    }

    async fn restore_outbox_runtime(
        &self,
        settings: &SyncSettings,
    ) -> Result<(), SyncServiceError> {
        if settings.role == SyncRole::Standalone {
            return Ok(());
        }
        let prepared = self.prepare_dictation_transfer(settings).await?;
        self.activate_prepared_outbox(prepared).await;
        Ok(())
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
        runtime.cancellation.cancel();
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
        local_device_id: audetic_core::sync::DeviceId,
        reset_destination: bool,
    ) -> Result<Vec<std::path::PathBuf>, SyncServiceError> {
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
        let obsolete_staged_paths = if reset_destination {
            crate::db::sync_outbox::SyncOutboxRepository::reset_for_new_destination(
                &transaction,
                local_device_id,
            )
            .map_err(SyncServiceError::Persistence)?
        } else {
            Vec::new()
        };
        transaction
            .commit()
            .context("committing Home Hub settings transaction")
            .map_err(SyncServiceError::Persistence)?;
        Ok(obsolete_staged_paths)
    }

    fn reclaim_obsolete_staged_paths(&self, paths: &[std::path::PathBuf], activation: &str) {
        if paths.is_empty() {
            return;
        }
        match crate::db::open_db_at(&self.db_path).context("opening sync database") {
            Ok(connection) => {
                if let Err(error) =
                    crate::db::sync_outbox::SyncOutboxRepository::reclaim_staged_paths(
                        &connection,
                        paths,
                    )
                {
                    tracing::warn!(%error, %activation, "failed to reclaim obsolete Recording Payload staging after activation");
                }
            }
            Err(error) => {
                tracing::warn!(%error, %activation, "failed to open database for staging cleanup after activation");
            }
        }
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

    use crate::sync::protocol::{Snapshot, SnapshotDisposition, SnapshotResult};
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
        blob_upload_calls: std::sync::atomic::AtomicUsize,
        snapshot_uploads: StdMutex<Vec<(HubId, Vec<RecordId>)>>,
        blob_uploads: StdMutex<Vec<(HubId, RecordId)>>,
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

        async fn upload_snapshots(
            &self,
            hub: &HubConnection,
            batch: SnapshotBatch,
        ) -> Result<SnapshotBatchResponse, HubTransferError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(HubTransferError::Retryable("hub offline".into()));
            }
            self.snapshot_uploads.lock().unwrap().push((
                hub.hub_id,
                batch.snapshots.iter().map(Snapshot::record_id).collect(),
            ));
            Ok(SnapshotBatchResponse {
                results: batch
                    .snapshots
                    .into_iter()
                    .map(|snapshot| SnapshotResult {
                        record_id: snapshot.record_id(),
                        disposition: SnapshotDisposition::Accepted,
                        authoritative_revision: Some(1),
                        error_code: None,
                        message: None,
                    })
                    .collect(),
            })
        }

        async fn upload_blob(
            &self,
            hub: &HubConnection,
            blob: &OutboxBlob,
        ) -> Result<(), HubTransferError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(HubTransferError::Retryable("hub offline".into()));
            }
            self.blob_upload_calls.fetch_add(1, Ordering::SeqCst);
            self.blob_uploads
                .lock()
                .unwrap()
                .push((hub.hub_id, blob.record_id));
            Ok(())
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

    fn seed_accepted_dictation(
        fixture: &Fixture,
        hub: HubConnection,
        source_name: &str,
        retain_staged_payload: bool,
    ) -> (RecordId, PathBuf, PathBuf) {
        let source = fixture.path.parent().unwrap().join(source_name);
        std::fs::write(&source, b"accepted by the old hub").unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        SyncSettingsRepository::save(
            &conn,
            &SyncSettings {
                role: SyncRole::ConnectedDevice,
                hub: Some(hub),
                upload_recording_payloads: true,
                ..SyncSettings::default()
            },
        )
        .unwrap();
        let (_, record_id) = crate::db::insert_workflow_record(
            &conn,
            &crate::db::Workflow::new(
                crate::db::WorkflowType::VoiceToText,
                crate::db::WorkflowData::VoiceToText(crate::db::VoiceToTextData {
                    text: "accepted metadata".into(),
                    audio_path: source.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap();
        let staged: String = conn
            .query_row(
                "SELECT staged_path FROM sync_outbox_blobs WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE sync_outbox_items SET state='synced',accepted_hub_revision=9 WHERE record_id=?1",
            [record_id.to_string()],
        )
        .unwrap();
        if retain_staged_payload {
            conn.execute(
                "UPDATE sync_outbox_blobs SET state='synced',availability='available'
                 WHERE record_id=?1",
                [record_id.to_string()],
            )
            .unwrap();
        } else {
            conn.execute(
                "UPDATE sync_outbox_blobs SET state='synced',availability='available',staged_path=NULL
                 WHERE record_id=?1",
                [record_id.to_string()],
            )
            .unwrap();
            std::fs::remove_file(&staged).unwrap();
        }
        (record_id, source, staged.into())
    }

    async fn wait_for_uploads(fixture: &Fixture, hub_id: HubId, record_id: RecordId) {
        for _ in 0..100 {
            let metadata_uploaded = fixture
                .hubs
                .snapshot_uploads
                .lock()
                .unwrap()
                .iter()
                .any(|(hub, records)| *hub == hub_id && records.contains(&record_id));
            let blob_uploaded = fixture
                .hubs
                .blob_uploads
                .lock()
                .unwrap()
                .contains(&(hub_id, record_id));
            if metadata_uploaded && blob_uploaded {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("record was not fully uploaded to the new hub");
    }

    #[tokio::test]
    async fn home_hub_activation_commits_before_historical_backfill_failures() {
        let fixture = fixture();
        insert_dictation_with_invalid_backfill_timestamp(&fixture.path);

        let result = fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();

        assert_eq!(result.status.role, SyncRole::HomeHub);
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::HomeHub
        );
        assert!(SyncServeRepository::get(&conn).unwrap().is_some());
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
        assert!(fixture.service.hub_runtime_running().await);
        assert!(fixture.service.outbox_runtime.lock().await.is_some());
        fixture.service.shutdown().await.unwrap();
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
    async fn connected_device_activation_commits_before_historical_backfill_failures() {
        let fixture = fixture();
        insert_dictation_with_invalid_backfill_timestamp(&fixture.path);

        let result = fixture
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap();

        assert_eq!(result.status.role, SyncRole::ConnectedDevice);
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::ConnectedDevice
        );
        assert!(fixture.service.outbox_runtime.lock().await.is_some());
        assert!(*fixture.service.hub_reachable.read().await);
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn reconnecting_through_standalone_to_a_different_hub_requeues_only_local_records() {
        let fixture = fixture();
        let old_hub = connected_request(HubId::new()).hub.unwrap();
        let (record_id, _source, _staged) =
            seed_accepted_dictation(&fixture, old_hub, "switch.wav", false);
        let foreign_id = RecordId::new();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        let snapshot_json: String = conn
            .query_row(
                "SELECT snapshot_json FROM sync_outbox_items WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let mut foreign: Snapshot = serde_json::from_str(&snapshot_json).unwrap();
        let Snapshot::Dictation(foreign_snapshot) = &mut foreign else {
            panic!("expected dictation snapshot");
        };
        foreign_snapshot.record_id = foreign_id;
        foreign_snapshot.origin_device_id = DeviceId::new();
        crate::db::sync_outbox::SyncOutboxRepository::enqueue_snapshot(&conn, &foreign).unwrap();
        conn.execute(
            "UPDATE sync_outbox_items SET state='synced',accepted_hub_revision=55
             WHERE record_id=?1",
            [foreign_id.to_string()],
        )
        .unwrap();
        drop(conn);

        fixture
            .service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap();

        let new_hub_id = HubId::new();
        let mut switch = connected_request(new_hub_id);
        switch.upload_recording_payloads = true;
        fixture.service.configure(switch).await.unwrap();
        wait_for_uploads(&fixture, new_hub_id, record_id).await;

        assert!(!fixture
            .hubs
            .snapshot_uploads
            .lock()
            .unwrap()
            .iter()
            .any(|(_, records)| records.contains(&foreign_id)));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        let foreign_state: (String, Option<u64>) = conn
            .query_row(
                "SELECT state,accepted_hub_revision FROM sync_outbox_items WHERE record_id=?1",
                [foreign_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(foreign_state, ("synced".into(), Some(55)));
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn standalone_to_home_hub_with_payloads_enabled_restages_unavailable_payload() {
        let fixture = fixture();
        let source = fixture.path.parent().unwrap().join("reactivate.wav");
        std::fs::write(&source, b"restage after reactivation").unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        SyncSettingsRepository::save(
            &conn,
            &SyncSettings {
                role: SyncRole::ConnectedDevice,
                hub: connected_request(HubId::new()).hub,
                upload_recording_payloads: false,
                ..SyncSettings::default()
            },
        )
        .unwrap();
        let (_, record_id) = crate::db::insert_workflow_record(
            &conn,
            &crate::db::Workflow::new(
                crate::db::WorkflowType::VoiceToText,
                crate::db::WorkflowData::VoiceToText(crate::db::VoiceToTextData {
                    text: "accepted without its payload".into(),
                    audio_path: source.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap();
        conn.execute(
            "UPDATE sync_outbox_items SET state='synced',accepted_hub_revision=9
             WHERE record_id=?1",
            [record_id.to_string()],
        )
        .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT state || ':' || availability FROM sync_outbox_blobs WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "synced:unavailable"
        );
        drop(conn);

        fixture
            .service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap();
        let mut reactivate = request(SyncRole::HomeHub);
        reactivate.upload_recording_payloads = true;
        fixture.service.configure(reactivate).await.unwrap();

        for _ in 0..100 {
            let conn = crate::db::open_db_at(&fixture.path).unwrap();
            let state = conn
                .query_row(
                    "SELECT state || ':' || availability FROM sync_outbox_blobs
                     WHERE record_id=?1",
                    [record_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .ok();
            if state.as_deref() == Some("synced:available") {
                fixture.service.shutdown().await.unwrap();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("reactivation did not restage and accept the Recording Payload");
    }

    #[tokio::test]
    async fn reconnecting_to_the_same_hub_preserves_accepted_outbox_state() {
        let fixture = fixture();
        let hub_id = HubId::new();
        let hub = connected_request(hub_id).hub.unwrap();
        let (record_id, _source, _staged) =
            seed_accepted_dictation(&fixture, hub, "reconnect.wav", false);

        let mut reconnect = connected_request(hub_id);
        reconnect.upload_recording_payloads = true;
        fixture.service.configure(reconnect).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        assert!(fixture.hubs.snapshot_uploads.lock().unwrap().is_empty());
        assert!(fixture.hubs.blob_uploads.lock().unwrap().is_empty());
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        let state: (String, Option<u64>, String) = conn
            .query_row(
                "SELECT i.state,i.accepted_hub_revision,b.availability
                 FROM sync_outbox_items i JOIN sync_outbox_blobs b USING(record_id,kind)
                 WHERE i.record_id=?1",
                [record_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, ("synced".into(), Some(9), "available".into()));
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn changing_hub_requeues_the_existing_durable_stage_before_reclaiming_it() {
        let fixture = fixture();
        let old_hub = connected_request(HubId::new()).hub.unwrap();
        let (record_id, source, staged) =
            seed_accepted_dictation(&fixture, old_hub, "staged-only.wav", true);
        std::fs::remove_file(source).unwrap();

        let new_hub_id = HubId::new();
        let mut switch = connected_request(new_hub_id);
        switch.upload_recording_payloads = true;
        fixture.service.configure(switch).await.unwrap();
        wait_for_uploads(&fixture, new_hub_id, record_id).await;

        assert!(!staged.exists());
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT state || ':' || availability FROM sync_outbox_blobs WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "synced:available"
        );
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn changing_hub_marks_an_absent_payload_unavailable() {
        let fixture = fixture();
        let old_hub = connected_request(HubId::new()).hub.unwrap();
        let (record_id, source, _staged) =
            seed_accepted_dictation(&fixture, old_hub, "gone.wav", false);
        std::fs::remove_file(source).unwrap();

        let new_hub_id = HubId::new();
        let mut switch = connected_request(new_hub_id);
        switch.upload_recording_payloads = true;
        fixture.service.configure(switch).await.unwrap();
        for _ in 0..100 {
            if fixture
                .hubs
                .snapshot_uploads
                .lock()
                .unwrap()
                .iter()
                .any(|(hub, records)| *hub == new_hub_id && records.contains(&record_id))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        assert!(!fixture
            .hubs
            .blob_uploads
            .lock()
            .unwrap()
            .iter()
            .any(|(_, uploaded)| *uploaded == record_id));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT state || ':' || availability FROM sync_outbox_blobs WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "synced:unavailable"
        );
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_hub_change_rolls_back_reset_and_restores_the_old_worker() {
        let fixture = fixture();
        let old_hub_id = HubId::new();
        let old_hub = connected_request(old_hub_id).hub.unwrap();
        let (record_id, _source, _staged) =
            seed_accepted_dictation(&fixture, old_hub, "rollback-switch.wav", false);
        fixture.service.initialize().await.unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_hub_reset BEFORE DELETE ON sync_outbox_items
             BEGIN SELECT RAISE(ABORT, 'simulated Hub reset failure'); END;",
        )
        .unwrap();
        drop(conn);

        let mut switch = connected_request(HubId::new());
        switch.upload_recording_payloads = true;
        let error = fixture.service.configure(switch).await.unwrap_err();

        assert!(matches!(error, SyncServiceError::Persistence(_)));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        let settings = SyncSettingsRepository::get(&conn).unwrap();
        assert_eq!(settings.hub.unwrap().hub_id, old_hub_id);
        assert_eq!(
            conn.query_row(
                "SELECT state || ':' || accepted_hub_revision FROM sync_outbox_items
                 WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "synced:9"
        );
        assert!(fixture.service.outbox_runtime.lock().await.is_some());
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn payload_policy_updates_offline_and_preserves_pending_staging() {
        let fixture = fixture();
        let source = fixture.path.parent().unwrap().join("policy.wav");
        std::fs::write(&source, b"policy payload").unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        let (_, record_id) = crate::db::insert_workflow_record(
            &conn,
            &crate::db::Workflow::new(
                crate::db::WorkflowType::VoiceToText,
                crate::db::WorkflowData::VoiceToText(crate::db::VoiceToTextData {
                    text: "policy backfill".into(),
                    audio_path: source.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap();
        drop(conn);
        fixture
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap();
        for _ in 0..50 {
            let conn = crate::db::open_db_at(&fixture.path).unwrap();
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sync_outbox_blobs WHERE record_id=?1)",
                    [record_id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            if exists {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        fixture.tailscale.fail_status.store(true, Ordering::SeqCst);
        fixture.hubs.fail.store(true, Ordering::SeqCst);
        assert!(fixture
            .service
            .update_recording_payload_policy(true)
            .await
            .unwrap());
        for _ in 0..50 {
            let conn = crate::db::open_db_at(&fixture.path).unwrap();
            let staged: Option<String> = conn
                .query_row(
                    "SELECT staged_path FROM sync_outbox_blobs WHERE record_id=?1",
                    [record_id.to_string()],
                    |row| row.get(0),
                )
                .ok();
            if staged
                .as_deref()
                .is_some_and(|path| std::path::Path::new(path).is_file())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!fixture
            .service
            .update_recording_payload_policy(false)
            .await
            .unwrap());

        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        let payload: (String, String, String) = conn
            .query_row(
                "SELECT availability,state,staged_path FROM sync_outbox_blobs WHERE record_id=?1",
                [record_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(payload.0, "pending");
        assert_eq!(payload.1, "pending");
        assert!(std::path::Path::new(&payload.2).is_file());
        assert!(
            !SyncSettingsRepository::get(&conn)
                .unwrap()
                .upload_recording_payloads
        );
        fixture.service.shutdown().await.unwrap();
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
    async fn restart_preserves_accepted_payload_and_does_not_reupload_it() {
        let fixture = fixture();
        let source = fixture.path.parent().unwrap().join("accepted.wav");
        std::fs::write(&source, b"accepted payload").unwrap();
        let hub = connected_request(HubId::new()).hub.unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        SyncSettingsRepository::save(
            &conn,
            &SyncSettings {
                role: SyncRole::ConnectedDevice,
                hub: Some(hub),
                upload_recording_payloads: true,
                ..SyncSettings::default()
            },
        )
        .unwrap();
        let (_, record_id) = crate::db::insert_workflow_record(
            &conn,
            &crate::db::Workflow::new(
                crate::db::WorkflowType::VoiceToText,
                crate::db::WorkflowData::VoiceToText(crate::db::VoiceToTextData {
                    text: "already accepted".into(),
                    audio_path: source.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap();
        let staged: String = conn
            .query_row(
                "SELECT staged_path FROM sync_outbox_blobs WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE sync_outbox_items SET state='synced',accepted_hub_revision=9 WHERE record_id=?1",
            [record_id.to_string()],
        )
        .unwrap();
        conn.execute(
            "UPDATE sync_outbox_blobs SET state='synced',availability='available' WHERE record_id=?1",
            [record_id.to_string()],
        )
        .unwrap();
        std::fs::remove_file(source).unwrap();
        std::fs::remove_file(staged).unwrap();
        drop(conn);

        fixture.service.initialize().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        assert_eq!(fixture.hubs.blob_upload_calls.load(Ordering::SeqCst), 0);
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT state || ':' || availability FROM sync_outbox_blobs WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "synced:available"
        );
        fixture.service.shutdown().await.unwrap();
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
    async fn failed_demotion_restores_the_prior_connected_worker() {
        let fixture = fixture();
        fixture
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_standalone_role
             BEFORE UPDATE OF role ON sync_settings
             WHEN NEW.role='standalone'
             BEGIN SELECT RAISE(ABORT, 'simulated demotion commit failure'); END;",
        )
        .unwrap();
        drop(conn);

        let error = fixture
            .service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap_err();

        assert!(matches!(error, SyncServiceError::Persistence(_)));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::ConnectedDevice
        );
        assert!(fixture.service.outbox_runtime.lock().await.is_some());
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_home_hub_demotion_restores_mapping_listener_and_worker() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_standalone_role
             BEFORE UPDATE OF role ON sync_settings
             WHEN NEW.role='standalone'
             BEGIN SELECT RAISE(ABORT, 'simulated demotion commit failure'); END;",
        )
        .unwrap();
        drop(conn);

        let error = fixture
            .service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap_err();

        assert!(matches!(error, SyncServiceError::Persistence(_)));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::HomeHub
        );
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
        assert!(fixture.service.hub_runtime_running().await);
        assert!(fixture.service.outbox_runtime.lock().await.is_some());
        fixture.service.shutdown().await.unwrap();
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
