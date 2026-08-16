#![allow(clippy::arc_with_non_send_sync)]

mod command;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{error, info, warn};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::api::ApiServer;
use crate::audio::device_watcher::{DeviceWatcher, SettledSwitch};
use crate::audio::stream_event::{CaptureSource, StreamDeath, StreamEventSink};
use crate::audio::{
    mic_source::MicAudioSource, system_source::SystemAudioSource, AudioStreamManager,
    BehaviorOptions, RecordingMachine, RecordingPhase, RecordingStatusHandle, ToggleResult,
};
use crate::config::Config;
use crate::meeting::{FfprobeMediaInspector, MediaInspector, MeetingMachine, MeetingStatusHandle};
use crate::post_processing::PostProcessingService;
use crate::text_io::TextIoService;
use crate::transcription::job_service::{
    LocalTranscriptionJobService, RemoteTranscriptionJobService,
};
use crate::transcription::{ProviderConfig, Transcriber, TranscriptionService};
use crate::ui::Indicator;

pub use command::DaemonCommand;

const DEFAULT_JOBS_API_URL: &str = "https://audio.audetic.link/api/v1/jobs";
const MEETING_TRANSCRIPTION_TIMEOUT_SECS: u64 = 7200; // 2 hours

#[async_trait::async_trait]
trait DictationCommandTarget {
    async fn toggle(&self, options: Option<crate::audio::JobOptions>) -> Result<ToggleResult>;
    async fn default_input_switched(&self) -> Result<()>;
    async fn stream_died(&self, death: StreamDeath) -> Result<()>;
}

#[async_trait::async_trait]
impl DictationCommandTarget for RecordingMachine {
    async fn toggle(&self, options: Option<crate::audio::JobOptions>) -> Result<ToggleResult> {
        self.toggle(options).await
    }

    async fn default_input_switched(&self) -> Result<()> {
        self.default_input_switched().await
    }

    async fn stream_died(&self, death: StreamDeath) -> Result<()> {
        self.stream_died(death).await
    }
}

#[async_trait::async_trait(?Send)]
trait MeetingCaptureCommandTarget {
    async fn default_input_switched(&mut self) -> Result<()>;
    async fn default_output_switched(&mut self) -> Result<()>;
    async fn microphone_stream_died(&mut self, death: StreamDeath) -> Result<()>;
    async fn system_stream_died(&mut self, death: StreamDeath) -> Result<()>;
}

#[async_trait::async_trait(?Send)]
impl MeetingCaptureCommandTarget for MeetingMachine {
    async fn default_input_switched(&mut self) -> Result<()> {
        MeetingMachine::default_input_switched(self).await
    }

    async fn microphone_stream_died(&mut self, death: StreamDeath) -> Result<()> {
        MeetingMachine::microphone_stream_died(self, death).await
    }

    async fn default_output_switched(&mut self) -> Result<()> {
        MeetingMachine::default_output_switched(self).await
    }

    async fn system_stream_died(&mut self, death: StreamDeath) -> Result<()> {
        MeetingMachine::system_stream_died(self, death).await
    }
}

fn capture_stream_event_sink(tx: &mpsc::Sender<DaemonCommand>) -> StreamEventSink {
    let tx = tx.downgrade();
    let pending = Arc::new(std::array::from_fn::<_, 3, _>(|_| AtomicU64::new(0)));
    let notify = Arc::new(Notify::new());
    let bridge_tx = tx.clone();
    let bridge_pending = pending.clone();
    let bridge_notify = notify.clone();
    tokio::spawn(async move {
        loop {
            bridge_notify.notified().await;
            for (slot, source) in [
                CaptureSource::Dictation,
                CaptureSource::MeetingMicrophone,
                CaptureSource::SystemTap,
            ]
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
        // Audio callbacks cannot wait for capacity. Coalesce overflow by source;
        // the bridge forwards it when the bounded command queue has room.
        if matches!(
            tx.try_send(DaemonCommand::CaptureStreamDied(death)),
            Err(mpsc::error::TrySendError::Full(_))
        ) {
            let slot = match death.source {
                CaptureSource::Dictation => 0,
                CaptureSource::MeetingMicrophone => 1,
                CaptureSource::SystemTap => 2,
            };
            pending[slot].fetch_max(death.generation.0, Ordering::SeqCst);
            notify.notify_one();
        }
    })
}

async fn handle_dictation_command(target: &impl DictationCommandTarget, command: DaemonCommand) {
    match command {
        DaemonCommand::ToggleRecording(job_options) => match target.toggle(job_options).await {
            Ok(ToggleResult {
                phase: RecordingPhase::Recording,
                job_id,
            }) => {
                info!("Recording started with job_id={:?}", job_id);
            }
            Ok(ToggleResult {
                phase: RecordingPhase::Processing,
                job_id,
            }) => {
                info!(
                    "Recording stopped, processing audio for job_id={:?}",
                    job_id
                );
            }
            Ok(ToggleResult { phase, job_id }) => {
                info!(
                    "RecordingMachine is currently {:?} (job_id={:?})",
                    phase, job_id
                );
            }
            Err(e) => error!("Failed to toggle recording: {}", e),
        },
        DaemonCommand::CaptureStreamDied(death) => {
            if let Err(e) = target.stream_died(death).await {
                error!("Failed to recover dictation from stream death: {e}");
            }
        }
        _ => unreachable!("non-dictation command passed to dictation dispatcher"),
    }
}

async fn handle_default_input_switched(
    dictation: &impl DictationCommandTarget,
    meeting: &mut impl MeetingCaptureCommandTarget,
) {
    let (_, meeting_result) = tokio::join!(
        async {
            if let Err(e) = dictation.default_input_switched().await {
                error!("Failed to switch dictation to the current Default Input: {e}");
            }
        },
        meeting.default_input_switched(),
    );
    if let Err(e) = meeting_result {
        error!("Failed to switch meeting microphone to the current Default Input: {e}");
    }
}

async fn handle_capture_stream_died(
    dictation: &impl DictationCommandTarget,
    meeting: &mut impl MeetingCaptureCommandTarget,
    death: StreamDeath,
) {
    info!(
        event = "audio_stream_death",
        source = ?death.source,
        generation = death.generation.0,
        "Capture stream death received"
    );
    match death.source {
        CaptureSource::Dictation => {
            handle_dictation_command(dictation, DaemonCommand::CaptureStreamDied(death)).await;
        }
        CaptureSource::MeetingMicrophone => {
            if let Err(e) = meeting.microphone_stream_died(death).await {
                error!("Failed to recover meeting microphone from stream death: {e}");
            }
        }
        CaptureSource::SystemTap => {
            if let Err(e) = meeting.system_stream_died(death).await {
                error!("Failed to recover System Tap from stream death: {e}");
            }
        }
    }
}

async fn handle_default_output_switched(meeting: &mut impl MeetingCaptureCommandTarget) {
    if let Err(e) = meeting.default_output_switched().await {
        error!("Failed to switch System Tap to the current Default Output: {e}");
    }
}

async fn handle_settled_switch(
    dictation: &impl DictationCommandTarget,
    meeting: &mut impl MeetingCaptureCommandTarget,
    settled: SettledSwitch,
) {
    info!(
        event = "audio_device_switch_settled",
        input_changed = settled.input_changed,
        output_changed = settled.output_changed,
        "Settled default-device switch"
    );
    if settled.input_changed {
        handle_default_input_switched(dictation, meeting).await;
    }
    if settled.output_changed {
        handle_default_output_switched(meeting).await;
    }
}

pub async fn run_service() -> Result<()> {
    info!("Starting Audetic service");

    // On macOS, fire the Screen Recording TCC prompt early if it isn't
    // already granted. AudioHardwareCreateProcessTap (cpal's loopback path)
    // doesn't auto-prompt reliably, so without this users get silent
    // captures with no UI signal that anything is wrong. The watcher
    // re-exits the daemon when the grant flips so launchd's KeepAlive
    // restarts us with the fresh TCC state — meetings then work without
    // the user ever opening System Settings.
    #[cfg(target_os = "macos")]
    crate::audio::system_source::permissions::spawn_grant_watcher_then_exit(
        std::time::Duration::from_secs(2),
    );

    let config = Config::load()?;

    let (tx, mut rx) = mpsc::channel::<DaemonCommand>(10);
    let watcher_tx = tx.downgrade();
    let _device_watcher = DeviceWatcher::start(move |settled| {
        let watcher_tx = watcher_tx.clone();
        async move {
            let Some(tx) = watcher_tx.upgrade() else {
                return;
            };
            let _ = tx.send(DaemonCommand::SettledDeviceSwitch(settled)).await;
        }
    })
    .context("failed to start Device Watcher")?;
    let stream_event_sink = capture_stream_event_sink(&tx);
    let audio_recorder = Arc::new(Mutex::new(AudioStreamManager::with_event_sink(
        stream_event_sink.clone(),
    )?));

    let whisper = build_transcriber(&config)?;
    let transcription_service = Arc::new(TranscriptionService::new(whisper)?);

    let text_io = TextIoService::new(
        Some(&config.wayland.input_method),
        config.behavior.preserve_clipboard,
    )?;
    let indicator =
        Indicator::from_config(&config.ui).with_audio_feedback(config.behavior.audio_feedback);

    // Post-processing service is shared across both pipelines + the API
    // server. Cheap to clone (zero-sized), so the Arc is only for the
    // explicit `&Arc<...>` shape MeetingMachine/RecordingMachine accept.
    let post_processing = Arc::new(PostProcessingService::new());

    let status_handle = RecordingStatusHandle::default();
    let recording_machine = RecordingMachine::new(
        audio_recorder.clone(),
        transcription_service,
        indicator.clone(),
        text_io,
        BehaviorOptions {
            auto_paste: config.behavior.auto_paste,
            delete_audio_files: config.behavior.delete_audio_files,
        },
        status_handle.clone(),
        Arc::clone(&post_processing),
    );

    // Sweep meetings a previous daemon left mid-pipeline (recording /
    // review / compressing / transcribing) into `error` before anything can
    // accept new work. The machine's state died with the old process, so
    // those rows would otherwise show "transcribing" forever; as `error`
    // they surface the interruption and the retry endpoint can re-submit
    // the audio still on disk. Failure to sweep is non-fatal — worst case
    // the stale rows remain, which is exactly the status quo without it.
    match crate::db::init_db()
        .and_then(|conn| crate::db::meetings::MeetingRepository::sweep_interrupted(&conn))
    {
        Ok(0) => {}
        Ok(n) => warn!(
            "Marked {} meeting(s) interrupted by a previous daemon shutdown as errored; \
             they can be retried from the meetings list",
            n
        ),
        Err(e) => warn!("Failed to sweep interrupted meetings: {e:#}"),
    }

    // Meeting pipeline (independent from recording pipeline). `meetings_dir`,
    // the media inspector, and the post-processing service all live at the
    // app level so the live recording machine and the import endpoint share
    // a single instance — no path drift between recording and imports, and
    // no duplicate dispatch of `meeting.completed` jobs.
    let meeting_status = MeetingStatusHandle::default();
    let meeting_transcription = build_meeting_transcription_service(&config);
    let meetings_dir = resolve_meetings_dir();
    let meeting_inspector: Arc<dyn MediaInspector> = Arc::new(FfprobeMediaInspector);

    let mut meeting_machine = build_meeting_machine(
        indicator,
        meeting_status.clone(),
        meeting_transcription.clone(),
        Arc::clone(&post_processing),
        meetings_dir.clone(),
        stream_event_sink,
    );

    let api_server = ApiServer::new(
        tx,
        status_handle.clone(),
        &config,
        Arc::clone(&post_processing),
    )
    .with_meeting_state(
        meeting_status.clone(),
        meeting_transcription.clone(),
        Arc::clone(&post_processing),
        meeting_inspector,
        meetings_dir.clone(),
    );

    tokio::spawn(async move {
        if let Err(e) = api_server.start().await {
            error!("API server failed: {}", e);
        }
    });

    let toggle_url = crate::api::url::api_url(crate::api::url::paths::TOGGLE);
    let meetings_toggle_url = crate::api::url::api_url(crate::api::url::paths::MEETINGS_TOGGLE);
    info!("Audetic is ready!");
    info!("Add this to your Hyprland config:");
    info!("bindd = SUPER, R, Audetic, exec, curl -X POST {toggle_url}");
    info!("bindd = SUPER SHIFT, R, Audetic Meeting, exec, curl -X POST {meetings_toggle_url}");
    info!("Or test manually: curl -X POST {toggle_url}");

    while let Some(command) = rx.recv().await {
        match command {
            command @ DaemonCommand::ToggleRecording(_) => {
                handle_dictation_command(&recording_machine, command).await;
            }
            DaemonCommand::SettledDeviceSwitch(settled) => {
                handle_settled_switch(&recording_machine, &mut meeting_machine, settled).await;
            }
            DaemonCommand::CaptureStreamDied(death) => {
                handle_capture_stream_died(&recording_machine, &mut meeting_machine, death).await;
            }
            DaemonCommand::MeetingStart { options, reply } => {
                let result = meeting_machine.start(options).await;
                match &result {
                    Ok(r) => info!(
                        "Meeting {} started: {:?} ({})",
                        r.meeting_id,
                        r.audio_path,
                        r.capture_state.as_str()
                    ),
                    Err(e) => error!("Failed to start meeting: {}", e),
                }
                let _ = reply.send(result);
            }
            DaemonCommand::MeetingStop { reply } => {
                let result = meeting_machine.stop().await;
                match &result {
                    Ok(r) => info!("Meeting {} stopped ({}s)", r.meeting_id, r.duration_seconds),
                    Err(e) => error!("Failed to stop meeting: {}", e),
                }
                let _ = reply.send(result);
            }
            DaemonCommand::MeetingCancel { reply } => {
                let result = meeting_machine.cancel().await;
                match &result {
                    Ok(r) => info!(
                        "Meeting {} cancelled ({}s)",
                        r.meeting_id, r.duration_seconds
                    ),
                    Err(e) => error!("Failed to cancel meeting: {}", e),
                }
                let _ = reply.send(result);
            }
            DaemonCommand::MeetingConfirm {
                start_seconds,
                end_seconds,
                reply,
            } => {
                let result = meeting_machine.confirm(start_seconds, end_seconds).await;
                match &result {
                    Ok(r) => info!(
                        "Meeting {} confirmed for transcription ({}s)",
                        r.meeting_id, r.duration_seconds
                    ),
                    Err(e) => error!("Failed to confirm meeting: {}", e),
                }
                let _ = reply.send(result);
            }
            DaemonCommand::MeetingToggle { options, reply } => {
                let result = meeting_machine.toggle(options).await;
                match &result {
                    Ok(outcome) => match outcome {
                        crate::meeting::ToggleOutcome::Started(r) => {
                            info!("Meeting {} started via toggle", r.meeting_id);
                        }
                        crate::meeting::ToggleOutcome::Stopped(r) => {
                            info!(
                                "Meeting {} stopped via toggle ({}s)",
                                r.meeting_id, r.duration_seconds
                            );
                        }
                    },
                    Err(e) => error!("Failed to toggle meeting: {}", e),
                }
                let _ = reply.send(result);
            }
        }
    }

    Ok(())
}

/// Build the transcription service used by the meeting pipeline. Lives at the
/// app level (not inside `build_meeting_machine`) so the API server can hand
/// the same instance to retry endpoints — re-running an old failed meeting
/// shouldn't double up the HTTP client or the timeout config.
fn build_meeting_transcription_service(
    config: &Config,
) -> Arc<dyn crate::transcription::job_service::TranscriptionJobService> {
    // On-device transcription: run the configured local engine directly instead
    // of submitting to the cloud jobs API. Falls back to remote if the local
    // engine can't be constructed (so a misconfigured local provider doesn't
    // wedge the meeting pipeline at startup).
    if config.whisper.provider.as_deref() == Some("local") {
        match build_transcriber(config).and_then(TranscriptionService::new) {
            Ok(service) => {
                info!("Meetings will transcribe on-device (local engine)");
                return Arc::new(LocalTranscriptionJobService::new(service));
            }
            Err(e) => {
                warn!("Failed to build local meeting transcription, falling back to remote: {e:#}")
            }
        }
    }

    let jobs_url = config
        .whisper
        .api_endpoint
        .as_ref()
        .map(|e| {
            if e.ends_with("/transcriptions") {
                e.replace("/transcriptions", "/jobs")
            } else {
                format!("{}/jobs", e.trim_end_matches('/'))
            }
        })
        .unwrap_or_else(|| DEFAULT_JOBS_API_URL.to_string());

    Arc::new(RemoteTranscriptionJobService::new(
        &jobs_url,
        Duration::from_secs(MEETING_TRANSCRIPTION_TIMEOUT_SECS),
    ))
}

fn build_meeting_machine(
    indicator: Indicator,
    status: MeetingStatusHandle,
    transcription: Arc<dyn crate::transcription::job_service::TranscriptionJobService>,
    post_processing: Arc<PostProcessingService>,
    meetings_dir: std::path::PathBuf,
    stream_event_sink: StreamEventSink,
) -> MeetingMachine {
    let mic_source: Box<dyn crate::audio::audio_source::MeetingMicSource> = Box::new(
        MicAudioSource::with_event_sink(16000, stream_event_sink.clone()),
    );

    let system_source = Box::new(SystemAudioSource::with_event_sink(16000, stream_event_sink));

    MeetingMachine::new(
        mic_source,
        system_source,
        transcription,
        post_processing,
        indicator,
        status,
        meetings_dir,
    )
}

/// Resolve the durable meetings directory used for both live recordings
/// and imported files. Falls back to `/tmp/audetic/meetings` if `dirs`
/// can't find a data dir (e.g. degraded container env), matching what
/// `MeetingMachine` did inline before this was hoisted.
fn resolve_meetings_dir() -> std::path::PathBuf {
    crate::global::data_dir()
        .map(|d| d.join("meetings"))
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/audetic/meetings"))
}

fn build_transcriber(config: &Config) -> Result<Transcriber> {
    let provider = config
        .whisper
        .provider
        .as_deref()
        .ok_or_else(|| anyhow!("No transcription provider configured. Set [whisper].provider in ~/.config/audetic/config.toml"))?;

    let provider_config = ProviderConfig {
        model: config.whisper.model.clone(),
        model_path: config.whisper.model_path.clone(),
        language: config.whisper.language.clone(),
        command_path: config.whisper.command_path.clone(),
        api_endpoint: config.whisper.api_endpoint.clone(),
        api_key: config.whisper.api_key.clone(),
    };

    Transcriber::with_provider(provider, provider_config)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    use crate::audio::device_watcher::{
        ActiveDeviceWatcher, DeviceWatcherBackend, RawDeviceSwitch, RawSwitchSink,
    };
    use crate::audio::input_device::{
        ActiveInput, CaptureBackend, InputDataCallback, InputErrorCallback,
    };
    use crate::audio::stream_event::{CaptureSource, StreamGeneration};

    use super::*;

    struct FakeDefaultInput {
        sample_rate: u32,
        samples: Vec<f32>,
    }

    struct FakeCaptureBackend {
        defaults: StdMutex<VecDeque<FakeDefaultInput>>,
        starts: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
        errors: Arc<StdMutex<Vec<InputErrorCallback>>>,
    }

    struct FakeStream {
        drops: Arc<AtomicUsize>,
    }

    #[derive(Clone, Default)]
    struct FakeWatcherHandle {
        sink: Arc<StdMutex<Option<RawSwitchSink>>>,
    }

    impl FakeWatcherHandle {
        fn emit(&self, event: RawDeviceSwitch) {
            self.sink.lock().unwrap().as_ref().unwrap()(event);
        }
    }

    struct FakeWatcherBackend(FakeWatcherHandle);

    struct FakeActiveWatcher;

    impl ActiveDeviceWatcher for FakeActiveWatcher {}

    impl DeviceWatcherBackend for FakeWatcherBackend {
        fn start(self: Box<Self>, sink: RawSwitchSink) -> Result<Box<dyn ActiveDeviceWatcher>> {
            *self.0.sink.lock().unwrap() = Some(sink);
            Ok(Box::new(FakeActiveWatcher))
        }
    }

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl CaptureBackend for FakeCaptureBackend {
        fn start_default_input(
            &self,
            mut on_data: InputDataCallback,
            on_error: InputErrorCallback,
        ) -> Result<ActiveInput> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let input =
                self.defaults.lock().unwrap().pop_front().ok_or_else(|| {
                    anyhow!("Default Input is unavailable for planned replacement")
                })?;
            on_data(&input.samples, 1);
            self.errors.lock().unwrap().push(on_error);
            Ok(ActiveInput::new(
                input.sample_rate,
                FakeStream {
                    drops: self.drops.clone(),
                },
            ))
        }
    }

    struct TestDictationTarget {
        audio: AudioStreamManager,
        recording: StdMutex<bool>,
        status: RecordingStatusHandle,
        output_path: PathBuf,
    }

    #[async_trait::async_trait]
    impl DictationCommandTarget for TestDictationTarget {
        async fn toggle(&self, _options: Option<crate::audio::JobOptions>) -> Result<ToggleResult> {
            let recording = *self.recording.lock().unwrap();
            if recording {
                self.status.set_processing().await;
                self.audio.stop_recording(self.output_path.clone()).await?;
                *self.recording.lock().unwrap() = false;
                Ok(ToggleResult {
                    phase: RecordingPhase::Processing,
                    job_id: None,
                })
            } else {
                self.audio.start_recording().await?;
                *self.recording.lock().unwrap() = true;
                self.status
                    .start_job("test-job".to_string(), crate::audio::JobOptions::default())
                    .await;
                Ok(ToggleResult {
                    phase: RecordingPhase::Recording,
                    job_id: None,
                })
            }
        }

        async fn default_input_switched(&self) -> Result<()> {
            let recovery = self.audio.default_input_switched().await?;
            self.status.apply_capture_recovery(recovery).await;
            Ok(())
        }

        async fn stream_died(&self, death: StreamDeath) -> Result<()> {
            let recovery = self.audio.stream_died(death).await?;
            self.status.apply_capture_recovery(recovery).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn command_loop_preserves_active_dictation_across_default_input_switch() {
        let starts = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let errors = Arc::new(StdMutex::new(Vec::new()));
        let backend = FakeCaptureBackend {
            defaults: StdMutex::new(VecDeque::from([
                FakeDefaultInput {
                    sample_rate: 48_000,
                    samples: vec![0.25; 480],
                },
                FakeDefaultInput {
                    sample_rate: 44_100,
                    samples: vec![-0.5; 441],
                },
            ])),
            starts: starts.clone(),
            drops: drops.clone(),
            errors,
        };
        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("switched-default.wav");
        let target = TestDictationTarget {
            audio: AudioStreamManager::with_backend(Box::new(backend), Arc::new(|_| {})),
            recording: StdMutex::new(false),
            status: RecordingStatusHandle::default(),
            output_path: output_path.clone(),
        };
        let (tx, mut rx) = mpsc::channel(10);

        for command in [
            DaemonCommand::ToggleRecording(None),
            DaemonCommand::SettledDeviceSwitch(SettledSwitch {
                input_changed: true,
                output_changed: false,
            }),
            DaemonCommand::ToggleRecording(None),
            DaemonCommand::SettledDeviceSwitch(SettledSwitch {
                input_changed: true,
                output_changed: false,
            }),
        ] {
            tx.send(command).await.unwrap();
        }
        drop(tx);

        while let Some(command) = rx.recv().await {
            match command {
                DaemonCommand::SettledDeviceSwitch(settled) => {
                    if settled.input_changed {
                        target.default_input_switched().await.unwrap();
                    }
                }
                command => handle_dictation_command(&target, command).await,
            }
        }

        let samples = hound::WavReader::open(output_path)
            .unwrap()
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(samples.len(), 320);
        assert!(samples[..160].iter().all(|sample| *sample > 0.0));
        assert!(samples[160..].iter().all(|sample| *sample < 0.0));
        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn watcher_burst_replaces_active_capture_once_and_leaves_idle_capture_closed() {
        let starts = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let backend = FakeCaptureBackend {
            defaults: StdMutex::new(VecDeque::from([
                FakeDefaultInput {
                    sample_rate: 48_000,
                    samples: vec![0.25; 480],
                },
                FakeDefaultInput {
                    sample_rate: 44_100,
                    samples: vec![-0.5; 441],
                },
            ])),
            starts: starts.clone(),
            drops: drops.clone(),
            errors: Arc::new(StdMutex::new(Vec::new())),
        };
        let output_dir = tempfile::tempdir().unwrap();
        let target = TestDictationTarget {
            audio: AudioStreamManager::with_backend(Box::new(backend), Arc::new(|_| {})),
            recording: StdMutex::new(false),
            status: RecordingStatusHandle::default(),
            output_path: output_dir.path().join("watcher-switched-default.wav"),
        };
        let watcher_backend = FakeWatcherHandle::default();
        let (tx, mut rx) = mpsc::channel(10);
        let settled_tx = tx.clone();
        let _watcher = DeviceWatcher::with_backend(
            Box::new(FakeWatcherBackend(watcher_backend.clone())),
            move |settled| {
                let settled_tx = settled_tx.clone();
                async move {
                    let _ = settled_tx
                        .send(DaemonCommand::SettledDeviceSwitch(settled))
                        .await;
                }
            },
        )
        .unwrap();
        let mut meeting = RoutingMeetingTarget::default();

        assert_eq!(starts.load(Ordering::SeqCst), 0);
        tx.send(DaemonCommand::ToggleRecording(None)).await.unwrap();
        handle_dictation_command(&target, rx.recv().await.unwrap()).await;

        watcher_backend.emit(RawDeviceSwitch::Input);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(400)).await;
        watcher_backend.emit(RawDeviceSwitch::Input);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
        let settled = match rx.recv().await.unwrap() {
            DaemonCommand::SettledDeviceSwitch(settled) => settled,
            _ => unreachable!("expected settled switch"),
        };
        handle_settled_switch(&target, &mut meeting, settled).await;
        assert!(rx.try_recv().is_err());
        assert_eq!(starts.load(Ordering::SeqCst), 2);

        tx.send(DaemonCommand::ToggleRecording(None)).await.unwrap();
        handle_dictation_command(&target, rx.recv().await.unwrap()).await;
        watcher_backend.emit(RawDeviceSwitch::Input);
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
        let settled = match rx.recv().await.unwrap() {
            DaemonCommand::SettledDeviceSwitch(settled) => settled,
            _ => unreachable!("expected settled switch"),
        };
        handle_settled_switch(&target, &mut meeting, settled).await;

        assert_eq!(starts.load(Ordering::SeqCst), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
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
            source: CaptureSource::MeetingMicrophone,
            generation: StreamGeneration(2),
        };

        sink(death);
        sink(StreamDeath {
            source: CaptureSource::MeetingMicrophone,
            generation: StreamGeneration(1),
        });
        assert!(matches!(
            rx.try_recv(),
            Ok(DaemonCommand::SettledDeviceSwitch(SettledSwitch {
                input_changed: true,
                output_changed: false,
            }))
        ));

        let received = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("overflow bridge timed out")
            .expect("command queue closed");
        assert!(matches!(
            received,
            DaemonCommand::CaptureStreamDied(received) if received == death
        ));

        drop(rx);
        sink(death);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_queued_behind_stream_recovery_preserves_audio_without_post_stop_swap() {
        let (tx, mut rx) = mpsc::channel(10);
        let starts = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let errors = Arc::new(StdMutex::new(Vec::new()));
        let backend = FakeCaptureBackend {
            defaults: StdMutex::new(VecDeque::from([FakeDefaultInput {
                sample_rate: 48_000,
                samples: vec![0.25; 480],
            }])),
            starts: starts.clone(),
            drops: drops.clone(),
            errors: errors.clone(),
        };
        let output_dir = tempfile::tempdir().unwrap();
        let output_path = output_dir.path().join("stopped-after-recovery.wav");
        let status = RecordingStatusHandle::default();
        let target = TestDictationTarget {
            audio: AudioStreamManager::with_backend(
                Box::new(backend),
                capture_stream_event_sink(&tx),
            ),
            recording: StdMutex::new(false),
            status: status.clone(),
            output_path: output_path.clone(),
        };

        tx.send(DaemonCommand::ToggleRecording(None)).await.unwrap();
        handle_dictation_command(&target, rx.recv().await.unwrap()).await;
        errors.lock().unwrap()[0]();
        errors.lock().unwrap()[0]();
        tx.send(DaemonCommand::ToggleRecording(None)).await.unwrap();
        tx.send(DaemonCommand::SettledDeviceSwitch(SettledSwitch {
            input_changed: true,
            output_changed: false,
        }))
        .await
        .unwrap();
        drop(tx);

        handle_dictation_command(&target, rx.recv().await.unwrap()).await;
        assert!(status.get().await.capture_degraded);

        handle_dictation_command(&target, rx.recv().await.unwrap()).await;
        assert!(status.get().await.capture_degraded);
        assert_eq!(starts.load(Ordering::SeqCst), 4);

        handle_dictation_command(&target, rx.recv().await.unwrap()).await;
        assert_eq!(status.get().await.phase, RecordingPhase::Processing);
        assert!(!status.get().await.capture_degraded);

        let settled = match rx.recv().await.unwrap() {
            DaemonCommand::SettledDeviceSwitch(settled) => settled,
            _ => unreachable!("expected settled switch"),
        };
        if settled.input_changed {
            target.default_input_switched().await.unwrap();
        }
        assert!(rx.recv().await.is_none());

        let samples = hound::WavReader::open(output_path)
            .unwrap()
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(samples.len(), 160);
        assert!(samples.iter().all(|sample| *sample > 0.0));
        assert_eq!(starts.load(Ordering::SeqCst), 4);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    struct RoutingDictationTarget {
        switches: AtomicUsize,
        deaths: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl DictationCommandTarget for RoutingDictationTarget {
        async fn toggle(&self, _options: Option<crate::audio::JobOptions>) -> Result<ToggleResult> {
            unreachable!("routing test does not toggle dictation")
        }

        async fn default_input_switched(&self) -> Result<()> {
            self.switches.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn stream_died(&self, _death: StreamDeath) -> Result<()> {
            self.deaths.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Default)]
    struct RoutingMeetingTarget {
        input_switches: usize,
        output_switches: usize,
        microphone_deaths: usize,
        system_deaths: usize,
    }

    struct SlowDictationTarget;

    #[async_trait::async_trait]
    impl DictationCommandTarget for SlowDictationTarget {
        async fn toggle(&self, _options: Option<crate::audio::JobOptions>) -> Result<ToggleResult> {
            unreachable!("fan-out timing test does not toggle dictation")
        }

        async fn default_input_switched(&self) -> Result<()> {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(())
        }

        async fn stream_died(&self, _death: StreamDeath) -> Result<()> {
            unreachable!("fan-out timing test does not inject stream death")
        }
    }

    #[derive(Default)]
    struct TimedMeetingTarget {
        switched_at: Option<tokio::time::Instant>,
    }

    #[async_trait::async_trait(?Send)]
    impl MeetingCaptureCommandTarget for TimedMeetingTarget {
        async fn default_input_switched(&mut self) -> Result<()> {
            self.switched_at = Some(tokio::time::Instant::now());
            Ok(())
        }

        async fn microphone_stream_died(&mut self, _death: StreamDeath) -> Result<()> {
            unreachable!("fan-out timing test does not inject stream death")
        }

        async fn default_output_switched(&mut self) -> Result<()> {
            unreachable!("fan-out timing test does not inject output switch")
        }

        async fn system_stream_died(&mut self, _death: StreamDeath) -> Result<()> {
            unreachable!("fan-out timing test does not inject System Tap death")
        }
    }

    #[async_trait::async_trait(?Send)]
    impl MeetingCaptureCommandTarget for RoutingMeetingTarget {
        async fn default_input_switched(&mut self) -> Result<()> {
            self.input_switches += 1;
            Ok(())
        }

        async fn microphone_stream_died(&mut self, _death: StreamDeath) -> Result<()> {
            self.microphone_deaths += 1;
            Ok(())
        }

        async fn default_output_switched(&mut self) -> Result<()> {
            self.output_switches += 1;
            Ok(())
        }

        async fn system_stream_died(&mut self, _death: StreamDeath) -> Result<()> {
            self.system_deaths += 1;
            Ok(())
        }
    }

    #[tokio::test]
    async fn settled_switch_routes_directions_and_stream_deaths_by_source() {
        let dictation = RoutingDictationTarget {
            switches: AtomicUsize::new(0),
            deaths: AtomicUsize::new(0),
        };
        let mut meeting = RoutingMeetingTarget::default();

        handle_settled_switch(
            &dictation,
            &mut meeting,
            SettledSwitch {
                input_changed: true,
                output_changed: false,
            },
        )
        .await;
        assert_eq!(dictation.switches.load(Ordering::SeqCst), 1);
        assert_eq!(meeting.input_switches, 1);
        assert_eq!(meeting.output_switches, 0);

        handle_settled_switch(
            &dictation,
            &mut meeting,
            SettledSwitch {
                input_changed: false,
                output_changed: true,
            },
        )
        .await;
        assert_eq!(dictation.switches.load(Ordering::SeqCst), 1);
        assert_eq!(meeting.input_switches, 1);
        assert_eq!(meeting.output_switches, 1);

        handle_settled_switch(
            &dictation,
            &mut meeting,
            SettledSwitch {
                input_changed: true,
                output_changed: true,
            },
        )
        .await;
        for source in [
            CaptureSource::Dictation,
            CaptureSource::MeetingMicrophone,
            CaptureSource::SystemTap,
        ] {
            handle_capture_stream_died(
                &dictation,
                &mut meeting,
                StreamDeath {
                    source,
                    generation: StreamGeneration(1),
                },
            )
            .await;
        }

        assert_eq!(dictation.switches.load(Ordering::SeqCst), 2);
        assert_eq!(meeting.input_switches, 2);
        assert_eq!(meeting.output_switches, 2);
        assert_eq!(dictation.deaths.load(Ordering::SeqCst), 1);
        assert_eq!(meeting.microphone_deaths, 1);
        assert_eq!(meeting.system_deaths, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_dictation_recovery_does_not_delay_meeting_switch() {
        let origin = tokio::time::Instant::now();
        let mut meeting = TimedMeetingTarget::default();

        handle_default_input_switched(&SlowDictationTarget, &mut meeting).await;

        assert_eq!(
            meeting.switched_at.unwrap().duration_since(origin),
            Duration::ZERO
        );
    }
}
