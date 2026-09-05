//! Sole lifecycle owner for role-dependent Library Sync tasks.

use audetic_core::sync::{HubConnection, HubId, SyncRole};
use thiserror::Error;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use std::net::SocketAddr;
use std::path::PathBuf;

use super::library::HubLibrary;
use super::outbox::{OutboxDestination, OutboxWorker};
use super::server::{HubServer, HubServerConfig};
use super::transport::HubCapabilities;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeSpec {
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
    pub const fn role(&self) -> SyncRole {
        match self {
            Self::Standalone { .. } => SyncRole::Standalone,
            Self::HomeHub { .. } => SyncRole::HomeHub,
            Self::ConnectedDevice { .. } => SyncRole::ConnectedDevice,
        }
    }

    pub const fn role_epoch(&self) -> u64 {
        match self {
            Self::Standalone { role_epoch }
            | Self::HomeHub { role_epoch, .. }
            | Self::ConnectedDevice { role_epoch, .. } => *role_epoch,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub role: Option<SyncRole>,
    pub role_epoch: Option<u64>,
    pub hub_listener_running: bool,
    pub outbox_worker_running: bool,
    pub hub_reachable: bool,
    pub shut_down: bool,
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("Home Hub listener failed: {0}")]
    Listener(String),
    #[error("invalid sync runtime state: {0}")]
    Invariant(String),
    #[error("sync runtime has shut down")]
    Shutdown,
}

struct HubRuntime {
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), super::server::HubServerError>>,
}

impl HubRuntime {
    async fn stop(mut self) -> Result<(), RuntimeError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task
            .await
            .map_err(|error| RuntimeError::Listener(error.to_string()))?
            .map_err(|error| RuntimeError::Listener(error.to_string()))
    }

    fn abort(&mut self) {
        self.shutdown.take();
        self.task.abort();
    }
}

struct OutboxRuntime {
    cancellation: CancellationToken,
    task: JoinHandle<()>,
}

impl OutboxRuntime {
    async fn stop(self) {
        self.cancellation.cancel();
        let _ = self.task.await;
    }

    fn abort(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
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

    async fn abort(mut self) {
        self.start.take();
        if let Some(runtime) = self.runtime.take() {
            runtime.stop().await;
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
}

impl ActiveRuntime {
    async fn stop(mut self) -> Result<(), RuntimeError> {
        if let Some(outbox) = self.outbox.take() {
            outbox.stop().await;
        }
        if let Some(hub) = self.hub.take() {
            hub.stop().await?;
        }
        Ok(())
    }
}

struct RuntimeInner {
    active: Option<ActiveRuntime>,
    shut_down: bool,
}

pub struct PreparedRuntime {
    spec: RuntimeSpec,
    hub: Option<HubRuntime>,
    outbox: Option<PreparedOutbox>,
    reuse_active_hub: bool,
    consumed: bool,
}

impl PreparedRuntime {
    pub fn spec(&self) -> &RuntimeSpec {
        &self.spec
    }

    pub async fn abort(mut self) {
        self.consumed = true;
        if let Some(outbox) = self.outbox.take() {
            outbox.abort().await;
        }
        if let Some(hub) = self.hub.take() {
            let _ = hub.stop().await;
        }
    }
}

impl Drop for PreparedRuntime {
    fn drop(&mut self) {
        if self.consumed {
            return;
        }
        if let Some(outbox) = self.outbox.as_mut() {
            if let Some(runtime) = outbox.runtime.as_mut() {
                runtime.abort();
            }
        }
        if let Some(hub) = self.hub.as_mut() {
            hub.abort();
        }
    }
}

#[derive(Clone, Debug)]
pub struct QuiescedRuntime {
    spec: RuntimeSpec,
}

#[derive(Clone, Debug)]
pub struct QuiescedWorker {
    spec: RuntimeSpec,
    was_running: bool,
}

/// Owns every cancellation primitive and task handle for Library Sync.
pub struct RuntimeSet {
    db_path: PathBuf,
    hub_capabilities: HubCapabilities,
    hub_bind_address: SocketAddr,
    inner: Mutex<RuntimeInner>,
}

impl RuntimeSet {
    pub fn new(
        db_path: PathBuf,
        hub_capabilities: HubCapabilities,
        hub_bind_address: SocketAddr,
    ) -> Self {
        Self {
            db_path,
            hub_capabilities,
            hub_bind_address,
            inner: Mutex::new(RuntimeInner {
                active: None,
                shut_down: false,
            }),
        }
    }

    /// Allocate and bind everything needed by `spec`. Outbox work remains
    /// gated; a new Home Hub listener may run provisionally for its handshake.
    pub async fn prepare(&self, spec: RuntimeSpec) -> Result<PreparedRuntime, RuntimeError> {
        let reuse_active_hub = {
            let inner = self.inner.lock().await;
            if inner.shut_down {
                return Err(RuntimeError::Shutdown);
            }
            matches!(spec, RuntimeSpec::HomeHub { .. })
                && inner.active.as_ref().is_some_and(|active| {
                    active.spec.role() == SyncRole::HomeHub
                        && active
                            .hub
                            .as_ref()
                            .is_some_and(|hub| !hub.task.is_finished())
                })
        };
        let hub = if matches!(spec, RuntimeSpec::HomeHub { .. }) && !reuse_active_hub {
            Some(self.prepare_hub(&spec).await?)
        } else {
            None
        };
        let outbox = if spec.role() == SyncRole::Standalone {
            None
        } else {
            Some(self.prepare_outbox(&spec)?)
        };
        Ok(PreparedRuntime {
            spec,
            hub,
            outbox,
            reuse_active_hub,
            consumed: false,
        })
    }

    /// Publish a prepared role without binding or allocating new resources.
    pub async fn activate(&self, mut prepared: PreparedRuntime) -> Result<(), RuntimeError> {
        let mut inner = self.inner.lock().await;
        if inner.shut_down {
            drop(inner);
            prepared.abort().await;
            return Err(RuntimeError::Shutdown);
        }
        let has_prepared_outbox = prepared
            .outbox
            .as_ref()
            .is_some_and(|outbox| outbox.runtime.is_some());
        let has_reusable_hub = inner
            .active
            .as_ref()
            .is_some_and(|active| active.spec.role() == SyncRole::HomeHub && active.hub.is_some());
        let valid = match prepared.spec.role() {
            SyncRole::Standalone => {
                prepared.hub.is_none() && prepared.outbox.is_none() && !prepared.reuse_active_hub
            }
            SyncRole::HomeHub => {
                has_prepared_outbox
                    && if prepared.reuse_active_hub {
                        prepared.hub.is_none() && has_reusable_hub
                    } else {
                        prepared.hub.is_some()
                    }
            }
            SyncRole::ConnectedDevice => {
                prepared.hub.is_none() && has_prepared_outbox && !prepared.reuse_active_hub
            }
        };
        if !valid {
            let error = RuntimeError::Invariant(format!(
                "prepared {:?} runtime has an invalid resource shape",
                prepared.spec.role()
            ));
            drop(inner);
            prepared.abort().await;
            return Err(error);
        }
        let mut outbox = match prepared.outbox.take().map(PreparedOutbox::activate) {
            Some(Ok(outbox)) => Some(outbox),
            Some(Err(error)) => {
                drop(inner);
                prepared.abort().await;
                return Err(error);
            }
            None => None,
        };
        let mut previous = inner.active.take();
        let hub = if prepared.spec.role() == SyncRole::HomeHub {
            let hub = if prepared.reuse_active_hub {
                previous.as_mut().and_then(|runtime| runtime.hub.take())
            } else {
                prepared.hub.take()
            };
            let Some(hub) = hub else {
                inner.active = previous;
                drop(inner);
                if let Some(outbox) = outbox.take() {
                    outbox.stop().await;
                }
                prepared.abort().await;
                return Err(RuntimeError::Invariant(
                    "prepared Home Hub has no listener".into(),
                ));
            };
            Some(hub)
        } else {
            None
        };
        let active = ActiveRuntime {
            spec: prepared.spec.clone(),
            hub,
            outbox,
            hub_reachable: false,
        };
        inner.active = Some(active);
        prepared.consumed = true;
        drop(inner);

        if let Some(previous) = previous {
            previous.stop().await?;
        }
        Ok(())
    }

    pub async fn start(&self, spec: RuntimeSpec) -> Result<(), RuntimeError> {
        let prepared = self.prepare(spec).await?;
        self.activate(prepared).await
    }

    pub async fn quiesce(&self) -> Result<Option<QuiescedRuntime>, RuntimeError> {
        let active = {
            let mut inner = self.inner.lock().await;
            if inner.shut_down {
                return Err(RuntimeError::Shutdown);
            }
            inner.active.take()
        };
        let Some(active) = active else {
            return Ok(None);
        };
        let spec = active.spec.clone();
        active.stop().await?;
        Ok(Some(QuiescedRuntime { spec }))
    }

    pub async fn restore(&self, quiesced: QuiescedRuntime) -> Result<(), RuntimeError> {
        self.start(quiesced.spec).await
    }

    /// Stop only the authority-writing worker while retaining a Home Hub
    /// listener. This preserves the established demotion ordering.
    pub async fn quiesce_worker(&self) -> Result<QuiescedWorker, RuntimeError> {
        let (spec, outbox) = {
            let mut inner = self.inner.lock().await;
            if inner.shut_down {
                return Err(RuntimeError::Shutdown);
            }
            let active = inner
                .active
                .as_mut()
                .ok_or_else(|| RuntimeError::Invariant("no active runtime to quiesce".into()))?;
            (active.spec.clone(), active.outbox.take())
        };
        let was_running = outbox.is_some();
        if let Some(outbox) = outbox {
            outbox.stop().await;
        }
        Ok(QuiescedWorker { spec, was_running })
    }

    pub async fn restore_worker(&self, quiesced: QuiescedWorker) -> Result<(), RuntimeError> {
        if !quiesced.was_running || quiesced.spec.role() == SyncRole::Standalone {
            return Ok(());
        }
        let outbox = self.prepare_outbox(&quiesced.spec)?.activate()?;
        let mut inner = self.inner.lock().await;
        let active = inner
            .active
            .as_mut()
            .ok_or_else(|| RuntimeError::Invariant("no active runtime to restore".into()))?;
        if active.spec != quiesced.spec || active.outbox.is_some() {
            drop(inner);
            outbox.stop().await;
            return Err(RuntimeError::Invariant(
                "runtime changed while its worker was quiesced".into(),
            ));
        }
        active.outbox = Some(outbox);
        Ok(())
    }

    pub async fn snapshot(&self) -> RuntimeSnapshot {
        let inner = self.inner.lock().await;
        let Some(active) = inner.active.as_ref() else {
            return RuntimeSnapshot {
                shut_down: inner.shut_down,
                ..RuntimeSnapshot::default()
            };
        };
        RuntimeSnapshot {
            role: Some(active.spec.role()),
            role_epoch: Some(active.spec.role_epoch()),
            hub_listener_running: active
                .hub
                .as_ref()
                .is_some_and(|runtime| !runtime.task.is_finished()),
            outbox_worker_running: active
                .outbox
                .as_ref()
                .is_some_and(|runtime| !runtime.task.is_finished()),
            hub_reachable: active.hub_reachable,
            shut_down: inner.shut_down,
        }
    }

    pub async fn observe_reachability(&self, role_epoch: u64, reachable: bool) -> bool {
        let mut inner = self.inner.lock().await;
        let Some(active) = inner.active.as_mut() else {
            return false;
        };
        if active.spec.role_epoch() != role_epoch {
            return false;
        }
        active.hub_reachable = reachable;
        true
    }

    pub async fn shutdown(&self) -> Result<(), RuntimeError> {
        let active = {
            let mut inner = self.inner.lock().await;
            if inner.shut_down {
                return Ok(());
            }
            inner.shut_down = true;
            inner.active.take()
        };
        if let Some(active) = active {
            active.stop().await?;
        }
        Ok(())
    }

    fn prepare_outbox(&self, spec: &RuntimeSpec) -> Result<PreparedOutbox, RuntimeError> {
        let destination = match spec {
            RuntimeSpec::HomeHub { .. } => {
                OutboxDestination::Local(HubLibrary::new(self.db_path.clone()))
            }
            RuntimeSpec::ConnectedDevice { hub, .. } => OutboxDestination::Remote {
                hub: hub.clone(),
                replication: self.hub_capabilities.replication(),
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
        let worker = OutboxWorker::new(self.db_path.clone(), destination)
            .with_payload_uploads(upload_recording_payloads)
            .with_role_epoch(spec.role_epoch());
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
            runtime: Some(OutboxRuntime { cancellation, task }),
        })
    }

    async fn prepare_hub(&self, spec: &RuntimeSpec) -> Result<HubRuntime, RuntimeError> {
        let RuntimeSpec::HomeHub {
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
        if !self.hub_bind_address.ip().is_loopback() {
            return Err(RuntimeError::Listener(format!(
                "non-loopback bind address {}",
                self.hub_bind_address
            )));
        }
        let listener = tokio::net::TcpListener::bind(self.hub_bind_address)
            .await
            .map_err(|error| RuntimeError::Listener(error.to_string()))?;
        let mut config = HubServerConfig::new(*hub_id, owner_login)
            .map_err(|error| RuntimeError::Listener(error.to_string()))?
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
        Ok(HubRuntime {
            shutdown: Some(shutdown),
            task,
        })
    }
}
