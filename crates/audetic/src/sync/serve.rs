//! Deep boundary for Audetic's path-scoped Tailscale Serve integration.
//!
//! Callers deal in network identities and compensation tokens. Raw Serve JSON,
//! command execution, exact mapping tuples, and rollback rules stay here.

use audetic_core::sync::{HubConnection, HubId, ServeMappingState, SyncNetworkAssessment};
use thiserror::Error;

use std::sync::Arc;

use crate::db::sync_serve::SyncServeOwnership;

use super::protocol::ServeSpec;
use super::tailscale::{MappingState, TailscaleControl, TailscaleError, TailscaleStatus};

#[derive(Debug, Error)]
pub enum ServeError {
    #[error("Tailscale is not ready: {0}")]
    Tailscale(#[from] TailscaleError),
    #[error("invalid sync request: Tailscale backend is {0:?}, expected Running")]
    BackendNotRunning(String),
    #[error("Home Hub verification failed: {0}")]
    Verification(String),
    #[error("sync runtime task failed: {0}")]
    BlockingTask(String),
    #[error("activation failed ({source_error}); rollback also failed ({rollback_error})")]
    Rollback {
        source_error: Box<ServeError>,
        rollback_error: Box<ServeError>,
    },
}

impl ServeError {
    pub const fn is_request_error(&self) -> bool {
        matches!(self, Self::BackendNotRunning(_))
    }

    pub const fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::Tailscale(TailscaleError::ServeCollision)
                | Self::Tailscale(TailscaleError::FunnelEnabled)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkDiscovery {
    pub owner_login: String,
    pub candidate_base_urls: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeHubNetwork {
    dns_name: String,
    owner_login: String,
    serve: ServeSpec,
}

impl HomeHubNetwork {
    pub fn owner_login(&self) -> &str {
        &self.owner_login
    }

    pub fn connection(&self, hub_id: HubId) -> HubConnection {
        HubConnection {
            base_url: self.serve.base_url(&self.dns_name),
            hub_id,
            owner_login: self.owner_login.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppliedServe {
    created: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemovedServe {
    removed: bool,
}

#[derive(Clone)]
pub struct ServeManager {
    tailscale: Arc<dyn TailscaleControl>,
    spec: ServeSpec,
}

impl ServeManager {
    pub fn new(tailscale: Arc<dyn TailscaleControl>) -> Self {
        Self {
            tailscale,
            spec: ServeSpec::audetic(),
        }
    }

    pub fn preview(&self) -> String {
        self.tailscale.serve_preview()
    }

    /// Synchronous uninstall path. The uninstall command is not running inside
    /// Tokio, but still goes through this boundary and the adapter's exact live
    /// mapping check.
    pub fn remove_planned_blocking(&self) -> Result<bool, ServeError> {
        self.tailscale
            .remove_audetic_serve()
            .map_err(ServeError::Tailscale)
    }

    pub async fn discovery(&self) -> Result<NetworkDiscovery, ServeError> {
        let status = self.status().await?;
        require_running(&status)?;
        let candidate_base_urls = status
            .discoverable_peers()
            .map(|peer| self.spec.base_url(&peer.dns_name))
            .collect();
        Ok(NetworkDiscovery {
            owner_login: status.owner_login,
            candidate_base_urls,
        })
    }

    pub async fn prepare_home_hub(&self) -> Result<HomeHubNetwork, ServeError> {
        let status = self.status().await?;
        require_running(&status)?;
        let assessment = self.assessment().await?;
        if assessment.funnel_enabled {
            return Err(TailscaleError::FunnelEnabled.into());
        }
        if assessment.mapping == MappingState::Collision {
            return Err(TailscaleError::ServeCollision.into());
        }
        Ok(HomeHubNetwork {
            dns_name: status.self_dns_name.trim_end_matches('.').to_owned(),
            owner_login: status.owner_login,
            serve: self.spec,
        })
    }

    /// Apply and re-read the exact mapping. If verification fails after this
    /// call created the mapping, compensation is completed before returning.
    pub async fn apply_verified(&self) -> Result<AppliedServe, ServeError> {
        let created = self.apply().await?;
        let verification = async {
            let assessment = self.assessment().await?;
            if assessment.mapping != MappingState::OwnedByAudetic {
                return Err(ServeError::Verification(
                    "Tailscale Serve did not retain the exact Audetic mapping".into(),
                ));
            }
            if assessment.funnel_enabled {
                return Err(TailscaleError::FunnelEnabled.into());
            }
            Ok(())
        }
        .await;
        if let Err(source_error) = verification {
            if created {
                if let Err(rollback_error) = self.remove().await {
                    return Err(ServeError::Rollback {
                        source_error: Box::new(source_error),
                        rollback_error: Box::new(rollback_error),
                    });
                }
            }
            return Err(source_error);
        }
        Ok(AppliedServe { created })
    }

    pub async fn verify_persisted(
        &self,
        ownership: Option<&SyncServeOwnership>,
    ) -> Result<HomeHubNetwork, ServeError> {
        let network = self.prepare_home_hub().await?;
        let assessment = self.assessment().await?;
        if assessment.mapping != MappingState::OwnedByAudetic
            || assessment.funnel_enabled
            || !ownership.is_some_and(is_exact_ownership)
        {
            return Err(ServeError::Verification(
                "persisted Home Hub Serve mapping is missing, changed, or exposed by Funnel".into(),
            ));
        }
        Ok(network)
    }

    /// Remove only when durable ownership still names Audetic's exact tuple.
    pub async fn remove_persisted(
        &self,
        ownership: Option<&SyncServeOwnership>,
    ) -> Result<RemovedServe, ServeError> {
        if !ownership.is_some_and(is_exact_ownership) {
            return Ok(RemovedServe::default());
        }
        Ok(RemovedServe {
            removed: self.remove().await?,
        })
    }

    pub async fn compensate_application(&self, applied: AppliedServe) -> Result<(), ServeError> {
        if applied.created {
            self.remove().await?;
        }
        Ok(())
    }

    pub async fn compensate_removal(&self, removed: RemovedServe) -> Result<(), ServeError> {
        if removed.removed {
            self.apply_verified().await?;
        }
        Ok(())
    }

    pub async fn network_assessment(&self) -> SyncNetworkAssessment {
        let preview = self.preview();
        let status = match self.status().await {
            Ok(status) => status,
            Err(error) => return failed_network_assessment(preview, error.to_string()),
        };
        let assessment = match self.assessment().await {
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

    async fn status(&self) -> Result<TailscaleStatus, ServeError> {
        let tailscale = Arc::clone(&self.tailscale);
        tokio::task::spawn_blocking(move || tailscale.status())
            .await
            .map_err(|error| ServeError::BlockingTask(error.to_string()))?
            .map_err(ServeError::Tailscale)
    }

    async fn assessment(&self) -> Result<super::tailscale::ServeAssessment, ServeError> {
        let tailscale = Arc::clone(&self.tailscale);
        tokio::task::spawn_blocking(move || tailscale.serve_assessment())
            .await
            .map_err(|error| ServeError::BlockingTask(error.to_string()))?
            .map_err(ServeError::Tailscale)
    }

    async fn apply(&self) -> Result<bool, ServeError> {
        let tailscale = Arc::clone(&self.tailscale);
        tokio::task::spawn_blocking(move || tailscale.apply_audetic_serve())
            .await
            .map_err(|error| ServeError::BlockingTask(error.to_string()))?
            .map_err(ServeError::Tailscale)
    }

    async fn remove(&self) -> Result<bool, ServeError> {
        let tailscale = Arc::clone(&self.tailscale);
        tokio::task::spawn_blocking(move || tailscale.remove_audetic_serve())
            .await
            .map_err(|error| ServeError::BlockingTask(error.to_string()))?
            .map_err(ServeError::Tailscale)
    }
}

pub fn expected_ownership() -> SyncServeOwnership {
    let spec = ServeSpec::audetic();
    SyncServeOwnership {
        https_port: spec.https_port(),
        mount_path: spec.mount_path().into(),
        proxy_url: spec.proxy_url().into(),
    }
}

/// Exact tuple check shared with migration-free uninstall inspection.
pub(crate) fn is_exact_ownership(ownership: &SyncServeOwnership) -> bool {
    ownership == &expected_ownership()
}

fn require_running(status: &TailscaleStatus) -> Result<(), ServeError> {
    if status.backend_state == "Running" {
        Ok(())
    } else {
        Err(ServeError::BackendNotRunning(status.backend_state.clone()))
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

#[cfg(test)]
mod tests {
    use super::*;

    use semver::Version;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct FakeTailscale {
        mapping: Mutex<MappingState>,
        funnel: AtomicBool,
        drift_after_apply: AtomicBool,
        apply_calls: AtomicUsize,
        remove_calls: AtomicUsize,
    }

    impl Default for FakeTailscale {
        fn default() -> Self {
            Self {
                mapping: Mutex::new(MappingState::Vacant),
                funnel: AtomicBool::new(false),
                drift_after_apply: AtomicBool::new(false),
                apply_calls: AtomicUsize::new(0),
                remove_calls: AtomicUsize::new(0),
            }
        }
    }

    impl TailscaleControl for FakeTailscale {
        fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
            Ok(TailscaleStatus {
                version: Version::parse("1.80.0").unwrap(),
                backend_state: "Running".into(),
                self_dns_name: "home.example.ts.net.".into(),
                owner_login: "owner@example.com".into(),
                self_is_tagged: false,
                peers: Vec::new(),
            })
        }

        fn serve_assessment(
            &self,
        ) -> Result<super::super::tailscale::ServeAssessment, TailscaleError> {
            Ok(super::super::tailscale::ServeAssessment {
                mapping: *self.mapping.lock().unwrap(),
                funnel_enabled: self.funnel.load(Ordering::SeqCst),
            })
        }

        fn apply_audetic_serve(&self) -> Result<bool, TailscaleError> {
            self.apply_calls.fetch_add(1, Ordering::SeqCst);
            let mut mapping = self.mapping.lock().unwrap();
            let created = *mapping == MappingState::Vacant;
            *mapping = if self.drift_after_apply.load(Ordering::SeqCst) {
                MappingState::Collision
            } else {
                MappingState::OwnedByAudetic
            };
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
            "exact preview".into()
        }
    }

    #[tokio::test]
    async fn readiness_rejects_funnel_and_collisions_without_applying() {
        for (mapping, funnel, expected) in [
            (MappingState::Collision, false, "non-Audetic Serve mapping"),
            (MappingState::Vacant, true, "Funnel is enabled"),
        ] {
            let tailscale = Arc::new(FakeTailscale::default());
            *tailscale.mapping.lock().unwrap() = mapping;
            tailscale.funnel.store(funnel, Ordering::SeqCst);
            let manager = ServeManager::new(tailscale.clone());

            let error = manager.prepare_home_hub().await.unwrap_err();

            assert!(error.to_string().contains(expected));
            assert_eq!(tailscale.apply_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn persisted_ownership_must_be_exact_before_live_removal() {
        let tailscale = Arc::new(FakeTailscale::default());
        *tailscale.mapping.lock().unwrap() = MappingState::OwnedByAudetic;
        let manager = ServeManager::new(tailscale.clone());
        let mut drifted = expected_ownership();
        drifted.proxy_url = "http://127.0.0.1:9999".into();

        let removed = manager.remove_persisted(Some(&drifted)).await.unwrap();
        manager.compensate_removal(removed).await.unwrap();

        assert_eq!(tailscale.remove_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            *tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
    }

    #[tokio::test]
    async fn failed_post_apply_verification_never_removes_a_changed_mapping() {
        let tailscale = Arc::new(FakeTailscale::default());
        tailscale.drift_after_apply.store(true, Ordering::SeqCst);
        let manager = ServeManager::new(tailscale.clone());

        let error = manager.apply_verified().await.unwrap_err();

        assert!(matches!(error, ServeError::Verification(_)));
        assert_eq!(tailscale.apply_calls.load(Ordering::SeqCst), 1);
        assert_eq!(tailscale.remove_calls.load(Ordering::SeqCst), 1);
        assert_eq!(*tailscale.mapping.lock().unwrap(), MappingState::Collision);
    }

    #[tokio::test]
    async fn removal_compensation_reinstates_the_exact_mapping() {
        let tailscale = Arc::new(FakeTailscale::default());
        *tailscale.mapping.lock().unwrap() = MappingState::OwnedByAudetic;
        let manager = ServeManager::new(tailscale.clone());

        let removed = manager
            .remove_persisted(Some(&expected_ownership()))
            .await
            .unwrap();
        manager.compensate_removal(removed).await.unwrap();

        assert_eq!(tailscale.remove_calls.load(Ordering::SeqCst), 1);
        assert_eq!(tailscale.apply_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *tailscale.mapping.lock().unwrap(),
            MappingState::OwnedByAudetic
        );
    }
}
