#![allow(clippy::arc_with_non_send_sync)]

use crate::api::{ApiCommand, ApiServer};
use crate::audio::{
    AudioStreamManager, BehaviorOptions, RecordingMachine, RecordingPhase, RecordingStatusHandle,
};
use crate::clipboard::ClipboardManager;
use crate::config::Config;
use crate::text_injection::TextInjector;
use crate::transcription::{ProviderConfig, Transcriber, TranscriptionService};
use crate::ui::Indicator;
use crate::update::{UpdateConfig, UpdateEngine};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};

pub async fn run_service() -> Result<()> {
    info!("Starting Audetic service");

    let config = Config::load()?;

    let (tx, mut rx) = mpsc::channel::<ApiCommand>(10);
    let audio_recorder = Arc::new(Mutex::new(AudioStreamManager::new()?));

    let whisper = build_transcriber(&config)?;
    let transcription_service = Arc::new(TranscriptionService::new(whisper)?);

    let text_injector = TextInjector::new(Some(&config.wayland.input_method))?;
    let clipboard = ClipboardManager::new()?.with_preserve(config.behavior.preserve_clipboard);
    let indicator =
        Indicator::from_config(&config.ui).with_audio_feedback(config.behavior.audio_feedback);

    let status_handle = RecordingStatusHandle::default();
    let recording_machine = RecordingMachine::new(
        audio_recorder.clone(),
        transcription_service,
        indicator,
        text_injector,
        clipboard,
        BehaviorOptions {
            auto_paste: config.behavior.auto_paste,
            delete_audio_files: config.behavior.delete_audio_files,
        },
        status_handle.clone(),
    );

    let api_server = ApiServer::new(tx, status_handle.clone(), &config);
    tokio::spawn(async move {
        if let Err(e) = api_server.start().await {
            error!("API server failed: {}", e);
        }
    });

    spawn_update_manager();

    info!("Audetic is ready!");
    info!("Add this to your Hyprland config:");
    info!("bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle");
    info!("Or test manually: curl -X POST http://127.0.0.1:3737/toggle");

    while let Some(command) = rx.recv().await {
        match command {
            ApiCommand::ToggleRecording => match recording_machine.toggle().await {
                Ok(RecordingPhase::Recording) => info!("Recording started"),
                Ok(RecordingPhase::Processing) => {
                    info!("Recording stopped, processing audio");
                }
                Ok(phase) => info!("RecordingMachine is currently {:?}", phase),
                Err(e) => error!("Failed to toggle recording: {}", e),
            },
        }
    }

    Ok(())
}

fn build_transcriber(config: &Config) -> Result<Transcriber> {
    let provider_config = ProviderConfig {
        model: config.whisper.model.clone(),
        model_path: config.whisper.model_path.clone(),
        language: config.whisper.language.clone(),
        command_path: config.whisper.command_path.clone(),
        api_endpoint: config.whisper.api_endpoint.clone(),
        api_key: config.whisper.api_key.clone(),
    };

    if let Some(provider) = &config.whisper.provider {
        Transcriber::with_provider(provider, provider_config)
    } else {
        Transcriber::auto_detect(provider_config)
    }
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
