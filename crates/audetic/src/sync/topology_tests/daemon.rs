use async_trait::async_trait;
use audetic_core::sync::{
    CacheLevel, HubCandidate, HubConnection, RecordId, SyncRole, SyncSetupRequest, SyncSetupResult,
};
use fs2::FileExt;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::db::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use crate::sync::client::{discover_hubs, HubClient};
use crate::sync::protocol::{
    DictationPage, MeetingPage, MeetingTitlePatch, RecordKind, SharedMeeting, SnapshotBatch,
    SnapshotBatchResponse,
};
use crate::sync::runtime::RuntimeDependencies;
use crate::sync::transport::{
    BlobUpload, DiscoveryOutcome, HubCapabilities, HubProbe, HubTransferError,
    RemoteDictationLibrary, RemoteLibraryMutations, RemoteMeetingLibrary, RemotePayloadSource,
    ReplicationTransport, StreamingPayloadResponse,
};
use crate::sync::SyncService;

use super::clock::{ManualClock, WorkerProbe};
use super::tailnet::{FakeTailnet, TailnetTransport};

#[derive(Clone)]
struct TailnetHubAdapter {
    transport: TailnetTransport,
}

impl TailnetHubAdapter {
    fn client(&self, hub: &HubConnection) -> Result<HubClient<TailnetTransport>, HubTransferError> {
        HubClient::with_transport(&hub.base_url, self.transport.clone()).map_err(Into::into)
    }
}

#[async_trait]
impl HubProbe for TailnetHubAdapter {
    async fn handshake(&self, hub: &HubConnection) -> Result<HubCandidate, HubTransferError> {
        HubProbe::handshake(&self.client(hub)?, hub).await
    }

    async fn discover(
        &self,
        candidates: Vec<String>,
        expected_owner_login: &str,
    ) -> DiscoveryOutcome {
        discover_hubs(self.transport.clone(), candidates, expected_owner_login).await
    }
}

#[async_trait]
impl ReplicationTransport for TailnetHubAdapter {
    async fn upload_snapshots(
        &self,
        hub: &HubConnection,
        batch: SnapshotBatch,
    ) -> Result<SnapshotBatchResponse, HubTransferError> {
        ReplicationTransport::upload_snapshots(&self.client(hub)?, hub, batch).await
    }

    async fn upload_blob(
        &self,
        hub: &HubConnection,
        blob: BlobUpload,
    ) -> Result<(), HubTransferError> {
        ReplicationTransport::upload_blob(&self.client(hub)?, hub, blob).await
    }
}

#[async_trait]
impl RemoteDictationLibrary for TailnetHubAdapter {
    async fn page_dictations(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DictationPage, HubTransferError> {
        RemoteDictationLibrary::page_dictations(
            &self.client(hub)?,
            hub,
            query,
            from,
            to,
            cursor,
            limit,
        )
        .await
    }
}

#[async_trait]
impl RemoteMeetingLibrary for TailnetHubAdapter {
    async fn page_meetings(
        &self,
        hub: &HubConnection,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MeetingPage, HubTransferError> {
        RemoteMeetingLibrary::page_meetings(&self.client(hub)?, hub, query, cursor, limit).await
    }

    async fn meeting(
        &self,
        hub: &HubConnection,
        id: RecordId,
    ) -> Result<Option<SharedMeeting>, HubTransferError> {
        RemoteMeetingLibrary::meeting(&self.client(hub)?, hub, id).await
    }
}

#[async_trait]
impl RemoteLibraryMutations for TailnetHubAdapter {
    async fn update_meeting_title(
        &self,
        hub: &HubConnection,
        id: RecordId,
        patch: MeetingTitlePatch,
    ) -> Result<SharedMeeting, HubTransferError> {
        RemoteLibraryMutations::update_meeting_title(&self.client(hub)?, hub, id, patch).await
    }

    async fn delete_record(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
    ) -> Result<(), HubTransferError> {
        RemoteLibraryMutations::delete_record(&self.client(hub)?, hub, id, kind).await
    }
}

#[async_trait]
impl RemotePayloadSource for TailnetHubAdapter {
    async fn stream_payload(
        &self,
        hub: &HubConnection,
        id: RecordId,
        kind: RecordKind,
        range: Option<&str>,
    ) -> Result<StreamingPayloadResponse, HubTransferError> {
        RemotePayloadSource::stream_payload(&self.client(hub)?, hub, id, kind, range).await
    }
}

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
        let adapter = TailnetHubAdapter {
            transport: tailnet.transport(name),
        };
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

    pub(super) async fn wait_for_cycle(&self) {
        self.probe
            .wait_for(|events| {
                events.iter().any(|event| {
                    matches!(
                        event,
                        crate::sync::observer::WorkerEvent::OutboxCycleFinished { .. }
                    )
                })
            })
            .await;
    }

    pub(super) async fn drive_cycle(&self) {
        self.drive_cycle_by(Duration::from_secs(2)).await;
    }

    pub(super) async fn drive_cycle_by(&self, duration: Duration) {
        let before = self.probe.finished_cycles();
        self.clock.wait_for_sleepers(1).await;
        self.clock.advance(duration);
        self.probe
            .wait_for(|events| {
                events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            crate::sync::observer::WorkerEvent::OutboxCycleFinished { .. }
                        )
                    })
                    .count()
                    > before
            })
            .await;
    }

    pub(super) async fn restart(&mut self) {
        let listener_stops = self
            .probe
            .events()
            .iter()
            .filter(|event| matches!(event, crate::sync::observer::WorkerEvent::ListenerStopped))
            .count();
        let worker_stops = self
            .probe
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    crate::sync::observer::WorkerEvent::OutboxStopped { .. }
                )
            })
            .count();
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
        let service = Self::make_service(
            &self.name,
            &self.db_path,
            &self.tailnet,
            &self.clock,
            &self.probe,
        );
        service.initialize().await.unwrap();
        self.service = Some(service);
        let role = self.service().status().await.unwrap().role;
        if role == SyncRole::HomeHub {
            assert_eq!(self.tailnet.published_router_count(&self.name), 1);
            assert!(
                self.probe
                    .events()
                    .iter()
                    .filter(|event| matches!(
                        event,
                        crate::sync::observer::WorkerEvent::ListenerStopped
                    ))
                    .count()
                    > listener_stops
            );
        }
        if role != SyncRole::Standalone {
            assert!(
                self.probe
                    .events()
                    .iter()
                    .filter(|event| matches!(
                        event,
                        crate::sync::observer::WorkerEvent::OutboxStopped { .. }
                    ))
                    .count()
                    > worker_stops
            );
        }
    }

    pub(super) fn direct_client(&self, target: &str) -> HubClient<TailnetTransport> {
        HubClient::with_transport(
            &self.tailnet.base_url(target),
            self.tailnet.transport(&self.name),
        )
        .unwrap()
    }
}
