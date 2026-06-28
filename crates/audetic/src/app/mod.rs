#![allow(clippy::arc_with_non_send_sync)]

use crate::api::{ApiCommand, ApiServer};
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
use crate::update::{UpdateConfig, UpdateEngine};
use anyhow::{anyhow, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

const DEFAULT_JOBS_API_URL: &str = "https://audio.audetic.link/api/v1/jobs";
const MEETING_TRANSCRIPTION_TIMEOUT_SECS: u64 = 7200; // 2 hours

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

    let (tx, mut rx) = mpsc::channel::<ApiCommand>(10);
    let audio_recorder = Arc::new(Mutex::new(AudioStreamManager::new()?));

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

    spawn_update_manager();

    let toggle_url = crate::api::url::api_url(crate::api::url::paths::TOGGLE);
    let meetings_toggle_url = crate::api::url::api_url(crate::api::url::paths::MEETINGS_TOGGLE);
    info!("Audetic is ready!");
    info!("Add this to your Hyprland config:");
    info!("bindd = SUPER, R, Audetic, exec, curl -X POST {toggle_url}");
    info!("bindd = SUPER SHIFT, R, Audetic Meeting, exec, curl -X POST {meetings_toggle_url}");
    info!("Or test manually: curl -X POST {toggle_url}");

    while let Some(command) = rx.recv().await {
        match command {
            ApiCommand::ToggleRecording(job_options) => {
                match recording_machine.toggle(job_options).await {
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
                }
            }
            ApiCommand::MeetingStart { options, reply } => {
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
            ApiCommand::MeetingStop { reply } => {
                let result = meeting_machine.stop().await;
                match &result {
                    Ok(r) => info!("Meeting {} stopped ({}s)", r.meeting_id, r.duration_seconds),
                    Err(e) => error!("Failed to stop meeting: {}", e),
                }
                let _ = reply.send(result);
            }
            ApiCommand::MeetingCancel { reply } => {
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
            ApiCommand::MeetingConfirm {
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
            ApiCommand::MeetingToggle { options, reply } => {
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
) -> MeetingMachine {
    let mic_source = MicAudioSource::new(16000)
        .map(|s| Box::new(s) as Box<dyn crate::audio::audio_source::AudioSource>)
        .unwrap_or_else(|e| {
            warn!(
                "Failed to create meeting mic source: {}. Using fallback.",
                e
            );
            Box::new(NullAudioSource)
        });

    let system_source = Box::new(SystemAudioSource::new(16000));

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

/// Fallback audio source that produces no samples (for when mic init fails).
struct NullAudioSource;

impl crate::audio::audio_source::AudioSource for NullAudioSource {
    fn start(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    fn stop(&mut self) -> anyhow::Result<Vec<f32>> {
        Ok(Vec::new())
    }
    fn is_active(&self) -> bool {
        false
    }
    fn sample_rate(&self) -> u32 {
        16000
    }
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

fn spawn_update_manager() {
    match UpdateConfig::detect(None)
        .and_then(UpdateEngine::new)
        .map(|engine| engine.spawn_background(None))
    {
        Ok(Some(_handle)) => info!("Auto-update manager running in background"),
        Ok(None) => info!("Auto-update manager not started (disabled or unsupported)"),
        Err(err) => warn!("Failed to initialize auto-update manager: {err:?}"),
    }
}
