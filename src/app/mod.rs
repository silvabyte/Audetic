#![allow(clippy::arc_with_non_send_sync)]

use crate::api::{ApiCommand, ApiServer};
use crate::audio::{
    mic_source::MicAudioSource, system_source::SystemAudioSource, AudioStreamManager,
    BehaviorOptions, RecordingMachine, RecordingPhase, RecordingStatusHandle, ToggleResult,
};
use crate::config::Config;
use crate::meeting::{MeetingMachine, MeetingStatusHandle, ShellCommandHook};
use crate::text_io::TextIoService;
use crate::transcription::job_service::RemoteTranscriptionJobService;
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

    let status_handle = RecordingStatusHandle::default();
    let recording_machine = RecordingMachine::new(
        audio_recorder.clone(),
        transcription_service,
        indicator,
        text_io,
        BehaviorOptions {
            auto_paste: config.behavior.auto_paste,
            delete_audio_files: config.behavior.delete_audio_files,
        },
        status_handle.clone(),
    );

    // Meeting pipeline (independent from recording pipeline)
    let meeting_status = MeetingStatusHandle::default();
    let mut meeting_machine = build_meeting_machine(&config, meeting_status.clone());

    let api_server = ApiServer::new(tx, status_handle.clone(), &config)
        .with_meeting_state(meeting_status.clone());

    tokio::spawn(async move {
        if let Err(e) = api_server.start().await {
            error!("API server failed: {}", e);
        }
    });

    spawn_update_manager();

    info!("Audetic is ready!");
    info!("Add this to your Hyprland config:");
    info!("bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle");
    info!("bindd = SUPER SHIFT, R, Audetic Meeting, exec, curl -X POST http://127.0.0.1:3737/meetings/toggle");
    info!("Or test manually: curl -X POST http://127.0.0.1:3737/toggle");

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
            ApiCommand::MeetingStart(options) => {
                match meeting_machine.start(options).await {
                    Ok(result) => {
                        info!(
                            "Meeting {} started: {:?}",
                            result.meeting_id, result.audio_path
                        );
                    }
                    Err(e) => error!("Failed to start meeting: {}", e),
                }
            }
            ApiCommand::MeetingStop => {
                match meeting_machine.stop().await {
                    Ok(result) => {
                        info!(
                            "Meeting {} stopped ({}s)",
                            result.meeting_id, result.duration_seconds
                        );
                    }
                    Err(e) => error!("Failed to stop meeting: {}", e),
                }
            }
            ApiCommand::MeetingToggle(options) => {
                match meeting_machine.toggle(options).await {
                    Ok(outcome) => match outcome {
                        crate::meeting::ToggleOutcome::Started(r) => {
                            info!("Meeting {} started via toggle", r.meeting_id);
                        }
                        crate::meeting::ToggleOutcome::Stopped(r) => {
                            info!("Meeting {} stopped via toggle ({}s)", r.meeting_id, r.duration_seconds);
                        }
                    },
                    Err(e) => error!("Failed to toggle meeting: {}", e),
                }
            }
        }
    }

    Ok(())
}

fn build_meeting_machine(config: &Config, status: MeetingStatusHandle) -> MeetingMachine {
    let mic_source = MicAudioSource::new(16000)
        .map(|s| Box::new(s) as Box<dyn crate::audio::audio_source::AudioSource>)
        .unwrap_or_else(|e| {
            warn!("Failed to create meeting mic source: {}. Using fallback.", e);
            Box::new(NullAudioSource)
        });

    let system_source = Box::new(SystemAudioSource::new(16000));

    // Determine jobs API URL
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

    let transcription = Box::new(RemoteTranscriptionJobService::new(
        &jobs_url,
        Duration::from_secs(MEETING_TRANSCRIPTION_TIMEOUT_SECS),
    ));

    // Post-meeting hook (optional)
    let hook: Option<Box<dyn crate::meeting::PostMeetingHook>> =
        if !config.meeting.post_command.is_empty() {
            Some(Box::new(ShellCommandHook::new(
                config.meeting.post_command.clone(),
                config.meeting.post_command_timeout_seconds,
            )))
        } else {
            None
        };

    MeetingMachine::new(mic_source, system_source, transcription, hook, status)
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
