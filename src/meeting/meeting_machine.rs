//! Meeting lifecycle orchestrator.
//!
//! Manages the full meeting recording pipeline:
//! start → stop → compress → transcribe → save → hook → done
//!
//! All dependencies are injected via constructor — no concrete types hardcoded.

use anyhow::{bail, Result};
use hound::{WavSpec, WavWriter};
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

use crate::audio::audio_mixer::AudioMixer;
use crate::audio::audio_source::AudioSource;
use crate::cli::compression::compress_for_transcription;
use crate::db::{self, meetings::MeetingRepository};
use crate::transcription::job_service::TranscriptionJobService;

use super::post_meeting_hook::{MeetingResult, PostMeetingHook};
use super::status::{MeetingPhase, MeetingStartOptions, MeetingStatusHandle};

/// Result returned from stopping a meeting.
pub struct MeetingStopResult {
    pub meeting_id: i64,
    pub duration_seconds: u64,
}

/// Result returned from starting a meeting.
pub struct MeetingStartResult {
    pub meeting_id: i64,
    pub audio_path: PathBuf,
}

pub struct MeetingMachine {
    mic_source: Box<dyn AudioSource>,
    system_source: Box<dyn AudioSource>,
    transcription: Box<dyn TranscriptionJobService>,
    hook: Option<Box<dyn PostMeetingHook>>,
    status: MeetingStatusHandle,
    meetings_dir: PathBuf,
}

impl MeetingMachine {
    pub fn new(
        mic_source: Box<dyn AudioSource>,
        system_source: Box<dyn AudioSource>,
        transcription: Box<dyn TranscriptionJobService>,
        hook: Option<Box<dyn PostMeetingHook>>,
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
            status,
            meetings_dir,
        }
    }

    /// Start a meeting recording.
    pub async fn start(&mut self, options: Option<MeetingStartOptions>) -> Result<MeetingStartResult> {
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
            MeetingRepository::insert(
                &conn,
                opts.title.as_deref(),
                &audio_path.to_string_lossy(),
            )?
        };

        // Start audio sources
        self.mic_source.start()?;

        if let Err(e) = self.system_source.start() {
            warn!("Failed to start system audio: {}. Recording mic only.", e);
        }

        self.status
            .start_recording(meeting_id, opts.title, audio_path.clone())
            .await;

        info!("Meeting {} recording started: {:?}", meeting_id, audio_path);

        Ok(MeetingStartResult {
            meeting_id,
            audio_path,
        })
    }

    /// Stop the meeting recording and spawn background processing.
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
            self.status.set_error("No audio captured".to_string()).await;
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

        // Process inline: compress → transcribe → save → hook
        self.process_meeting(meeting_id, audio_path.clone(), title, duration_seconds)
            .await;

        Ok(MeetingStopResult {
            meeting_id,
            duration_seconds,
        })
    }

    /// Toggle meeting recording.
    pub async fn toggle(
        &mut self,
        options: Option<MeetingStartOptions>,
    ) -> Result<ToggleOutcome> {
        let state = self.status.get().await;
        match state.phase {
            MeetingPhase::Recording => {
                let result = self.stop().await?;
                Ok(ToggleOutcome::Stopped(result))
            }
            MeetingPhase::Idle | MeetingPhase::Completed | MeetingPhase::Error => {
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

    /// Run post-recording processing: compress → transcribe → save → hook
    async fn process_meeting(
        &self,
        meeting_id: i64,
        audio_path: PathBuf,
        title: Option<String>,
        duration_seconds: u64,
    ) {
        // Phase: Compressing
        self.status.set_phase(MeetingPhase::Compressing).await;
        let compressed_path = match self.compress_audio(&audio_path) {
            Ok(path) => {
                // Delete original WAV after successful compression
                if let Err(e) = std::fs::remove_file(&audio_path) {
                    warn!("Failed to delete original WAV: {}", e);
                }
                path
            }
            Err(e) => {
                warn!("Compression failed, using WAV: {}", e);
                audio_path.clone()
            }
        };

        // Phase: Transcribing
        self.status.set_phase(MeetingPhase::Transcribing).await;
        {
            let conn = db::init_db().ok();
            if let Some(conn) = &conn {
                let _ = MeetingRepository::update_status(conn, meeting_id, MeetingPhase::Transcribing);
            }
        }

        let transcription_result = self
            .transcription
            .submit_and_poll(&compressed_path, None)
            .await;

        match transcription_result {
            Ok(result) => {
                // Save transcript file
                let transcript_path = compressed_path.with_extension("txt");
                if let Err(e) = std::fs::write(&transcript_path, &result.text) {
                    error!("Failed to write transcript file: {}", e);
                }

                // Update DB
                {
                    let conn = db::init_db().ok();
                    if let Some(conn) = &conn {
                        let _ = MeetingRepository::complete(
                            conn,
                            meeting_id,
                            &transcript_path.to_string_lossy(),
                            &result.text,
                            duration_seconds as i64,
                        );
                    }
                }

                info!(
                    "Meeting {} transcription complete: {} chars",
                    meeting_id,
                    result.text.len()
                );

                // Phase: RunningHook
                if let Some(hook) = &self.hook {
                    self.status.set_phase(MeetingPhase::RunningHook).await;
                    let meeting_result = MeetingResult {
                        meeting_id,
                        title,
                        audio_path: compressed_path,
                        transcript_path,
                        transcript_text: result.text,
                        duration_seconds,
                    };

                    if let Err(e) = hook.execute(&meeting_result).await {
                        warn!("Post-meeting hook failed: {}", e);
                        // Hook failure does NOT affect meeting status
                    }
                }

                self.status.complete().await;
            }
            Err(e) => {
                error!("Meeting {} transcription failed: {}", meeting_id, e);
                let error_msg = e.to_string();

                {
                    let conn = db::init_db().ok();
                    if let Some(conn) = &conn {
                        let _ = MeetingRepository::fail(conn, meeting_id, &error_msg);
                    }
                }

                self.status.set_error(error_msg).await;
            }
        }
    }

    fn compress_audio(&self, wav_path: &Path) -> Result<PathBuf> {
        info!("Compressing meeting audio: {:?}", wav_path);
        let compressed = compress_for_transcription(wav_path)?;

        // Move compressed file to meetings directory with matching name
        let final_path = wav_path.with_extension("mp3");
        std::fs::rename(&compressed, &final_path)?;

        info!("Compressed to: {:?}", final_path);
        Ok(final_path)
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

        info!("Meeting audio saved: {:?} ({} samples)", path, samples.len());
        Ok(())
    }

    fn generate_audio_path(&self) -> PathBuf {
        let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let filename = format!("meeting-{}.wav", timestamp);
        let path = self.meetings_dir.join(&filename);

        // Handle collision by appending counter
        if path.exists() {
            for i in 1..100 {
                let filename = format!("meeting-{}-{}.wav", timestamp, i);
                let alt_path = self.meetings_dir.join(&filename);
                if !alt_path.exists() {
                    return alt_path;
                }
            }
        }

        path
    }
}

/// Outcome of a toggle operation.
pub enum ToggleOutcome {
    Started(MeetingStartResult),
    Stopped(MeetingStopResult),
}
