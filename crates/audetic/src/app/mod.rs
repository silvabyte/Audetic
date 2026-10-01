//! Single daemon owner of Audio Note capture. Processing outlives capture and
//! never prevents opening the next microphone session.
#![allow(clippy::arc_with_non_send_sync)]

mod command;
pub use command::DaemonCommand;

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};
use tracing::{error, info};

use crate::api::ApiServer;
use crate::audio::device_watcher::{DeviceWatcher, SettledSwitch};
use crate::audio::stream_event::{CaptureSource, StreamDeath, StreamEventSink};
use crate::audio::{mic_source::MicAudioSource, system_source::SystemAudioSource};
use crate::audio_notes::{
    AudioNoteMachine, AudioNoteStatusHandle, FfprobeMediaInspector, ProcessingServices,
};
use crate::config::Config;
use crate::transcription::job_service::{
    LocalTranscriptionJobService, RemoteTranscriptionJobService, TranscriptionJobService,
};
use crate::transcription::{ProviderConfig, Transcriber, TranscriptionService};
use crate::ui::Indicator;

#[async_trait::async_trait(?Send)]
trait CaptureCommandTarget {
    async fn default_input_switched(&mut self) -> Result<()>;
    async fn default_output_switched(&mut self) -> Result<()>;
    async fn microphone_stream_died(&mut self, death: StreamDeath) -> Result<()>;
    async fn system_stream_died(&mut self, death: StreamDeath) -> Result<()>;
}

#[async_trait::async_trait(?Send)]
impl CaptureCommandTarget for AudioNoteMachine {
    async fn default_input_switched(&mut self) -> Result<()> {
        AudioNoteMachine::default_input_switched(self).await
    }
    async fn default_output_switched(&mut self) -> Result<()> {
        AudioNoteMachine::default_output_switched(self).await
    }
    async fn microphone_stream_died(&mut self, death: StreamDeath) -> Result<()> {
        AudioNoteMachine::microphone_stream_died(self, death).await
    }
    async fn system_stream_died(&mut self, death: StreamDeath) -> Result<()> {
        AudioNoteMachine::system_stream_died(self, death).await
    }
}

fn capture_stream_event_sink(tx: &mpsc::Sender<DaemonCommand>) -> StreamEventSink {
    let tx = tx.downgrade();
    let pending = Arc::new(std::array::from_fn::<_, 2, _>(|_| AtomicU64::new(0)));
    let notify = Arc::new(Notify::new());
    let bridge_tx = tx.clone();
    let bridge_pending = pending.clone();
    let bridge_notify = notify.clone();
    tokio::spawn(async move {
        loop {
            bridge_notify.notified().await;
            for (slot, source) in [CaptureSource::Microphone, CaptureSource::SystemTap]
                .into_iter()
                .enumerate()
            {
                let generation = bridge_pending[slot].swap(0, Ordering::SeqCst);
                if generation == 0 {
                    continue;
                }
                let Some(tx) = bridge_tx.upgrade() else {
                    return;
                };
                if tx
                    .send(DaemonCommand::CaptureStreamDied(StreamDeath {
                        source,
                        generation: generation.into(),
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    });
    Arc::new(move |death| {
        let Some(tx) = tx.upgrade() else {
            return;
        };
        if matches!(
            tx.try_send(DaemonCommand::CaptureStreamDied(death)),
            Err(mpsc::error::TrySendError::Full(_))
        ) {
            let slot = match death.source {
                CaptureSource::Microphone => 0,
                CaptureSource::SystemTap => 1,
            };
            pending[slot].fetch_max(death.generation.0, Ordering::SeqCst);
            notify.notify_one();
        }
    })
}

async fn handle_settled_switch(capture: &mut impl CaptureCommandTarget, settled: SettledSwitch) {
    if settled.input_changed {
        if let Err(error) = capture.default_input_switched().await {
            error!("Default Input recovery failed: {error:#}");
        }
    }
    if settled.output_changed {
        if let Err(error) = capture.default_output_switched().await {
            error!("Default Output recovery failed: {error:#}");
        }
    }
}

async fn handle_capture_stream_died(capture: &mut impl CaptureCommandTarget, death: StreamDeath) {
    let result = match death.source {
        CaptureSource::Microphone => capture.microphone_stream_died(death).await,
        CaptureSource::SystemTap => capture.system_stream_died(death).await,
    };
    if let Err(error) = result {
        error!("Capture recovery failed: {error:#}");
    }
}

pub async fn run_service() -> Result<()> {
    audetic_core::url::port()?;
    // Refuse legacy schemas before opening devices, requesting permissions, or
    // accepting HTTP requests. The error contains offline migration instructions.
    let db_path = crate::global::db_file()?;
    let _instance_lock = acquire_instance_lock(&db_path)?;
    let api_listener =
        tokio::net::TcpListener::bind((audetic_core::url::HOST, audetic_core::url::port()?))
            .await
            .context("The Audetic API port is already in use; another daemon may be running")?;
    let ingress_listener =
        tokio::net::TcpListener::bind(crate::integrations::ingress::ingress_bind_addr()?)
            .await
            .context("The Audetic ingress port is already in use")?;
    {
        let conn = crate::db::init_db_at(&db_path)?;
        crate::db::agent_profiles::AgentProfileRepository::ensure_builtin_profiles(&conn)?;
        crate::db::audio_notes::AudioNoteRepository::sweep_interrupted(&conn)?;
        crate::db::audio_notes::AudioNoteRepository::sweep_interrupted_enrichment(&conn)?;
        crate::db::audio_note_artifacts::AudioNoteArtifactRepository::sweep_interrupted(&conn)?;
    }
    // CoreAudio's process tap does not reliably show the Screen Recording
    // prompt itself. Keep the permission/grant restart path for system-audio
    // capture, after the database upgrade check has succeeded.
    #[cfg(target_os = "macos")]
    crate::audio::system_source::permissions::spawn_grant_watcher_then_exit(
        std::time::Duration::from_secs(2),
    );
    let config = Config::load()?;
    let transcription = build_transcription_service(&config)?;
    let audio_notes_dir = crate::global::data_dir()?.join("audio-notes");
    std::fs::create_dir_all(&audio_notes_dir)?;
    let (tx, mut rx) = mpsc::channel::<DaemonCommand>(10);
    let watcher_tx = tx.downgrade();
    let _watcher = DeviceWatcher::start(move |settled| {
        let watcher_tx = watcher_tx.clone();
        async move {
            if let Some(tx) = watcher_tx.upgrade() {
                let _ = tx.send(DaemonCommand::SettledDeviceSwitch(settled)).await;
            }
        }
    })
    .context("Failed to start Device Watcher")?;
    let sink = capture_stream_event_sink(&tx);
    let status = AudioNoteStatusHandle::default();
    let indicator =
        Indicator::from_config(&config.ui).with_audio_feedback(config.behavior.audio_feedback);
    let mut capture = AudioNoteMachine::new(
        Box::new(MicAudioSource::with_event_sink(16000, sink.clone())),
        Box::new(SystemAudioSource::with_event_sink(16000, sink)),
        transcription.clone(),
        indicator,
        status.clone(),
        audio_notes_dir.clone(),
        db_path.clone(),
    );
    let processing_services = ProcessingServices::new(transcription.clone(), db_path.clone());
    let inspector = Arc::new(FfprobeMediaInspector);
    let state = crate::api::routes::audio_notes::AudioNoteState {
        config_path: crate::global::config_file()?,
        tx,
        status,
        transcription: transcription.clone(),
        services: processing_services.clone(),
        inspector: inspector.clone(),
        audio_notes_dir: audio_notes_dir.clone(),
    };
    let integration_service = crate::integrations::IntegrationService::new(
        db_path.clone(),
        audio_notes_dir,
        processing_services,
        inspector,
    );
    integration_service.recover_interrupted_work().await?;
    integration_service.spawn_plaud_scheduler();
    let server = ApiServer::new(
        state,
        &config,
        Arc::new(crate::post_processing::PostProcessingService::new(db_path)),
        integration_service.clone(),
    );
    tokio::spawn(async move {
        if let Err(error) = server.start(api_listener).await {
            error!("API server failed: {error:#}");
        }
    });
    let ingress = crate::integrations::ingress::IngressServer::new(integration_service);
    tokio::spawn(async move {
        if let Err(error) = ingress.serve(ingress_listener).await {
            error!("External audio ingress server failed: {error:#}");
        }
    });
    info!("Audetic Audio Notes ready");
    while let Some(command) = rx.recv().await {
        match command {
            DaemonCommand::SettledDeviceSwitch(settled) => {
                handle_settled_switch(&mut capture, settled).await
            }
            DaemonCommand::CaptureStreamDied(death) => {
                handle_capture_stream_died(&mut capture, death).await
            }
            DaemonCommand::AudioNoteStart { options, reply } => {
                let _ = reply.send(capture.start(options).await);
            }
            DaemonCommand::AudioNoteStop { reply } => {
                let _ = reply.send(capture.stop().await);
            }
            DaemonCommand::AudioNoteConfirm {
                start_seconds,
                end_seconds,
                reply,
            } => {
                let _ = reply.send(capture.confirm(start_seconds, end_seconds).await);
            }
            DaemonCommand::AudioNoteCancel { reply } => {
                let _ = reply.send(capture.cancel().await);
            }
            DaemonCommand::AudioNoteToggle { options, reply } => {
                let _ = reply.send(capture.toggle(options).await);
            }
        }
    }
    Ok(())
}

fn acquire_instance_lock(db_path: &Path) -> Result<File> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = db_path.with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("Failed to open daemon lock at {}", lock_path.display()))?;
    FileExt::try_lock_exclusive(&lock)
        .context("Another Audetic daemon is already using this data directory")?;
    Ok(lock)
}

/// The adapter runs the configured provider, not a silent cloud fallback. It
/// supports local engines as well as synchronous hosted/OpenAI-compatible APIs.
fn build_transcription_service(config: &Config) -> Result<Arc<dyn TranscriptionJobService>> {
    if config.whisper.provider.as_deref() == Some("audetic-api") {
        let endpoint = config
            .whisper
            .api_endpoint
            .as_deref()
            .unwrap_or("https://audio.audetic.link/api/v1/transcriptions");
        let jobs_url = if let Some(base) = endpoint
            .trim_end_matches('/')
            .strip_suffix("/transcriptions")
        {
            format!("{base}/jobs")
        } else if endpoint.trim_end_matches('/').ends_with("/jobs") {
            endpoint.trim_end_matches('/').to_string()
        } else {
            format!("{}/jobs", endpoint.trim_end_matches('/'))
        };
        return Ok(Arc::new(ConfiguredJobService {
            inner: RemoteTranscriptionJobService::new(
                &jobs_url,
                std::time::Duration::from_secs(7200),
            ),
            language: config.whisper.language.clone(),
        }));
    }
    Ok(Arc::new(LocalTranscriptionJobService::new(
        TranscriptionService::new(build_transcriber(config)?)?,
    )))
}

struct ConfiguredJobService {
    inner: RemoteTranscriptionJobService,
    language: Option<String>,
}

#[async_trait::async_trait]
impl TranscriptionJobService for ConfiguredJobService {
    async fn submit_and_poll(
        &self,
        path: &std::path::Path,
        language: Option<&str>,
    ) -> Result<crate::transcription::job_service::TranscriptionJobResult> {
        self.inner
            .submit_and_poll(path, language.or(self.language.as_deref()))
            .await
    }
}

fn build_transcriber(config: &Config) -> Result<Transcriber> {
    let provider = config.whisper.provider.as_deref().ok_or_else(|| {
        anyhow!("No transcription provider configured. Set [whisper].provider in config.toml")
    })?;
    Transcriber::with_provider(
        provider,
        ProviderConfig {
            model: config.whisper.model.clone(),
            model_path: config.whisper.model_path.clone(),
            language: config.whisper.language.clone(),
            command_path: config.whisper.command_path.clone(),
            api_endpoint: config.whisper.api_endpoint.clone(),
            api_key: config.whisper.api_key.clone(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::stream_event::StreamGeneration;
    use std::time::Duration;

    #[derive(Default)]
    struct RoutingCapture {
        input: usize,
        output: usize,
        microphone_deaths: usize,
        system_deaths: usize,
    }
    #[async_trait::async_trait(?Send)]
    impl CaptureCommandTarget for RoutingCapture {
        async fn default_input_switched(&mut self) -> Result<()> {
            self.input += 1;
            Ok(())
        }
        async fn default_output_switched(&mut self) -> Result<()> {
            self.output += 1;
            Ok(())
        }
        async fn microphone_stream_died(&mut self, _: StreamDeath) -> Result<()> {
            self.microphone_deaths += 1;
            Ok(())
        }
        async fn system_stream_died(&mut self, _: StreamDeath) -> Result<()> {
            self.system_deaths += 1;
            Ok(())
        }
    }

    #[tokio::test]
    async fn settled_switch_routes_directions_and_stream_deaths_by_source() {
        let mut capture = RoutingCapture::default();
        handle_settled_switch(
            &mut capture,
            SettledSwitch {
                input_changed: true,
                output_changed: false,
            },
        )
        .await;
        assert_eq!((capture.input, capture.output), (1, 0));
        handle_settled_switch(
            &mut capture,
            SettledSwitch {
                input_changed: false,
                output_changed: true,
            },
        )
        .await;
        assert_eq!((capture.input, capture.output), (1, 1));
        handle_settled_switch(
            &mut capture,
            SettledSwitch {
                input_changed: true,
                output_changed: true,
            },
        )
        .await;
        for source in [CaptureSource::Microphone, CaptureSource::SystemTap] {
            handle_capture_stream_died(
                &mut capture,
                StreamDeath {
                    source,
                    generation: StreamGeneration(1),
                },
            )
            .await;
        }
        assert_eq!(
            (
                capture.input,
                capture.output,
                capture.microphone_deaths,
                capture.system_deaths
            ),
            (2, 2, 1, 1)
        );
    }

    #[tokio::test]
    async fn stream_death_sink_forwards_reports_after_a_full_queue_drains() {
        let (tx, mut rx) = mpsc::channel(1);
        let sink = capture_stream_event_sink(&tx);
        tx.try_send(DaemonCommand::SettledDeviceSwitch(SettledSwitch {
            input_changed: true,
            output_changed: false,
        }))
        .unwrap();
        let death = StreamDeath {
            source: CaptureSource::Microphone,
            generation: StreamGeneration(2),
        };
        sink(death);
        sink(StreamDeath {
            source: CaptureSource::Microphone,
            generation: StreamGeneration(1),
        });
        assert!(matches!(
            rx.try_recv(),
            Ok(DaemonCommand::SettledDeviceSwitch(_))
        ));
        let received = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(received, DaemonCommand::CaptureStreamDied(received) if received == death)
        );
        drop(rx);
        sink(death);
    }

    #[test]
    fn invalid_provider_does_not_silently_upload_to_cloud() {
        let mut config = Config::default();
        config.whisper.provider = Some("not-a-provider".to_string());
        assert!(build_transcription_service(&config).is_err());
    }

    #[test]
    fn database_lock_rejects_a_second_daemon() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("audetic.db");
        let first = acquire_instance_lock(&db_path).unwrap();
        assert!(acquire_instance_lock(&db_path).is_err());
        drop(first);
        assert!(acquire_instance_lock(&db_path).is_ok());
    }
}
