use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::audio::AudioStreamManager;
use crate::db::{self, VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use crate::text_io::TextIoService;
use crate::transcription::TranscriptionService;
use crate::ui::Indicator;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingPhase {
    Idle,
    Recording,
    Processing,
    Error,
}

impl RecordingPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            RecordingPhase::Idle => "idle",
            RecordingPhase::Recording => "recording",
            RecordingPhase::Processing => "processing",
            RecordingPhase::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordingStatus {
    pub phase: RecordingPhase,
    pub last_error: Option<String>,
}

impl Default for RecordingStatus {
    fn default() -> Self {
        Self {
            phase: RecordingPhase::Idle,
            last_error: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct RecordingStatusHandle {
    inner: Arc<Mutex<RecordingStatus>>,
}

impl RecordingStatusHandle {
    pub async fn get(&self) -> RecordingStatus {
        self.inner.lock().await.clone()
    }

    pub async fn set(&self, phase: RecordingPhase, last_error: Option<String>) {
        let mut status = self.inner.lock().await;
        status.phase = phase;
        status.last_error = last_error;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BehaviorOptions {
    pub auto_paste: bool,
    pub delete_audio_files: bool,
}

pub struct RecordingMachine {
    audio: Arc<Mutex<AudioStreamManager>>,
    transcription: Arc<TranscriptionService>,
    indicator: Indicator,
    text_io: TextIoService,
    behavior: BehaviorOptions,
    status: RecordingStatusHandle,
}

impl RecordingMachine {
    pub fn new(
        audio: Arc<Mutex<AudioStreamManager>>,
        transcription: Arc<TranscriptionService>,
        indicator: Indicator,
        text_io: TextIoService,
        behavior: BehaviorOptions,
        status: RecordingStatusHandle,
    ) -> Self {
        Self {
            audio,
            transcription,
            indicator,
            text_io,
            behavior,
            status,
        }
    }

    pub async fn toggle(&self) -> Result<RecordingPhase> {
        enum Transition {
            StartRecording,
            StopRecording,
            Busy(RecordingPhase),
        }

        let current = self.status.get().await;
        let transition = match current.phase {
            RecordingPhase::Idle | RecordingPhase::Error => Transition::StartRecording,
            RecordingPhase::Recording => Transition::StopRecording,
            RecordingPhase::Processing => Transition::Busy(RecordingPhase::Processing),
        };

        match transition {
            Transition::StartRecording => {
                info!("RecordingMachine: starting recording");
                if let Err(e) = self.start_recording().await {
                    error!("Failed to start recording: {}", e);
                    self.status
                        .set(RecordingPhase::Error, Some(e.to_string()))
                        .await;
                    let _ = self
                        .indicator
                        .show_error(&format!("Recording failed: {e}"))
                        .await;
                    return Err(e);
                }

                self.status.set(RecordingPhase::Recording, None).await;
                Ok(RecordingPhase::Recording)
            }
            Transition::StopRecording => {
                info!("RecordingMachine: stopping recording and processing");
                self.status.set(RecordingPhase::Processing, None).await;
                if let Err(e) = self.begin_processing().await {
                    error!("Failed to start processing task: {}", e);
                    self.status
                        .set(RecordingPhase::Error, Some(e.to_string()))
                        .await;
                    let _ = self
                        .indicator
                        .show_error(&format!("Processing failed: {e}"))
                        .await;
                    return Err(e);
                }

                Ok(RecordingPhase::Processing)
            }
            //NOTE: this could be annoying
            Transition::Busy(phase) => {
                warn!(
                    "RecordingMachine: toggle requested while busy in {:?}",
                    phase
                );
                Ok(phase)
            }
        }
    }

    async fn start_recording(&self) -> Result<()> {
        if let Err(e) = self.indicator.show_recording().await {
            warn!("Failed to show recording indicator: {}", e);
        }

        let recorder = self.audio.lock().await;
        recorder.start_recording().await
    }

    async fn begin_processing(&self) -> Result<()> {
        let temp_path = Self::temp_audio_path();

        {
            let recorder = self.audio.lock().await;
            recorder.stop_recording(temp_path.clone()).await?;
        }

        let indicator_for_task = self.indicator.clone();
        if let Err(e) = indicator_for_task.show_processing().await {
            warn!("Failed to show processing indicator: {}", e);
        }
        let indicator_for_error = self.indicator.clone();

        let transcription = Arc::clone(&self.transcription);
        let text_io = self.text_io.clone();
        let behavior = self.behavior;
        let status = self.status.clone();

        tokio::spawn(async move {
            let result = RecordingMachine::run_processing_task(
                transcription,
                indicator_for_task,
                text_io,
                behavior,
                temp_path,
            )
            .await;

            match result {
                Ok(_) => {
                    status.set(RecordingPhase::Idle, None).await;
                }
                Err(e) => {
                    error!("Recording pipeline failed: {}", e);
                    status.set(RecordingPhase::Error, Some(e.to_string())).await;
                    let _ = indicator_for_error
                        .show_error(&format!("Transcription failed: {e}"))
                        .await;
                }
            }
        });

        Ok(())
    }

    async fn run_processing_task(
        transcription: Arc<TranscriptionService>,
        indicator: Indicator,
        text_io: TextIoService,
        behavior: BehaviorOptions,
        temp_path: PathBuf,
    ) -> Result<()> {
        match transcription.transcribe(&temp_path).await {
            Ok(text) => {
                if text.trim().is_empty() {
                    warn!("No speech detected in recording");
                    let _ = indicator.show_error("No speech detected").await;
                } else {
                    info!("Transcription complete: {} chars", text.len());
                    if let Err(e) = text_io.copy_to_clipboard(&text).await {
                        error!("Failed to copy to clipboard: {}", e);
                    }

                    if behavior.auto_paste {
                        if let Err(e) = text_io.inject_text(&text).await {
                            error!("Failed to inject text: {}", e);
                            let _ = text_io.paste_from_clipboard().await;
                        }
                    }

                    if let Err(e) = indicator.show_complete(&text).await {
                        warn!("Failed to show completion indicator: {}", e);
                    }

                    // Save transcription to database
                    let text_clone = text.clone();
                    let temp_path_clone = temp_path.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = save_to_database(&text_clone, &temp_path_clone) {
                            error!("Failed to save transcription to database: {:?}", e);
                        }
                    });
                }
            }
            Err(e) => {
                return Err(e);
            }
        }

        if behavior.delete_audio_files {
            if let Err(e) = tokio::fs::remove_file(&temp_path).await {
                warn!("Failed to delete temp audio file {:?}: {}", temp_path, e);
            } else {
                debug!("Deleted temp audio file {:?}", temp_path);
            }
        }

        Ok(())
    }

    fn temp_audio_path() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        PathBuf::from(format!("/tmp/audetic_{timestamp}.wav"))
    }
}

fn save_to_database(text: &str, audio_path: &PathBuf) -> Result<()> {
    let conn = db::init_db()?;

    let workflow_data = WorkflowData::VoiceToText(VoiceToTextData {
        text: text.to_string(),
        audio_path: audio_path.to_string_lossy().to_string(),
    });

    let workflow = Workflow::new(WorkflowType::VoiceToText, workflow_data);

    let id = db::insert_workflow(&conn, &workflow)?;
    debug!("Saved transcription to database with ID: {}", id);

    // Prune old workflows if count exceeds 10,000
    let pruned = db::prune_old_workflows(&conn, 10_000)?;
    if pruned > 0 {
        info!("Pruned {} old transcriptions from database", pruned);
    }

    Ok(())
}
