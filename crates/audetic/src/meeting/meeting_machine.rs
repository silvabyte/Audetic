//! Meeting lifecycle orchestrator.
//!
//! Manages the full meeting recording pipeline:
//! start → stop → compress → transcribe → save → hook → done
//!
//! Post-recording processing (compress + transcribe + hook) runs in a spawned
//! background task so the `stop()` call returns to the caller quickly.
//! Phase transitions are surfaced via the injected `Indicator` (Hyprland
//! notifications + audio feedback) and the `MeetingStatusHandle`.

use anyhow::{bail, Result};
use hound::{WavSpec, WavWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::audio::audio_mixer::AudioMixer;
use crate::audio::audio_source::AudioSource;
use crate::cli::compression::{cleanup_temp_file, prepare_for_upload};
use crate::db::{self, meetings::MeetingRepository};
use crate::transcription::job_service::TranscriptionJobService;
use crate::ui::Indicator;

use super::post_meeting_hook::{MeetingResult, PostMeetingHook};
use super::status::{MeetingPhase, MeetingStartOptions, MeetingStatusHandle};

/// Which audio sources were actually capturing at the start of a meeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureState {
    /// Both microphone and system audio are being captured.
    Both,
    /// Only the microphone is being captured (system audio unavailable).
    MicOnly,
    /// Only the system audio is being captured (mic unavailable).
    SystemOnly,
}

impl CaptureState {
    /// Human-readable label for CLI output / notifications.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Both => "mic + system audio",
            Self::MicOnly => "mic only (system audio unavailable)",
            Self::SystemOnly => "system audio only (mic unavailable)",
        }
    }

    /// Stable machine-readable tag for the HTTP API. Wire consumers
    /// (the Electron UI) switch on these values; `as_str()` is for
    /// humans and may change wording without breaking the contract.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::MicOnly => "mic_only",
            Self::SystemOnly => "system_only",
        }
    }
}

/// Result returned from stopping a meeting.
#[derive(Debug, Clone)]
pub struct MeetingStopResult {
    pub meeting_id: i64,
    pub duration_seconds: u64,
}

/// Result returned from starting a meeting.
#[derive(Debug, Clone)]
pub struct MeetingStartResult {
    pub meeting_id: i64,
    pub audio_path: PathBuf,
    pub capture_state: CaptureState,
}

pub struct MeetingMachine {
    mic_source: Box<dyn AudioSource>,
    system_source: Box<dyn AudioSource>,
    transcription: Arc<dyn TranscriptionJobService>,
    hook: Option<Arc<dyn PostMeetingHook>>,
    indicator: Indicator,
    status: MeetingStatusHandle,
    meetings_dir: PathBuf,
}

impl MeetingMachine {
    pub fn new(
        mic_source: Box<dyn AudioSource>,
        system_source: Box<dyn AudioSource>,
        transcription: Arc<dyn TranscriptionJobService>,
        hook: Option<Arc<dyn PostMeetingHook>>,
        indicator: Indicator,
        status: MeetingStatusHandle,
    ) -> Self {
        let meetings_dir = crate::global::data_dir()
            .map(|d| d.join("meetings"))
            .unwrap_or_else(|_| PathBuf::from("/tmp/audetic/meetings"));

        Self {
            mic_source,
            system_source,
            transcription,
            hook,
            indicator,
            status,
            meetings_dir,
        }
    }

    /// Start a meeting recording.
    ///
    /// Returns an error if a meeting is already recording or if both audio
    /// sources fail to start. Gracefully degrades to mic-only or system-only
    /// if just one source fails; the `capture_state` on the result tells the
    /// caller which sources are live.
    pub async fn start(
        &mut self,
        options: Option<MeetingStartOptions>,
    ) -> Result<MeetingStartResult> {
        let current = self.status.get().await;
        if current.phase == MeetingPhase::Recording {
            bail!(
                "Meeting already in progress (id: {}). Stop it first or use toggle.",
                current.meeting_id.unwrap_or(0)
            );
        }

        let opts = options.unwrap_or_default();
        let audio_path = self.generate_audio_path();

        // Ensure meetings directory exists
        if let Some(parent) = audio_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Insert meeting record in DB
        let meeting_id = {
            let conn = db::init_db()?;
            MeetingRepository::insert(&conn, opts.title.as_deref(), &audio_path.to_string_lossy())?
        };

        // Start audio sources — track which ones actually came up.
        let mic_ok = match self.mic_source.start() {
            Ok(()) => true,
            Err(e) => {
                warn!("Failed to start mic: {}", e);
                false
            }
        };

        let system_ok = match self.system_source.start() {
            Ok(()) => true,
            Err(e) => {
                warn!("Failed to start system audio: {}", e);
                false
            }
        };

        let capture_state = match (mic_ok, system_ok) {
            (true, true) => CaptureState::Both,
            (true, false) => CaptureState::MicOnly,
            (false, true) => CaptureState::SystemOnly,
            (false, false) => {
                // Clean up DB row so we don't leave a dangling "recording" meeting
                if let Ok(conn) = db::init_db() {
                    let _ = MeetingRepository::fail(
                        &conn,
                        meeting_id,
                        "Failed to start any audio source",
                        0,
                    );
                }
                bail!("Failed to start any audio source");
            }
        };

        self.status
            .start_recording(meeting_id, opts.title.clone(), audio_path.clone())
            .await;

        info!(
            "Meeting {} recording started ({}): {:?}",
            meeting_id,
            capture_state.as_str(),
            audio_path
        );

        // Fire the "recording" notification + start beep.
        if let Err(e) = self.indicator.show_recording().await {
            warn!("Failed to show recording indicator: {}", e);
        }

        // If system audio silently dropped, surface it as a warning notification
        // so the user isn't surprised by a mic-only recording later.
        if matches!(capture_state, CaptureState::MicOnly) {
            if let Err(e) = self
                .indicator
                .show_error("System audio unavailable — recording mic only")
                .await
            {
                warn!("Failed to show capture warning: {}", e);
            }
        }

        Ok(MeetingStartResult {
            meeting_id,
            audio_path,
            capture_state,
        })
    }

    /// Stop the meeting recording.
    ///
    /// Halts audio sources, mixes the captured samples, writes the WAV file,
    /// and spawns a background task to handle compression + transcription +
    /// the post-meeting hook. Returns `MeetingStopResult` immediately after
    /// the WAV is written so the HTTP caller unblocks within milliseconds.
    pub async fn stop(&mut self) -> Result<MeetingStopResult> {
        let state = self.status.get().await;
        if state.phase != MeetingPhase::Recording {
            bail!(
                "No meeting recording in progress (current phase: {})",
                state.phase.as_str()
            );
        }

        let meeting_id = state.meeting_id.unwrap_or(0);
        let duration_seconds = state.duration_seconds().unwrap_or(0);
        let audio_path = state.audio_path.clone().unwrap_or_default();
        let title = state.title.clone();

        // Stop audio sources and collect samples
        let mic_samples = match self.mic_source.stop() {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to stop mic: {}", e);
                Vec::new()
            }
        };

        let mic_rate = self.mic_source.sample_rate();

        let system_samples = match self.system_source.stop() {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to stop system audio: {}", e);
                Vec::new()
            }
        };

        let system_rate = self.system_source.sample_rate();

        if mic_samples.is_empty() && system_samples.is_empty() {
            // Persist failure so the meeting row isn't left stuck in `recording`.
            if let Ok(conn) = db::init_db() {
                let _ = MeetingRepository::fail(
                    &conn,
                    meeting_id,
                    "No audio captured",
                    duration_seconds as i64,
                );
            }
            self.status.set_error("No audio captured".to_string()).await;
            let _ = self.indicator.show_error("No audio captured").await;
            bail!("No audio samples captured during meeting");
        }

        info!(
            "Meeting {} stopped: mic={} samples ({}Hz), system={} samples ({}Hz), duration={}s",
            meeting_id,
            mic_samples.len(),
            mic_rate,
            system_samples.len(),
            system_rate,
            duration_seconds,
        );

        // Mix audio (resample if needed, then mix)
        let target_rate: u32 = 16000; // Whisper optimal
        let mic_resampled = AudioMixer::resample(&mic_samples, mic_rate, target_rate);
        let system_resampled = AudioMixer::resample(&system_samples, system_rate, target_rate);
        let mixed = AudioMixer::mix(&[mic_resampled, system_resampled]);

        // Write WAV file
        self.write_wav(&audio_path, &mixed, target_rate)?;

        // Transition to Compressing phase and notify the user that processing
        // has started. Both happen before the background task so the status is
        // coherent when the HTTP handler returns.
        self.status.set_phase(MeetingPhase::Compressing).await;
        if let Err(e) = self.indicator.show_processing().await {
            warn!("Failed to show processing indicator: {}", e);
        }

        // Spawn the compress → transcribe → hook pipeline so the caller
        // doesn't have to wait for it. The spawned task owns its own clones
        // of the non-`!Send` dependencies.
        let ctx = ProcessContext {
            meeting_id,
            audio_path: audio_path.clone(),
            title,
            duration_seconds,
            status: self.status.clone(),
            transcription: Arc::clone(&self.transcription),
            hook: self.hook.as_ref().map(Arc::clone),
            indicator: self.indicator.clone(),
        };
        tokio::spawn(async move { run_processing_task(ctx).await });

        Ok(MeetingStopResult {
            meeting_id,
            duration_seconds,
        })
    }

    /// Cancel a meeting in progress without running the transcription pipeline.
    ///
    /// Halts audio sources, discards captured samples, deletes any partial
    /// WAV file, and marks the meeting as `cancelled` in the DB. Returns an
    /// error if no meeting is currently recording.
    pub async fn cancel(&mut self) -> Result<MeetingStopResult> {
        let state = self.status.get().await;
        if state.phase != MeetingPhase::Recording {
            bail!(
                "No meeting recording in progress to cancel (current phase: {})",
                state.phase.as_str()
            );
        }

        let meeting_id = state.meeting_id.unwrap_or(0);
        let duration_seconds = state.duration_seconds().unwrap_or(0);
        let audio_path = state.audio_path.clone().unwrap_or_default();

        // Stop sources and throw away whatever samples we collected.
        let _ = self.mic_source.stop();
        let _ = self.system_source.stop();

        // If the partial WAV file was created (unlikely since we write on
        // stop, but we may in the future), clean it up.
        if audio_path.exists() {
            if let Err(e) = std::fs::remove_file(&audio_path) {
                warn!(
                    "Failed to remove partial meeting WAV {:?}: {}",
                    audio_path, e
                );
            }
        }

        // Persist cancelled status.
        if let Ok(conn) = db::init_db() {
            let _ = MeetingRepository::cancel(&conn, meeting_id, duration_seconds as i64);
        }

        self.status.cancelled().await;
        self.status.reset().await;

        info!(
            "Meeting {} cancelled after {}s",
            meeting_id, duration_seconds
        );

        Ok(MeetingStopResult {
            meeting_id,
            duration_seconds,
        })
    }

    /// Toggle meeting recording.
    pub async fn toggle(&mut self, options: Option<MeetingStartOptions>) -> Result<ToggleOutcome> {
        let state = self.status.get().await;
        match state.phase {
            MeetingPhase::Recording => {
                let result = self.stop().await?;
                Ok(ToggleOutcome::Stopped(result))
            }
            MeetingPhase::Idle
            | MeetingPhase::Completed
            | MeetingPhase::Error
            | MeetingPhase::Cancelled => {
                let result = self.start(options).await?;
                Ok(ToggleOutcome::Started(result))
            }
            phase => {
                bail!(
                    "Cannot toggle meeting while {} — please wait",
                    phase.as_str()
                );
            }
        }
    }

    fn write_wav(&self, path: &Path, samples: &[f32], sample_rate: u32) -> Result<()> {
        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let mut writer = WavWriter::create(path, spec)?;
        for &sample in samples {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;

        info!(
            "Meeting audio saved: {:?} ({} samples)",
            path,
            samples.len()
        );
        Ok(())
    }

    fn generate_audio_path(&self) -> PathBuf {
        let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        // The random suffix keeps two meetings created within the same second
        // on distinct paths — a live daemon only runs one meeting at a time,
        // but parallel test threads can collide. Without it both WAVs (and the
        // temp mp3s derived from those filenames) would clobber each other.
        let unique = uuid::Uuid::new_v4().simple();
        self.meetings_dir
            .join(format!("meeting-{timestamp}-{unique}.wav"))
    }
}

/// Outcome of a toggle operation.
pub enum ToggleOutcome {
    Started(MeetingStartResult),
    Stopped(MeetingStopResult),
}

/// Everything the background post-processing task needs.
/// All fields are `Send + Sync` (or cheaply clonable) so the whole struct can
/// move into a `tokio::spawn` without borrowing the `MeetingMachine` (which
/// is `!Send` due to `cpal::Stream` in `MicAudioSource`).
struct ProcessContext {
    meeting_id: i64,
    audio_path: PathBuf,
    title: Option<String>,
    duration_seconds: u64,
    status: MeetingStatusHandle,
    transcription: Arc<dyn TranscriptionJobService>,
    hook: Option<Arc<dyn PostMeetingHook>>,
    indicator: Indicator,
}

/// Run the post-recording pipeline: compress → transcribe → persist → hook.
///
/// Updates `status`, DB row, and fires indicator notifications for every phase
/// transition. Errors at any stage are persisted and surfaced via an error
/// notification; the meeting row is left in a terminal (`completed` or
/// `error`) state before this function returns.
async fn run_processing_task(ctx: ProcessContext) {
    info!("Compressing meeting audio: {:?}", ctx.audio_path);
    let (temp_upload, temp_to_cleanup) = match prepare_for_upload(&ctx.audio_path, false) {
        Ok(v) => v,
        Err(e) => {
            let error_msg = e.to_string();
            error!(
                "Meeting {} compression failed: {}",
                ctx.meeting_id, error_msg
            );
            if let Ok(conn) = db::init_db() {
                let _ = MeetingRepository::fail(
                    &conn,
                    ctx.meeting_id,
                    &error_msg,
                    ctx.duration_seconds as i64,
                );
            }
            ctx.status.set_error(error_msg.clone()).await;
            if let Err(e) = ctx.indicator.show_error(&error_msg).await {
                warn!("Failed to show error indicator: {}", e);
            }
            return;
        }
    };

    // Move the compressed mp3 next to the original WAV via copy (cross-fs safe,
    // unlike rename — the temp dir is typically on tmpfs while the meetings dir
    // lives under ~/.local/share). The durable mp3 is what the post-meeting
    // hook and history reference. Drop the WAV once the mp3 is in place.
    let durable_mp3 = if temp_to_cleanup.is_some() {
        let durable = ctx.audio_path.with_extension("mp3");
        match std::fs::copy(&temp_upload, &durable) {
            Ok(_) => {
                if let Err(e) = std::fs::remove_file(&ctx.audio_path) {
                    warn!("Failed to delete original WAV: {}", e);
                }
                durable
            }
            Err(e) => {
                warn!("Failed to copy compressed mp3 next to WAV: {}", e);
                ctx.audio_path.clone()
            }
        }
    } else {
        temp_upload.clone()
    };

    info!("Compressed meeting audio at: {:?}", durable_mp3);

    ctx.status.set_phase(MeetingPhase::Transcribing).await;
    if let Ok(conn) = db::init_db() {
        let _ = MeetingRepository::update_status(&conn, ctx.meeting_id, MeetingPhase::Transcribing);
        // Keep the DB row pointing at the file that actually exists. The WAV
        // is gone after a successful copy; retries / file UI need the .mp3
        // path or they'll error out trying to read a deleted file.
        if durable_mp3 != ctx.audio_path {
            let _ = MeetingRepository::update_audio_path(
                &conn,
                ctx.meeting_id,
                &durable_mp3.to_string_lossy(),
            );
        }
    }

    let transcription_result = ctx.transcription.submit_and_poll(&temp_upload, None).await;

    if let Some(temp) = &temp_to_cleanup {
        cleanup_temp_file(temp);
    }

    match transcription_result {
        Ok(result) => {
            let transcript_path = durable_mp3.with_extension("txt");
            if let Err(e) = std::fs::write(&transcript_path, &result.text) {
                error!("Failed to write transcript file: {}", e);
            }

            if let Ok(conn) = db::init_db() {
                let _ = MeetingRepository::complete(
                    &conn,
                    ctx.meeting_id,
                    &transcript_path.to_string_lossy(),
                    &result.text,
                    ctx.duration_seconds as i64,
                );
            }

            info!(
                "Meeting {} transcription complete: {} chars",
                ctx.meeting_id,
                result.text.len()
            );

            // Optional post-meeting hook — failures don't flip the meeting to
            // `error` because the transcription itself succeeded.
            if let Some(hook) = &ctx.hook {
                ctx.status.set_phase(MeetingPhase::RunningHook).await;
                let meeting_result = MeetingResult {
                    meeting_id: ctx.meeting_id,
                    title: ctx.title,
                    audio_path: durable_mp3,
                    transcript_path,
                    transcript_text: result.text.clone(),
                    duration_seconds: ctx.duration_seconds,
                };

                if let Err(e) = hook.execute(&meeting_result).await {
                    warn!("Post-meeting hook failed: {}", e);
                }
            }

            ctx.status.complete().await;

            // Final "complete" notification with transcript preview.
            if let Err(e) = ctx.indicator.show_complete(&result.text).await {
                warn!("Failed to show completion indicator: {}", e);
            }
        }
        Err(e) => {
            error!("Meeting {} transcription failed: {}", ctx.meeting_id, e);
            let error_msg = e.to_string();

            if let Ok(conn) = db::init_db() {
                let _ = MeetingRepository::fail(
                    &conn,
                    ctx.meeting_id,
                    &error_msg,
                    ctx.duration_seconds as i64,
                );
            }

            ctx.status.set_error(error_msg.clone()).await;

            if let Err(e) = ctx.indicator.show_error(&error_msg).await {
                warn!("Failed to show error indicator: {}", e);
            }
        }
    }
}

/// Re-run transcription for an existing meeting whose audio file is still on
/// disk. Used by `POST /meetings/:id/retry` after a failed transcription
/// (e.g. backend timeout) so the user doesn't have to re-record. Skips the
/// compress step entirely — the durable mp3 from the original run is the
/// upload payload.
///
/// Updates the DB row to `transcribing` immediately, then `completed` or
/// `error` once the polling resolves. Writes the transcript to a `.txt`
/// alongside the audio. Does NOT touch the live `MeetingStatusHandle` — that
/// reflects the active recording machine, which a retry must not interfere
/// with.
pub async fn retry_meeting_transcription(
    meeting_id: i64,
    audio_path: PathBuf,
    duration_seconds: i64,
    transcription: Arc<dyn TranscriptionJobService>,
) {
    info!(
        "Retrying transcription for meeting {} from {:?}",
        meeting_id, audio_path
    );

    if let Ok(conn) = db::init_db() {
        if let Err(e) =
            MeetingRepository::update_status(&conn, meeting_id, MeetingPhase::Transcribing)
        {
            warn!("Failed to mark meeting {} transcribing: {}", meeting_id, e);
        }
    }

    let result = transcription.submit_and_poll(&audio_path, None).await;

    match result {
        Ok(r) => {
            let transcript_path = audio_path.with_extension("txt");
            if let Err(e) = std::fs::write(&transcript_path, &r.text) {
                warn!("Failed to write transcript file: {}", e);
            }

            if let Ok(conn) = db::init_db() {
                if let Err(e) = MeetingRepository::complete(
                    &conn,
                    meeting_id,
                    &transcript_path.to_string_lossy(),
                    &r.text,
                    duration_seconds,
                ) {
                    error!("Failed to mark meeting {} completed: {}", meeting_id, e);
                }
            }

            info!(
                "Meeting {} retry transcription complete: {} chars",
                meeting_id,
                r.text.len()
            );
        }
        Err(e) => {
            error!("Meeting {} retry transcription failed: {}", meeting_id, e);
            let error_msg = e.to_string();
            if let Ok(conn) = db::init_db() {
                let _ = MeetingRepository::fail(&conn, meeting_id, &error_msg, duration_seconds);
            }
        }
    }
}
