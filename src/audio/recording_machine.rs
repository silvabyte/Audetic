use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::audio::AudioStreamManager;
use crate::db::{self, VoiceToTextData, Workflow, WorkflowData, WorkflowType};
use crate::text_io::TextIoService;
use crate::transcription::TranscriptionService;
use crate::ui::Indicator;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// Information about a completed transcription job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedJob {
    /// The job UUID assigned when recording started
    pub job_id: String,
    /// The database history ID for retrieving via /history/:id
    pub history_id: i64,
    /// The transcribed text
    pub text: String,
    /// When the job completed
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct RecordingStatus {
    pub phase: RecordingPhase,
    /// Current job ID (set when recording starts)
    pub current_job_id: Option<String>,
    /// Current job options (set when recording starts)
    pub current_job_options: Option<JobOptions>,
    /// Last successfully completed job
    pub last_completed_job: Option<CompletedJob>,
    pub last_error: Option<String>,
}

impl Default for RecordingStatus {
    fn default() -> Self {
        Self {
            phase: RecordingPhase::Idle,
            current_job_id: None,
            current_job_options: None,
            last_completed_job: None,
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

    pub async fn set_phase(&self, phase: RecordingPhase, last_error: Option<String>) {
        let mut status = self.inner.lock().await;
        status.phase = phase;
        status.last_error = last_error;
    }

    pub async fn start_job(&self, job_id: String, options: JobOptions) {
        let mut status = self.inner.lock().await;
        status.phase = RecordingPhase::Recording;
        status.current_job_id = Some(job_id);
        status.current_job_options = Some(options);
        status.last_error = None;
    }

    pub async fn complete_job(&self, completed_job: CompletedJob) {
        let mut status = self.inner.lock().await;
        status.phase = RecordingPhase::Idle;
        status.current_job_id = None;
        status.current_job_options = None;
        status.last_completed_job = Some(completed_job);
        status.last_error = None;
    }

    pub async fn fail_job(&self, error: String) {
        let mut status = self.inner.lock().await;
        status.phase = RecordingPhase::Error;
        status.current_job_id = None;
        status.current_job_options = None;
        status.last_error = Some(error);
    }

    pub async fn set_processing(&self) {
        let mut status = self.inner.lock().await;
        status.phase = RecordingPhase::Processing;
        // Keep the current_job_id during processing
    }

    pub async fn get_current_job_id(&self) -> Option<String> {
        self.inner.lock().await.current_job_id.clone()
    }

    pub async fn get_current_job_options(&self) -> Option<JobOptions> {
        self.inner.lock().await.current_job_options
    }
}

/// Result of a toggle operation, containing phase and job information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToggleResult {
    /// The current recording phase after the toggle
    pub phase: RecordingPhase,
    /// The job UUID (set when recording starts or during processing)
    pub job_id: Option<String>,
}

/// Per-job options that can override default behavior.
/// These are set when starting a recording via the API.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct JobOptions {
    /// Whether to copy the transcription to clipboard (default: true)
    pub copy_to_clipboard: bool,
    /// Whether to auto-paste/inject text into the focused app (default: from config)
    pub auto_paste: bool,
}

impl Default for JobOptions {
    fn default() -> Self {
        Self {
            copy_to_clipboard: true,
            auto_paste: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BehaviorOptions {
    pub auto_paste: bool,
    pub delete_audio_files: bool,
}

/// Context for running a transcription processing task.
struct ProcessingContext {
    transcription: Arc<TranscriptionService>,
    indicator: Indicator,
    text_io: TextIoService,
    job_options: JobOptions,
    temp_path: PathBuf,
    job_id: Option<String>,
    delete_audio_files: bool,
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

    /// Toggle recording state and return the result with job information.
    ///
    /// Returns a `ToggleResult` containing:
    /// - `phase`: The new recording phase
    /// - `job_id`: The job UUID (only set when starting a new recording)
    ///
    /// # Arguments
    /// * `options` - Optional per-job options to override default behavior.
    ///   If None, uses defaults from config (auto_paste from config, copy_to_clipboard=true).
    pub async fn toggle(&self, options: Option<JobOptions>) -> Result<ToggleResult> {
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
                // Generate a new job ID for this recording session
                let job_id = Uuid::new_v4().to_string();

                // Use provided options or create defaults from config
                let job_options = options.unwrap_or(JobOptions {
                    copy_to_clipboard: true,
                    auto_paste: self.behavior.auto_paste,
                });

                info!(
                    "RecordingMachine: starting recording with job_id={}, options={:?}",
                    job_id, job_options
                );

                if let Err(e) = self.start_recording().await {
                    error!("Failed to start recording: {}", e);
                    self.status.fail_job(e.to_string()).await;
                    let _ = self
                        .indicator
                        .show_error(&format!("Recording failed: {e}"))
                        .await;
                    return Err(e);
                }

                self.status.start_job(job_id.clone(), job_options).await;
                Ok(ToggleResult {
                    phase: RecordingPhase::Recording,
                    job_id: Some(job_id),
                })
            }
            Transition::StopRecording => {
                let job_id = current.current_job_id.clone();
                // Job options should always be set when recording started, fall back to defaults if not
                let job_options = current.current_job_options.unwrap_or(JobOptions {
                    copy_to_clipboard: true,
                    auto_paste: self.behavior.auto_paste,
                });
                info!(
                    "RecordingMachine: stopping recording and processing job_id={:?}, options={:?}",
                    job_id, job_options
                );
                self.status.set_processing().await;

                if let Err(e) = self.begin_processing(job_id.clone(), job_options).await {
                    error!("Failed to start processing task: {}", e);
                    self.status.fail_job(e.to_string()).await;
                    let _ = self
                        .indicator
                        .show_error(&format!("Processing failed: {e}"))
                        .await;
                    return Err(e);
                }

                Ok(ToggleResult {
                    phase: RecordingPhase::Processing,
                    job_id,
                })
            }
            //NOTE: this could be annoying
            Transition::Busy(phase) => {
                warn!(
                    "RecordingMachine: toggle requested while busy in {:?}",
                    phase
                );
                Ok(ToggleResult {
                    phase,
                    job_id: current.current_job_id,
                })
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

    async fn begin_processing(
        &self,
        job_id: Option<String>,
        job_options: JobOptions,
    ) -> Result<()> {
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

        let status = self.status.clone();

        let ctx = ProcessingContext {
            transcription: Arc::clone(&self.transcription),
            indicator: indicator_for_task,
            text_io: self.text_io.clone(),
            job_options,
            temp_path,
            job_id,
            delete_audio_files: self.behavior.delete_audio_files,
        };

        tokio::spawn(async move {
            let result = RecordingMachine::run_processing_task(ctx).await;

            match result {
                Ok(completed_job) => {
                    if let Some(job) = completed_job {
                        status.complete_job(job).await;
                    } else {
                        // No speech detected case - just go back to idle
                        status.set_phase(RecordingPhase::Idle, None).await;
                    }
                }
                Err(e) => {
                    error!("Recording pipeline failed: {}", e);
                    status.fail_job(e.to_string()).await;
                    let _ = indicator_for_error
                        .show_error(&format!("Transcription failed: {e}"))
                        .await;
                }
            }
        });

        Ok(())
    }

    /// Run the transcription processing task.
    /// Returns `Ok(Some(CompletedJob))` on success, `Ok(None)` if no speech detected.
    async fn run_processing_task(ctx: ProcessingContext) -> Result<Option<CompletedJob>> {
        let completed_job = match ctx.transcription.transcribe(&ctx.temp_path).await {
            Ok(text) => {
                if text.trim().is_empty() {
                    warn!("No speech detected in recording");
                    let _ = ctx.indicator.show_error("No speech detected").await;
                    None
                } else {
                    info!("Transcription complete: {} chars", text.len());

                    // Use job_options to control clipboard/paste behavior
                    if ctx.job_options.copy_to_clipboard {
                        if let Err(e) = ctx.text_io.copy_to_clipboard(&text).await {
                            error!("Failed to copy to clipboard: {}", e);
                        }
                    }

                    if ctx.job_options.auto_paste {
                        if let Err(e) = ctx.text_io.inject_text(&text).await {
                            error!("Failed to inject text: {}", e);
                            // Only try paste fallback if we copied to clipboard
                            if ctx.job_options.copy_to_clipboard {
                                let _ = ctx.text_io.paste_from_clipboard().await;
                            }
                        }
                    }

                    if let Err(e) = ctx.indicator.show_complete(&text).await {
                        warn!("Failed to show completion indicator: {}", e);
                    }

                    // Save transcription to database and get the history ID
                    let text_for_db = text.clone();
                    let temp_path_for_db = ctx.temp_path.clone();
                    let job_id_for_db = ctx.job_id.clone();

                    let db_result = tokio::task::spawn_blocking(move || {
                        save_to_database(&text_for_db, &temp_path_for_db)
                    })
                    .await;

                    match db_result {
                        Ok(Ok(history_id)) => {
                            let completed = CompletedJob {
                                job_id: ctx.job_id.unwrap_or_else(|| "unknown".to_string()),
                                history_id,
                                text,
                                created_at: chrono::Utc::now().to_rfc3339(),
                            };
                            Some(completed)
                        }
                        Ok(Err(e)) => {
                            error!("Failed to save transcription to database: {:?}", e);
                            // Still return a completed job but with id 0
                            Some(CompletedJob {
                                job_id: job_id_for_db.unwrap_or_else(|| "unknown".to_string()),
                                history_id: 0,
                                text,
                                created_at: chrono::Utc::now().to_rfc3339(),
                            })
                        }
                        Err(e) => {
                            error!("Database task panicked: {:?}", e);
                            None
                        }
                    }
                }
            }
            Err(e) => {
                return Err(e);
            }
        };

        if ctx.delete_audio_files {
            if let Err(e) = tokio::fs::remove_file(&ctx.temp_path).await {
                warn!(
                    "Failed to delete temp audio file {:?}: {}",
                    ctx.temp_path, e
                );
            } else {
                debug!("Deleted temp audio file {:?}", ctx.temp_path);
            }
        }

        Ok(completed_job)
    }

    fn temp_audio_path() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        PathBuf::from(format!("/tmp/audetic_{timestamp}.wav"))
    }
}

/// Save transcription to database and return the history ID.
fn save_to_database(text: &str, audio_path: &Path) -> Result<i64> {
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

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_phase_as_str() {
        assert_eq!(RecordingPhase::Idle.as_str(), "idle");
        assert_eq!(RecordingPhase::Recording.as_str(), "recording");
        assert_eq!(RecordingPhase::Processing.as_str(), "processing");
        assert_eq!(RecordingPhase::Error.as_str(), "error");
    }

    #[test]
    fn test_recording_phase_serialization() {
        // Test that phases serialize to lowercase strings
        let phase = RecordingPhase::Recording;
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(json, "\"recording\"");

        // Test deserialization
        let parsed: RecordingPhase = serde_json::from_str("\"idle\"").unwrap();
        assert_eq!(parsed, RecordingPhase::Idle);
    }

    #[test]
    fn test_recording_status_default() {
        let status = RecordingStatus::default();
        assert_eq!(status.phase, RecordingPhase::Idle);
        assert!(status.current_job_id.is_none());
        assert!(status.current_job_options.is_none());
        assert!(status.last_completed_job.is_none());
        assert!(status.last_error.is_none());
    }

    #[tokio::test]
    async fn test_status_handle_start_job() {
        let handle = RecordingStatusHandle::default();

        // Start a job with default options
        let options = JobOptions::default();
        handle.start_job("test-job-123".to_string(), options).await;

        let status = handle.get().await;
        assert_eq!(status.phase, RecordingPhase::Recording);
        assert_eq!(status.current_job_id, Some("test-job-123".to_string()));
        assert!(status.current_job_options.is_some());
        assert!(status.last_error.is_none());
    }

    #[tokio::test]
    async fn test_status_handle_start_job_with_custom_options() {
        let handle = RecordingStatusHandle::default();

        // Start a job with custom options (no clipboard, no auto-paste)
        let options = JobOptions {
            copy_to_clipboard: false,
            auto_paste: false,
        };
        handle
            .start_job("test-job-custom".to_string(), options)
            .await;

        let status = handle.get().await;
        assert_eq!(status.phase, RecordingPhase::Recording);
        assert_eq!(status.current_job_id, Some("test-job-custom".to_string()));

        let job_options = status.current_job_options.unwrap();
        assert!(!job_options.copy_to_clipboard);
        assert!(!job_options.auto_paste);
    }

    #[tokio::test]
    async fn test_status_handle_set_processing() {
        let handle = RecordingStatusHandle::default();

        // Start a job then transition to processing
        handle
            .start_job("test-job-456".to_string(), JobOptions::default())
            .await;
        handle.set_processing().await;

        let status = handle.get().await;
        assert_eq!(status.phase, RecordingPhase::Processing);
        // Job ID and options should be preserved during processing
        assert_eq!(status.current_job_id, Some("test-job-456".to_string()));
        assert!(status.current_job_options.is_some());
    }

    #[tokio::test]
    async fn test_status_handle_complete_job() {
        let handle = RecordingStatusHandle::default();

        // Start and complete a job
        handle
            .start_job("test-job-789".to_string(), JobOptions::default())
            .await;
        handle.set_processing().await;

        let completed = CompletedJob {
            job_id: "test-job-789".to_string(),
            history_id: 42,
            text: "Hello world".to_string(),
            created_at: "2025-01-15T10:30:00Z".to_string(),
        };
        handle.complete_job(completed).await;

        let status = handle.get().await;
        assert_eq!(status.phase, RecordingPhase::Idle);
        assert!(status.current_job_id.is_none()); // Cleared after completion
        assert!(status.current_job_options.is_none()); // Cleared after completion

        let last_job = status.last_completed_job.unwrap();
        assert_eq!(last_job.job_id, "test-job-789");
        assert_eq!(last_job.history_id, 42);
        assert_eq!(last_job.text, "Hello world");
    }

    #[tokio::test]
    async fn test_status_handle_fail_job() {
        let handle = RecordingStatusHandle::default();

        // Start a job then fail it
        handle
            .start_job("test-job-fail".to_string(), JobOptions::default())
            .await;
        handle.fail_job("Something went wrong".to_string()).await;

        let status = handle.get().await;
        assert_eq!(status.phase, RecordingPhase::Error);
        assert!(status.current_job_id.is_none()); // Cleared on failure
        assert!(status.current_job_options.is_none()); // Cleared on failure
        assert_eq!(status.last_error, Some("Something went wrong".to_string()));
    }

    #[tokio::test]
    async fn test_status_handle_job_lifecycle() {
        let handle = RecordingStatusHandle::default();

        // Full lifecycle: idle -> recording -> processing -> idle (with completed job)
        let status = handle.get().await;
        assert_eq!(status.phase, RecordingPhase::Idle);

        // Start recording
        handle
            .start_job("lifecycle-test".to_string(), JobOptions::default())
            .await;
        let status = handle.get().await;
        assert_eq!(status.phase, RecordingPhase::Recording);
        assert_eq!(status.current_job_id, Some("lifecycle-test".to_string()));

        // Start processing
        handle.set_processing().await;
        let status = handle.get().await;
        assert_eq!(status.phase, RecordingPhase::Processing);
        assert_eq!(status.current_job_id, Some("lifecycle-test".to_string()));

        // Complete
        let completed = CompletedJob {
            job_id: "lifecycle-test".to_string(),
            history_id: 100,
            text: "Test transcription".to_string(),
            created_at: "2025-01-15T12:00:00Z".to_string(),
        };
        handle.complete_job(completed).await;

        let status = handle.get().await;
        assert_eq!(status.phase, RecordingPhase::Idle);
        assert!(status.current_job_id.is_none());
        assert!(status.last_completed_job.is_some());
        assert_eq!(status.last_completed_job.unwrap().history_id, 100);
    }

    #[tokio::test]
    async fn test_completed_job_persists_across_new_jobs() {
        let handle = RecordingStatusHandle::default();

        // Complete first job
        let first_job = CompletedJob {
            job_id: "first-job".to_string(),
            history_id: 1,
            text: "First".to_string(),
            created_at: "2025-01-15T10:00:00Z".to_string(),
        };
        handle.complete_job(first_job).await;

        // Start a new job - last_completed_job should still be available
        handle
            .start_job("second-job".to_string(), JobOptions::default())
            .await;

        let status = handle.get().await;
        assert_eq!(status.current_job_id, Some("second-job".to_string()));
        assert!(status.last_completed_job.is_some());
        assert_eq!(status.last_completed_job.unwrap().job_id, "first-job");
    }

    #[test]
    fn test_job_options_default() {
        let options = JobOptions::default();
        assert!(options.copy_to_clipboard);
        assert!(options.auto_paste);
    }

    #[test]
    fn test_job_options_serialization() {
        let options = JobOptions {
            copy_to_clipboard: false,
            auto_paste: true,
        };

        let json = serde_json::to_string(&options).unwrap();
        assert!(json.contains("\"copy_to_clipboard\":false"));
        assert!(json.contains("\"auto_paste\":true"));

        // Test deserialization
        let parsed: JobOptions = serde_json::from_str(&json).unwrap();
        assert!(!parsed.copy_to_clipboard);
        assert!(parsed.auto_paste);
    }

    #[test]
    fn test_toggle_result_serialization() {
        let result = ToggleResult {
            phase: RecordingPhase::Recording,
            job_id: Some("abc-123".to_string()),
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"phase\":\"recording\""));
        assert!(json.contains("\"job_id\":\"abc-123\""));

        // Test deserialization
        let parsed: ToggleResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.phase, RecordingPhase::Recording);
        assert_eq!(parsed.job_id, Some("abc-123".to_string()));
    }

    #[test]
    fn test_completed_job_serialization() {
        let job = CompletedJob {
            job_id: "test-uuid".to_string(),
            history_id: 42,
            text: "Hello world".to_string(),
            created_at: "2025-01-15T10:30:00Z".to_string(),
        };

        let json = serde_json::to_string(&job).unwrap();
        assert!(json.contains("\"job_id\":\"test-uuid\""));
        assert!(json.contains("\"history_id\":42"));
        assert!(json.contains("\"text\":\"Hello world\""));

        // Test round-trip
        let parsed: CompletedJob = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.job_id, "test-uuid");
        assert_eq!(parsed.history_id, 42);
    }
}
