use audetic_core::sync::{CacheLevel, SyncRole, SyncSetupRequest};
use semver::Version;

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::db::{VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use crate::sync::client::NetworkHubAdapter;
use crate::sync::clock::SystemSyncClock;
use crate::sync::observer::{WorkerEvent, WorkerObserver};
use crate::sync::runtime::{RuntimeDependencies, TcpHubRuntimeLauncher};
use crate::sync::tailscale::{
    MappingState, ServeAssessment, TailscaleControl, TailscaleError, TailscaleStatus,
};
use crate::sync::transport::HubCapabilities;
use crate::sync::SyncService;

use super::clock::{ManualClock, WorkerProbe};
use super::tailnet::FakeTailnet;
use super::watchdog;

const HELPER_TEST: &str = "sync::topology_tests::process_contract::process_lock_helper";

struct ProcessTailscale {
    mapping: Arc<Mutex<MappingState>>,
}

impl ProcessTailscale {
    fn new(mapping: MappingState) -> Self {
        Self {
            mapping: Arc::new(Mutex::new(mapping)),
        }
    }
}

impl TailscaleControl for ProcessTailscale {
    fn status(&self) -> Result<TailscaleStatus, TailscaleError> {
        Ok(TailscaleStatus {
            version: Version::parse("1.80.0").unwrap(),
            backend_state: "Running".into(),
            self_dns_name: "process-helper.audetic.test.ts.net.".into(),
            owner_login: "owner@example.com".into(),
            self_is_tagged: false,
            peers: Vec::new(),
        })
    }

    fn serve_assessment(&self) -> Result<ServeAssessment, TailscaleError> {
        Ok(ServeAssessment {
            mapping: *self.mapping.lock().unwrap(),
            funnel_enabled: false,
        })
    }

    fn apply_audetic_serve(&self) -> Result<bool, TailscaleError> {
        let mut mapping = self.mapping.lock().unwrap();
        let created = *mapping == MappingState::Vacant;
        *mapping = MappingState::OwnedByAudetic;
        Ok(created)
    }

    fn remove_audetic_serve(&self) -> Result<bool, TailscaleError> {
        let mut mapping = self.mapping.lock().unwrap();
        let removed = *mapping == MappingState::OwnedByAudetic;
        *mapping = MappingState::Vacant;
        Ok(removed)
    }

    fn serve_preview(&self) -> String {
        "process helper serve preview".into()
    }
}

struct FileObserver(PathBuf);

impl WorkerObserver for FileObserver {
    fn observe(&self, event: WorkerEvent) {
        let mut output = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)
            .unwrap();
        writeln!(output, "{event:?}").unwrap();
    }
}

fn process_service(db_path: &Path, address: SocketAddr, event_log: &Path) -> SyncService {
    SyncService::with_runtime_dependencies(
        db_path.to_path_buf(),
        Arc::new(ProcessTailscale::new(MappingState::OwnedByAudetic)),
        HubCapabilities::from_adapter(NetworkHubAdapter::default()),
        address,
        RuntimeDependencies {
            launcher: Arc::new(TcpHubRuntimeLauncher),
            clock: Arc::new(SystemSyncClock),
            observer: Arc::new(FileObserver(event_log.to_path_buf())),
        },
    )
}

#[test]
#[ignore = "spawned only by the process-lock contract test"]
fn process_lock_helper() {
    let Ok(db_path) = std::env::var("AUDETIC_PROCESS_HELPER_DB") else {
        return;
    };
    let address = std::env::var("AUDETIC_PROCESS_HELPER_ADDRESS")
        .unwrap()
        .parse()
        .unwrap();
    let event_log = PathBuf::from(std::env::var("AUDETIC_PROCESS_HELPER_EVENTS").unwrap());
    let expect_locked = std::env::var_os("AUDETIC_PROCESS_HELPER_EXPECT_LOCKED").is_some();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let service = process_service(Path::new(&db_path), address, &event_log);
        match service.initialize().await {
            Err(error) if expect_locked => {
                let message = error.to_string();
                assert!(message.contains("another Library Sync runtime owns"));
                println!("PROCESS_LOCKED {message}");
                std::io::stdout().flush().unwrap();
                return;
            }
            Err(error) => panic!("process helper failed to initialize: {error}"),
            Ok(_) if expect_locked => panic!("second process unexpectedly acquired ownership"),
            Ok(_) => {}
        }
        println!("PROCESS_READY");
        std::io::stdout().flush().unwrap();
        let mut stop = String::new();
        std::io::stdin().read_line(&mut stop).unwrap();
        service.shutdown().await.unwrap();
    });
}

struct RunningHelper {
    child: Child,
    lines: std::sync::mpsc::Receiver<String>,
}

fn helper_command(db_path: &Path, address: SocketAddr, event_log: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .arg("--exact")
        .arg(HELPER_TEST)
        .arg("--ignored")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("AUDETIC_PROCESS_HELPER_DB", db_path)
        .env("AUDETIC_PROCESS_HELPER_ADDRESS", address.to_string())
        .env("AUDETIC_PROCESS_HELPER_EVENTS", event_log);
    command
}

fn spawn_running_helper(db_path: &Path, address: SocketAddr, event_log: &Path) -> RunningHelper {
    let mut child = helper_command(db_path, address, event_log)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });
    RunningHelper { child, lines }
}

async fn wait_for_helper_ready(helper: &RunningHelper) {
    watchdog("waiting for child SyncService readiness", async {
        loop {
            match helper
                .lines
                .recv_timeout(std::time::Duration::from_millis(100))
            {
                Ok(line) if line.contains("PROCESS_READY") => return,
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("process helper output closed before readiness: {error}"),
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

async fn stop_helper(mut helper: RunningHelper) {
    helper
        .child
        .stdin
        .take()
        .unwrap()
        .write_all(b"stop\n")
        .unwrap();
    let status = watchdog(
        "waiting for child SyncService shutdown",
        tokio::task::spawn_blocking(move || helper.child.wait()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn process_lock_prevents_second_owner_and_fresh_process_reconstructs_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let db_path = temp.path().join("shared.db");
    let event_log = temp.path().join("events.log");
    crate::db::migrate_db_at(&db_path).unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);

    let bootstrap_tailnet = FakeTailnet::default();
    bootstrap_tailnet.add_node("process-helper", "owner@example.com");
    let bootstrap = SyncService::with_runtime_dependencies(
        db_path.clone(),
        bootstrap_tailnet.tailscale("process-helper"),
        HubCapabilities::from_adapter(NetworkHubAdapter::from_transport(
            bootstrap_tailnet.transport("process-helper"),
        )),
        address,
        RuntimeDependencies {
            launcher: bootstrap_tailnet.launcher("process-helper"),
            clock: Arc::new(ManualClock::new()),
            observer: Arc::new(WorkerProbe::default()),
        },
    );
    bootstrap.initialize().await.unwrap();
    bootstrap
        .configure(SyncSetupRequest {
            role: SyncRole::HomeHub,
            device_name: Some("process helper".into()),
            hub: None,
            upload_recording_payloads: false,
            cache_level: CacheLevel::LiveOnly,
            shared_config_enabled: true,
            confirm_serve_change: true,
        })
        .await
        .unwrap();
    bootstrap.shutdown().await.unwrap();

    let connection = crate::db::open_db_at(&db_path).unwrap();
    let (_, record_id) = crate::db::insert_workflow_record(
        &connection,
        &Workflow::new(
            WorkflowType::VoiceToText,
            WorkflowData::VoiceToText(VoiceToTextData {
                text: "process lock pending work".into(),
                audio_path: temp
                    .path()
                    .join("missing.wav")
                    .to_string_lossy()
                    .into_owned(),
            }),
        ),
    )
    .unwrap();
    connection
        .execute(
            "UPDATE sync_outbox_items SET next_attempt_at='2999-01-01T00:00:00Z' WHERE record_id=?1",
            [record_id.to_string()],
        )
        .unwrap();
    drop(connection);

    let first = spawn_running_helper(&db_path, address, &event_log);
    wait_for_helper_ready(&first).await;

    let mut second_command = helper_command(&db_path, address, &event_log);
    second_command.env("AUDETIC_PROCESS_HELPER_EXPECT_LOCKED", "1");
    let second = watchdog(
        "waiting for rejected second process",
        tokio::task::spawn_blocking(move || second_command.output()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(second.status.success());
    assert!(String::from_utf8_lossy(&second.stdout).contains("PROCESS_LOCKED"));
    assert_eq!(
        crate::db::open_db_at(&db_path)
            .unwrap()
            .query_row(
                "SELECT state,attempts,lease_owner FROM sync_outbox_items WHERE record_id=?1",
                [record_id.to_string()],
                |row| Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, Option<String>>(2)?
                )),
            )
            .unwrap(),
        ("pending".into(), 0, None)
    );

    stop_helper(first).await;
    let fresh = spawn_running_helper(&db_path, address, &event_log);
    wait_for_helper_ready(&fresh).await;
    stop_helper(fresh).await;

    let events = std::fs::read_to_string(&event_log).unwrap();
    assert_eq!(events.matches("ListenerStarted").count(), 2);
    assert_eq!(events.matches("ListenerStopped").count(), 2);
    assert_eq!(events.matches("OutboxStarted").count(), 2);
    assert_eq!(events.matches("OutboxStopped").count(), 2);
}
