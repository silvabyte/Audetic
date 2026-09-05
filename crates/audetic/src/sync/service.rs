//! Library Sync composition root and lifecycle control surface.
//!
//! Data-plane routes receive the independent [`SharedLibrary`] interface;
//! this type retains only initialization, role control, health, and shutdown.

use audetic_core::sync::{SyncSetupRequest, SyncSetupResult, SyncStatus};

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use super::client::NetworkHubAdapter;
use super::protocol::HUB_LISTENER_ADDRESS;
use super::runtime::RuntimeDependencies;
use super::shared_library::SharedLibrary;
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
    library: SharedLibrary,
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
        service.library = SharedLibrary::standalone(service.coordinator.clone());
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
        Self::with_runtime_dependencies(
            db_path,
            tailscale,
            hub_capabilities,
            hub_bind_address,
            RuntimeDependencies::default(),
        )
    }

    pub(crate) fn with_runtime_dependencies(
        db_path: PathBuf,
        tailscale: Arc<dyn TailscaleControl>,
        hub_capabilities: HubCapabilities,
        hub_bind_address: SocketAddr,
        runtime_dependencies: RuntimeDependencies,
    ) -> Self {
        let coordinator = RoleCoordinator::new(
            db_path,
            tailscale,
            hub_capabilities,
            hub_bind_address,
            runtime_dependencies,
        );
        let library = SharedLibrary::new(coordinator.clone());
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

    /// Route-facing data-plane interface. Cloning it is cheap and does not
    /// expose lifecycle state, repositories, or database paths.
    pub fn library(&self) -> SharedLibrary {
        self.library.clone()
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
