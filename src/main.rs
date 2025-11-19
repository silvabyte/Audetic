#![allow(clippy::arc_with_non_send_sync)]

mod api;
mod audio;
mod clipboard;
mod config;
mod normalizer;
mod text_injection;
mod transcription;
mod ui;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use crate::api::{ApiCommand, ApiServer};
use crate::audio::{
    AudioStreamManager, BehaviorOptions, RecordingMachine, RecordingPhase, RecordingStatusHandle,
};
use crate::clipboard::ClipboardManager;
use crate::config::Config;
use crate::text_injection::TextInjector;
use crate::transcription::{ProviderConfig, TranscriptionService, WhisperTranscriber};
use crate::ui::Indicator;

#[derive(Parser)]
#[command(name = "audetic")]
#[command(about = "Voice to text for Hyprland", long_about = None)]
struct Args {
    #[arg(short, long)]
    config: Option<PathBuf>,

    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let log_level = if args.verbose { "debug" } else { "info" };
    let env_filter = EnvFilter::try_new(log_level).unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    info!("Starting Audetic");

    let config = if let Some(config_path) = args.config {
        Config::load_from_path(config_path)?
    } else {
        Config::load()?
    };

    // Initialize components
    let (tx, mut rx) = mpsc::channel::<ApiCommand>(10);

    let audio_recorder = Arc::new(Mutex::new(AudioStreamManager::new()?));

    // Build whisper transcriber
    let whisper = if let Some(provider) = &config.whisper.provider {
        let provider_config = ProviderConfig {
            model: config.whisper.model.clone(),
            model_path: config.whisper.model_path.clone(),
            language: config.whisper.language.clone(),
            command_path: config.whisper.command_path.clone(),
            api_endpoint: config.whisper.api_endpoint.clone(),
            api_key: config.whisper.api_key.clone(),
        };
        WhisperTranscriber::with_provider(provider, provider_config)?
    } else {
        // Auto-detect provider when no provider specified
        let provider_config = ProviderConfig {
            model: config.whisper.model.clone(),
            model_path: config.whisper.model_path.clone(),
            language: config.whisper.language.clone(),
            command_path: config.whisper.command_path.clone(),
            api_endpoint: config.whisper.api_endpoint.clone(),
            api_key: config.whisper.api_key.clone(),
        };
        WhisperTranscriber::auto_detect(provider_config)?
    };

    // Compose transcription service with whisper and normalizer
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

    // Create and start API server
    let api_server = ApiServer::new(tx, status_handle.clone(), &config);

    // Start API server in background
    tokio::spawn(async move {
        if let Err(e) = api_server.start().await {
            error!("API server failed: {}", e);
        }
    });

    //TODO: spawn auto-update service

    info!("Audetic is ready!");
    info!("Add this to your Hyprland config:");
    info!("bindd = SUPER, R, Audetic, exec, curl -X POST http://127.0.0.1:3737/toggle");
    info!("Or test manually: curl -X POST http://127.0.0.1:3737/toggle");

    // Main event loop
    while let Some(command) = rx.recv().await {
        match command {
            ApiCommand::ToggleRecording => match recording_machine.toggle().await {
                Ok(RecordingPhase::Recording) => info!("Recording started"),
                Ok(RecordingPhase::Processing) => info!("Recording stopped, processing audio"),
                Ok(phase) => info!("RecordingMachine is currently {:?}", phase),
                Err(e) => error!("Failed to toggle recording: {}", e),
            },
        }
    }

    Ok(())
}
