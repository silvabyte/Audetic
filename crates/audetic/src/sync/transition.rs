//! Cancellation-safe coordinator for every durable Library Sync role change.

use anyhow::Context;
use audetic_core::sync::{
    HubConnection, ServeMappingState, SyncDiscoveryFailure, SyncNetworkAssessment, SyncRole,
    SyncSetupRequest, SyncSetupResult, SyncStatus,
};
use thiserror::Error;
use tokio::sync::Mutex;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use crate::db::sync_settings::SyncSettings;

use super::client::canonicalize_base_url;
use super::runtime::{
    ActivationOutcome, RoleVersion, RuntimeError, RuntimeSet, RuntimeSpec, RuntimeTransition,
};
use super::serve::{AppliedServe, HomeHubNetwork, RemovedServe, ServeError, ServeManager};
use super::state::HomeHubCommit;
use super::state::{CommitEffects, InstallationSnapshot, InstallationState, StateError};
use super::tailscale::TailscaleControl;
use super::tailscale::TailscaleError;
use super::transport::{DiscoveryOutcome, HubCapabilities, HubTransferError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransitionCheckpoint {
    RestorePrepared,
    Prepared,
    Verified,
    Quiesced,
    Committed,
}

#[cfg(test)]
#[derive(Clone)]
struct TransitionPause {
    checkpoint: TransitionCheckpoint,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[derive(Clone)]
struct ReceiptEnrichmentPause {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[derive(Debug, Error)]
pub enum TransitionError {
    #[error("invalid sync request: {0}")]
    InvalidRequest(String),
    #[error("invalid sync role transition: {0}")]
    InvalidTransition(String),
    #[error(transparent)]
    Serve(#[from] ServeError),
    #[error("Home Hub verification failed: {0}")]
    Hub(#[from] HubTransferError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("sync data operation failed: {0}")]
    Data(#[source] anyhow::Error),
    #[error("sync coordinator task failed: {0}")]
    TaskJoin(#[source] tokio::task::JoinError),
    #[error("sync transition failed: {0}")]
    Saga(#[from] SagaFailure),
}

#[derive(Debug, Error)]
#[error("{primary}; compensation also failed: {compensation:?}")]
pub struct SagaFailure {
    #[source]
    pub primary: Box<TransitionError>,
    pub compensation: Vec<CompensationError>,
}

#[derive(Debug, Error)]
pub enum CompensationError {
    #[error("runtime compensation failed: {0}")]
    Runtime(#[source] RuntimeError),
    #[error("Serve compensation failed: {0}")]
    Serve(#[source] ServeError),
    #[error("transition compensation failed: {0}")]
    Transition(#[source] Box<TransitionError>),
}

impl TransitionError {
    pub const fn is_request_error(&self) -> bool {
        matches!(
            self,
            Self::InvalidRequest(_)
                | Self::InvalidTransition(_)
                | Self::Serve(ServeError::BackendNotRunning(_))
                | Self::State(StateError::EpochMismatch(_))
        )
    }

    pub const fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::Serve(ServeError::Tailscale(TailscaleError::ServeCollision))
                | Self::Serve(ServeError::Tailscale(TailscaleError::FunnelEnabled))
        )
    }

    pub const fn is_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Serve(_) | Self::Hub(_) | Self::Runtime(_) | Self::Saga(_)
        )
    }
}

type SyncServiceError = TransitionError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConfigureReceipt {
    pub(super) role_version: RoleVersion,
    operation_generation: OperationGeneration,
    pub(super) status: SyncStatus,
    pub(super) activation: ActivationHealth,
    pub(super) serve_preview: Option<String>,
    pub(super) verified_connection: Option<HubConnection>,
    pub(super) setup_command: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OperationGeneration(u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ActivationHealth {
    Inactive,
    Healthy,
    Degraded { error: String },
}

impl ConfigureReceipt {
    pub(super) fn into_setup_result(self) -> SyncSetupResult {
        let Self {
            role_version,
            operation_generation,
            status,
            activation,
            serve_preview,
            verified_connection,
            setup_command,
        } = self;
        let _committed_transition = (
            role_version,
            operation_generation,
            activation,
            verified_connection,
        );
        SyncSetupResult {
            status,
            discovered_hubs: Vec::new(),
            discovery_failures: Vec::new(),
            setup_command,
            serve_preview,
        }
    }
}

#[derive(Clone)]
pub(super) struct LibraryContext {
    pub(super) db_path: PathBuf,
    pub(super) role: LibraryRole,
    pub(super) capabilities: HubCapabilities,
    observation: ObservationToken,
}

#[derive(Clone, Debug)]
pub(super) enum LibraryRole {
    Standalone,
    HomeHub,
    ConnectedDevice { hub: HubConnection },
}

#[derive(Clone, Debug)]
pub(super) enum LibraryObservation {
    Reachable,
    Unreachable(String),
}

#[derive(Clone, Copy)]
struct ObservationToken(RoleVersion);

enum VerificationIntent<'a> {
    Committed,
    HomeHubPromotion(&'a HomeHubNetwork),
}

struct VerifiedEnvironment {
    connection: Option<HubConnection>,
    applied_serve: AppliedServe,
}

struct ConfiguredTransition {
    target: InstallationSnapshot,
    verified_connection: Option<HubConnection>,
    activation: ActivationHealth,
    cleanup_error: Option<String>,
}

/// Owns every external side effect until the durable role commit succeeds.
/// There is one compensation path, so adding a new pre-commit failure cannot
/// accidentally skip Serve or runtime restoration.
struct TransitionSaga {
    runtime: Option<RuntimeTransition>,
    applied_serve: AppliedServe,
    removed_serve: RemovedServe,
}

enum TransitionPlan {
    Standalone(SyncSetupRequest),
    HomeHub {
        request: SyncSetupRequest,
        preview_only: bool,
    },
    ConnectedDevice(SyncSetupRequest),
}

impl TransitionPlan {
    fn configure(
        request: SyncSetupRequest,
        installation: &InstallationSnapshot,
    ) -> Result<Self, TransitionError> {
        let current = &installation.settings;
        if current.role != SyncRole::Standalone
            && request.role != SyncRole::Standalone
            && request.upload_recording_payloads != current.upload_recording_payloads
        {
            return Err(TransitionError::InvalidRequest(
                "Recording Payload upload policy for an active Shared Library role must be changed with PUT /sync/payload-policy".into(),
            ));
        }
        match request.role {
            SyncRole::Standalone => Ok(Self::Standalone(request)),
            SyncRole::HomeHub if current.role == SyncRole::ConnectedDevice => {
                Err(TransitionError::InvalidTransition(
                    "demote the Connected Device to Standalone before promotion".into(),
                ))
            }
            SyncRole::HomeHub => Ok(Self::HomeHub {
                preview_only: current.role == SyncRole::Standalone && !request.confirm_serve_change,
                request,
            }),
            SyncRole::ConnectedDevice if current.role == SyncRole::HomeHub => {
                Err(TransitionError::InvalidTransition(
                    "demote the Home Hub to Standalone before connecting to another hub".into(),
                ))
            }
            SyncRole::ConnectedDevice => Ok(Self::ConnectedDevice(request)),
        }
    }
}

impl TransitionSaga {
    fn new(runtime: RuntimeTransition) -> Self {
        Self {
            runtime: Some(runtime),
            applied_serve: AppliedServe::default(),
            removed_serve: RemovedServe::default(),
        }
    }

    fn committed(mut self) -> RuntimeTransition {
        self.runtime.take().expect("transition saga owns runtime")
    }
}

/// Single owner of durable sync settings and all role-dependent runtime tasks.
///
/// The transition mutex covers preview/verify/apply/commit/rollback as one
/// serialized operation. SQLite transactions are kept short and never cross an
/// await point.
#[derive(Clone)]
pub(super) struct RoleCoordinator {
    state: InstallationState,
    runtime: RuntimeSet,
    serve: ServeManager,
    hub_capabilities: HubCapabilities,
    transition: Arc<Mutex<()>>,
    operation_generation: Arc<AtomicU64>,
    shut_down: Arc<AtomicBool>,
    #[cfg(test)]
    transition_pause: Arc<std::sync::Mutex<Option<TransitionPause>>>,
    #[cfg(test)]
    receipt_enrichment_pause: Arc<std::sync::Mutex<Option<ReceiptEnrichmentPause>>>,
}

impl RoleCoordinator {
    pub(super) fn new(
        db_path: PathBuf,
        tailscale: Arc<dyn TailscaleControl>,
        hub_capabilities: HubCapabilities,
        hub_bind_address: SocketAddr,
    ) -> Self {
        let state = InstallationState::new(db_path);
        let serve = ServeManager::new(tailscale);
        let runtime = RuntimeSet::new(state.clone(), hub_capabilities.clone(), hub_bind_address);
        Self {
            state,
            runtime,
            serve,
            hub_capabilities,
            transition: Arc::new(Mutex::new(())),
            operation_generation: Arc::new(AtomicU64::new(0)),
            shut_down: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            transition_pause: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(test)]
            receipt_enrichment_pause: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Reconstruct the persisted role. Network/listener failures are recorded
    /// as degraded status and deliberately do not fail daemon startup.
    pub(super) async fn initialize(&self) -> Result<(), SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.initialize_owned().await })
            .await
            .map_err(SyncServiceError::TaskJoin)?
    }

    pub(super) async fn status(&self) -> Result<SyncStatus, SyncServiceError> {
        self.status_receipt().await.map(|(_, status)| status)
    }

    /// Enrichment is deliberately outside the transition lock and cannot turn
    /// an already committed mutation into an API error.
    pub(super) async fn enrich_configure_receipt(
        &self,
        mut receipt: ConfigureReceipt,
    ) -> SyncSetupResult {
        #[cfg(test)]
        self.pause_receipt_enrichment().await;

        let outbox = crate::db::open_db_at(self.state.db_path())
            .context("opening sync database for configure receipt enrichment")
            .and_then(|connection| {
                let (items, error) =
                    crate::db::sync_outbox::SyncOutboxRepository::counts(&connection)?;
                let bytes =
                    crate::db::sync_outbox::SyncOutboxRepository::pending_bytes(&connection)?;
                Ok((items, bytes, error))
            });
        let network = self.serve.network_assessment().await;

        // Collection may be slow, so it happens without transition ownership.
        // Reacquire only to prove the receipt's generation is still current
        // before merging observations into its immutable committed fields.
        let _transition = self.transition.lock().await;
        let current_role_version = self.state.current_role_epoch().map(RoleVersion::new);
        let current_operation_generation = self.current_operation_generation();
        if !matches!(current_role_version, Ok(version) if version == receipt.role_version)
            || current_operation_generation != receipt.operation_generation
        {
            return receipt.into_setup_result();
        }

        match outbox {
            Ok((items, bytes, error)) => {
                receipt.status.pending_items = items;
                receipt.status.pending_bytes = bytes;
                if let Some(error) = error {
                    append_receipt_error(&mut receipt.status, error);
                }
            }
            Err(error) => append_receipt_error(
                &mut receipt.status,
                format!("configure receipt outbox enrichment failed: {error}"),
            ),
        }
        if let Some(error) = network.error.clone() {
            append_receipt_error(
                &mut receipt.status,
                format!("configure receipt network enrichment failed: {error}"),
            );
        }
        receipt.status.network = network;
        receipt.into_setup_result()
    }

    async fn status_receipt(&self) -> Result<(RoleVersion, SyncStatus), SyncServiceError> {
        self.check_available()?;
        let installation = self.load()?;
        let role_version = RoleVersion::new(installation.role_epoch);
        let status = self.status_for(&installation).await?;
        Ok((role_version, status))
    }

    async fn status_for(
        &self,
        installation: &InstallationSnapshot,
    ) -> Result<SyncStatus, SyncServiceError> {
        let identity = installation.identity.clone();
        let settings = installation.settings.clone();
        let runtime = self.runtime.snapshot().await;
        let role_version = RoleVersion::new(installation.role_epoch);
        let runtime_is_current = runtime.role_version == Some(role_version);
        let reachable = match settings.role {
            SyncRole::Standalone => false,
            SyncRole::HomeHub => {
                runtime_is_current && runtime.hub_listener_running && runtime.hub_reachable
            }
            SyncRole::ConnectedDevice => runtime_is_current && runtime.hub_reachable,
        };
        let connection = crate::db::open_db_at(self.state.db_path())
            .context("opening sync database")
            .map_err(SyncServiceError::Data)?;
        let (pending_items, outbox_error) =
            crate::db::sync_outbox::SyncOutboxRepository::counts(&connection)
                .map_err(SyncServiceError::Data)?;
        let pending_bytes =
            crate::db::sync_outbox::SyncOutboxRepository::pending_bytes(&connection)
                .map_err(SyncServiceError::Data)?;
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
                .or_else(|| {
                    runtime_is_current
                        .then_some(runtime.listener_error)
                        .flatten()
                })
                .or(settings.last_error),
            upload_recording_payloads: settings.upload_recording_payloads,
            cache_level: settings.cache_level,
            shared_config_enabled: settings.shared_config_enabled,
            applied_shared_config_version: settings.shared_config_version,
            network: self.serve.network_assessment().await,
        })
    }

    pub(super) fn library_context(&self) -> Result<LibraryContext, SyncServiceError> {
        self.check_available()?;
        let installation = self.load()?;
        let role = match installation.settings.role {
            SyncRole::Standalone => LibraryRole::Standalone,
            SyncRole::HomeHub => LibraryRole::HomeHub,
            SyncRole::ConnectedDevice => LibraryRole::ConnectedDevice {
                hub: installation.settings.hub.ok_or_else(|| {
                    SyncServiceError::InvalidTransition(
                        "persisted Connected Device role has no Home Hub".into(),
                    )
                })?,
            },
        };
        Ok(LibraryContext {
            db_path: self.state.db_path().to_path_buf(),
            role,
            capabilities: self.hub_capabilities.clone(),
            observation: ObservationToken(RoleVersion::new(installation.role_epoch)),
        })
    }

    pub(super) async fn record_library_observation(
        &self,
        context: &LibraryContext,
        observation: LibraryObservation,
    ) -> Result<(), SyncServiceError> {
        if matches!(context.role, LibraryRole::Standalone) {
            return Ok(());
        }
        let (reachable, error) = match &observation {
            LibraryObservation::Reachable => (true, None),
            LibraryObservation::Unreachable(error) => (false, Some(error.as_str())),
        };
        let role_version = context.observation.0;
        let current = self
            .state
            .observe_contact(role_version.value(), reachable, error)
            .map_err(SyncServiceError::State)?;
        if current {
            self.runtime
                .observe_reachability(role_version, reachable)
                .await;
        }
        Ok(())
    }

    pub(super) async fn discover(&self) -> Result<SyncSetupResult, SyncServiceError> {
        self.check_available()?;
        let network = self.serve.discovery().await?;
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
            status: self.status().await?,
            discovered_hubs,
            discovery_failures,
            setup_command: None,
            serve_preview: None,
        })
    }

    async fn initialize_owned(&self) -> Result<(), SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.begin_transition_attempt();
        self.check_available()?;
        self.runtime
            .acquire_ownership()
            .await
            .map_err(SyncServiceError::Runtime)?;
        let result = self.initialize_with_ownership().await;
        if result.is_err() {
            self.runtime.release_ownership_if_idle().await;
        }
        result
    }

    async fn initialize_with_ownership(&self) -> Result<(), SyncServiceError> {
        let installation = self.load()?;
        let spec = runtime_spec(&installation, installation.role_epoch)?;
        let persisted = self
            .runtime
            .begin_persisted_restore(spec)
            .await
            .map_err(SyncServiceError::Runtime)?;
        let transition = persisted.transition;
        self.transition_checkpoint(TransitionCheckpoint::RestorePrepared)
            .await;
        let verification_error = self
            .verify_environment(&installation, VerificationIntent::Committed)
            .await
            .err();
        let startup_error = persisted
            .listener_error
            .map(|error| SyncServiceError::Runtime(RuntimeError::Listener(error)))
            .or(verification_error);
        let health_result = if let Some(error) = startup_error.as_ref() {
            self.state
                .record_error(installation.role_epoch, Some(&error.to_string()))
                .map(|_| ())
        } else if installation.settings.role != SyncRole::Standalone {
            self.state
                .record_contact(installation.role_epoch)
                .map(|_| ())
        } else {
            self.state
                .record_error(installation.role_epoch, None)
                .map(|_| ())
        };
        if let Err(error) = health_result {
            let _ = self.runtime.abort_transition(transition).await;
            return Err(SyncServiceError::State(error));
        }
        let activation = match self.runtime.commit_transition(transition).await {
            Ok(activation) => activation,
            Err(error) => return Err(error.into()),
        };
        if startup_error.is_none() {
            if let Some(error) = activation.listener_error() {
                self.state
                    .record_error(installation.role_epoch, Some(error))
                    .map_err(SyncServiceError::State)?;
            }
        }
        self.runtime
            .observe_reachability(
                RoleVersion::new(installation.role_epoch),
                installation.settings.role != SyncRole::Standalone
                    && startup_error.is_none()
                    && activation.is_healthy(),
            )
            .await;
        Ok(())
    }

    pub(super) async fn retry(&self) -> Result<u64, SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.retry_owned().await })
            .await
            .map_err(SyncServiceError::TaskJoin)?
    }

    async fn retry_owned(&self) -> Result<u64, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.begin_transition_attempt();
        self.check_available()?;
        self.acquire_runtime_ownership().await?;
        let installation = self.load()?;
        let runtime_transition = match installation.settings.role {
            SyncRole::HomeHub => {
                // A Home Hub verifies its public handshake through Serve. The
                // local listener therefore has to be accepting before that
                // verification can be meaningful.
                let transition = self
                    .prepare_runtime(runtime_spec(&installation, installation.role_epoch)?)
                    .await?;
                if let Err(error) = self
                    .verify_environment(&installation, VerificationIntent::Committed)
                    .await
                {
                    return Err(match self.abort_runtime(transition).await {
                        Ok(()) => error,
                        Err(rollback) => saga_error(
                            error,
                            vec![CompensationError::Transition(Box::new(rollback))],
                        ),
                    });
                }
                Some(transition)
            }
            SyncRole::ConnectedDevice => {
                self.verify_environment(&installation, VerificationIntent::Committed)
                    .await?;
                Some(
                    self.prepare_runtime(runtime_spec(&installation, installation.role_epoch)?)
                        .await?,
                )
            }
            SyncRole::Standalone => {
                self.verify_environment(&installation, VerificationIntent::Committed)
                    .await?;
                None
            }
        };
        if let Some(transition) = runtime_transition {
            self.seal_runtime_transition(transition).await?;
        }
        let result = crate::db::open_db_at(self.state.db_path())
            .context("opening sync database")
            .and_then(|connection| {
                crate::db::sync_outbox::SyncOutboxRepository::retry_all(&connection)
                    .map(|count| count as u64)
            })
            .map_err(SyncServiceError::Data);
        if let Some(transition) = runtime_transition {
            let activation = self.activate_runtime(transition).await?;
            self.record_verified_activation(installation.role_epoch, &activation)
                .await?;
        }
        result
    }

    pub(super) async fn update_recording_payload_policy(
        &self,
        enabled: bool,
    ) -> Result<bool, SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.update_recording_payload_policy_owned(enabled).await })
            .await
            .map_err(SyncServiceError::TaskJoin)?
    }

    async fn update_recording_payload_policy_owned(
        &self,
        enabled: bool,
    ) -> Result<bool, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.begin_transition_attempt();
        self.check_available()?;
        self.acquire_runtime_ownership().await?;
        let installation = self.load()?;
        let mut settings = installation.settings.clone();
        if settings.role == SyncRole::Standalone {
            return Err(SyncServiceError::InvalidRequest(
                "Recording Payload upload policy requires an active Shared Library role".into(),
            ));
        }
        if settings.upload_recording_payloads == enabled {
            return Ok(enabled);
        }

        settings.upload_recording_payloads = enabled;
        let next_epoch = installation
            .role_epoch
            .checked_add(1)
            .ok_or_else(|| SyncServiceError::InvalidTransition("role version exhausted".into()))?;
        let target = InstallationSnapshot {
            settings,
            role_epoch: next_epoch,
            ..installation.clone()
        };
        let runtime_transition = self
            .prepare_runtime(runtime_spec(&target, next_epoch)?)
            .await?;
        self.transition_checkpoint(TransitionCheckpoint::Prepared)
            .await;
        self.seal_runtime_transition(runtime_transition).await?;
        self.transition_checkpoint(TransitionCheckpoint::Quiesced)
            .await;
        let committed = self
            .state
            .commit_payload_policy(installation.role_epoch, enabled)
            .map_err(SyncServiceError::State);
        if let Err(error) = committed {
            return Err(match self.abort_runtime(runtime_transition).await {
                Ok(()) => error,
                Err(rollback) => saga_error(
                    error,
                    vec![CompensationError::Transition(Box::new(rollback))],
                ),
            });
        }
        self.transition_checkpoint(TransitionCheckpoint::Committed)
            .await;
        self.activate_runtime(runtime_transition).await?;
        Ok(enabled)
    }

    pub(super) async fn configure(
        &self,
        request: SyncSetupRequest,
    ) -> Result<ConfigureReceipt, SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.configure_owned(request).await })
            .await
            .map_err(SyncServiceError::TaskJoin)?
    }

    async fn configure_owned(
        &self,
        request: SyncSetupRequest,
    ) -> Result<ConfigureReceipt, SyncServiceError> {
        let _transition = self.transition.lock().await;
        let operation_generation = self.begin_transition_attempt();
        self.check_available()?;
        self.acquire_runtime_ownership().await?;
        let installation = self.load()?;
        let serve_preview = (request.role == SyncRole::HomeHub).then(|| self.serve.preview());
        let plan = TransitionPlan::configure(request, &installation)?;

        let configured = match plan {
            TransitionPlan::Standalone(request) => {
                self.configure_standalone(request, &installation).await?
            }
            TransitionPlan::HomeHub {
                request,
                preview_only,
            } => {
                if preview_only {
                    self.serve
                        .prepare_home_hub()
                        .await
                        .map_err(TransitionError::from)?;
                    return Ok(configure_receipt(
                        &installation,
                        operation_generation,
                        None,
                        ActivationHealth::Inactive,
                        None,
                        serve_preview,
                    ));
                }
                self.configure_home_hub(request, &installation).await?
            }
            TransitionPlan::ConnectedDevice(request) => {
                self.configure_connected_device(request, &installation)
                    .await?
            }
        };
        Ok(configure_receipt(
            &configured.target,
            operation_generation,
            configured.verified_connection,
            configured.activation,
            configured.cleanup_error,
            serve_preview,
        ))
    }

    pub(super) async fn shutdown(&self) -> Result<(), SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.shutdown_owned().await })
            .await
            .map_err(SyncServiceError::TaskJoin)?
    }

    async fn shutdown_owned(&self) -> Result<(), SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.begin_transition_attempt();
        self.shut_down.store(true, Ordering::SeqCst);
        self.runtime
            .shutdown()
            .await
            .map_err(SyncServiceError::Runtime)
    }

    async fn configure_standalone(
        &self,
        request: SyncSetupRequest,
        installation: &InstallationSnapshot,
    ) -> Result<ConfiguredTransition, SyncServiceError> {
        if request.hub.is_some() {
            return Err(SyncServiceError::InvalidRequest(
                "Standalone settings cannot contain a Home Hub connection".into(),
            ));
        }

        let current = &installation.settings;
        let runtime_transition = self
            .prepare_runtime(RuntimeSpec::Standalone {
                role_epoch: installation.role_epoch.checked_add(1).ok_or_else(|| {
                    SyncServiceError::InvalidTransition("role version exhausted".into())
                })?,
            })
            .await?;
        let mut saga = TransitionSaga::new(runtime_transition);
        saga.removed_serve = if current.role == SyncRole::HomeHub {
            match self
                .serve
                .remove_persisted(installation.serve_ownership.as_ref())
                .await
                .map_err(TransitionError::from)
            {
                Ok(removed) => removed,
                Err(error) => {
                    return Err(self.compensate_saga(saga, error).await);
                }
            }
        } else {
            RemovedServe::default()
        };

        self.transition_checkpoint(TransitionCheckpoint::Verified)
            .await;
        if let Err(error) = self.quiesce_worker(runtime_transition).await {
            return Err(self.compensate_saga(saga, error).await);
        }
        self.transition_checkpoint(TransitionCheckpoint::Quiesced)
            .await;
        let settings = settings_from_request(request, None);
        let effects = match self
            .state
            .commit_standalone(installation.role_epoch, &settings)
            .map_err(SyncServiceError::State)
        {
            Ok(effects) => effects,
            Err(error) => return Err(self.compensate_saga(saga, error).await),
        };
        self.transition_checkpoint(TransitionCheckpoint::Committed)
            .await;
        let activation = self.activate_runtime(saga.committed()).await?;
        self.record_committed_cleanup(effects.role_epoch, &activation);
        Ok(ConfiguredTransition {
            target: InstallationSnapshot {
                settings,
                serve_ownership: None,
                role_epoch: effects.role_epoch,
                ..installation.clone()
            },
            verified_connection: None,
            activation: ActivationHealth::Inactive,
            cleanup_error: activation_cleanup_error(&activation),
        })
    }

    async fn configure_home_hub(
        &self,
        request: SyncSetupRequest,
        installation: &InstallationSnapshot,
    ) -> Result<ConfiguredTransition, SyncServiceError> {
        if request.hub.is_some() {
            return Err(SyncServiceError::InvalidRequest(
                "Home Hub settings cannot contain another hub connection".into(),
            ));
        }
        let network = self
            .serve
            .prepare_home_hub()
            .await
            .map_err(TransitionError::from)?;
        let identity = &installation.identity;
        let current = &installation.settings;
        let hub_id = identity.hub_id.unwrap_or_default();
        let owner_login = network.owner_login().to_owned();

        if current.role == SyncRole::HomeHub
            && identity.owner_login.as_deref() != Some(owner_login.as_str())
        {
            return Err(SyncServiceError::InvalidRequest(
                "the current Tailscale owner differs from the persisted Home Hub owner; demote explicitly before changing ownership".into(),
            ));
        }

        let mut settings = settings_from_request(request, None);
        settings.shared_config_enabled = true;
        settings.last_contact_at = Some(now());
        let next_epoch = installation
            .role_epoch
            .checked_add(1)
            .ok_or_else(|| SyncServiceError::InvalidTransition("role version exhausted".into()))?;
        let mut target_identity = installation.identity.clone();
        target_identity.hub_id = Some(hub_id);
        target_identity.owner_login = Some(owner_login.clone());
        let target = InstallationSnapshot {
            identity: target_identity,
            settings: settings.clone(),
            serve_ownership: Some(super::serve::expected_ownership()),
            role_epoch: next_epoch,
        };
        let runtime_transition = self
            .prepare_runtime(runtime_spec_with_home_identity(
                &target,
                hub_id,
                &owner_login,
            )?)
            .await?;
        let mut saga = TransitionSaga::new(runtime_transition);
        self.transition_checkpoint(TransitionCheckpoint::Prepared)
            .await;
        let verified = match self
            .verify_environment(&target, VerificationIntent::HomeHubPromotion(&network))
            .await
        {
            Ok(verified) => verified,
            Err(error) => {
                return Err(self.compensate_saga(saga, error).await);
            }
        };
        saga.applied_serve = verified.applied_serve;
        let verified_connection = verified.connection;
        self.transition_checkpoint(TransitionCheckpoint::Verified)
            .await;
        if let Err(error) = self.quiesce_worker(runtime_transition).await {
            return Err(self.compensate_saga(saga, error).await);
        }
        self.transition_checkpoint(TransitionCheckpoint::Quiesced)
            .await;
        let ownership = super::serve::expected_ownership();
        if let Err(error) = self.validate_runtime(runtime_transition).await {
            return Err(self.compensate_saga(saga, error).await);
        }
        let effects = match self.state.commit_home_hub(
            installation.role_epoch,
            HomeHubCommit {
                settings: &settings,
                hub_id,
                owner_login: &owner_login,
                local_device_id: identity.device_id,
                reset_destination: current.role == SyncRole::Standalone,
                ownership: &ownership,
            },
        ) {
            Ok(effects) => effects,
            Err(error) => {
                return Err(self
                    .compensate_saga(saga, SyncServiceError::State(error))
                    .await);
            }
        };
        self.cleanup_after_commit(&effects, "Home Hub activation");
        self.transition_checkpoint(TransitionCheckpoint::Committed)
            .await;
        let activation = self.activate_runtime(saga.committed()).await?;
        self.record_committed_activation(effects.role_epoch, &activation)
            .await;
        let mut target = target;
        target.role_epoch = effects.role_epoch;
        Ok(ConfiguredTransition {
            target,
            verified_connection,
            activation: activation_health(&activation),
            cleanup_error: activation_cleanup_error(&activation),
        })
    }

    async fn configure_connected_device(
        &self,
        request: SyncSetupRequest,
        installation: &InstallationSnapshot,
    ) -> Result<ConfiguredTransition, SyncServiceError> {
        let mut requested_hub = request.hub.clone().ok_or_else(|| {
            SyncServiceError::InvalidRequest(
                "Connected Device settings require a Home Hub connection".into(),
            )
        })?;
        requested_hub.base_url = canonicalize_base_url(&requested_hub.base_url)
            .map_err(|error| SyncServiceError::InvalidRequest(error.to_string()))?
            .to_string();
        let mut settings = settings_from_request(request, Some(requested_hub));
        settings.last_contact_at = Some(now());
        settings.last_error = None;
        let current = &installation.settings;
        let destination_changed = current.role == SyncRole::Standalone
            || (current.role == SyncRole::ConnectedDevice
                && current.hub.as_ref().map(|hub| hub.hub_id)
                    != settings.hub.as_ref().map(|hub| hub.hub_id));
        let next_epoch = installation
            .role_epoch
            .checked_add(1)
            .ok_or_else(|| SyncServiceError::InvalidTransition("role version exhausted".into()))?;
        let mut target = InstallationSnapshot {
            settings: settings.clone(),
            role_epoch: next_epoch,
            ..installation.clone()
        };
        let verified = self
            .verify_environment(&target, VerificationIntent::Committed)
            .await?;
        settings.hub = verified.connection.clone();
        target.settings = settings.clone();
        let runtime_transition = self
            .prepare_runtime(runtime_spec(&target, next_epoch)?)
            .await?;
        self.transition_checkpoint(TransitionCheckpoint::Prepared)
            .await;
        self.seal_runtime_transition(runtime_transition).await?;
        self.transition_checkpoint(TransitionCheckpoint::Quiesced)
            .await;
        let effects = match self.state.commit_connected_device(
            installation.role_epoch,
            &settings,
            installation.identity.device_id,
            destination_changed,
        ) {
            Ok(effects) => effects,
            Err(error) => {
                let error = SyncServiceError::State(error);
                return Err(match self.abort_runtime(runtime_transition).await {
                    Ok(()) => error,
                    Err(rollback) => saga_error(
                        error,
                        vec![CompensationError::Transition(Box::new(rollback))],
                    ),
                });
            }
        };
        self.cleanup_after_commit(&effects, "Connected Device activation");
        self.transition_checkpoint(TransitionCheckpoint::Committed)
            .await;
        let activation = self.activate_runtime(runtime_transition).await?;
        self.record_committed_activation(effects.role_epoch, &activation)
            .await;
        target.role_epoch = effects.role_epoch;
        Ok(ConfiguredTransition {
            target,
            verified_connection: verified.connection,
            activation: activation_health(&activation),
            cleanup_error: activation_cleanup_error(&activation),
        })
    }

    /// The only role/environment invariant checker. Configure, persisted
    /// reconstruction, and retry all pass through this exact path.
    async fn verify_environment(
        &self,
        installation: &InstallationSnapshot,
        intent: VerificationIntent<'_>,
    ) -> Result<VerifiedEnvironment, SyncServiceError> {
        match installation.settings.role {
            SyncRole::Standalone => Ok(VerifiedEnvironment {
                connection: None,
                applied_serve: AppliedServe::default(),
            }),
            SyncRole::HomeHub => {
                let hub_id = installation.identity.hub_id.ok_or_else(|| {
                    SyncServiceError::InvalidTransition(
                        "persisted Home Hub role has no Hub ID".into(),
                    )
                })?;
                let persisted_owner =
                    installation
                        .identity
                        .owner_login
                        .as_deref()
                        .ok_or_else(|| {
                            SyncServiceError::InvalidTransition(
                                "persisted Home Hub role has no owner login".into(),
                            )
                        })?;
                let (network, applied_serve) = match intent {
                    VerificationIntent::HomeHubPromotion(network) => {
                        let applied = self.serve.apply_verified().await?;
                        (network.clone(), applied)
                    }
                    VerificationIntent::Committed => (
                        self.serve
                            .verify_persisted(installation.serve_ownership.as_ref())
                            .await?,
                        AppliedServe::default(),
                    ),
                };
                let verification = async {
                    if network.owner_login() != persisted_owner {
                        return Err(SyncServiceError::InvalidTransition(
                            "the current Tailscale owner differs from the persisted Home Hub owner"
                                .into(),
                        ));
                    }
                    let connection = network.connection(hub_id);
                    let candidate = self
                        .hub_capabilities
                        .probe()
                        .handshake(&connection)
                        .await
                        .map_err(SyncServiceError::Hub)?;
                    if candidate.connection != connection {
                        return Err(SyncServiceError::InvalidTransition(
                            "Home Hub identity changed during verification".into(),
                        ));
                    }
                    Ok(candidate.connection)
                }
                .await;
                let connection = match verification {
                    Ok(connection) => connection,
                    Err(primary) => {
                        return Err(
                            match self.serve.compensate_application(applied_serve).await {
                                Ok(()) => primary,
                                Err(compensation) => saga_error(
                                    primary,
                                    vec![CompensationError::Serve(compensation)],
                                ),
                            },
                        );
                    }
                };
                Ok(VerifiedEnvironment {
                    connection: Some(connection),
                    applied_serve,
                })
            }
            SyncRole::ConnectedDevice => {
                let hub = installation.settings.hub.as_ref().ok_or_else(|| {
                    SyncServiceError::InvalidTransition(
                        "persisted Connected Device role has no Home Hub".into(),
                    )
                })?;
                let network = self.serve.discovery().await?;
                if network.owner_login != hub.owner_login {
                    return Err(SyncServiceError::InvalidRequest(format!(
                        "the local Tailscale owner {:?} does not match the Home Hub owner {:?}",
                        network.owner_login, hub.owner_login
                    )));
                }
                let candidate = self
                    .hub_capabilities
                    .probe()
                    .handshake(hub)
                    .await
                    .map_err(SyncServiceError::Hub)?;
                if candidate.connection.hub_id != hub.hub_id
                    || candidate.connection.owner_login != hub.owner_login
                {
                    return Err(SyncServiceError::InvalidTransition(
                        "Home Hub identity changed during verification".into(),
                    ));
                }
                Ok(VerifiedEnvironment {
                    connection: Some(candidate.connection),
                    applied_serve: AppliedServe::default(),
                })
            }
        }
    }

    #[cfg(test)]
    async fn hub_runtime_running(&self) -> bool {
        self.runtime.snapshot().await.hub_listener_running
    }

    async fn prepare_runtime(
        &self,
        spec: RuntimeSpec,
    ) -> Result<RuntimeTransition, SyncServiceError> {
        self.runtime
            .begin_transition(spec)
            .await
            .map_err(SyncServiceError::Runtime)
    }

    async fn activate_runtime(
        &self,
        transition: RuntimeTransition,
    ) -> Result<ActivationOutcome, SyncServiceError> {
        self.runtime
            .commit_transition(transition)
            .await
            .map_err(SyncServiceError::Runtime)
    }

    async fn quiesce_worker(&self, transition: RuntimeTransition) -> Result<(), SyncServiceError> {
        self.runtime
            .quiesce_current_worker(transition)
            .await
            .map_err(SyncServiceError::Runtime)
    }

    async fn validate_runtime(
        &self,
        transition: RuntimeTransition,
    ) -> Result<(), SyncServiceError> {
        self.runtime
            .validate_transition(transition)
            .await
            .map_err(SyncServiceError::Runtime)
    }

    async fn seal_runtime_transition(
        &self,
        transition: RuntimeTransition,
    ) -> Result<(), SyncServiceError> {
        if let Err(error) = self.quiesce_worker(transition).await {
            return Err(match self.abort_runtime(transition).await {
                Ok(()) => error,
                Err(rollback) => saga_error(
                    error,
                    vec![CompensationError::Transition(Box::new(rollback))],
                ),
            });
        }
        Ok(())
    }

    async fn abort_runtime(&self, transition: RuntimeTransition) -> Result<(), SyncServiceError> {
        self.runtime
            .abort_transition(transition)
            .await
            .map_err(SyncServiceError::Runtime)
    }

    async fn acquire_runtime_ownership(&self) -> Result<(), SyncServiceError> {
        self.runtime
            .acquire_ownership()
            .await
            .map_err(SyncServiceError::Runtime)
    }

    async fn transition_checkpoint(&self, checkpoint: TransitionCheckpoint) {
        #[cfg(test)]
        {
            let pause = self
                .transition_pause
                .lock()
                .ok()
                .and_then(|mut configured| {
                    (configured.as_ref().map(|pause| pause.checkpoint) == Some(checkpoint))
                        .then(|| configured.take())
                        .flatten()
                });
            if let Some(pause) = pause {
                pause.entered.notify_one();
                pause.release.notified().await;
            }
        }
        #[cfg(not(test))]
        let _ = checkpoint;
    }

    #[cfg(test)]
    async fn pause_receipt_enrichment(&self) {
        let pause = self
            .receipt_enrichment_pause
            .lock()
            .ok()
            .and_then(|mut configured| configured.take());
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    async fn compensate_saga(
        &self,
        mut saga: TransitionSaga,
        primary: SyncServiceError,
    ) -> SyncServiceError {
        let mut compensation = Vec::new();
        if let Some(runtime) = saga.runtime.take() {
            if let Err(error) = self.runtime.abort_transition(runtime).await {
                compensation.push(CompensationError::Runtime(error));
            }
        }
        if let Err(error) = self.serve.compensate_application(saga.applied_serve).await {
            compensation.push(CompensationError::Serve(error));
        }
        if let Err(error) = self.serve.compensate_removal(saga.removed_serve).await {
            compensation.push(CompensationError::Serve(error));
        }
        if compensation.is_empty() {
            primary
        } else {
            saga_error(primary, compensation)
        }
    }

    async fn record_verified_activation(
        &self,
        role_epoch: u64,
        activation: &ActivationOutcome,
    ) -> Result<(), SyncServiceError> {
        let cleanup_error = activation_cleanup_error(activation);
        match activation {
            ActivationOutcome::Healthy { role_version, .. } => {
                if let Some(error) = cleanup_error.as_deref() {
                    self.state
                        .record_error(role_epoch, Some(error))
                        .map_err(SyncServiceError::State)?;
                } else {
                    self.state
                        .record_contact(role_epoch)
                        .map_err(SyncServiceError::State)?;
                }
                self.runtime.observe_reachability(*role_version, true).await;
            }
            ActivationOutcome::Degraded {
                role_version,
                listener_error,
                ..
            } => {
                let error = cleanup_error
                    .map(|cleanup| format!("{listener_error}; {cleanup}"))
                    .unwrap_or_else(|| listener_error.clone());
                self.state
                    .record_error(role_epoch, Some(&error))
                    .map_err(SyncServiceError::State)?;
                self.runtime
                    .observe_reachability(*role_version, false)
                    .await;
            }
        }
        Ok(())
    }

    async fn record_committed_activation(&self, role_epoch: u64, activation: &ActivationOutcome) {
        if let Err(error) = self
            .record_verified_activation(role_epoch, activation)
            .await
        {
            tracing::warn!(
                %error,
                role_epoch,
                "committed sync activation health could not be persisted"
            );
        }
    }

    fn record_committed_cleanup(&self, role_epoch: u64, activation: &ActivationOutcome) {
        let Some(error) = activation_cleanup_error(activation) else {
            return;
        };
        if let Err(persist_error) = self.state.record_error(role_epoch, Some(&error)) {
            tracing::warn!(
                %persist_error,
                %error,
                role_epoch,
                "committed sync cleanup diagnostic could not be persisted"
            );
        }
    }

    fn cleanup_after_commit(&self, effects: &CommitEffects, activation: &str) {
        let cleanup = self
            .state
            .reclaim_obsolete_staged_paths(effects.role_epoch, &effects.obsolete_staged_paths);
        let health = match cleanup {
            Ok(true) => return,
            Ok(false) => format!("{activation} cleanup skipped after a newer role activation"),
            Err(error) => format!("{activation} cleanup failed: {error}"),
        };
        let _ = self.state.record_error(effects.role_epoch, Some(&health));
        tracing::warn!(%health, %activation, "post-commit sync cleanup was incomplete");
    }

    fn load(&self) -> Result<InstallationSnapshot, SyncServiceError> {
        self.state.load().map_err(SyncServiceError::State)
    }

    fn check_available(&self) -> Result<(), SyncServiceError> {
        if self.shut_down.load(Ordering::SeqCst) {
            Err(SyncServiceError::Runtime(RuntimeError::Shutdown))
        } else {
            Ok(())
        }
    }

    fn begin_transition_attempt(&self) -> OperationGeneration {
        OperationGeneration(
            self.operation_generation
                .fetch_add(1, Ordering::SeqCst)
                .wrapping_add(1),
        )
    }

    fn current_operation_generation(&self) -> OperationGeneration {
        OperationGeneration(self.operation_generation.load(Ordering::SeqCst))
    }
}

fn runtime_spec(
    installation: &InstallationSnapshot,
    role_epoch: u64,
) -> Result<RuntimeSpec, SyncServiceError> {
    match installation.settings.role {
        SyncRole::Standalone => Ok(RuntimeSpec::Standalone { role_epoch }),
        SyncRole::HomeHub => {
            let hub_id = installation.identity.hub_id.ok_or_else(|| {
                SyncServiceError::InvalidTransition("persisted Home Hub role has no Hub ID".into())
            })?;
            let owner_login = installation.identity.owner_login.clone().ok_or_else(|| {
                SyncServiceError::InvalidTransition(
                    "persisted Home Hub role has no owner login".into(),
                )
            })?;
            runtime_spec_with_home_identity(installation, hub_id, &owner_login)
        }
        SyncRole::ConnectedDevice => Ok(RuntimeSpec::ConnectedDevice {
            role_epoch,
            hub: installation.settings.hub.clone().ok_or_else(|| {
                SyncServiceError::InvalidTransition(
                    "Connected Device runtime has no Home Hub".into(),
                )
            })?,
            upload_recording_payloads: installation.settings.upload_recording_payloads,
        }),
    }
}

fn runtime_spec_with_home_identity(
    installation: &InstallationSnapshot,
    hub_id: audetic_core::sync::HubId,
    owner_login: &str,
) -> Result<RuntimeSpec, SyncServiceError> {
    if installation.settings.role != SyncRole::HomeHub {
        return Err(SyncServiceError::InvalidTransition(
            "Home Hub runtime requires Home Hub settings".into(),
        ));
    }
    Ok(RuntimeSpec::HomeHub {
        role_epoch: installation.role_epoch,
        hub_id,
        owner_login: owner_login.to_owned(),
        device_name: installation.settings.device_name.clone(),
        upload_recording_payloads: installation.settings.upload_recording_payloads,
    })
}

fn saga_error(primary: SyncServiceError, compensation: Vec<CompensationError>) -> SyncServiceError {
    SyncServiceError::Saga(SagaFailure {
        primary: Box::new(primary),
        compensation,
    })
}

fn activation_health(outcome: &ActivationOutcome) -> ActivationHealth {
    match outcome {
        ActivationOutcome::Healthy { .. } => ActivationHealth::Healthy,
        ActivationOutcome::Degraded { listener_error, .. } => ActivationHealth::Degraded {
            error: listener_error.clone(),
        },
    }
}

fn activation_cleanup_error(outcome: &ActivationOutcome) -> Option<String> {
    let diagnostics = outcome.cleanup_diagnostics();
    (!diagnostics.is_empty()).then(|| {
        diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })
}

fn configure_receipt(
    target: &InstallationSnapshot,
    operation_generation: OperationGeneration,
    verified_connection: Option<HubConnection>,
    activation: ActivationHealth,
    cleanup_error: Option<String>,
    serve_preview: Option<String>,
) -> ConfigureReceipt {
    let role_version = RoleVersion::new(target.role_epoch);
    let activation_error = match &activation {
        ActivationHealth::Degraded { error } => Some(error.clone()),
        ActivationHealth::Inactive | ActivationHealth::Healthy => None,
    };
    let hub_reachable = target.settings.role != SyncRole::Standalone
        && matches!(activation, ActivationHealth::Healthy);
    let dns_name = verified_connection.as_ref().and_then(|connection| {
        canonicalize_base_url(&connection.base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
    });
    let owner_login = verified_connection
        .as_ref()
        .map(|connection| connection.owner_login.clone())
        .or_else(|| target.identity.owner_login.clone());
    let network = SyncNetworkAssessment {
        ready: hub_reachable,
        backend_state: hub_reachable.then(|| "Running".into()),
        dns_name,
        owner_login,
        serve_mapping: (target.settings.role == SyncRole::HomeHub)
            .then_some(ServeMappingState::Audetic),
        funnel_enabled: (target.settings.role == SyncRole::HomeHub).then_some(false),
        serve_preview: serve_preview.clone().unwrap_or_default(),
        error: activation_error.clone(),
    };
    let setup_command = if target.settings.role == SyncRole::HomeHub {
        verified_connection.as_ref().map(|connection| {
            format!(
                "audetic setup --sync-role connected-device --hub-url {} --hub-id {}",
                connection.base_url, connection.hub_id
            )
        })
    } else {
        None
    };
    ConfigureReceipt {
        role_version,
        operation_generation,
        status: SyncStatus {
            device_id: target.identity.device_id,
            role: target.settings.role,
            device_name: target.settings.device_name.clone(),
            local_hub_id: target.identity.hub_id,
            hub: target.settings.hub.clone(),
            hub_reachable,
            last_contact_at: target.settings.last_contact_at.clone(),
            pending_items: 0,
            pending_bytes: 0,
            last_error: activation_error
                .or(cleanup_error)
                .or_else(|| target.settings.last_error.clone()),
            upload_recording_payloads: target.settings.upload_recording_payloads,
            cache_level: target.settings.cache_level,
            shared_config_enabled: target.settings.shared_config_enabled,
            applied_shared_config_version: target.settings.shared_config_version,
            network,
        },
        activation,
        serve_preview,
        verified_connection,
        setup_command,
    }
}

fn append_receipt_error(status: &mut SyncStatus, error: String) {
    status.last_error = Some(match status.last_error.take() {
        Some(existing) => format!("{existing}; {error}"),
        None => error,
    });
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

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use audetic_core::sync::{CacheLevel, DeviceId, HubCandidate, HubId, RecordId};
    use semver::Version;
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use crate::db::sync_identity::SyncIdentityRepository;
    use crate::db::sync_serve::SyncServeRepository;
    use crate::db::sync_settings::SyncSettingsRepository;
    use crate::sync::protocol::{
        DictationPage, MeetingPage, MeetingTitlePatch, RecordKind, SharedMeeting, Snapshot,
        SnapshotBatch, SnapshotBatchResponse, SnapshotDisposition, SnapshotResult,
    };
    use crate::sync::tailscale::{
        MappingState, ServeAssessment, TailscaleControl, TailscaleStatus,
    };
    use crate::sync::transport::{
        BlobUpload, DiscoveryOutcome, HubProbe, HubTransferError, RemoteDictationLibrary,
        RemoteLibraryMutations, RemoteMeetingLibrary, RemotePayloadSource, ReplicationTransport,
        StreamingPayloadResponse,
    };
    use crate::sync::SyncService;

    struct FakeTailscale {
        mapping: StdMutex<MappingState>,
        funnel: AtomicBool,
        owner_login: StdMutex<String>,
        fail_apply: AtomicBool,
        fail_status: AtomicBool,
        apply_calls: std::sync::atomic::AtomicUsize,
        remove_calls: std::sync::atomic::AtomicUsize,
    }

    impl Default for FakeTailscale {
        fn default() -> Self {
            Self {
                mapping: StdMutex::new(MappingState::Vacant),
                funnel: AtomicBool::new(false),
                owner_login: StdMutex::new("owner@example.com".into()),
                fail_apply: AtomicBool::new(false),
                fail_status: AtomicBool::new(false),
                apply_calls: std::sync::atomic::AtomicUsize::new(0),
                remove_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl FakeTailscale {
        fn status_value(&self) -> TailscaleStatus {
            TailscaleStatus {
                version: Version::parse("1.80.0").unwrap(),
                backend_state: "Running".into(),
                self_dns_name: "home.example.ts.net.".into(),
                owner_login: self.owner_login.lock().unwrap().clone(),
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
                Ok(self.status_value())
            }
        }

        fn serve_assessment(&self) -> Result<ServeAssessment, TailscaleError> {
            Ok(ServeAssessment {
                mapping: *self.mapping.lock().unwrap(),
                funnel_enabled: self.funnel.load(Ordering::SeqCst),
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
        listener_address: StdMutex<Option<SocketAddr>>,
        blob_upload_calls: std::sync::atomic::AtomicUsize,
        snapshot_uploads: StdMutex<Vec<(HubId, Vec<RecordId>)>>,
        blob_uploads: StdMutex<Vec<(HubId, RecordId)>>,
    }

    #[async_trait]
    impl HubProbe for FakeHubs {
        async fn handshake(&self, hub: &HubConnection) -> Result<HubCandidate, HubTransferError> {
            if self.fail.load(Ordering::SeqCst) {
                Err(HubTransferError::Transport("hub offline".into()))
            } else {
                let listener_address = *self.listener_address.lock().unwrap();
                if let Some(address) = listener_address {
                    let listener_handshake = async {
                        let mut stream = tokio::net::TcpStream::connect(address).await?;
                        stream
                            .write_all(b"GET /v1/info HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                            .await?;
                        let mut response = [0_u8; 1024];
                        let read = stream.read(&mut response).await?;
                        anyhow::ensure!(
                            response[..read]
                                .windows(b"x-audetic".len())
                                .any(|value| value == b"x-audetic"),
                            "listener did not return an Audetic response"
                        );
                        anyhow::Ok(())
                    };
                    tokio::time::timeout(std::time::Duration::from_millis(250), listener_handshake)
                        .await
                        .map_err(|_| {
                            HubTransferError::Transport("listener handshake timed out".into())
                        })?
                        .map_err(|error| HubTransferError::Transport(error.to_string()))?;
                }
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

    #[async_trait]
    impl ReplicationTransport for FakeHubs {
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
            blob: BlobUpload,
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

    struct UnusedRemoteLibrary;

    #[async_trait]
    impl RemoteDictationLibrary for UnusedRemoteLibrary {
        async fn page_dictations(
            &self,
            _hub: &HubConnection,
            _query: Option<&str>,
            _from: Option<&str>,
            _to: Option<&str>,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> Result<DictationPage, HubTransferError> {
            Err(HubTransferError::Retryable("unused".to_owned()))
        }
    }

    #[async_trait]
    impl RemoteMeetingLibrary for UnusedRemoteLibrary {
        async fn page_meetings(
            &self,
            _hub: &HubConnection,
            _query: Option<&str>,
            _cursor: Option<&str>,
            _limit: usize,
        ) -> Result<MeetingPage, HubTransferError> {
            Err(HubTransferError::Retryable("unused".to_owned()))
        }

        async fn meeting(
            &self,
            _hub: &HubConnection,
            _id: RecordId,
        ) -> Result<Option<SharedMeeting>, HubTransferError> {
            Err(HubTransferError::Retryable("unused".to_owned()))
        }
    }

    #[async_trait]
    impl RemoteLibraryMutations for UnusedRemoteLibrary {
        async fn update_meeting_title(
            &self,
            _hub: &HubConnection,
            _id: RecordId,
            _patch: MeetingTitlePatch,
        ) -> Result<SharedMeeting, HubTransferError> {
            Err(HubTransferError::Retryable("unused".to_owned()))
        }

        async fn delete_record(
            &self,
            _hub: &HubConnection,
            _id: RecordId,
            _kind: RecordKind,
        ) -> Result<(), HubTransferError> {
            Err(HubTransferError::Retryable("unused".to_owned()))
        }
    }

    struct UnusedRemotePayloads;

    #[async_trait]
    impl RemotePayloadSource for UnusedRemotePayloads {
        async fn stream_payload(
            &self,
            _hub: &HubConnection,
            _id: RecordId,
            _kind: RecordKind,
            _range: Option<&str>,
        ) -> Result<StreamingPayloadResponse, HubTransferError> {
            Err(HubTransferError::Retryable("unused".to_owned()))
        }
    }

    fn test_capabilities(hubs: Arc<FakeHubs>) -> HubCapabilities {
        let unused_library = Arc::new(UnusedRemoteLibrary);
        HubCapabilities::for_test(
            hubs.clone(),
            hubs,
            unused_library.clone(),
            unused_library.clone(),
            unused_library,
            Arc::new(UnusedRemotePayloads),
        )
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
            test_capabilities(hubs.clone()),
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

    #[tokio::test]
    async fn stale_library_observation_cannot_cross_a_role_transition() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let connection = crate::db::open_db_at(&fixture.path).unwrap();
        connection
            .execute(
                "UPDATE sync_settings SET role='home_hub', role_epoch=role_epoch+1, \
                 last_error=NULL WHERE singleton=1",
                [],
            )
            .unwrap();
        let stale = fixture.service.coordinator.library_context().unwrap();
        connection
            .execute(
                "UPDATE sync_settings SET role='standalone', role_epoch=role_epoch+1, \
                 last_error='new role health' WHERE singleton=1",
                [],
            )
            .unwrap();

        fixture
            .service
            .coordinator
            .record_library_observation(
                &stale,
                LibraryObservation::Unreachable("stale query failure".into()),
            )
            .await
            .unwrap();

        let settings = SyncSettingsRepository::get(&connection).unwrap();
        assert_eq!(settings.role, SyncRole::Standalone);
        assert_eq!(settings.last_error.as_deref(), Some("new role health"));
    }

    fn pause_at(
        service: &SyncService,
        checkpoint: TransitionCheckpoint,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *service.coordinator.transition_pause.lock().unwrap() = Some(TransitionPause {
            checkpoint,
            entered: entered.clone(),
            release: release.clone(),
        });
        (entered, release)
    }

    fn pause_receipt_enrichment(
        service: &SyncService,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *service.coordinator.receipt_enrichment_pause.lock().unwrap() =
            Some(ReceiptEnrichmentPause {
                entered: entered.clone(),
                release: release.clone(),
            });
        (entered, release)
    }

    async fn wait_for_settled_runtime(service: &SyncService, role: SyncRole) {
        for _ in 0..100 {
            let snapshot = service.coordinator.runtime.snapshot().await;
            if snapshot.role == Some(role) && !snapshot.transition_in_progress {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("runtime did not settle as {role:?}");
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

    async fn assert_allowed_transition(from: SyncRole, to: SyncRole) {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let hub_id = HubId::new();
        match from {
            SyncRole::Standalone => {}
            SyncRole::HomeHub => {
                fixture
                    .service
                    .configure(request(SyncRole::HomeHub))
                    .await
                    .unwrap();
            }
            SyncRole::ConnectedDevice => {
                fixture
                    .service
                    .configure(connected_request(hub_id))
                    .await
                    .unwrap();
            }
        }
        let target = match to {
            SyncRole::ConnectedDevice => connected_request(hub_id),
            role => request(role),
        };

        let result = fixture.service.configure(target).await.unwrap();

        assert_eq!(result.status.role, to, "{from:?} -> {to:?}");
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn every_permitted_role_transition_is_accepted() {
        for (from, to) in [
            (SyncRole::Standalone, SyncRole::Standalone),
            (SyncRole::Standalone, SyncRole::HomeHub),
            (SyncRole::Standalone, SyncRole::ConnectedDevice),
            (SyncRole::HomeHub, SyncRole::HomeHub),
            (SyncRole::HomeHub, SyncRole::Standalone),
            (SyncRole::ConnectedDevice, SyncRole::ConnectedDevice),
            (SyncRole::ConnectedDevice, SyncRole::Standalone),
        ] {
            assert_allowed_transition(from, to).await;
        }
    }

    #[tokio::test]
    async fn direct_home_hub_and_connected_device_transitions_are_rejected() {
        let home = fixture();
        home.service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        let error = home
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap_err();
        assert!(matches!(error, SyncServiceError::InvalidTransition(_)));
        home.service.shutdown().await.unwrap();

        let connected = fixture();
        connected
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap();
        let error = connected
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap_err();
        assert!(matches!(error, SyncServiceError::InvalidTransition(_)));
        connected.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn owner_mismatch_is_rejected_for_connected_and_existing_home_hub_roles() {
        let connected = fixture();
        *connected.tailscale.owner_login.lock().unwrap() = "other@example.com".into();
        let error = connected
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap_err();
        assert!(matches!(error, SyncServiceError::InvalidRequest(_)));

        let home = fixture();
        home.service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        *home.tailscale.owner_login.lock().unwrap() = "other@example.com".into();
        let error = home
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap_err();
        assert!(matches!(error, SyncServiceError::InvalidRequest(_)));
        home.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn role_epoch_cas_failure_rolls_back_runtime_and_created_serve_mapping() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let (entered, release) = pause_at(&fixture.service, TransitionCheckpoint::Quiesced);
        let service = fixture.service.clone();
        let caller =
            tokio::spawn(async move { service.configure(request(SyncRole::HomeHub)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
            .await
            .expect("promotion did not quiesce");
        let connection = crate::db::open_db_at(&fixture.path).unwrap();
        connection
            .execute("UPDATE sync_settings SET role_epoch = role_epoch + 1", [])
            .unwrap();
        drop(connection);
        release.notify_one();

        let error = caller.await.unwrap().unwrap_err();

        assert!(matches!(
            error,
            SyncServiceError::State(StateError::EpochMismatch(_))
        ));
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::Vacant
        );
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert_eq!(runtime.role, Some(SyncRole::Standalone));
        assert!(!runtime.transition_in_progress);
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn dormant_home_hub_reactivation_preserves_its_hub_identity() {
        let fixture = fixture();
        let first = fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap()
            .status
            .local_hub_id
            .unwrap();
        fixture
            .service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap();

        let second = fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap()
            .status
            .local_hub_id
            .unwrap();

        assert_eq!(second, first);
        fixture.service.shutdown().await.unwrap();
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

        assert!(matches!(error, SyncServiceError::Hub(_)));
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
        assert!(fixture.service.coordinator.hub_runtime_running().await);
        assert!(
            fixture
                .service
                .coordinator
                .runtime
                .snapshot()
                .await
                .outbox_worker_running
        );
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

        assert!(matches!(error, SyncServiceError::State(_)));
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
        assert!(!fixture.service.coordinator.hub_runtime_running().await);
        assert!(
            !fixture
                .service
                .coordinator
                .runtime
                .snapshot()
                .await
                .outbox_worker_running
        );
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
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert!(runtime.outbox_worker_running);
        assert!(runtime.hub_reachable);
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

        assert!(matches!(error, SyncServiceError::State(_)));
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
        assert!(
            fixture
                .service
                .coordinator
                .runtime
                .snapshot()
                .await
                .outbox_worker_running
        );
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
        fixture.service.shutdown().await.unwrap();
        fixture.hubs.fail.store(true, Ordering::SeqCst);

        let restarted = SyncService::with_dependencies(
            fixture.path.clone(),
            fixture.tailscale.clone(),
            test_capabilities(fixture.hubs.clone()),
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

        assert!(matches!(error, SyncServiceError::State(_)));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::ConnectedDevice
        );
        assert!(
            fixture
                .service
                .coordinator
                .runtime
                .snapshot()
                .await
                .outbox_worker_running
        );
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

        assert!(matches!(error, SyncServiceError::State(_)));
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        assert_eq!(
            SyncSettingsRepository::get(&conn).unwrap().role,
            SyncRole::HomeHub
        );
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
        assert!(fixture.service.coordinator.hub_runtime_running().await);
        assert!(
            fixture
                .service
                .coordinator
                .runtime
                .snapshot()
                .await
                .outbox_worker_running
        );
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

    #[tokio::test]
    async fn runtime_shape_tracks_exactly_one_current_role() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        assert_eq!(
            fixture.service.coordinator.runtime.snapshot().await,
            super::super::runtime::RuntimeSnapshot {
                role: Some(SyncRole::Standalone),
                role_version: Some(RoleVersion::new(0)),
                ownership_held: true,
                ..Default::default()
            }
        );

        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        let home = fixture.service.coordinator.runtime.snapshot().await;
        assert_eq!(home.role, Some(SyncRole::HomeHub));
        assert!(home.hub_listener_running);
        assert!(home.outbox_worker_running);

        fixture
            .service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap();
        let standalone = fixture.service.coordinator.runtime.snapshot().await;
        assert_eq!(standalone.role, Some(SyncRole::Standalone));
        assert!(!standalone.hub_listener_running);
        assert!(!standalone.outbox_worker_running);

        fixture
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap();
        let connected = fixture.service.coordinator.runtime.snapshot().await;
        assert_eq!(connected.role, Some(SyncRole::ConnectedDevice));
        assert!(!connected.hub_listener_running);
        assert!(connected.outbox_worker_running);
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn prepared_home_runtime_abort_releases_listener_without_activation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        crate::db::migrate_db_at(&path).unwrap();
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        drop(reserved);
        let tailscale = Arc::new(FakeTailscale::default());
        let hubs = Arc::new(FakeHubs::default());
        let service =
            SyncService::with_dependencies(path, tailscale, test_capabilities(hubs), address);
        service
            .coordinator
            .runtime
            .acquire_ownership()
            .await
            .unwrap();
        let transition = service
            .coordinator
            .runtime
            .begin_transition(RuntimeSpec::HomeHub {
                role_epoch: 1,
                hub_id: HubId::new(),
                owner_login: "owner@example.com".into(),
                device_name: Some("Hub".into()),
                upload_recording_payloads: false,
            })
            .await
            .unwrap();
        tokio::net::TcpStream::connect(address).await.unwrap();

        service
            .coordinator
            .runtime
            .abort_transition(transition)
            .await
            .unwrap();

        assert_eq!(service.coordinator.runtime.snapshot().await.role, None);
        tokio::net::TcpListener::bind(address).await.unwrap();
    }

    #[tokio::test]
    async fn status_does_not_wait_for_the_transition_mutex() {
        let fixture = fixture();
        let _transition = fixture.service.coordinator.transition.lock().await;

        let status =
            tokio::time::timeout(std::time::Duration::from_secs(1), fixture.service.status())
                .await
                .expect("status should use only the short runtime inspection lock")
                .unwrap();

        assert_eq!(status.role, SyncRole::Standalone);
    }

    #[tokio::test]
    async fn concurrent_configure_receipts_are_transition_coherent() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let (entered, release) = pause_at(&fixture.service, TransitionCheckpoint::Committed);
        let promote_service = fixture.service.clone();
        let promote = tokio::spawn(async move {
            promote_service
                .configure(request(SyncRole::HomeHub))
                .await
                .unwrap()
        });
        entered.notified().await;
        let demote_service = fixture.service.clone();
        let demote = tokio::spawn(async move {
            demote_service
                .configure(request(SyncRole::Standalone))
                .await
                .unwrap()
        });
        release.notify_one();

        let promoted = promote.await.unwrap();
        let demoted = demote.await.unwrap();

        assert_eq!(promoted.status.role, SyncRole::HomeHub);
        assert!(promoted.status.hub_reachable);
        assert!(promoted.setup_command.is_some());
        assert_eq!(demoted.status.role, SyncRole::Standalone);
        assert!(demoted.setup_command.is_none());
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn configure_receipt_survives_outbox_enrichment_failure_after_commit() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let receipt = fixture
            .service
            .coordinator
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap();
        let connection = crate::db::open_db_at(&fixture.path).unwrap();
        connection
            .execute("DROP TABLE sync_outbox_items", [])
            .unwrap();
        drop(connection);

        let result = fixture
            .service
            .coordinator
            .enrich_configure_receipt(receipt)
            .await;

        assert_eq!(result.status.role, SyncRole::Standalone);
        assert!(result
            .status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("outbox enrichment failed")));
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn configure_receipt_survives_network_enrichment_failure_after_commit() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let receipt = fixture
            .service
            .coordinator
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        fixture.tailscale.fail_status.store(true, Ordering::SeqCst);

        let result = fixture
            .service
            .coordinator
            .enrich_configure_receipt(receipt)
            .await;

        assert_eq!(result.status.role, SyncRole::HomeHub);
        assert!(result.status.hub_reachable);
        assert!(result
            .status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("network enrichment failed")));
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn overlapping_transition_discards_new_role_receipt_enrichment() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let receipt = fixture
            .service
            .coordinator
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        let connection = crate::db::open_db_at(&fixture.path).unwrap();
        let (_, record_id) = crate::db::insert_workflow_record(
            &connection,
            &crate::db::Workflow::new(
                crate::db::WorkflowType::VoiceToText,
                crate::db::WorkflowData::VoiceToText(crate::db::VoiceToTextData {
                    text: "belongs to the next receipt generation".into(),
                    audio_path: "/missing".into(),
                }),
            ),
        )
        .unwrap();
        drop(connection);
        let (entered, release) = pause_receipt_enrichment(&fixture.service);
        let coordinator = fixture.service.coordinator.clone();
        let enrichment =
            tokio::spawn(async move { coordinator.enrich_configure_receipt(receipt).await });
        entered.notified().await;

        let next = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            fixture
                .service
                .coordinator
                .configure(request(SyncRole::Standalone)),
        )
        .await
        .expect("receipt enrichment must not retain the transition lock")
        .unwrap();
        assert_eq!(next.status.role, SyncRole::Standalone);
        let connection = crate::db::open_db_at(&fixture.path).unwrap();
        connection
            .execute(
                "UPDATE sync_outbox_items SET state='pending' WHERE record_id=?1",
                [record_id.to_string()],
            )
            .unwrap();
        drop(connection);
        release.notify_one();

        let old = enrichment.await.unwrap();
        let current = fixture.service.status().await.unwrap();

        assert_eq!(old.status.role, SyncRole::HomeHub);
        assert_eq!(old.status.pending_items, 0);
        assert_eq!(
            old.status.network.serve_mapping,
            Some(ServeMappingState::Audetic)
        );
        assert!(old.status.network.ready);
        assert_eq!(current.role, SyncRole::Standalone);
        assert_eq!(current.pending_items, 1);
        assert_eq!(
            current.network.serve_mapping,
            Some(ServeMappingState::Vacant)
        );
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_overlapping_promotion_discards_provisional_serve_enrichment() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let receipt = fixture
            .service
            .coordinator
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap();
        let committed_epoch = fixture
            .service
            .coordinator
            .state
            .current_role_epoch()
            .unwrap();
        let (entered, release) = pause_receipt_enrichment(&fixture.service);
        let coordinator = fixture.service.coordinator.clone();
        let enrichment =
            tokio::spawn(async move { coordinator.enrich_configure_receipt(receipt).await });
        entered.notified().await;

        fixture.hubs.fail.store(true, Ordering::SeqCst);
        let error = fixture
            .service
            .coordinator
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap_err();
        assert!(matches!(error, SyncServiceError::Hub(_)));
        assert_eq!(fixture.tailscale.apply_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.tailscale.remove_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture
                .service
                .coordinator
                .state
                .current_role_epoch()
                .unwrap(),
            committed_epoch
        );
        let connection = crate::db::open_db_at(&fixture.path).unwrap();
        connection
            .execute("DROP TABLE sync_outbox_items", [])
            .unwrap();
        drop(connection);
        release.notify_one();

        let original = enrichment.await.unwrap();

        assert_eq!(original.status.role, SyncRole::Standalone);
        assert_eq!(original.status.pending_items, 0);
        assert_eq!(original.status.network.serve_mapping, None);
        assert!(original.status.last_error.is_none());
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn demotion_quiescence_failure_restores_runtime_and_serve_mapping() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        fixture.service.coordinator.runtime.fail_next_quiesce();

        let error = fixture
            .service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap_err();

        assert!(matches!(error, SyncServiceError::Runtime(_)));
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
        let status = fixture.service.status().await.unwrap();
        assert_eq!(status.role, SyncRole::HomeHub);
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert!(runtime.hub_listener_running);
        assert!(runtime.outbox_worker_running);
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn failed_obsolete_listener_cleanup_does_not_fail_committed_demotion() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        crate::db::migrate_db_at(&path).unwrap();
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        drop(reserved);
        let tailscale = Arc::new(FakeTailscale::default());
        let hubs = Arc::new(FakeHubs::default());
        let service = SyncService::with_dependencies(
            path,
            tailscale.clone(),
            test_capabilities(hubs),
            address,
        );
        service.initialize().await.unwrap();
        service.configure(request(SyncRole::HomeHub)).await.unwrap();
        service
            .coordinator
            .runtime
            .fail_active_listener_cleanup()
            .await
            .unwrap();

        let result = service
            .configure(request(SyncRole::Standalone))
            .await
            .unwrap();

        assert_eq!(result.status.role, SyncRole::Standalone);
        assert!(result
            .status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("obsolete Home Hub listener cleanup failed")));
        assert_eq!(*tailscale.mapping.lock().unwrap(), MappingState::Vacant);
        let runtime = service.coordinator.runtime.snapshot().await;
        assert_eq!(runtime.role, Some(SyncRole::Standalone));
        assert!(!runtime.hub_listener_running);
        assert!(!runtime.outbox_worker_running);
        assert!(!runtime.transition_in_progress);
        tokio::net::TcpListener::bind(address).await.unwrap();
        service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn status_remains_responsive_while_worker_join_is_held() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        let (entered, release) = fixture.service.coordinator.runtime.install_quiesce_pause();
        let service = fixture.service.clone();
        let demotion =
            tokio::spawn(async move { service.configure(request(SyncRole::Standalone)).await });
        entered.notified().await;

        let status = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            fixture.service.status(),
        )
        .await
        .expect("status must not wait for worker/listener joins")
        .unwrap();
        assert_eq!(status.role, SyncRole::HomeHub);

        release.notify_one();
        demotion.await.unwrap().unwrap();
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn restart_reconstructs_the_current_home_hub_runtime() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        fixture.service.shutdown().await.unwrap();
        let restarted = SyncService::with_dependencies(
            fixture.path.clone(),
            fixture.tailscale.clone(),
            test_capabilities(fixture.hubs.clone()),
            "127.0.0.1:0".parse().unwrap(),
        );

        let status = restarted.initialize().await.unwrap();
        let runtime = restarted.coordinator.runtime.snapshot().await;

        assert_eq!(status.role, SyncRole::HomeHub);
        assert!(runtime.hub_listener_running);
        assert!(runtime.outbox_worker_running);
        restarted.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn persisted_home_hub_bind_failure_is_degraded_but_keeps_local_outbox_running() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        fixture.service.shutdown().await.unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        let (_, record_id) = crate::db::insert_workflow_record(
            &conn,
            &crate::db::Workflow::new(
                crate::db::WorkflowType::VoiceToText,
                crate::db::WorkflowData::VoiceToText(crate::db::VoiceToTextData {
                    text: "queued while the listener is unavailable".into(),
                    audio_path: "/missing".into(),
                }),
            ),
        )
        .unwrap();
        drop(conn);
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = occupied.local_addr().unwrap();
        let restarted = SyncService::with_dependencies(
            fixture.path.clone(),
            fixture.tailscale.clone(),
            test_capabilities(fixture.hubs.clone()),
            address,
        );

        let status = restarted.initialize().await.unwrap();

        assert_eq!(status.role, SyncRole::HomeHub);
        assert!(!status.hub_reachable);
        assert!(status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("listener")));
        let runtime = restarted.coordinator.runtime.snapshot().await;
        assert!(!runtime.hub_listener_running);
        assert!(runtime.outbox_worker_running);
        for _ in 0..100 {
            let conn = crate::db::open_db_at(&fixture.path).unwrap();
            let state: String = conn
                .query_row(
                    "SELECT state FROM sync_outbox_items WHERE record_id=?1",
                    [record_id.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            if state == "synced" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        let state: String = conn
            .query_row(
                "SELECT state FROM sync_outbox_items WHERE record_id=?1",
                [record_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "synced");
        restarted.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retry_reactivates_a_home_hub_degraded_during_startup() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        fixture.service.shutdown().await.unwrap();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = occupied.local_addr().unwrap();
        let restarted = SyncService::with_dependencies(
            fixture.path.clone(),
            fixture.tailscale.clone(),
            test_capabilities(fixture.hubs.clone()),
            address,
        );
        let degraded = restarted.initialize().await.unwrap();
        assert!(!degraded.hub_reachable);
        drop(occupied);

        restarted.retry().await.unwrap();

        let recovered = restarted.status().await.unwrap();
        assert!(recovered.hub_reachable);
        assert!(recovered.last_error.is_none());
        let runtime = restarted.coordinator.runtime.snapshot().await;
        assert!(runtime.hub_listener_running);
        assert!(runtime.outbox_worker_running);
        restarted.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retry_prepares_home_listener_before_the_public_handshake() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("audetic.db");
        crate::db::migrate_db_at(&path).unwrap();
        let tailscale = Arc::new(FakeTailscale::default());
        let hubs = Arc::new(FakeHubs::default());
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        drop(reserved);
        *hubs.listener_address.lock().unwrap() = Some(address);
        let service = SyncService::with_dependencies(
            path.clone(),
            tailscale.clone(),
            test_capabilities(hubs.clone()),
            address,
        );
        service.configure(request(SyncRole::HomeHub)).await.unwrap();
        service.shutdown().await.unwrap();

        let occupied = std::net::TcpListener::bind(address).unwrap();
        let restarted =
            SyncService::with_dependencies(path, tailscale, test_capabilities(hubs), address);
        assert!(!restarted.initialize().await.unwrap().hub_reachable);
        drop(occupied);

        restarted.retry().await.unwrap();

        let status = restarted.status().await.unwrap();
        assert!(status.hub_reachable);
        assert!(status.last_error.is_none());
        restarted.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retry_clears_health_only_after_full_environment_verification() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        fixture.service.shutdown().await.unwrap();
        *fixture.tailscale.owner_login.lock().unwrap() = "other@example.com".into();
        let restarted = SyncService::with_dependencies(
            fixture.path.clone(),
            fixture.tailscale.clone(),
            test_capabilities(fixture.hubs.clone()),
            "127.0.0.1:0".parse().unwrap(),
        );
        let degraded = restarted.initialize().await.unwrap();
        assert!(!degraded.hub_reachable);
        let degraded_error = degraded.last_error.clone();

        assert!(matches!(
            restarted.retry().await.unwrap_err(),
            SyncServiceError::InvalidTransition(_)
        ));
        let still_degraded = restarted.status().await.unwrap();
        assert!(!still_degraded.hub_reachable);
        assert_eq!(still_degraded.last_error, degraded_error);

        *fixture.tailscale.owner_login.lock().unwrap() = "owner@example.com".into();
        restarted.retry().await.unwrap();
        let healthy = restarted.status().await.unwrap();
        assert!(healthy.hub_reachable);
        assert!(healthy.last_error.is_none());
        restarted.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn second_runtime_for_the_same_database_cannot_write_to_persisted_authority() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let old_hub = HubId::new();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        SyncSettingsRepository::save(
            &conn,
            &SyncSettings {
                role: SyncRole::ConnectedDevice,
                hub: connected_request(old_hub).hub,
                ..SyncSettings::default()
            },
        )
        .unwrap();
        crate::db::insert_workflow_record(
            &conn,
            &crate::db::Workflow::new(
                crate::db::WorkflowType::VoiceToText,
                crate::db::WorkflowData::VoiceToText(crate::db::VoiceToTextData {
                    text: "must not be uploaded by a second runtime".into(),
                    audio_path: "/missing".into(),
                }),
            ),
        )
        .unwrap();
        drop(conn);
        let second_hubs = Arc::new(FakeHubs::default());
        let second = SyncService::with_dependencies(
            fixture.path.clone(),
            fixture.tailscale.clone(),
            test_capabilities(second_hubs.clone()),
            "127.0.0.1:0".parse().unwrap(),
        );

        let error = second.initialize().await.unwrap_err();

        assert!(matches!(error, SyncServiceError::Runtime(_)));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(second_hubs.snapshot_uploads.lock().unwrap().is_empty());
        assert!(second_hubs.blob_uploads.lock().unwrap().is_empty());
        assert!(!second.coordinator.runtime.snapshot().await.ownership_held);
        drop(fixture.service);
        let status = second.initialize().await.unwrap();
        assert_eq!(status.role, SyncRole::ConnectedDevice);
        second.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn dropped_callers_cannot_cancel_home_hub_promotion_at_transition_boundaries() {
        for checkpoint in [
            TransitionCheckpoint::Prepared,
            TransitionCheckpoint::Verified,
            TransitionCheckpoint::Quiesced,
            TransitionCheckpoint::Committed,
        ] {
            let fixture = fixture();
            fixture.service.initialize().await.unwrap();
            let (entered, release) = pause_at(&fixture.service, checkpoint);
            let service = fixture.service.clone();
            let caller =
                tokio::spawn(async move { service.configure(request(SyncRole::HomeHub)).await });
            tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
                .await
                .expect("transition did not reach checkpoint");

            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            release.notify_one();

            wait_for_settled_runtime(&fixture.service, SyncRole::HomeHub).await;
            let runtime = fixture.service.coordinator.runtime.snapshot().await;
            assert!(runtime.hub_listener_running, "checkpoint {checkpoint:?}");
            assert!(runtime.outbox_worker_running, "checkpoint {checkpoint:?}");
            assert_eq!(
                *fixture.tailscale.mapping.lock().unwrap(),
                MappingState::OwnedByAudetic,
                "checkpoint {checkpoint:?}"
            );
            fixture.service.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn dropped_demotion_caller_still_restores_worker_and_serve_mapping_on_commit_failure() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();
        let conn = crate::db::open_db_at(&fixture.path).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_cancelled_standalone_role
             BEFORE UPDATE OF role ON sync_settings
             WHEN NEW.role='standalone'
             BEGIN SELECT RAISE(ABORT, 'simulated cancelled demotion failure'); END;",
        )
        .unwrap();
        drop(conn);
        let (entered, release) = pause_at(&fixture.service, TransitionCheckpoint::Quiesced);
        let service = fixture.service.clone();
        let caller =
            tokio::spawn(async move { service.configure(request(SyncRole::Standalone)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
            .await
            .expect("demotion did not quiesce the worker");

        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        release.notify_one();

        wait_for_settled_runtime(&fixture.service, SyncRole::HomeHub).await;
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert!(runtime.hub_listener_running);
        assert!(runtime.outbox_worker_running);
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn listener_termination_before_commit_rolls_back_promotion() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let (entered, release) = pause_at(&fixture.service, TransitionCheckpoint::Prepared);
        let service = fixture.service.clone();
        let caller =
            tokio::spawn(async move { service.configure(request(SyncRole::HomeHub)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
            .await
            .expect("promotion did not prepare its listener");
        fixture
            .service
            .coordinator
            .runtime
            .terminate_provisional_listener()
            .await
            .unwrap();
        release.notify_one();

        let error = caller.await.unwrap().unwrap_err();

        assert!(matches!(
            error,
            SyncServiceError::Runtime(RuntimeError::Listener(_))
        ));
        let installation = fixture.service.coordinator.state.load().unwrap();
        assert_eq!(installation.settings.role, SyncRole::Standalone);
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::Vacant
        );
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert_eq!(runtime.role, Some(SyncRole::Standalone));
        assert!(!runtime.transition_in_progress);
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn listener_termination_after_quiescence_rolls_back_promotion() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let (entered, release) = pause_at(&fixture.service, TransitionCheckpoint::Quiesced);
        let service = fixture.service.clone();
        let caller =
            tokio::spawn(async move { service.configure(request(SyncRole::HomeHub)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
            .await
            .expect("promotion did not quiesce its prior worker");
        fixture
            .service
            .coordinator
            .runtime
            .terminate_provisional_listener()
            .await
            .unwrap();
        release.notify_one();

        let error = caller.await.unwrap().unwrap_err();

        assert!(matches!(
            error,
            SyncServiceError::Runtime(RuntimeError::Listener(_))
        ));
        let installation = fixture.service.coordinator.state.load().unwrap();
        assert_eq!(installation.settings.role, SyncRole::Standalone);
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::Vacant
        );
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert_eq!(runtime.role, Some(SyncRole::Standalone));
        assert!(!runtime.transition_in_progress);
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn listener_termination_after_commit_degrades_home_runtime_and_retry_recovers() {
        let fixture = fixture();
        fixture.service.initialize().await.unwrap();
        let (entered, release) = pause_at(&fixture.service, TransitionCheckpoint::Committed);
        let service = fixture.service.clone();
        let caller =
            tokio::spawn(async move { service.configure(request(SyncRole::HomeHub)).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
            .await
            .expect("promotion did not durably commit");
        fixture
            .service
            .coordinator
            .runtime
            .terminate_provisional_listener()
            .await
            .unwrap();
        release.notify_one();

        let result = caller.await.unwrap().unwrap();

        assert_eq!(result.status.role, SyncRole::HomeHub);
        assert!(!result.status.hub_reachable);
        assert!(result
            .status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("listener")));
        let installation = fixture.service.coordinator.state.load().unwrap();
        assert_eq!(installation.settings.role, SyncRole::HomeHub);
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert_eq!(runtime.role, Some(SyncRole::HomeHub));
        assert_eq!(
            runtime.role_version,
            Some(RoleVersion::new(installation.role_epoch))
        );
        assert!(!runtime.hub_listener_running);
        assert!(runtime.outbox_worker_running);
        assert!(!runtime.transition_in_progress);

        fixture.service.retry().await.unwrap();

        let recovered = fixture.service.status().await.unwrap();
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert!(recovered.hub_reachable);
        assert!(recovered.last_error.is_none());
        assert_eq!(runtime.role, Some(SyncRole::HomeHub));
        assert_eq!(
            runtime.role_version,
            Some(RoleVersion::new(installation.role_epoch))
        );
        assert!(runtime.hub_listener_running);
        assert!(runtime.outbox_worker_running);
        assert!(!runtime.transition_in_progress);
        fixture.service.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_failed_initialize_cleans_provisional_runtime_and_process_lease() {
        let fixture = fixture();
        fixture
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap();
        fixture.service.shutdown().await.unwrap();
        let connection = crate::db::open_db_at(&fixture.path).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_initialize_health
                 BEFORE UPDATE OF last_contact_at,last_error ON sync_settings
                 BEGIN SELECT RAISE(ABORT, 'simulated initialize health failure'); END;",
            )
            .unwrap();
        drop(connection);
        let restarted = SyncService::with_dependencies(
            fixture.path.clone(),
            fixture.tailscale.clone(),
            test_capabilities(fixture.hubs.clone()),
            "127.0.0.1:0".parse().unwrap(),
        );
        let (entered, release) = pause_at(&restarted, TransitionCheckpoint::RestorePrepared);
        let service = restarted.clone();
        let caller = tokio::spawn(async move { service.initialize().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
            .await
            .expect("initialize did not prepare the persisted runtime");

        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        release.notify_one();

        for _ in 0..100 {
            let runtime = restarted.coordinator.runtime.snapshot().await;
            if !runtime.transition_in_progress && !runtime.ownership_held {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let runtime = restarted.coordinator.runtime.snapshot().await;
        assert!(!runtime.transition_in_progress);
        assert!(!runtime.ownership_held);
        let connection = crate::db::open_db_at(&fixture.path).unwrap();
        connection
            .execute_batch("DROP TRIGGER reject_initialize_health")
            .unwrap();
        drop(connection);
        let status = restarted.initialize().await.unwrap();
        assert_eq!(status.role, SyncRole::ConnectedDevice);
        restarted.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_shutdown_is_resumed_and_releases_process_ownership() {
        let fixture = fixture();
        fixture
            .service
            .configure(connected_request(HubId::new()))
            .await
            .unwrap();
        let (entered, release) = fixture.service.coordinator.runtime.install_shutdown_pause();
        let service = fixture.service.clone();
        let caller = tokio::spawn(async move { service.shutdown().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), entered.notified())
            .await
            .expect("shutdown did not reach runtime task joins");

        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        release.notify_one();
        fixture.service.shutdown().await.unwrap();

        let second = SyncService::with_dependencies(
            fixture.path.clone(),
            fixture.tailscale.clone(),
            test_capabilities(fixture.hubs.clone()),
            "127.0.0.1:0".parse().unwrap(),
        );
        let status = second.initialize().await.unwrap();
        assert_eq!(status.role, SyncRole::ConnectedDevice);
        second.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn active_listener_termination_marks_degraded_health_and_retains_worker() {
        let fixture = fixture();
        fixture
            .service
            .configure(request(SyncRole::HomeHub))
            .await
            .unwrap();

        fixture
            .service
            .coordinator
            .runtime
            .terminate_active_listener()
            .await
            .unwrap();

        let status = fixture.service.status().await.unwrap();
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert!(!status.hub_reachable);
        assert!(status
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("listener")));
        assert!(!runtime.hub_listener_running);
        assert!(runtime.outbox_worker_running);
        fixture.service.shutdown().await.unwrap();
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
