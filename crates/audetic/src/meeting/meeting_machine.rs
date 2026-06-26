//! Meeting lifecycle orchestrator for live recordings.
//!
//! Owns mic + system audio capture and writes the final WAV. Post-recording
//! processing (compress → transcribe → write transcript → dispatch
//! `meeting.completed` to user-defined jobs → mark completed) is delegated
//! to `meeting::processing::process_meeting`, which is the shared pipeline
//! reused by retries and imported media files. Phase transitions during
//! processing are forwarded to the singleton `MeetingStatusHandle` and the
//! `Indicator` via `LiveProgressObserver`.

use anyhow::{bail, Context, Result};
use hound::{WavSpec, WavWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info, warn};

use crate::audio::audio_mixer::AudioMixer;
use crate::audio::audio_source::AudioSource;
use crate::db::{self, meetings::MeetingRepository};
use crate::post_processing::PostProcessingService;
use crate::transcription::job_service::TranscriptionJobService;
use crate::ui::Indicator;

use super::processing::{process_meeting, ProcessingArgs, ProcessingServices};
use super::progress::LiveProgressObserver;
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
    post_processing: Arc<PostProcessingService>,
    indicator: Indicator,
    status: MeetingStatusHandle,
    meetings_dir: PathBuf,
}

impl MeetingMachine {
    pub fn new(
        mic_source: Box<dyn AudioSource>,
        system_source: Box<dyn AudioSource>,
        transcription: Arc<dyn TranscriptionJobService>,
        post_processing: Arc<PostProcessingService>,
        indicator: Indicator,
        status: MeetingStatusHandle,
        meetings_dir: PathBuf,
    ) -> Self {
        Self {
            mic_source,
            system_source,
            transcription,
            post_processing,
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
    /// Halts audio sources, mixes the captured samples, and writes the WAV
    /// file, then transitions the meeting into the `Review` phase. The
    /// recording is **not** sent for transcription yet — the user reviews it
    /// (and may trim the start/end) and then calls `confirm`, or discards it
    /// with `cancel`. Returns `MeetingStopResult` immediately after the WAV is
    /// written so the HTTP caller unblocks within milliseconds.
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

        // Pause for user review instead of transcribing immediately. Persist
        // the captured duration and freeze the live timer so the UI shows the
        // recording's length (and the trim end bound). The user proceeds via
        // `confirm` (optionally trimming) or discards via `cancel`.
        if let Ok(conn) = db::init_db() {
            if let Err(e) =
                MeetingRepository::set_review(&conn, meeting_id, duration_seconds as i64)
            {
                warn!(
                    "Failed to persist review status for meeting {}: {}",
                    meeting_id, e
                );
            }
        }
        self.status.enter_review(duration_seconds).await;
        if let Err(e) = self.indicator.show_review().await {
            warn!("Failed to show review indicator: {}", e);
        }

        Ok(MeetingStopResult {
            meeting_id,
            duration_seconds,
        })
    }

    /// Confirm a meeting that is awaiting review and send it for
    /// transcription, optionally trimming the recording to the half-open range
    /// `[start_seconds, end_seconds)` first. Either bound may be omitted to
    /// keep that edge; both omitted sends the recording untouched. Trimming
    /// slices the lossless WAV by sample index, so the cut is exactly
    /// sample-accurate. Returns an error unless a meeting is currently
    /// awaiting review.
    pub async fn confirm(
        &mut self,
        start_seconds: Option<f64>,
        end_seconds: Option<f64>,
    ) -> Result<MeetingStopResult> {
        let state = self.status.get().await;
        if state.phase != MeetingPhase::Review {
            bail!(
                "No meeting awaiting review (current phase: {})",
                state.phase.as_str()
            );
        }

        let meeting_id = state.meeting_id.unwrap_or(0);
        let audio_path = state.audio_path.clone().unwrap_or_default();
        let title = state.title.clone();
        let mut duration_seconds = state.duration_seconds().unwrap_or(0);

        // Apply the trim if either boundary was provided.
        if start_seconds.is_some() || end_seconds.is_some() {
            let trimmed = Self::trim_wav(&audio_path, start_seconds, end_seconds)
                .context("Failed to trim meeting audio")?;
            duration_seconds = trimmed.round().max(0.0) as u64;
            info!(
                "Meeting {} trimmed to {:.3}s (start={:?}, end={:?})",
                meeting_id, trimmed, start_seconds, end_seconds
            );
        }

        self.spawn_processing(meeting_id, audio_path, title, duration_seconds)
            .await;

        Ok(MeetingStopResult {
            meeting_id,
            duration_seconds,
        })
    }

    /// Build and spawn the compress → transcribe → dispatch pipeline for a
    /// meeting whose WAV is already on disk. Transitions the live status to
    /// `Compressing` and shows the processing indicator first so the status is
    /// coherent the moment this returns; the pipeline itself runs detached.
    /// `LiveProgressObserver` forwards later phase transitions to the
    /// singleton status handle and the indicator.
    async fn spawn_processing(
        &self,
        meeting_id: i64,
        audio_path: PathBuf,
        title: Option<String>,
        duration_seconds: u64,
    ) {
        self.status.set_phase(MeetingPhase::Compressing).await;
        if let Err(e) = self.indicator.show_processing().await {
            warn!("Failed to show processing indicator: {}", e);
        }

        let observer = Arc::new(LiveProgressObserver::new(
            self.status.clone(),
            self.indicator.clone(),
        ));
        let args = ProcessingArgs {
            meeting_id,
            audio_path,
            title,
            duration_seconds,
            services: ProcessingServices {
                transcription: Arc::clone(&self.transcription),
                post_processing: Arc::clone(&self.post_processing),
            },
            observer,
        };
        tokio::spawn(async move { process_meeting(args).await });
    }

    /// Cancel a meeting without running the transcription pipeline.
    ///
    /// Works while still `Recording` (discards captured samples) or while
    /// awaiting `Review` (discards the recorded WAV). Deletes any WAV on disk
    /// and marks the meeting `cancelled` in the DB. Returns an error if no
    /// meeting is recording or awaiting review.
    pub async fn cancel(&mut self) -> Result<MeetingStopResult> {
        let state = self.status.get().await;
        if !matches!(state.phase, MeetingPhase::Recording | MeetingPhase::Review) {
            bail!(
                "No meeting recording or awaiting review to cancel (current phase: {})",
                state.phase.as_str()
            );
        }

        let meeting_id = state.meeting_id.unwrap_or(0);
        let duration_seconds = state.duration_seconds().unwrap_or(0);
        let audio_path = state.audio_path.clone().unwrap_or_default();

        // Stop sources only if still recording — from Review they were already
        // halted by `stop()` and the WAV is finalized on disk.
        if state.phase == MeetingPhase::Recording {
            let _ = self.mic_source.stop();
            let _ = self.system_source.stop();
        }

        // Remove the recorded WAV if present (always present in Review).
        if audio_path.exists() {
            if let Err(e) = std::fs::remove_file(&audio_path) {
                warn!("Failed to remove meeting WAV {:?}: {}", audio_path, e);
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

    /// Trim a mono float WAV in place to the half-open range
    /// `[start_seconds, end_seconds)`. Missing bounds default to the start/end
    /// of the file; out-of-range values are clamped. Because the source is
    /// lossless PCM, slicing by sample index (`round(seconds * sample_rate)`)
    /// is exactly sample-accurate — there is no ffmpeg keyframe seeking
    /// involved. Returns the trimmed duration in seconds.
    fn trim_wav(path: &Path, start_seconds: Option<f64>, end_seconds: Option<f64>) -> Result<f64> {
        let mut reader = hound::WavReader::open(path)
            .with_context(|| format!("Failed to open WAV for trimming: {path:?}"))?;
        let spec = reader.spec();
        let sample_rate = spec.sample_rate;

        let samples: Vec<f32> = reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<f32>, _>>()
            .context("Failed to read WAV samples")?;
        let total = samples.len();

        let to_index = |secs: f64| -> usize {
            ((secs.max(0.0) * sample_rate as f64).round() as usize).min(total)
        };
        let start_idx = start_seconds.map(to_index).unwrap_or(0);
        let end_idx = end_seconds.map(to_index).unwrap_or(total);

        if start_idx >= end_idx {
            bail!(
                "Invalid trim range: start ({:?}s) is at or after end ({:?}s)",
                start_seconds,
                end_seconds
            );
        }

        let trimmed = &samples[start_idx..end_idx];

        let mut writer = WavWriter::create(path, spec)
            .with_context(|| format!("Failed to rewrite trimmed WAV: {path:?}"))?;
        for &sample in trimmed {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;

        info!(
            "Trimmed WAV {:?}: {} → {} samples ([{}, {}))",
            path,
            total,
            trimmed.len(),
            start_idx,
            end_idx
        );

        Ok(trimmed.len() as f64 / sample_rate as f64)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_wav(path: &Path, samples: &[f32], sample_rate: u32) {
        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = WavWriter::create(path, spec).unwrap();
        for &s in samples {
            writer.write_sample(s).unwrap();
        }
        writer.finalize().unwrap();
    }

    fn read_wav(path: &Path) -> Vec<f32> {
        let mut reader = hound::WavReader::open(path).unwrap();
        reader.samples::<f32>().map(|s| s.unwrap()).collect()
    }

    fn temp_wav_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "audetic-trim-test-{}.wav",
            uuid::Uuid::new_v4().simple()
        ))
    }

    #[test]
    fn test_trim_wav_slices_by_sample_index() {
        let sample_rate = 16000u32;
        // 4 seconds of a ramp so we can assert exact sample boundaries.
        let samples: Vec<f32> = (0..sample_rate * 4).map(|i| i as f32).collect();
        let path = temp_wav_path();
        write_test_wav(&path, &samples, sample_rate);

        // Trim to [1.0s, 3.0s) → exactly 2 seconds == 32000 samples.
        let dur = MeetingMachine::trim_wav(&path, Some(1.0), Some(3.0)).unwrap();
        assert!((dur - 2.0).abs() < 1e-9);

        let out = read_wav(&path);
        assert_eq!(out.len(), (sample_rate * 2) as usize);
        // First kept sample is index 16000, last is index 47999 of the ramp.
        assert_eq!(out[0], sample_rate as f32);
        assert_eq!(out[out.len() - 1], (sample_rate * 3 - 1) as f32);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_trim_wav_defaults_and_clamps_out_of_range_end() {
        let sample_rate = 16000u32;
        let samples: Vec<f32> = (0..sample_rate * 2).map(|i| i as f32).collect();
        let path = temp_wav_path();
        write_test_wav(&path, &samples, sample_rate);

        // Only an end is given, past the end of the file → clamps to full length.
        let dur = MeetingMachine::trim_wav(&path, None, Some(99.0)).unwrap();
        assert!((dur - 2.0).abs() < 1e-9);
        assert_eq!(read_wav(&path).len(), (sample_rate * 2) as usize);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_trim_wav_rejects_inverted_range() {
        let sample_rate = 16000u32;
        let samples: Vec<f32> = (0..sample_rate * 2).map(|i| i as f32).collect();
        let path = temp_wav_path();
        write_test_wav(&path, &samples, sample_rate);

        assert!(MeetingMachine::trim_wav(&path, Some(1.5), Some(1.0)).is_err());

        std::fs::remove_file(&path).ok();
    }
}
