//! Sole lifecycle and process-ownership boundary for Library Sync runtimes.

use audetic_core::sync::{HubConnection, HubId, SyncRole};
use fs2::FileExt;
use thiserror::Error;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use std::fs::{File, OpenOptions};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use super::library::HubLibrary;
use super::outbox::{OutboxDestination, OutboxWorker};
use super::server::{HubServer, HubServerConfig};
use super::state::InstallationState;
use super::transport::HubCapabilities;

/// Opaque identity for one committed role generation. Runtime observations can
/// only be applied with a token issued for that generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RoleVersion(u64);

impl RoleVersion {
    pub(super) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(super) const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RuntimeSpec {
    Standalone {
        role_epoch: u64,
    },
    HomeHub {
        role_epoch: u64,
        hub_id: HubId,
        owner_login: String,
        device_name: Option<String>,
        upload_recording_payloads: bool,
    },
    ConnectedDevice {
        role_epoch: u64,
        hub: HubConnection,
        upload_recording_payloads: bool,
    },
}

impl RuntimeSpec {
    pub(super) const fn role(&self) -> SyncRole {
        match self {
            Self::Standalone { .. } => SyncRole::Standalone,
            Self::HomeHub { .. } => SyncRole::HomeHub,
            Self::ConnectedDevice { .. } => SyncRole::ConnectedDevice,
        }
    }

    pub(super) const fn role_version(&self) -> RoleVersion {
        match self {
            Self::Standalone { role_epoch }
            | Self::HomeHub { role_epoch, .. }
            | Self::ConnectedDevice { role_epoch, .. } => RoleVersion::new(*role_epoch),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RuntimeSnapshot {
    pub role: Option<SyncRole>,
    pub(super) role_version: Option<RoleVersion>,
    pub hub_listener_running: bool,
    pub outbox_worker_running: bool,
    pub hub_reachable: bool,
    pub listener_error: Option<String>,
    pub ownership_held: bool,
    pub transition_in_progress: bool,
    pub shut_down: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RuntimeTransition {
    id: u64,
}

#[derive(Clone, Debug)]
pub(super) struct PersistedTransition {
    pub(super) transition: RuntimeTransition,
    pub(super) listener_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ActivationOutcome {
    Healthy {
        role_version: RoleVersion,
        cleanup_diagnostics: Vec<RuntimeCleanupDiagnostic>,
    },
    Degraded {
        role_version: RoleVersion,
        listener_error: String,
        cleanup_diagnostics: Vec<RuntimeCleanupDiagnostic>,
    },
}

impl ActivationOutcome {
    pub(super) const fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy { .. })
    }

    pub(super) fn listener_error(&self) -> Option<&str> {
        match self {
            Self::Healthy { .. } => None,
            Self::Degraded { listener_error, .. } => Some(listener_error),
        }
    }

    pub(super) fn cleanup_diagnostics(&self) -> &[RuntimeCleanupDiagnostic] {
        match self {
            Self::Healthy {
                cleanup_diagnostics,
                ..
            }
            | Self::Degraded {
                cleanup_diagnostics,
                ..
            } => cleanup_diagnostics,
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(super) enum RuntimeCleanupDiagnostic {
    #[error("obsolete sync outbox worker cleanup failed: {0}")]
    OutboxWorker(String),
    #[error("obsolete Home Hub listener cleanup failed: {0}")]
    HomeHubListener(String),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RuntimeError {
    #[error("Home Hub listener failed: {0}")]
    Listener(String),
    #[error("invalid sync runtime state: {0}")]
    Invariant(String),
    #[error("another Library Sync runtime owns {0}")]
    Ownership(String),
    #[error("sync runtime has no process ownership lease")]
    NoOwnership,
    #[error("sync runtime has shut down")]
    Shutdown,
}

struct RuntimeLease {
    file: File,
    path: PathBuf,
}

impl RuntimeLease {
    fn acquire(db_path: &Path) -> Result<Self, RuntimeError> {
        let root = db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("sync");
        std::fs::create_dir_all(&root).map_err(|error| {
            RuntimeError::Ownership(format!(
                "{} (creating lease directory failed: {error})",
                root.display()
            ))
        })?;
        let path = root.join(".runtime.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                RuntimeError::Ownership(format!(
                    "{} (opening lease failed: {error})",
                    path.display()
                ))
            })?;
        file.try_lock_exclusive()
            .map_err(|error| RuntimeError::Ownership(format!("{} ({error})", path.display())))?;
        Ok(Self { file, path })
    }
}

impl Drop for RuntimeLease {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            tracing::warn!(path = %self.path.display(), %error, "failed to release sync runtime lease");
        }
    }
}

struct HubRuntime {
    shutdown: Option<oneshot::Sender<()>>,
    cancellation: CancellationToken,
    task: Option<JoinHandle<Result<(), super::server::HubServerError>>>,
    failure: Arc<StdMutex<Option<String>>>,
    #[cfg(test)]
    fail_cleanup: bool,
}

impl HubRuntime {
    fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    fn liveness_error(&self) -> Option<String> {
        if self.is_running() {
            return None;
        }
        self.failure
            .lock()
            .ok()
            .and_then(|failure| failure.clone())
            .or_else(|| Some("Home Hub listener terminated unexpectedly".into()))
    }

    fn request_stop(&mut self) {
        self.cancellation.cancel();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    async fn stop(mut self) -> Result<(), RuntimeError> {
        self.request_stop();
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        let result = task
            .await
            .map_err(|error| RuntimeError::Listener(error.to_string()))?
            .map_err(|error| RuntimeError::Listener(error.to_string()));
        #[cfg(test)]
        if result.is_ok() && self.fail_cleanup {
            return Err(RuntimeError::Listener(
                "injected obsolete listener cleanup failure".into(),
            ));
        }
        result
    }

    fn abort(&mut self) {
        self.cancellation.cancel();
        self.shutdown.take();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for HubRuntime {
    fn drop(&mut self) {
        self.abort();
    }
}

struct OutboxRuntime {
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl OutboxRuntime {
    fn request_stop(&self) {
        self.cancellation.cancel();
    }

    async fn stop(mut self) {
        self.request_stop();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    async fn stop_with_diagnostic(mut self) -> Option<RuntimeCleanupDiagnostic> {
        self.request_stop();
        let task = self.task.take()?;
        task.await.err().map(|error| {
            RuntimeCleanupDiagnostic::OutboxWorker(format!("worker task join failed: {error}"))
        })
    }

    fn abort(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }
}

impl Drop for OutboxRuntime {
    fn drop(&mut self) {
        self.abort();
    }
}

struct PreparedOutbox {
    start: Option<oneshot::Sender<()>>,
    runtime: Option<OutboxRuntime>,
}

impl PreparedOutbox {
    fn activate(mut self) -> Result<OutboxRuntime, RuntimeError> {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| RuntimeError::Invariant("prepared outbox has no runtime".into()))?;
        if let Some(start) = self.start.take() {
            let _ = start.send(());
        }
        Ok(runtime)
    }

    async fn stop(mut self) {
        self.request_stop();
        if let Some(runtime) = self.runtime.take() {
            runtime.stop().await;
        }
    }

    fn request_stop(&mut self) {
        self.start.take();
        if let Some(runtime) = self.runtime.as_ref() {
            runtime.request_stop();
        }
    }
}

impl Drop for PreparedOutbox {
    fn drop(&mut self) {
        self.start.take();
        if let Some(runtime) = self.runtime.as_mut() {
            runtime.abort();
        }
    }
}

struct ActiveRuntime {
    spec: RuntimeSpec,
    hub: Option<HubRuntime>,
    outbox: Option<OutboxRuntime>,
    hub_reachable: bool,
    listener_error: Option<String>,
}

impl ActiveRuntime {
    fn request_stop(&mut self) {
        if let Some(outbox) = self.outbox.as_ref() {
            outbox.request_stop();
        }
        if let Some(hub) = self.hub.as_mut() {
            hub.request_stop();
        }
    }

    async fn stop(mut self) -> Result<(), RuntimeError> {
        self.request_stop();
        if let Some(outbox) = self.outbox.take() {
            outbox.stop().await;
        }
        if let Some(hub) = self.hub.take() {
            hub.stop().await?;
        }
        Ok(())
    }

    async fn cleanup(mut self) -> Vec<RuntimeCleanupDiagnostic> {
        // Request every cancellation before joining either resource. Cleanup
        // diagnostics must never leave a later handle unconsumed.
        self.request_stop();
        let mut diagnostics = Vec::new();
        if let Some(outbox) = self.outbox.take() {
            if let Some(diagnostic) = outbox.stop_with_diagnostic().await {
                diagnostics.push(diagnostic);
            }
        }
        if let Some(hub) = self.hub.take() {
            if let Err(error) = hub.stop().await {
                diagnostics.push(RuntimeCleanupDiagnostic::HomeHubListener(error.to_string()));
            }
        }
        diagnostics
    }
}

struct ProvisionalRuntime {
    id: u64,
    spec: RuntimeSpec,
    hub: Option<HubRuntime>,
    outbox: Option<PreparedOutbox>,
    reuse_active_hub: bool,
    allow_degraded_listener: bool,
    listener_error: Option<String>,
    worker_quiesced: bool,
}

impl ProvisionalRuntime {
    fn request_stop(&mut self) {
        if let Some(outbox) = self.outbox.as_mut() {
            outbox.request_stop();
        }
        if let Some(hub) = self.hub.as_mut() {
            hub.request_stop();
        }
    }

    async fn stop(mut self) {
        if let Some(outbox) = self.outbox.take() {
            outbox.stop().await;
        }
        if let Some(hub) = self.hub.take() {
            let _ = hub.stop().await;
        }
    }
}

struct RuntimeInner {
    lease: Option<RuntimeLease>,
    active: Option<ActiveRuntime>,
    provisional: Option<ProvisionalRuntime>,
    next_transition_id: u64,
    shut_down: bool,
}

struct RuntimeShared {
    state: InstallationState,
    hub_capabilities: HubCapabilities,
    hub_bind_address: SocketAddr,
    inner: Mutex<RuntimeInner>,
    #[cfg(test)]
    shutdown_pause: StdMutex<Option<ShutdownPause>>,
    #[cfg(test)]
    fail_next_quiesce: AtomicBool,
    #[cfg(test)]
    quiesce_pause: StdMutex<Option<ShutdownPause>>,
}

#[cfg(test)]
#[derive(Clone)]
struct ShutdownPause {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

/// Owns the process lease plus every active and provisional sync resource.
#[derive(Clone)]
pub(super) struct RuntimeSet {
    shared: Arc<RuntimeShared>,
}

impl RuntimeSet {
    pub(super) fn new(
        state: InstallationState,
        hub_capabilities: HubCapabilities,
        hub_bind_address: SocketAddr,
    ) -> Self {
        Self {
            shared: Arc::new(RuntimeShared {
                state,
                hub_capabilities,
                hub_bind_address,
                inner: Mutex::new(RuntimeInner {
                    lease: None,
                    active: None,
                    provisional: None,
                    next_transition_id: 1,
                    shut_down: false,
                }),
                #[cfg(test)]
                shutdown_pause: StdMutex::new(None),
                #[cfg(test)]
                fail_next_quiesce: AtomicBool::new(false),
                #[cfg(test)]
                quiesce_pause: StdMutex::new(None),
            }),
        }
    }

    pub(super) async fn acquire_ownership(&self) -> Result<(), RuntimeError> {
        {
            let inner = self.shared.inner.lock().await;
            if inner.shut_down {
                return Err(RuntimeError::Shutdown);
            }
            if inner.lease.is_some() {
                return Ok(());
            }
        }
        let db_path = self.shared.state.db_path().to_path_buf();
        let lease = tokio::task::spawn_blocking(move || RuntimeLease::acquire(&db_path))
            .await
            .map_err(|error| RuntimeError::Ownership(error.to_string()))??;
        let mut inner = self.shared.inner.lock().await;
        if inner.shut_down {
            return Err(RuntimeError::Shutdown);
        }
        if inner.lease.is_none() {
            inner.lease = Some(lease);
        }
        Ok(())
    }

    pub(super) async fn release_ownership_if_idle(&self) {
        let mut inner = self.shared.inner.lock().await;
        if inner.active.is_none() && inner.provisional.is_none() {
            inner.lease.take();
        }
    }

    pub(super) async fn begin_transition(
        &self,
        spec: RuntimeSpec,
    ) -> Result<RuntimeTransition, RuntimeError> {
        self.begin(spec, false).await.map(|value| value.transition)
    }

    pub(super) async fn begin_persisted_restore(
        &self,
        spec: RuntimeSpec,
    ) -> Result<PersistedTransition, RuntimeError> {
        self.begin(spec, true).await
    }

    async fn begin(
        &self,
        spec: RuntimeSpec,
        allow_degraded_listener: bool,
    ) -> Result<PersistedTransition, RuntimeError> {
        // Phase one only inspects the generation we intend to replace. Binding
        // and worker construction happen without the lifecycle mutex.
        let (id, reuse_active_hub, observed_version) = {
            let mut inner = self.shared.inner.lock().await;
            Self::ensure_available(&inner)?;
            if inner.provisional.is_some() {
                return Err(RuntimeError::Invariant(
                    "a sync runtime transition is already in progress".into(),
                ));
            }
            let reuse_active_hub = !allow_degraded_listener
                && matches!(spec, RuntimeSpec::HomeHub { .. })
                && inner.active.as_ref().is_some_and(|active| {
                    active.spec.role() == SyncRole::HomeHub
                        && active.hub.as_ref().is_some_and(HubRuntime::is_running)
                });
            let id = inner.next_transition_id;
            inner.next_transition_id = inner.next_transition_id.wrapping_add(1).max(1);
            (
                id,
                reuse_active_hub,
                inner
                    .active
                    .as_ref()
                    .map(|active| active.spec.role_version()),
            )
        };
        let (hub, listener_error) =
            if matches!(spec, RuntimeSpec::HomeHub { .. }) && !reuse_active_hub {
                match self.prepare_hub(&spec).await {
                    Ok(hub) => (Some(hub), None),
                    Err(error) if allow_degraded_listener => (None, Some(error.to_string())),
                    Err(error) => return Err(error),
                }
            } else {
                (None, None)
            };
        let outbox = if spec.role() == SyncRole::Standalone {
            None
        } else {
            Some(self.prepare_outbox(&spec)?)
        };
        // Phase two validates that shutdown or another owner did not change the
        // runtime while slow resources were being prepared.
        let mut inner = self.shared.inner.lock().await;
        if let Err(error) = Self::ensure_available(&inner) {
            drop(inner);
            ProvisionalRuntime {
                id,
                spec,
                hub,
                outbox,
                reuse_active_hub,
                allow_degraded_listener,
                listener_error,
                worker_quiesced: false,
            }
            .stop()
            .await;
            return Err(error);
        }
        if inner.provisional.is_some()
            || inner
                .active
                .as_ref()
                .map(|active| active.spec.role_version())
                != observed_version
        {
            drop(inner);
            ProvisionalRuntime {
                id,
                spec,
                hub,
                outbox,
                reuse_active_hub,
                allow_degraded_listener,
                listener_error,
                worker_quiesced: false,
            }
            .stop()
            .await;
            return Err(RuntimeError::Invariant(
                "runtime changed while preparing its transition".into(),
            ));
        }
        inner.provisional = Some(ProvisionalRuntime {
            id,
            spec,
            hub,
            outbox,
            reuse_active_hub,
            allow_degraded_listener,
            listener_error: listener_error.clone(),
            worker_quiesced: false,
        });
        Ok(PersistedTransition {
            transition: RuntimeTransition { id },
            listener_error,
        })
    }

    pub(super) async fn quiesce_current_worker(
        &self,
        transition: RuntimeTransition,
    ) -> Result<(), RuntimeError> {
        let outbox = {
            let mut inner = self.shared.inner.lock().await;
            Self::ensure_available(&inner)?;
            Self::provisional(&inner, transition)?;
            let outbox = inner
                .active
                .as_mut()
                .and_then(|active| active.outbox.take());
            if outbox.is_some() {
                Self::provisional_mut(&mut inner, transition)?.worker_quiesced = true;
            }
            outbox
        };
        if let Some(outbox) = outbox {
            #[cfg(test)]
            self.pause_quiesce_before_join().await;
            outbox.stop().await;
        }
        #[cfg(test)]
        if self.shared.fail_next_quiesce.swap(false, Ordering::SeqCst) {
            return Err(RuntimeError::Invariant(
                "injected worker quiescence failure".into(),
            ));
        }
        let inner = self.shared.inner.lock().await;
        Self::ensure_available(&inner)?;
        let provisional = Self::provisional(&inner, transition)?;
        if provisional.spec.role() == SyncRole::HomeHub {
            let hub = if provisional.reuse_active_hub {
                inner.active.as_ref().and_then(|active| active.hub.as_ref())
            } else {
                provisional.hub.as_ref()
            }
            .ok_or_else(|| RuntimeError::Listener("prepared listener is missing".into()))?;
            if let Some(error) = hub.liveness_error() {
                return Err(RuntimeError::Listener(error));
            }
        }
        Ok(())
    }

    pub(super) async fn validate_transition(
        &self,
        transition: RuntimeTransition,
    ) -> Result<(), RuntimeError> {
        let inner = self.shared.inner.lock().await;
        Self::ensure_available(&inner)?;
        let provisional = Self::provisional(&inner, transition)?;
        Self::validate_provisional_listener(&inner, provisional)
    }

    pub(super) async fn abort_transition(
        &self,
        transition: RuntimeTransition,
    ) -> Result<(), RuntimeError> {
        let (provisional, restore_spec) = {
            let mut inner = self.shared.inner.lock().await;
            let provisional = Self::take_provisional(&mut inner, transition)?;
            let restore_spec = provisional.worker_quiesced.then(|| {
                inner
                    .active
                    .as_ref()
                    .map(|active| active.spec.clone())
                    .ok_or_else(|| RuntimeError::Invariant("quiesced runtime disappeared".into()))
            });
            (provisional, restore_spec)
        };
        provisional.stop().await;
        if let Some(restore_spec) = restore_spec.transpose()? {
            let outbox = self.prepare_outbox(&restore_spec)?.activate()?;
            let mut inner = self.shared.inner.lock().await;
            let active = inner.active.as_mut().ok_or_else(|| {
                RuntimeError::Invariant("runtime disappeared during abort".into())
            })?;
            if active.spec != restore_spec || active.outbox.is_some() {
                drop(inner);
                outbox.stop().await;
                return Err(RuntimeError::Invariant(
                    "runtime changed while aborting its transition".into(),
                ));
            }
            active.outbox = Some(outbox);
        }
        Ok(())
    }

    pub(super) async fn commit_transition(
        &self,
        transition: RuntimeTransition,
    ) -> Result<ActivationOutcome, RuntimeError> {
        let (mut provisional, mut previous) = {
            let mut inner = self.shared.inner.lock().await;
            Self::ensure_available(&inner)?;
            let provisional = Self::take_provisional(&mut inner, transition)?;
            let previous = inner.active.take();
            (provisional, previous)
        };
        let mut hub = if provisional.spec.role() == SyncRole::HomeHub {
            if provisional.reuse_active_hub {
                previous.as_mut().and_then(|runtime| runtime.hub.take())
            } else {
                provisional.hub.take()
            }
        } else {
            None
        };
        let candidate_error =
            if provisional.spec.role() == SyncRole::HomeHub {
                match hub.as_ref() {
                    Some(hub) => hub.liveness_error(),
                    None => Some(provisional.listener_error.clone().unwrap_or_else(|| {
                        "Home Hub listener is unavailable after activation".into()
                    })),
                }
            } else {
                None
            };
        if candidate_error.is_some() {
            if let Some(hub) = hub.take() {
                let _ = hub.stop().await;
            }
        }
        let listener_error = if provisional.spec.role() == SyncRole::HomeHub {
            provisional.listener_error.clone().or(candidate_error)
        } else {
            None
        };
        let outbox = match provisional.outbox.take().map(PreparedOutbox::activate) {
            Some(result) => Some(result?),
            None => None,
        };
        let role_version = provisional.spec.role_version();
        let active = ActiveRuntime {
            spec: provisional.spec,
            hub,
            outbox,
            hub_reachable: false,
            listener_error: listener_error.clone(),
        };
        {
            let mut inner = self.shared.inner.lock().await;
            Self::ensure_available(&inner)?;
            if inner.active.is_some() || inner.provisional.is_some() {
                drop(inner);
                active.stop().await?;
                return Err(RuntimeError::Invariant(
                    "runtime changed while activating its transition".into(),
                ));
            }
            inner.active = Some(active);
        }
        let cleanup_diagnostics = match previous {
            Some(previous) => previous.cleanup().await,
            None => Vec::new(),
        };
        Ok(match listener_error {
            Some(listener_error) => ActivationOutcome::Degraded {
                role_version,
                listener_error,
                cleanup_diagnostics,
            },
            None => ActivationOutcome::Healthy {
                role_version,
                cleanup_diagnostics,
            },
        })
    }

    pub(super) async fn snapshot(&self) -> RuntimeSnapshot {
        let inner = self.shared.inner.lock().await;
        let Some(active) = inner.active.as_ref() else {
            return RuntimeSnapshot {
                ownership_held: inner.lease.is_some(),
                transition_in_progress: inner.provisional.is_some(),
                shut_down: inner.shut_down,
                ..RuntimeSnapshot::default()
            };
        };
        let listener_error = active
            .listener_error
            .clone()
            .or_else(|| active.hub.as_ref().and_then(HubRuntime::liveness_error));
        RuntimeSnapshot {
            role: Some(active.spec.role()),
            role_version: Some(active.spec.role_version()),
            hub_listener_running: active.hub.as_ref().is_some_and(HubRuntime::is_running),
            outbox_worker_running: active
                .outbox
                .as_ref()
                .is_some_and(OutboxRuntime::is_running),
            hub_reachable: active.hub_reachable,
            listener_error,
            ownership_held: inner.lease.is_some(),
            transition_in_progress: inner.provisional.is_some(),
            shut_down: inner.shut_down,
        }
    }

    pub(super) async fn observe_reachability(
        &self,
        role_version: RoleVersion,
        reachable: bool,
    ) -> bool {
        let mut inner = self.shared.inner.lock().await;
        let Some(active) = inner.active.as_mut() else {
            return false;
        };
        if active.spec.role_version() != role_version {
            return false;
        }
        active.hub_reachable = reachable;
        true
    }

    pub(super) async fn shutdown(&self) -> Result<(), RuntimeError> {
        let (mut provisional, mut active) = {
            let mut inner = self.shared.inner.lock().await;
            if inner.shut_down {
                return Ok(());
            }
            inner.shut_down = true;
            (inner.provisional.take(), inner.active.take())
        };
        if let Some(provisional) = provisional.as_mut() {
            provisional.request_stop();
        }
        if let Some(active) = active.as_mut() {
            active.request_stop();
        }
        #[cfg(test)]
        self.pause_shutdown_before_task_joins().await;
        if let Some(provisional) = provisional {
            provisional.stop().await;
        }
        let result = if let Some(active) = active {
            active.stop().await
        } else {
            Ok(())
        };
        self.shared.inner.lock().await.lease.take();
        result
    }

    #[cfg(test)]
    pub(super) fn install_shutdown_pause(
        &self,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *self.shared.shutdown_pause.lock().unwrap() = Some(ShutdownPause {
            entered: entered.clone(),
            release: release.clone(),
        });
        (entered, release)
    }

    #[cfg(test)]
    pub(super) fn fail_next_quiesce(&self) {
        self.shared.fail_next_quiesce.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn install_quiesce_pause(
        &self,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        *self.shared.quiesce_pause.lock().unwrap() = Some(ShutdownPause {
            entered: entered.clone(),
            release: release.clone(),
        });
        (entered, release)
    }

    #[cfg(test)]
    async fn pause_quiesce_before_join(&self) {
        let pause = self
            .shared
            .quiesce_pause
            .lock()
            .ok()
            .and_then(|mut pause| pause.take());
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(test)]
    async fn pause_shutdown_before_task_joins(&self) {
        let pause = self
            .shared
            .shutdown_pause
            .lock()
            .ok()
            .and_then(|mut pause| pause.take());
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    #[cfg(test)]
    pub(super) async fn terminate_provisional_listener(&self) -> Result<(), RuntimeError> {
        let shutdown = {
            let mut inner = self.shared.inner.lock().await;
            inner
                .provisional
                .as_mut()
                .and_then(|runtime| runtime.hub.as_mut())
                .ok_or_else(|| RuntimeError::Invariant("provisional listener is missing".into()))?
                .shutdown
                .take()
        };
        if let Some(shutdown) = shutdown {
            let _ = shutdown.send(());
        }
        self.wait_for_listener_exit(true).await;
        Ok(())
    }

    #[cfg(test)]
    pub(super) async fn terminate_active_listener(&self) -> Result<(), RuntimeError> {
        let shutdown = {
            let mut inner = self.shared.inner.lock().await;
            inner
                .active
                .as_mut()
                .and_then(|runtime| runtime.hub.as_mut())
                .ok_or_else(|| RuntimeError::Invariant("active listener is missing".into()))?
                .shutdown
                .take()
        };
        if let Some(shutdown) = shutdown {
            let _ = shutdown.send(());
        }
        self.wait_for_listener_exit(false).await;
        Ok(())
    }

    #[cfg(test)]
    pub(super) async fn fail_active_listener_cleanup(&self) -> Result<(), RuntimeError> {
        let mut inner = self.shared.inner.lock().await;
        inner
            .active
            .as_mut()
            .and_then(|runtime| runtime.hub.as_mut())
            .ok_or_else(|| RuntimeError::Invariant("active listener is missing".into()))?
            .fail_cleanup = true;
        Ok(())
    }

    #[cfg(test)]
    async fn wait_for_listener_exit(&self, provisional: bool) {
        for _ in 0..100 {
            let running = {
                let inner = self.shared.inner.lock().await;
                if provisional {
                    inner
                        .provisional
                        .as_ref()
                        .and_then(|runtime| runtime.hub.as_ref())
                        .is_some_and(HubRuntime::is_running)
                } else {
                    inner
                        .active
                        .as_ref()
                        .and_then(|runtime| runtime.hub.as_ref())
                        .is_some_and(HubRuntime::is_running)
                }
            };
            if !running {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    fn ensure_available(inner: &RuntimeInner) -> Result<(), RuntimeError> {
        if inner.shut_down {
            Err(RuntimeError::Shutdown)
        } else if inner.lease.is_none() {
            Err(RuntimeError::NoOwnership)
        } else {
            Ok(())
        }
    }

    fn provisional(
        inner: &RuntimeInner,
        transition: RuntimeTransition,
    ) -> Result<&ProvisionalRuntime, RuntimeError> {
        inner
            .provisional
            .as_ref()
            .filter(|runtime| runtime.id == transition.id)
            .ok_or_else(|| RuntimeError::Invariant("runtime transition is no longer active".into()))
    }

    fn provisional_mut(
        inner: &mut RuntimeInner,
        transition: RuntimeTransition,
    ) -> Result<&mut ProvisionalRuntime, RuntimeError> {
        inner
            .provisional
            .as_mut()
            .filter(|runtime| runtime.id == transition.id)
            .ok_or_else(|| RuntimeError::Invariant("runtime transition is no longer active".into()))
    }

    fn take_provisional(
        inner: &mut RuntimeInner,
        transition: RuntimeTransition,
    ) -> Result<ProvisionalRuntime, RuntimeError> {
        if inner.provisional.as_ref().map(|runtime| runtime.id) != Some(transition.id) {
            return Err(RuntimeError::Invariant(
                "runtime transition is no longer active".into(),
            ));
        }
        inner
            .provisional
            .take()
            .ok_or_else(|| RuntimeError::Invariant("provisional runtime is missing".into()))
    }

    fn validate_provisional_listener(
        inner: &RuntimeInner,
        provisional: &ProvisionalRuntime,
    ) -> Result<(), RuntimeError> {
        if provisional.spec.role() != SyncRole::HomeHub || provisional.allow_degraded_listener {
            return Ok(());
        }
        let hub = if provisional.reuse_active_hub {
            inner.active.as_ref().and_then(|active| active.hub.as_ref())
        } else {
            provisional.hub.as_ref()
        }
        .ok_or_else(|| RuntimeError::Listener("prepared listener is missing".into()))?;
        if let Some(error) = hub.liveness_error() {
            Err(RuntimeError::Listener(error))
        } else {
            Ok(())
        }
    }

    fn prepare_outbox(&self, spec: &RuntimeSpec) -> Result<PreparedOutbox, RuntimeError> {
        let destination = match spec {
            RuntimeSpec::HomeHub { .. } => {
                OutboxDestination::Local(HubLibrary::new(self.shared.state.db_path().to_path_buf()))
            }
            RuntimeSpec::ConnectedDevice { hub, .. } => OutboxDestination::Remote {
                hub: hub.clone(),
                replication: self.shared.hub_capabilities.replication(),
            },
            RuntimeSpec::Standalone { .. } => {
                return Err(RuntimeError::Invariant(
                    "Standalone cannot own an outbox worker".into(),
                ));
            }
        };
        let upload_recording_payloads = match spec {
            RuntimeSpec::HomeHub {
                upload_recording_payloads,
                ..
            }
            | RuntimeSpec::ConnectedDevice {
                upload_recording_payloads,
                ..
            } => *upload_recording_payloads,
            RuntimeSpec::Standalone { .. } => false,
        };
        let worker = OutboxWorker::new(self.shared.state.db_path().to_path_buf(), destination)
            .with_payload_uploads(upload_recording_payloads)
            .with_role_epoch(spec.role_version().value());
        let (start, start_receiver) = oneshot::channel();
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            if start_receiver.await.is_ok() {
                worker.run(worker_cancellation).await;
            }
        });
        Ok(PreparedOutbox {
            start: Some(start),
            runtime: Some(OutboxRuntime {
                cancellation,
                task: Some(task),
            }),
        })
    }

    async fn prepare_hub(&self, spec: &RuntimeSpec) -> Result<HubRuntime, RuntimeError> {
        let RuntimeSpec::HomeHub {
            role_epoch,
            hub_id,
            owner_login,
            device_name,
            ..
        } = spec
        else {
            return Err(RuntimeError::Invariant(
                "only a Home Hub can prepare a listener".into(),
            ));
        };
        if !self.shared.hub_bind_address.ip().is_loopback() {
            return Err(RuntimeError::Listener(format!(
                "non-loopback bind address {}",
                self.shared.hub_bind_address
            )));
        }
        let listener = tokio::net::TcpListener::bind(self.shared.hub_bind_address)
            .await
            .map_err(|error| RuntimeError::Listener(error.to_string()))?;
        let mut config = HubServerConfig::new(*hub_id, owner_login)
            .map_err(|error| RuntimeError::Listener(error.to_string()))?
            .with_library(HubLibrary::new(self.shared.state.db_path().to_path_buf()));
        if let Some(device_name) = device_name {
            config = config.with_device_name(device_name);
        }
        let server = HubServer::new(config);
        let (shutdown, receiver) = oneshot::channel();
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let failure = Arc::new(StdMutex::new(None));
        let task_failure = Arc::clone(&failure);
        let state = self.shared.state.clone();
        let role_version = RoleVersion::new(*role_epoch);
        let task = tokio::spawn(async move {
            let result = server
                .serve_with_shutdown(listener, async move {
                    let _ = receiver.await;
                })
                .await;
            if !task_cancellation.is_cancelled() {
                let message = match &result {
                    Ok(()) => "Home Hub listener terminated unexpectedly".to_owned(),
                    Err(error) => format!("Home Hub listener failed: {error}"),
                };
                if let Ok(mut failure) = task_failure.lock() {
                    *failure = Some(message.clone());
                }
                if let Err(error) = state.record_error(role_version.value(), Some(&message)) {
                    tracing::warn!(%error, role_version = role_version.value(), "failed to record Home Hub listener failure");
                }
            }
            result
        });
        let runtime = HubRuntime {
            shutdown: Some(shutdown),
            cancellation,
            task: Some(task),
            failure,
            #[cfg(test)]
            fail_cleanup: false,
        };
        tokio::task::yield_now().await;
        if let Some(error) = runtime.liveness_error() {
            return Err(RuntimeError::Listener(error));
        }
        Ok(runtime)
    }
}
