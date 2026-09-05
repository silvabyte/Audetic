use audetic_core::sync::{
    CacheLevel, HubConnection, RecordId, SyncRole, SyncSetupRequest, SyncSetupResult,
};
use fs2::FileExt;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::db::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use crate::sync::client::{HubClient, NetworkHubAdapter};
use crate::sync::runtime::RuntimeDependencies;
use crate::sync::transport::HubCapabilities;
use crate::sync::SyncService;

use super::clock::{ManualClock, WorkerProbe};
use super::tailnet::{FakeTailnet, TailnetTransport};

pub(super) struct HomeHubTopology {
    _root: tempfile::TempDir,
    pub(super) tailnet: FakeTailnet,
}

impl HomeHubTopology {
    pub(super) fn new() -> Self {
        Self {
            _root: tempfile::tempdir().unwrap(),
            tailnet: FakeTailnet::default(),
        }
    }

    pub(super) fn daemon(&self, name: &str, owner_login: &str) -> TestDaemon {
        self.tailnet.add_node(name, owner_login);
        let root = self._root.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        let db_path = root.join("audetic.db");
        crate::db::migrate_db_at(&db_path).unwrap();
        TestDaemon::new(
            name,
            root,
            db_path,
            self.tailnet.clone(),
            Arc::new(ManualClock::new()),
        )
    }
}

pub(super) struct TestDaemon {
    pub(super) name: String,
    pub(super) root: PathBuf,
    pub(super) db_path: PathBuf,
    tailnet: FakeTailnet,
    clock: Arc<ManualClock>,
    pub(super) probe: Arc<WorkerProbe>,
    service: Option<SyncService>,
}

impl TestDaemon {
    fn new(
        name: &str,
        root: PathBuf,
        db_path: PathBuf,
        tailnet: FakeTailnet,
        clock: Arc<ManualClock>,
    ) -> Self {
        let probe = Arc::new(WorkerProbe::default());
        let service = Self::make_service(name, &db_path, &tailnet, &clock, &probe);
        Self {
            name: name.to_owned(),
            root,
            db_path,
            tailnet,
            clock,
            probe,
            service: Some(service),
        }
    }

    fn make_service(
        name: &str,
        db_path: &Path,
        tailnet: &FakeTailnet,
        clock: &Arc<ManualClock>,
        probe: &Arc<WorkerProbe>,
    ) -> SyncService {
        let adapter = NetworkHubAdapter::from_transport(tailnet.transport(name));
        SyncService::with_runtime_dependencies(
            db_path.to_path_buf(),
            tailnet.tailscale(name),
            HubCapabilities::from_adapter(adapter),
            "127.0.0.1:0".parse().unwrap(),
            RuntimeDependencies {
                launcher: tailnet.launcher(name),
                clock: clock.clone(),
                observer: probe.clone(),
            },
        )
    }

    pub(super) fn service(&self) -> &SyncService {
        self.service.as_ref().unwrap()
    }

    pub(super) async fn start(&self) {
        self.service().initialize().await.unwrap();
    }

    pub(super) async fn activate_hub(&self, upload_payloads: bool) -> HubConnection {
        let preview = self
            .service()
            .configure(SyncSetupRequest {
                role: SyncRole::HomeHub,
                device_name: Some(self.name.clone()),
                hub: None,
                upload_recording_payloads: upload_payloads,
                cache_level: CacheLevel::LiveOnly,
                shared_config_enabled: true,
                confirm_serve_change: false,
            })
            .await
            .unwrap();
        assert_eq!(preview.status.role, SyncRole::Standalone);
        let activated = self
            .service()
            .configure(SyncSetupRequest {
                role: SyncRole::HomeHub,
                device_name: Some(self.name.clone()),
                hub: None,
                upload_recording_payloads: upload_payloads,
                cache_level: CacheLevel::LiveOnly,
                shared_config_enabled: true,
                confirm_serve_change: true,
            })
            .await
            .unwrap();
        HubConnection {
            base_url: self.tailnet.base_url(&self.name),
            hub_id: activated.status.local_hub_id.unwrap(),
            owner_login: activated.status.network.owner_login.unwrap(),
        }
    }

    pub(super) async fn discover(&self) -> SyncSetupResult {
        self.service().discover().await.unwrap()
    }

    pub(super) async fn connect(&self, hub: HubConnection, upload_payloads: bool) {
        self.service()
            .configure(SyncSetupRequest {
                role: SyncRole::ConnectedDevice,
                device_name: Some(self.name.clone()),
                hub: Some(hub),
                upload_recording_payloads: upload_payloads,
                cache_level: CacheLevel::LiveOnly,
                shared_config_enabled: false,
                confirm_serve_change: false,
            })
            .await
            .unwrap();
    }

    pub(super) fn insert_dictation(&self, text: &str, audio: Option<&[u8]>) -> (i64, RecordId) {
        let audio_path = self.root.join(format!("{}.wav", text.replace(' ', "-")));
        if let Some(bytes) = audio {
            std::fs::write(&audio_path, bytes).unwrap();
        }
        let connection = crate::db::open_db_at(&self.db_path).unwrap();
        crate::db::insert_workflow_record(
            &connection,
            &Workflow::new(
                WorkflowType::VoiceToText,
                WorkflowData::VoiceToText(VoiceToTextData {
                    text: text.into(),
                    audio_path: audio_path.to_string_lossy().into_owned(),
                }),
            ),
        )
        .unwrap()
    }

    pub(super) fn connection(&self) -> rusqlite::Connection {
        crate::db::open_db_at(&self.db_path).unwrap()
    }

    pub(super) fn role_epoch(&self) -> u64 {
        self.connection()
            .query_row(
                "SELECT role_epoch FROM sync_settings WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    pub(super) async fn wait_for_cycle(&self) {
        let role_epoch = self.role_epoch();
        self.probe
            .wait_for(|events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        crate::sync::observer::WorkerEvent::OutboxCycleSucceeded {
                            role_epoch: event_epoch
                        } if *event_epoch == role_epoch
                    )
                })
            })
            .await;
    }

    pub(super) async fn drive_cycle(&self) {
        self.drive_cycle_by(Duration::from_secs(2)).await;
    }

    pub(super) async fn drive_cycle_by(&self, duration: Duration) {
        self.drive_cycle_with_result(duration, true).await;
    }

    pub(super) async fn drive_failed_cycle(&self) {
        self.drive_cycle_with_result(Duration::from_secs(2), false)
            .await;
    }

    pub(super) async fn drive_failed_cycle_by(&self, duration: Duration) {
        self.drive_cycle_with_result(duration, false).await;
    }

    pub(super) async fn begin_cycle_by(&self, duration: Duration) {
        self.clock.wait_for_sleepers(1).await;
        self.clock.advance(duration);
    }

    async fn drive_cycle_with_result(&self, duration: Duration, success: bool) {
        let role_epoch = self.role_epoch();
        let before = self.probe.successful_cycles(role_epoch);
        let failed_before = self
            .probe
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    crate::sync::observer::WorkerEvent::OutboxCycleFailed {
                        role_epoch: event_epoch,
                        ..
                    } if *event_epoch == role_epoch
                )
            })
            .count();
        self.clock.wait_for_sleepers(1).await;
        self.clock.advance(duration);
        self.probe
            .wait_for(|events| {
                if success {
                    events
                        .iter()
                        .filter(|event| {
                            matches!(
                                event,
                                crate::sync::observer::WorkerEvent::OutboxCycleSucceeded {
                                    role_epoch: event_epoch
                                } if *event_epoch == role_epoch
                            )
                        })
                        .count()
                        > before
                } else {
                    events
                        .iter()
                        .filter(|event| {
                            matches!(
                                event,
                                crate::sync::observer::WorkerEvent::OutboxCycleFailed {
                                    role_epoch: event_epoch,
                                    ..
                                } if *event_epoch == role_epoch
                            )
                        })
                        .count()
                        > failed_before
                }
            })
            .await;
    }

    pub(super) async fn restart(&mut self) {
        let role_epoch = self.role_epoch();
        let listener_stops = self
            .probe
            .events()
            .iter()
            .filter(|event| {
                matches!(event, crate::sync::observer::WorkerEvent::ListenerStopped {
                    role_epoch: event_epoch
                } if *event_epoch == role_epoch)
            })
            .count();
        let worker_stops = self
            .probe
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    crate::sync::observer::WorkerEvent::OutboxStopped {
                        role_epoch: event_epoch
                    } if *event_epoch == role_epoch
                )
            })
            .count();
        self.shutdown_for_restart().await;
        self.reconstruct().await;
        let role = self.service().status().await.unwrap().role;
        let events = self.probe.events();
        if role == SyncRole::HomeHub {
            assert_eq!(self.tailnet.published_router_count(&self.name), 1);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        crate::sync::observer::WorkerEvent::ListenerStopped {
                            role_epoch: event_epoch
                        } if *event_epoch == role_epoch
                    ))
                    .count(),
                listener_stops + 1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        crate::sync::observer::WorkerEvent::ListenerStarted {
                            role_epoch: event_epoch
                        } if *event_epoch == role_epoch
                    ))
                    .count(),
                2
            );
        }
        if role != SyncRole::Standalone {
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        crate::sync::observer::WorkerEvent::OutboxStopped {
                            role_epoch: event_epoch
                        } if *event_epoch == role_epoch
                    ))
                    .count(),
                worker_stops + 1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(
                        event,
                        crate::sync::observer::WorkerEvent::OutboxStarted {
                            role_epoch: event_epoch
                        } if *event_epoch == role_epoch
                    ))
                    .count(),
                2
            );
        }
    }

    pub(super) async fn shutdown_for_restart(&mut self) {
        let old = self.service.take().unwrap();
        old.shutdown().await.unwrap();
        drop(old);
        let lease = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.root.join("sync/.runtime.lock"))
            .unwrap();
        lease
            .try_lock_exclusive()
            .expect("shutdown must release the process ownership lease");
        lease.unlock().unwrap();
    }

    pub(super) async fn reconstruct(&mut self) {
        let service = Self::make_service(
            &self.name,
            &self.db_path,
            &self.tailnet,
            &self.clock,
            &self.probe,
        );
        service.initialize().await.unwrap();
        self.service = Some(service);
    }

    pub(super) fn direct_client(&self, target: &str) -> HubClient<TailnetTransport> {
        HubClient::with_transport(
            &self.tailnet.base_url(target),
            self.tailnet.transport(&self.name),
        )
        .unwrap()
    }
}
