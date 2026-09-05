//! Cancellation-safe coordinator for every durable Library Sync role change.

use anyhow::Context;
use audetic_core::sync::{HubConnection, SyncRole, SyncSetupRequest};
use thiserror::Error;
use tokio::sync::Mutex;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::db::sync_settings::SyncSettings;

use super::client::canonicalize_base_url;
use super::runtime::{ActivationOutcome, RuntimeError, RuntimeSet, RuntimeSpec, RuntimeTransition};
use super::serve::{AppliedServe, HomeHubNetwork, RemovedServe, ServeError, ServeManager};
use super::state::HomeHubCommit;
use super::state::{CommitEffects, EpochMismatch, InstallationSnapshot, InstallationState};
use super::tailscale::TailscaleError;
use super::transport::HubCapabilities;

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

#[derive(Debug, Error)]
pub enum TransitionError {
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
        source_error: Box<TransitionError>,
        rollback_error: String,
    },
}

impl TransitionError {
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

impl From<ServeError> for TransitionError {
    fn from(error: ServeError) -> Self {
        match error {
            ServeError::Tailscale(error) => Self::Tailscale(error),
            ServeError::BackendNotRunning(state) => {
                Self::InvalidRequest(format!("Tailscale backend is {state:?}, expected Running"))
            }
            ServeError::Verification(error) => Self::HubVerification(error),
            ServeError::BlockingTask(error) => Self::RuntimeTask(error),
            ServeError::Rollback {
                source_error,
                rollback_error,
            } => Self::Rollback {
                source_error: Box::new((*source_error).into()),
                rollback_error: rollback_error.to_string(),
            },
        }
    }
}

type SyncServiceError = TransitionError;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigureOutcome {
    pub serve_preview: Option<String>,
}

/// Single owner of durable sync settings and all role-dependent runtime tasks.
///
/// The transition mutex covers preview/verify/apply/commit/rollback as one
/// serialized operation. SQLite transactions are kept short and never cross an
/// await point.
#[derive(Clone)]
pub struct RoleCoordinator {
    state: InstallationState,
    runtime: RuntimeSet,
    serve: ServeManager,
    hub_capabilities: HubCapabilities,
    transition: Arc<Mutex<()>>,
    shut_down: Arc<AtomicBool>,
    #[cfg(test)]
    transition_pause: Arc<std::sync::Mutex<Option<TransitionPause>>>,
}

impl RoleCoordinator {
    pub fn new(
        state: InstallationState,
        runtime: RuntimeSet,
        serve: ServeManager,
        hub_capabilities: HubCapabilities,
    ) -> Self {
        Self {
            state,
            runtime,
            serve,
            hub_capabilities,
            transition: Arc::new(Mutex::new(())),
            shut_down: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            transition_pause: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Reconstruct the persisted role. Network/listener failures are recorded
    /// as degraded status and deliberately do not fail daemon startup.
    pub async fn initialize(&self) -> Result<(), SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.initialize_owned().await })
            .await
            .map_err(|error| SyncServiceError::RuntimeTask(error.to_string()))?
    }

    async fn initialize_owned(&self) -> Result<(), SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
        self.runtime
            .acquire_ownership()
            .await
            .map_err(map_runtime_error)?;
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
            .map_err(map_runtime_error)?;
        let transition = persisted.transition;
        self.transition_checkpoint(TransitionCheckpoint::RestorePrepared)
            .await;
        let startup_error = match installation.settings.role {
            SyncRole::Standalone => None,
            SyncRole::HomeHub => match persisted.listener_error {
                Some(error) => Some(SyncServiceError::Listener(error)),
                None => self.reconstruct_home_hub(&installation).await.err(),
            },
            SyncRole::ConnectedDevice => self
                .verify_connected_device(&installation.settings)
                .await
                .err(),
        };
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
            return Err(SyncServiceError::Persistence(error));
        }
        let activation = match self.runtime.commit_transition(transition).await {
            Ok(activation) => activation,
            Err(error) => return Err(map_runtime_error(error)),
        };
        if startup_error.is_none() {
            if let Some(error) = activation.listener_error() {
                self.state
                    .record_error(installation.role_epoch, Some(error))
                    .map_err(SyncServiceError::Persistence)?;
            }
        }
        self.runtime
            .observe_reachability(
                installation.role_epoch,
                installation.settings.role != SyncRole::Standalone
                    && startup_error.is_none()
                    && activation.is_healthy(),
            )
            .await;
        Ok(())
    }

    pub async fn retry(&self) -> Result<u64, SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.retry_owned().await })
            .await
            .map_err(|error| SyncServiceError::RuntimeTask(error.to_string()))?
    }

    async fn retry_owned(&self) -> Result<u64, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
        self.acquire_runtime_ownership().await?;
        let installation = self.load()?;
        let runtime_transition = if installation.settings.role == SyncRole::Standalone {
            None
        } else {
            Some(
                self.prepare_runtime(runtime_spec(&installation, installation.role_epoch)?)
                    .await?,
            )
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
            .map_err(SyncServiceError::Persistence);
        if let Some(transition) = runtime_transition {
            let activation = self.activate_runtime(transition).await?;
            if installation.settings.role == SyncRole::HomeHub {
                self.record_home_activation(installation.role_epoch, &activation)
                    .await?;
            }
        }
        result
    }

    pub async fn update_recording_payload_policy(
        &self,
        enabled: bool,
    ) -> Result<bool, SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.update_recording_payload_policy_owned(enabled).await })
            .await
            .map_err(|error| SyncServiceError::RuntimeTask(error.to_string()))?
    }

    async fn update_recording_payload_policy_owned(
        &self,
        enabled: bool,
    ) -> Result<bool, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
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
        let next_epoch = installation.role_epoch.checked_add(1).ok_or_else(|| {
            SyncServiceError::Persistence(anyhow::anyhow!("role epoch exhausted"))
        })?;
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
            .map_err(map_state_error);
        if let Err(error) = committed {
            return Err(match self.abort_runtime(runtime_transition).await {
                Ok(()) => error,
                Err(rollback) => SyncServiceError::Rollback {
                    source_error: Box::new(error),
                    rollback_error: rollback.to_string(),
                },
            });
        }
        self.transition_checkpoint(TransitionCheckpoint::Committed)
            .await;
        self.activate_runtime(runtime_transition).await?;
        Ok(enabled)
    }

    pub async fn configure(
        &self,
        request: SyncSetupRequest,
    ) -> Result<ConfigureOutcome, SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.configure_owned(request).await })
            .await
            .map_err(|error| SyncServiceError::RuntimeTask(error.to_string()))?
    }

    async fn configure_owned(
        &self,
        request: SyncSetupRequest,
    ) -> Result<ConfigureOutcome, SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.ensure_running()?;
        self.acquire_runtime_ownership().await?;
        let installation = self.load()?;
        let current = installation.settings.clone();
        if current.role != SyncRole::Standalone
            && request.role != SyncRole::Standalone
            && request.upload_recording_payloads != current.upload_recording_payloads
        {
            return Err(SyncServiceError::InvalidRequest(
                "Recording Payload upload policy for an active Shared Library role must be changed with PUT /sync/payload-policy".into(),
            ));
        }
        let serve_preview = (request.role == SyncRole::HomeHub).then(|| self.serve.preview());

        match request.role {
            SyncRole::Standalone => self.configure_standalone(request, &installation).await?,
            SyncRole::HomeHub => {
                if current.role == SyncRole::ConnectedDevice {
                    return Err(SyncServiceError::InvalidTransition(
                        "demote the Connected Device to Standalone before promotion".into(),
                    ));
                }
                if current.role == SyncRole::Standalone && !request.confirm_serve_change {
                    self.serve
                        .prepare_home_hub()
                        .await
                        .map_err(TransitionError::from)?;
                    return Ok(ConfigureOutcome { serve_preview });
                }
                self.configure_home_hub(request, &installation).await?;
            }
            SyncRole::ConnectedDevice => {
                if current.role == SyncRole::HomeHub {
                    return Err(SyncServiceError::InvalidTransition(
                        "demote the Home Hub to Standalone before connecting to another hub".into(),
                    ));
                }
                self.configure_connected_device(request, &installation)
                    .await?;
            }
        }

        Ok(ConfigureOutcome { serve_preview })
    }

    pub async fn shutdown(&self) -> Result<(), SyncServiceError> {
        let service = self.clone();
        tokio::spawn(async move { service.shutdown_owned().await })
            .await
            .map_err(|error| SyncServiceError::RuntimeTask(error.to_string()))?
    }

    async fn shutdown_owned(&self) -> Result<(), SyncServiceError> {
        let _transition = self.transition.lock().await;
        self.shut_down.store(true, Ordering::SeqCst);
        self.runtime.shutdown().await.map_err(map_runtime_error)
    }

    async fn configure_standalone(
        &self,
        request: SyncSetupRequest,
        installation: &InstallationSnapshot,
    ) -> Result<(), SyncServiceError> {
        if request.hub.is_some() {
            return Err(SyncServiceError::InvalidRequest(
                "Standalone settings cannot contain a Home Hub connection".into(),
            ));
        }

        let current = &installation.settings;
        let runtime_transition = self
            .prepare_runtime(RuntimeSpec::Standalone {
                role_epoch: installation.role_epoch.checked_add(1).ok_or_else(|| {
                    SyncServiceError::Persistence(anyhow::anyhow!("role epoch exhausted"))
                })?,
            })
            .await?;
        let removed_mapping = if current.role == SyncRole::HomeHub {
            match self
                .serve
                .remove_persisted(installation.serve_ownership.as_ref())
                .await
                .map_err(TransitionError::from)
            {
                Ok(removed) => removed,
                Err(error) => {
                    return Err(match self.abort_runtime(runtime_transition).await {
                        Ok(()) => error,
                        Err(rollback) => SyncServiceError::Rollback {
                            source_error: Box::new(error),
                            rollback_error: rollback.to_string(),
                        },
                    });
                }
            }
        } else {
            RemovedServe::default()
        };

        self.transition_checkpoint(TransitionCheckpoint::Verified)
            .await;
        self.seal_runtime_transition(runtime_transition).await?;
        self.transition_checkpoint(TransitionCheckpoint::Quiesced)
            .await;
        let settings = settings_from_request(request, None);
        if let Err(error) = self
            .state
            .commit_standalone(installation.role_epoch, &settings)
            .map_err(map_state_error)
        {
            let mut rollback_errors = Vec::new();
            if let Err(rollback) = self.abort_runtime(runtime_transition).await {
                rollback_errors.push(rollback.to_string());
            }
            if let Err(rollback) = self.serve.compensate_removal(removed_mapping).await {
                rollback_errors.push(rollback.to_string());
            }
            return Err(if rollback_errors.is_empty() {
                error
            } else {
                SyncServiceError::Rollback {
                    source_error: Box::new(error),
                    rollback_error: rollback_errors.join("; "),
                }
            });
        }
        self.transition_checkpoint(TransitionCheckpoint::Committed)
            .await;
        self.activate_runtime(runtime_transition).await?;
        Ok(())
    }

    async fn configure_home_hub(
        &self,
        request: SyncSetupRequest,
        installation: &InstallationSnapshot,
    ) -> Result<(), SyncServiceError> {
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
        let next_epoch = installation.role_epoch.checked_add(1).ok_or_else(|| {
            SyncServiceError::Persistence(anyhow::anyhow!("role epoch exhausted"))
        })?;
        let target = InstallationSnapshot {
            identity: installation.identity.clone(),
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
        self.transition_checkpoint(TransitionCheckpoint::Prepared)
            .await;
        let applied_serve = match self
            .verify_home_hub_network(&network, hub_id, &owner_login)
            .await
        {
            Ok(applied) => applied,
            Err((error, applied)) => {
                return Err(self
                    .rollback_home_transition(runtime_transition, applied, error)
                    .await);
            }
        };
        self.transition_checkpoint(TransitionCheckpoint::Verified)
            .await;
        if let Err(error) = self.quiesce_worker(runtime_transition).await {
            return Err(self
                .rollback_home_transition(runtime_transition, applied_serve, error)
                .await);
        }
        self.transition_checkpoint(TransitionCheckpoint::Quiesced)
            .await;
        let ownership = super::serve::expected_ownership();
        if let Err(error) = self.validate_runtime(runtime_transition).await {
            return Err(self
                .rollback_home_transition(runtime_transition, applied_serve, error)
                .await);
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
                    .rollback_home_transition(
                        runtime_transition,
                        applied_serve,
                        map_state_error(error),
                    )
                    .await);
            }
        };
        self.cleanup_after_commit(&effects, "Home Hub activation");
        self.transition_checkpoint(TransitionCheckpoint::Committed)
            .await;
        let activation = self.activate_runtime(runtime_transition).await?;
        self.record_home_activation(effects.role_epoch, &activation)
            .await?;
        Ok(())
    }

    async fn verify_home_hub_network(
        &self,
        network: &HomeHubNetwork,
        hub_id: audetic_core::sync::HubId,
        owner_login: &str,
    ) -> Result<AppliedServe, (SyncServiceError, AppliedServe)> {
        let applied = self
            .serve
            .apply_verified()
            .await
            .map_err(|error| (error.into(), AppliedServe::default()))?;
        let connection = network.connection(hub_id, owner_login);
        self.hub_capabilities
            .probe()
            .handshake(&connection)
            .await
            .map_err(|error| {
                (
                    SyncServiceError::HubVerification(error.to_string()),
                    applied,
                )
            })?;
        Ok(applied)
    }

    async fn configure_connected_device(
        &self,
        request: SyncSetupRequest,
        installation: &InstallationSnapshot,
    ) -> Result<(), SyncServiceError> {
        let mut requested_hub = request.hub.clone().ok_or_else(|| {
            SyncServiceError::InvalidRequest(
                "Connected Device settings require a Home Hub connection".into(),
            )
        })?;
        requested_hub.base_url = canonicalize_base_url(&requested_hub.base_url)
            .map_err(|error| SyncServiceError::InvalidRequest(error.to_string()))?
            .to_string();
        let network = self
            .serve
            .discovery()
            .await
            .map_err(TransitionError::from)?;
        if network.owner_login != requested_hub.owner_login {
            return Err(SyncServiceError::InvalidRequest(format!(
                "the local Tailscale owner {:?} does not match the Home Hub owner {:?}",
                network.owner_login, requested_hub.owner_login
            )));
        }
        let candidate = self
            .hub_capabilities
            .probe()
            .handshake(&requested_hub)
            .await
            .map_err(|error| SyncServiceError::HubVerification(error.to_string()))?;
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
        let current = &installation.settings;
        let destination_changed = current.role == SyncRole::Standalone
            || (current.role == SyncRole::ConnectedDevice
                && current.hub.as_ref().map(|hub| hub.hub_id)
                    != settings.hub.as_ref().map(|hub| hub.hub_id));
        let next_epoch = installation.role_epoch.checked_add(1).ok_or_else(|| {
            SyncServiceError::Persistence(anyhow::anyhow!("role epoch exhausted"))
        })?;
        let target = InstallationSnapshot {
            settings: settings.clone(),
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
        let effects = match self.state.commit_connected_device(
            installation.role_epoch,
            &settings,
            installation.identity.device_id,
            destination_changed,
        ) {
            Ok(effects) => effects,
            Err(error) => {
                let error = map_state_error(error);
                return Err(match self.abort_runtime(runtime_transition).await {
                    Ok(()) => error,
                    Err(rollback) => SyncServiceError::Rollback {
                        source_error: Box::new(error),
                        rollback_error: rollback.to_string(),
                    },
                });
            }
        };
        self.cleanup_after_commit(&effects, "Connected Device activation");
        self.transition_checkpoint(TransitionCheckpoint::Committed)
            .await;
        self.activate_runtime(runtime_transition).await?;
        self.runtime
            .observe_reachability(effects.role_epoch, true)
            .await;
        Ok(())
    }

    async fn reconstruct_home_hub(
        &self,
        installation: &InstallationSnapshot,
    ) -> Result<(), SyncServiceError> {
        let identity = &installation.identity;
        let hub_id = identity.hub_id.ok_or_else(|| {
            SyncServiceError::Persistence(anyhow::anyhow!("persisted Home Hub role has no Hub ID"))
        })?;
        let owner_login = identity.owner_login.as_deref().ok_or_else(|| {
            SyncServiceError::Persistence(anyhow::anyhow!(
                "persisted Home Hub role has no owner login"
            ))
        })?;
        let network = self
            .serve
            .verify_persisted(installation.serve_ownership.as_ref())
            .await
            .map_err(TransitionError::from)?;
        let connection = network.connection(hub_id, owner_login);
        self.hub_capabilities
            .probe()
            .handshake(&connection)
            .await
            .map_err(|error| SyncServiceError::HubVerification(error.to_string()))?;
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
        self.hub_capabilities
            .probe()
            .handshake(hub)
            .await
            .map_err(|error| SyncServiceError::HubVerification(error.to_string()))?;
        Ok(())
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
            .map_err(map_runtime_error)
    }

    async fn activate_runtime(
        &self,
        transition: RuntimeTransition,
    ) -> Result<ActivationOutcome, SyncServiceError> {
        self.runtime
            .commit_transition(transition)
            .await
            .map_err(map_runtime_error)
    }

    async fn quiesce_worker(&self, transition: RuntimeTransition) -> Result<(), SyncServiceError> {
        self.runtime
            .quiesce_current_worker(transition)
            .await
            .map_err(map_runtime_error)
    }

    async fn validate_runtime(
        &self,
        transition: RuntimeTransition,
    ) -> Result<(), SyncServiceError> {
        self.runtime
            .validate_transition(transition)
            .await
            .map_err(map_runtime_error)
    }

    async fn seal_runtime_transition(
        &self,
        transition: RuntimeTransition,
    ) -> Result<(), SyncServiceError> {
        if let Err(error) = self.quiesce_worker(transition).await {
            return Err(match self.abort_runtime(transition).await {
                Ok(()) => error,
                Err(rollback) => SyncServiceError::Rollback {
                    source_error: Box::new(error),
                    rollback_error: rollback.to_string(),
                },
            });
        }
        Ok(())
    }

    async fn abort_runtime(&self, transition: RuntimeTransition) -> Result<(), SyncServiceError> {
        self.runtime
            .abort_transition(transition)
            .await
            .map_err(map_runtime_error)
    }

    async fn acquire_runtime_ownership(&self) -> Result<(), SyncServiceError> {
        self.runtime
            .acquire_ownership()
            .await
            .map_err(map_runtime_error)
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

    async fn rollback_home_transition(
        &self,
        transition: RuntimeTransition,
        applied_serve: AppliedServe,
        source_error: SyncServiceError,
    ) -> SyncServiceError {
        let mut rollback_errors = Vec::new();
        if let Err(error) = self.abort_runtime(transition).await {
            rollback_errors.push(error.to_string());
        }
        if let Err(rollback) = self.serve.compensate_application(applied_serve).await {
            rollback_errors.push(rollback.to_string());
        }
        if rollback_errors.is_empty() {
            source_error
        } else {
            SyncServiceError::Rollback {
                source_error: Box::new(source_error),
                rollback_error: rollback_errors.join("; "),
            }
        }
    }

    pub async fn observe_contact(
        &self,
        role_epoch: u64,
        reachable: bool,
        error: Option<&str>,
    ) -> Result<(), SyncServiceError> {
        self.state
            .observe_contact(role_epoch, reachable, error)
            .map_err(SyncServiceError::Persistence)?;
        self.runtime
            .observe_reachability(role_epoch, reachable)
            .await;
        Ok(())
    }

    pub async fn runtime_snapshot(&self) -> super::runtime::RuntimeSnapshot {
        self.runtime.snapshot().await
    }

    async fn record_home_activation(
        &self,
        role_epoch: u64,
        activation: &ActivationOutcome,
    ) -> Result<(), SyncServiceError> {
        match activation {
            ActivationOutcome::Healthy => {
                self.state
                    .record_contact(role_epoch)
                    .map_err(SyncServiceError::Persistence)?;
                self.runtime.observe_reachability(role_epoch, true).await;
            }
            ActivationOutcome::Degraded { listener_error } => {
                self.state
                    .record_error(role_epoch, Some(listener_error))
                    .map_err(SyncServiceError::Persistence)?;
                self.runtime.observe_reachability(role_epoch, false).await;
            }
        }
        Ok(())
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
        self.state.load().map_err(SyncServiceError::Persistence)
    }

    pub fn ensure_running(&self) -> Result<(), SyncServiceError> {
        if self.shut_down.load(Ordering::SeqCst) {
            Err(SyncServiceError::Shutdown)
        } else {
            Ok(())
        }
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
                SyncServiceError::Persistence(anyhow::anyhow!(
                    "persisted Home Hub role has no Hub ID"
                ))
            })?;
            let owner_login = installation.identity.owner_login.clone().ok_or_else(|| {
                SyncServiceError::Persistence(anyhow::anyhow!(
                    "persisted Home Hub role has no owner login"
                ))
            })?;
            runtime_spec_with_home_identity(installation, hub_id, &owner_login)
        }
        SyncRole::ConnectedDevice => Ok(RuntimeSpec::ConnectedDevice {
            role_epoch,
            hub: installation.settings.hub.clone().ok_or_else(|| {
                SyncServiceError::Persistence(anyhow::anyhow!(
                    "Connected Device runtime has no Home Hub"
                ))
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

fn map_runtime_error(error: RuntimeError) -> SyncServiceError {
    match error {
        RuntimeError::Listener(error) => SyncServiceError::Listener(error),
        RuntimeError::Invariant(error) => SyncServiceError::RuntimeTask(error),
        RuntimeError::Ownership(error) => SyncServiceError::RuntimeTask(error),
        RuntimeError::NoOwnership => {
            SyncServiceError::RuntimeTask("sync runtime has no process ownership lease".into())
        }
        RuntimeError::Shutdown => SyncServiceError::Shutdown,
    }
}

fn map_state_error(error: anyhow::Error) -> SyncServiceError {
    if let Some(error) = error.downcast_ref::<EpochMismatch>() {
        SyncServiceError::InvalidTransition(error.to_string())
    } else {
        SyncServiceError::Persistence(error)
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

        assert!(matches!(error, SyncServiceError::InvalidTransition(_)));
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

        assert!(matches!(error, SyncServiceError::Persistence(_)));
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
                role_epoch: Some(0),
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

        assert!(matches!(error, SyncServiceError::RuntimeTask(_)));
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

        assert!(matches!(error, SyncServiceError::Listener(_)));
        let installation = fixture.service.state.load().unwrap();
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

        assert!(matches!(error, SyncServiceError::Listener(_)));
        let installation = fixture.service.state.load().unwrap();
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
        let installation = fixture.service.state.load().unwrap();
        assert_eq!(installation.settings.role, SyncRole::HomeHub);
        assert_eq!(
            *fixture.tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert_eq!(runtime.role, Some(SyncRole::HomeHub));
        assert_eq!(runtime.role_epoch, Some(installation.role_epoch));
        assert!(!runtime.hub_listener_running);
        assert!(runtime.outbox_worker_running);
        assert!(!runtime.transition_in_progress);

        fixture.service.retry().await.unwrap();

        let recovered = fixture.service.status().await.unwrap();
        let runtime = fixture.service.coordinator.runtime.snapshot().await;
        assert!(recovered.hub_reachable);
        assert!(recovered.last_error.is_none());
        assert_eq!(runtime.role, Some(SyncRole::HomeHub));
        assert_eq!(runtime.role_epoch, Some(installation.role_epoch));
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
