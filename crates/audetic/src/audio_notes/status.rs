//! AudioNote status types and shared state handle.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::audio::capture_recovery::CaptureRecovery;

/// Phase of a meeting recording lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AudioNotePhase {
    Idle,
    Recording,
    /// Recording has stopped and the WAV is on disk, but the user has not yet
    /// confirmed it for transcription. They can play it back and trim the
    /// start/end before sending it on (or discard it). See
    /// `AudioNoteMachine::confirm`.
    Review,
    Compressing,
    Transcribing,
    Completed,
    Error,
    Cancelled,
}

impl AudioNotePhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Recording => "recording",
            Self::Review => "review",
            Self::Compressing => "compressing",
            Self::Transcribing => "transcribing",
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    /// Stored `status` strings considered terminal (settled) and therefore safe
    /// to soft-delete. Single source of truth shared by [`Self::is_terminal`]
    /// and the guarded `DELETE` SQL in `AudioNoteRepository::soft_delete`, so the
    /// Rust check and the SQL predicate can't drift apart. Recording, review,
    /// and the processing phases are deliberately absent: while in-flight, the
    /// meeting machine and background pipeline still hold the id, so hiding the
    /// row would 404 the active/review UI (`/audio-notes/:id/audio` and detail)
    /// and break completion auto-nav.
    pub const TERMINAL_STATUSES: [&'static str; 3] = ["completed", "error", "cancelled"];

    /// Whether a meeting with this stored `status` is settled and therefore
    /// safe to soft-delete. Allow-lists terminal states so any future in-flight
    /// phase defaults to non-deletable.
    pub fn is_terminal(status: &str) -> bool {
        Self::TERMINAL_STATUSES.contains(&status)
    }
}

/// Options for starting a meeting.
#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AudioNoteStartOptions {
    pub title: Option<String>,
    pub capture_source: AudioNoteCaptureSource,
    pub review_before_processing: bool,
    pub auto_paste: bool,
    pub copy_to_clipboard: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AudioNoteCaptureSource {
    #[default]
    Microphone,
    MicrophoneAndSystem,
}

impl AudioNoteCaptureSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::MicrophoneAndSystem => "microphone_and_system",
        }
    }
}

/// Current meeting state, readable by API handlers.
#[derive(Debug, Clone)]
pub struct AudioNoteState {
    pub phase: AudioNotePhase,
    pub capture_degraded: bool,
    pub note_id: Option<i64>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub title: Option<String>,
    pub audio_path: Option<PathBuf>,
    pub last_error: Option<String>,
    /// Recorded length frozen at stop. Once set (Review onward), it is the
    /// duration reported to clients so the timer stops climbing and the trim
    /// UI has an accurate end bound.
    pub recorded_duration_seconds: Option<u64>,
    microphone_capturing: bool,
    system_capturing: bool,
    system_expected: bool,
}

impl Default for AudioNoteState {
    fn default() -> Self {
        Self {
            phase: AudioNotePhase::Idle,
            capture_degraded: false,
            note_id: None,
            started_at: None,
            title: None,
            audio_path: None,
            last_error: None,
            recorded_duration_seconds: None,
            microphone_capturing: false,
            system_capturing: false,
            system_expected: true,
        }
    }
}

impl AudioNoteState {
    /// Duration of the meeting in seconds. While recording this is the live
    /// elapsed time; once the recording is frozen (Review onward) it is the
    /// captured length set at stop.
    pub fn duration_seconds(&self) -> Option<u64> {
        if let Some(frozen) = self.recorded_duration_seconds {
            return Some(frozen);
        }
        self.started_at.map(|started| {
            let elapsed = chrono::Utc::now() - started;
            elapsed.num_seconds().max(0) as u64
        })
    }

    fn update_capture_degraded(&mut self) {
        self.capture_degraded = self.phase == AudioNotePhase::Recording
            && (!self.microphone_capturing || (self.system_expected && !self.system_capturing));
    }

    fn clear_capture_health(&mut self) {
        self.capture_degraded = false;
        self.microphone_capturing = false;
        self.system_capturing = false;
    }
}

/// Thread-safe handle for sharing meeting state between the machine and API handlers.
#[derive(Clone, Default)]
pub struct AudioNoteStatusHandle {
    inner: Arc<Mutex<AudioNoteState>>,
}

impl AudioNoteStatusHandle {
    pub async fn set_capture_source(&self, source: AudioNoteCaptureSource) {
        let mut state = self.inner.lock().await;
        state.system_expected = source == AudioNoteCaptureSource::MicrophoneAndSystem;
        state.update_capture_degraded();
    }

    /// Compare-and-update under the same lock: an old pipeline must not change
    /// a newer capture's phase or error.
    pub async fn progress_if_current(
        &self,
        note_id: i64,
        phase: AudioNotePhase,
        error: Option<String>,
    ) -> bool {
        let mut state = self.inner.lock().await;
        if state.note_id != Some(note_id) {
            return false;
        }
        state.phase = phase;
        state.last_error = error;
        if phase != AudioNotePhase::Recording {
            state.clear_capture_health();
        }
        true
    }
    pub async fn get(&self) -> AudioNoteState {
        self.inner.lock().await.clone()
    }

    pub async fn start_recording(
        &self,
        note_id: i64,
        title: Option<String>,
        audio_path: PathBuf,
        microphone_capturing: bool,
        system_capturing: bool,
    ) {
        let mut state = self.inner.lock().await;
        state.phase = AudioNotePhase::Recording;
        state.note_id = Some(note_id);
        state.started_at = Some(chrono::Utc::now());
        state.title = title;
        state.audio_path = Some(audio_path);
        state.last_error = None;
        // Clear any duration frozen by a previous meeting's Review phase;
        // otherwise the new recording inherits the old meeting's length (the
        // live timer freezes and the trim UI gets a bogus end bound).
        state.recorded_duration_seconds = None;
        state.microphone_capturing = microphone_capturing;
        state.system_capturing = system_capturing;
        state.update_capture_degraded();
    }

    pub async fn set_phase(&self, phase: AudioNotePhase) {
        let mut state = self.inner.lock().await;
        state.phase = phase;
        if phase != AudioNotePhase::Recording {
            state.clear_capture_health();
        }
    }

    /// Keep the live status snapshot aligned when the current meeting is
    /// renamed through the repository-backed HTTP endpoint.
    pub async fn set_title_if_current(&self, note_id: i64, title: Option<String>) {
        let mut state = self.inner.lock().await;
        if state.note_id == Some(note_id) {
            state.title = title;
        }
    }

    /// Transition into the Review phase, freezing the recorded duration so the
    /// reported timer stops climbing and the trim UI knows the end bound.
    pub async fn enter_review(&self, duration_seconds: u64) {
        let mut state = self.inner.lock().await;
        state.phase = AudioNotePhase::Review;
        state.recorded_duration_seconds = Some(duration_seconds);
        state.last_error = None;
        state.clear_capture_health();
    }

    pub async fn set_error(&self, error: String) {
        let mut state = self.inner.lock().await;
        state.phase = AudioNotePhase::Error;
        state.last_error = Some(error);
        state.clear_capture_health();
    }

    pub async fn reset(&self) {
        let mut state = self.inner.lock().await;
        *state = AudioNoteState::default();
    }

    /// Reset to Idle, but only if the state still describes the given meeting.
    ///
    /// Called after a soft-delete so `GET /audio-notes/status` stops reporting a
    /// meeting that is hidden everywhere else. Check-and-reset happens under a
    /// single lock acquisition: meeting ids are never reused and the delete's
    /// SQL guard only hides terminal rows, so an id match here can only be the
    /// settled meeting the machine has finished with — never a live recording
    /// that started after the delete was accepted. Returns whether the state
    /// was cleared.
    pub async fn clear_if_current(&self, note_id: i64) -> bool {
        let mut state = self.inner.lock().await;
        if state.note_id != Some(note_id) {
            return false;
        }
        *state = AudioNoteState::default();
        true
    }

    pub async fn complete(&self) {
        let mut state = self.inner.lock().await;
        state.phase = AudioNotePhase::Completed;
        state.clear_capture_health();
    }

    pub async fn cancelled(&self) {
        let mut state = self.inner.lock().await;
        state.phase = AudioNotePhase::Cancelled;
        state.clear_capture_health();
    }

    pub(crate) async fn apply_microphone_recovery(&self, recovery: CaptureRecovery) {
        let mut state = self.inner.lock().await;
        if state.phase != AudioNotePhase::Recording {
            return;
        }
        match recovery {
            CaptureRecovery::Ignored => return,
            CaptureRecovery::Capturing => state.microphone_capturing = true,
            CaptureRecovery::Degraded => state.microphone_capturing = false,
        }
        state.update_capture_degraded();
    }

    pub(crate) async fn mark_microphone_degraded(&self) {
        let mut state = self.inner.lock().await;
        if state.phase != AudioNotePhase::Recording {
            return;
        }
        state.microphone_capturing = false;
        state.update_capture_degraded();
    }

    pub(crate) async fn apply_system_recovery(&self, recovery: CaptureRecovery) {
        let mut state = self.inner.lock().await;
        if state.phase != AudioNotePhase::Recording {
            return;
        }
        match recovery {
            CaptureRecovery::Ignored => return,
            CaptureRecovery::Capturing => state.system_capturing = true,
            CaptureRecovery::Degraded => state.system_capturing = false,
        }
        state.update_capture_degraded();
    }

    pub(crate) async fn mark_system_degraded(&self) {
        let mut state = self.inner.lock().await;
        if state.phase != AudioNotePhase::Recording {
            return;
        }
        state.system_capturing = false;
        state.update_capture_degraded();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_defaults_are_microphone_without_review_or_delivery() {
        let options: AudioNoteStartOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(options.capture_source, AudioNoteCaptureSource::Microphone);
        assert!(!options.review_before_processing);
        assert!(!options.auto_paste);
        assert!(!options.copy_to_clipboard);
        assert!(
            serde_json::from_str::<AudioNoteStartOptions>(r#"{"capture_source":"import"}"#)
                .is_err()
        );
    }

    #[tokio::test]
    async fn microphone_only_does_not_report_missing_system_capture() {
        let status = AudioNoteStatusHandle::default();
        status
            .start_recording(1, None, "/tmp/mic.wav".into(), true, false)
            .await;
        status
            .set_capture_source(AudioNoteCaptureSource::Microphone)
            .await;
        assert!(!status.get().await.capture_degraded);
        status.mark_microphone_degraded().await;
        assert!(status.get().await.capture_degraded);
    }

    #[tokio::test]
    async fn prior_background_processing_does_not_overwrite_a_new_capture() {
        let status = AudioNoteStatusHandle::default();
        status
            .start_recording(2, None, "/tmp/new.wav".into(), true, false)
            .await;
        assert!(
            !status
                .progress_if_current(1, AudioNotePhase::Completed, None)
                .await
        );
        assert!(
            !status
                .progress_if_current(1, AudioNotePhase::Error, Some("old error".into()))
                .await
        );
        assert_eq!(status.get().await.phase, AudioNotePhase::Recording);
    }

    #[test]
    fn test_audio_note_phase_as_str() {
        assert_eq!(AudioNotePhase::Idle.as_str(), "idle");
        assert_eq!(AudioNotePhase::Recording.as_str(), "recording");
        assert_eq!(AudioNotePhase::Review.as_str(), "review");
        assert_eq!(AudioNotePhase::Compressing.as_str(), "compressing");
        assert_eq!(AudioNotePhase::Transcribing.as_str(), "transcribing");
        assert_eq!(AudioNotePhase::Completed.as_str(), "completed");
        assert_eq!(AudioNotePhase::Error.as_str(), "error");
    }

    #[test]
    fn test_terminal_statuses_stay_aligned() {
        // Every terminal variant's stored string is in the set...
        for phase in [
            AudioNotePhase::Completed,
            AudioNotePhase::Error,
            AudioNotePhase::Cancelled,
        ] {
            assert!(
                AudioNotePhase::is_terminal(phase.as_str()),
                "{} should be terminal",
                phase.as_str()
            );
        }
        // ...and every in-flight variant is excluded, so deletion is refused.
        for phase in [
            AudioNotePhase::Idle,
            AudioNotePhase::Recording,
            AudioNotePhase::Review,
            AudioNotePhase::Compressing,
            AudioNotePhase::Transcribing,
        ] {
            assert!(
                !AudioNotePhase::is_terminal(phase.as_str()),
                "{} should be in-flight",
                phase.as_str()
            );
        }
    }

    #[test]
    fn test_audio_note_phase_serialization() {
        let phase = AudioNotePhase::Recording;
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(json, "\"recording\"");

        let parsed: AudioNotePhase = serde_json::from_str("\"transcribing\"").unwrap();
        assert_eq!(parsed, AudioNotePhase::Transcribing);
    }

    #[tokio::test]
    async fn title_updates_only_change_the_matching_live_meeting() {
        let status = AudioNoteStatusHandle::default();
        status
            .start_recording(7, None, PathBuf::from("/tmp/seven.wav"), true, true)
            .await;

        status
            .set_title_if_current(8, Some("Wrong AudioNote".to_string()))
            .await;
        assert_eq!(status.get().await.title, None);
        status
            .set_title_if_current(7, Some("Canonical AudioNote Title".to_string()))
            .await;
        assert_eq!(
            status.get().await.title.as_deref(),
            Some("Canonical AudioNote Title")
        );
    }

    #[test]
    fn test_audio_note_state_default() {
        let state = AudioNoteState::default();
        assert_eq!(state.phase, AudioNotePhase::Idle);
        assert!(state.note_id.is_none());
        assert!(state.started_at.is_none());
        assert!(state.title.is_none());
        assert!(state.audio_path.is_none());
        assert!(state.last_error.is_none());
    }

    #[tokio::test]
    async fn test_status_handle_start_recording() {
        let handle = AudioNoteStatusHandle::default();
        handle
            .start_recording(
                1,
                Some("Standup".to_string()),
                PathBuf::from("/tmp/test.wav"),
                true,
                true,
            )
            .await;

        let state = handle.get().await;
        assert_eq!(state.phase, AudioNotePhase::Recording);
        assert_eq!(state.note_id, Some(1));
        assert_eq!(state.title, Some("Standup".to_string()));
        assert!(state.started_at.is_some());
    }

    #[tokio::test]
    async fn test_start_recording_clears_prior_frozen_duration() {
        let handle = AudioNoteStatusHandle::default();

        // AudioNote 1 stops and freezes its duration in Review, then errors —
        // neither `enter_review` nor `set_error` clears the frozen value.
        handle
            .start_recording(1, None, PathBuf::from("/tmp/one.wav"), true, true)
            .await;
        handle.enter_review(654).await;
        handle.set_error("boom".to_string()).await;

        // AudioNote 2 starts without an intervening reset(). Its duration must
        // be the live elapsed time, not meeting 1's frozen 654s.
        handle
            .start_recording(2, None, PathBuf::from("/tmp/two.wav"), true, true)
            .await;
        let state = handle.get().await;
        assert_eq!(state.recorded_duration_seconds, None);
        assert!(state.duration_seconds().unwrap() < 654);
    }

    #[tokio::test]
    async fn test_status_handle_set_phase() {
        let handle = AudioNoteStatusHandle::default();
        handle.set_phase(AudioNotePhase::Compressing).await;
        assert_eq!(handle.get().await.phase, AudioNotePhase::Compressing);
    }

    #[tokio::test]
    async fn test_status_handle_error() {
        let handle = AudioNoteStatusHandle::default();
        handle.set_error("test error".to_string()).await;

        let state = handle.get().await;
        assert_eq!(state.phase, AudioNotePhase::Error);
        assert_eq!(state.last_error, Some("test error".to_string()));
    }

    #[tokio::test]
    async fn test_status_handle_reset() {
        let handle = AudioNoteStatusHandle::default();
        handle
            .start_recording(
                1,
                Some("Test".to_string()),
                PathBuf::from("/tmp/test.wav"),
                true,
                true,
            )
            .await;
        handle.reset().await;

        let state = handle.get().await;
        assert_eq!(state.phase, AudioNotePhase::Idle);
        assert!(state.note_id.is_none());
    }

    #[tokio::test]
    async fn test_clear_if_current_resets_matching_meeting() {
        let handle = AudioNoteStatusHandle::default();
        handle
            .start_recording(
                7,
                Some("Test".to_string()),
                PathBuf::from("/tmp/test.wav"),
                true,
                true,
            )
            .await;
        handle.complete().await;

        // The terminal meeting lingers in the handle; deleting it clears it.
        assert!(handle.clear_if_current(7).await);
        let state = handle.get().await;
        assert_eq!(state.phase, AudioNotePhase::Idle);
        assert!(state.note_id.is_none());
        assert!(state.title.is_none());
        assert!(state.audio_path.is_none());
    }

    #[tokio::test]
    async fn test_clear_if_current_ignores_other_meeting() {
        let handle = AudioNoteStatusHandle::default();
        handle
            .start_recording(
                8,
                Some("Live".to_string()),
                PathBuf::from("/tmp/live.wav"),
                true,
                true,
            )
            .await;

        // Deleting an older meeting must not disturb the current one.
        assert!(!handle.clear_if_current(7).await);
        let state = handle.get().await;
        assert_eq!(state.phase, AudioNotePhase::Recording);
        assert_eq!(state.note_id, Some(8));
    }

    #[tokio::test]
    async fn test_clear_if_current_noop_when_idle() {
        let handle = AudioNoteStatusHandle::default();
        assert!(!handle.clear_if_current(7).await);
        assert_eq!(handle.get().await.phase, AudioNotePhase::Idle);
    }

    #[tokio::test]
    async fn test_status_handle_lifecycle() {
        let handle = AudioNoteStatusHandle::default();

        // Start
        handle
            .start_recording(1, None, PathBuf::from("/tmp/meeting.wav"), true, true)
            .await;
        assert_eq!(handle.get().await.phase, AudioNotePhase::Recording);

        // Compress
        handle.set_phase(AudioNotePhase::Compressing).await;
        assert_eq!(handle.get().await.phase, AudioNotePhase::Compressing);

        // Transcribe
        handle.set_phase(AudioNotePhase::Transcribing).await;
        assert_eq!(handle.get().await.phase, AudioNotePhase::Transcribing);

        // Complete
        handle.complete().await;
        assert_eq!(handle.get().await.phase, AudioNotePhase::Completed);
    }

    #[tokio::test]
    async fn capture_health_tracks_each_expected_audio_note_leg() {
        use crate::audio::capture_recovery::CaptureRecovery;

        let handle = AudioNoteStatusHandle::default();
        handle
            .start_recording(1, None, PathBuf::from("/tmp/meeting.wav"), false, true)
            .await;
        let degraded = handle.get().await;
        assert_eq!(degraded.phase, AudioNotePhase::Recording);
        assert!(degraded.capture_degraded);

        handle
            .apply_microphone_recovery(CaptureRecovery::Capturing)
            .await;
        assert!(!handle.get().await.capture_degraded);

        handle.mark_microphone_degraded().await;
        assert!(handle.get().await.capture_degraded);
        handle
            .apply_microphone_recovery(CaptureRecovery::Capturing)
            .await;
        assert!(!handle.get().await.capture_degraded);

        handle
            .start_recording(2, None, PathBuf::from("/tmp/meeting-2.wav"), true, false)
            .await;
        handle
            .apply_microphone_recovery(CaptureRecovery::Capturing)
            .await;
        assert!(
            handle.get().await.capture_degraded,
            "System Tap is still unavailable"
        );

        handle
            .apply_system_recovery(CaptureRecovery::Capturing)
            .await;
        assert!(!handle.get().await.capture_degraded);
        handle.mark_system_degraded().await;
        assert!(handle.get().await.capture_degraded);

        handle.enter_review(1).await;
        assert!(!handle.get().await.capture_degraded);
    }

    #[tokio::test]
    async fn capture_health_resets_on_every_session_end() {
        let handle = AudioNoteStatusHandle::default();
        let path = PathBuf::from("/tmp/meeting.wav");

        handle
            .start_recording(1, None, path.clone(), false, true)
            .await;
        handle.set_error("capture failed".to_string()).await;
        assert!(!handle.get().await.capture_degraded);

        handle
            .start_recording(2, None, path.clone(), false, true)
            .await;
        handle.complete().await;
        assert!(!handle.get().await.capture_degraded);

        handle
            .start_recording(3, None, path.clone(), false, true)
            .await;
        handle.cancelled().await;
        assert!(!handle.get().await.capture_degraded);

        handle
            .start_recording(4, None, path.clone(), false, true)
            .await;
        handle.reset().await;
        assert!(!handle.get().await.capture_degraded);

        handle.start_recording(5, None, path, true, true).await;
        assert!(!handle.get().await.capture_degraded);
    }
}
