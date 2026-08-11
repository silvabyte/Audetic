//! Meeting status types and shared state handle.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::audio::capture_recovery::CaptureRecovery;

/// Phase of a meeting recording lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MeetingPhase {
    Idle,
    Recording,
    /// Recording has stopped and the WAV is on disk, but the user has not yet
    /// confirmed it for transcription. They can play it back and trim the
    /// start/end before sending it on (or discard it). See
    /// `MeetingMachine::confirm`.
    Review,
    Compressing,
    Transcribing,
    Completed,
    Error,
    Cancelled,
}

impl MeetingPhase {
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
    /// and the guarded `DELETE` SQL in `MeetingRepository::soft_delete`, so the
    /// Rust check and the SQL predicate can't drift apart. Recording, review,
    /// and the processing phases are deliberately absent: while in-flight, the
    /// meeting machine and background pipeline still hold the id, so hiding the
    /// row would 404 the active/review UI (`/meetings/:id/audio` and detail)
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeetingStartOptions {
    pub title: Option<String>,
}

/// Current meeting state, readable by API handlers.
#[derive(Debug, Clone)]
pub struct MeetingState {
    pub phase: MeetingPhase,
    pub capture_degraded: bool,
    pub meeting_id: Option<i64>,
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
}

impl Default for MeetingState {
    fn default() -> Self {
        Self {
            phase: MeetingPhase::Idle,
            capture_degraded: false,
            meeting_id: None,
            started_at: None,
            title: None,
            audio_path: None,
            last_error: None,
            recorded_duration_seconds: None,
            microphone_capturing: false,
            system_capturing: false,
        }
    }
}

impl MeetingState {
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
        self.capture_degraded = self.phase == MeetingPhase::Recording
            && (!self.microphone_capturing || !self.system_capturing);
    }

    fn clear_capture_health(&mut self) {
        self.capture_degraded = false;
        self.microphone_capturing = false;
        self.system_capturing = false;
    }
}

/// Thread-safe handle for sharing meeting state between the machine and API handlers.
#[derive(Clone, Default)]
pub struct MeetingStatusHandle {
    inner: Arc<Mutex<MeetingState>>,
}

impl MeetingStatusHandle {
    pub async fn get(&self) -> MeetingState {
        self.inner.lock().await.clone()
    }

    pub async fn start_recording(
        &self,
        meeting_id: i64,
        title: Option<String>,
        audio_path: PathBuf,
        microphone_capturing: bool,
        system_capturing: bool,
    ) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Recording;
        state.meeting_id = Some(meeting_id);
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

    pub async fn set_phase(&self, phase: MeetingPhase) {
        let mut state = self.inner.lock().await;
        state.phase = phase;
        if phase != MeetingPhase::Recording {
            state.clear_capture_health();
        }
    }

    /// Transition into the Review phase, freezing the recorded duration so the
    /// reported timer stops climbing and the trim UI knows the end bound.
    pub async fn enter_review(&self, duration_seconds: u64) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Review;
        state.recorded_duration_seconds = Some(duration_seconds);
        state.last_error = None;
        state.clear_capture_health();
    }

    pub async fn set_error(&self, error: String) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Error;
        state.last_error = Some(error);
        state.clear_capture_health();
    }

    pub async fn reset(&self) {
        let mut state = self.inner.lock().await;
        *state = MeetingState::default();
    }

    /// Reset to Idle, but only if the state still describes the given meeting.
    ///
    /// Called after a soft-delete so `GET /meetings/status` stops reporting a
    /// meeting that is hidden everywhere else. Check-and-reset happens under a
    /// single lock acquisition: meeting ids are never reused and the delete's
    /// SQL guard only hides terminal rows, so an id match here can only be the
    /// settled meeting the machine has finished with — never a live recording
    /// that started after the delete was accepted. Returns whether the state
    /// was cleared.
    pub async fn clear_if_current(&self, meeting_id: i64) -> bool {
        let mut state = self.inner.lock().await;
        if state.meeting_id != Some(meeting_id) {
            return false;
        }
        *state = MeetingState::default();
        true
    }

    pub async fn complete(&self) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Completed;
        state.clear_capture_health();
    }

    pub async fn cancelled(&self) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Cancelled;
        state.clear_capture_health();
    }

    pub(crate) async fn apply_microphone_recovery(&self, recovery: CaptureRecovery) {
        let mut state = self.inner.lock().await;
        if state.phase != MeetingPhase::Recording {
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
        if state.phase != MeetingPhase::Recording {
            return;
        }
        state.microphone_capturing = false;
        state.update_capture_degraded();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_meeting_phase_as_str() {
        assert_eq!(MeetingPhase::Idle.as_str(), "idle");
        assert_eq!(MeetingPhase::Recording.as_str(), "recording");
        assert_eq!(MeetingPhase::Review.as_str(), "review");
        assert_eq!(MeetingPhase::Compressing.as_str(), "compressing");
        assert_eq!(MeetingPhase::Transcribing.as_str(), "transcribing");
        assert_eq!(MeetingPhase::Completed.as_str(), "completed");
        assert_eq!(MeetingPhase::Error.as_str(), "error");
    }

    #[test]
    fn test_terminal_statuses_stay_aligned() {
        // Every terminal variant's stored string is in the set...
        for phase in [
            MeetingPhase::Completed,
            MeetingPhase::Error,
            MeetingPhase::Cancelled,
        ] {
            assert!(
                MeetingPhase::is_terminal(phase.as_str()),
                "{} should be terminal",
                phase.as_str()
            );
        }
        // ...and every in-flight variant is excluded, so deletion is refused.
        for phase in [
            MeetingPhase::Idle,
            MeetingPhase::Recording,
            MeetingPhase::Review,
            MeetingPhase::Compressing,
            MeetingPhase::Transcribing,
        ] {
            assert!(
                !MeetingPhase::is_terminal(phase.as_str()),
                "{} should be in-flight",
                phase.as_str()
            );
        }
    }

    #[test]
    fn test_meeting_phase_serialization() {
        let phase = MeetingPhase::Recording;
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(json, "\"recording\"");

        let parsed: MeetingPhase = serde_json::from_str("\"transcribing\"").unwrap();
        assert_eq!(parsed, MeetingPhase::Transcribing);
    }

    #[test]
    fn test_meeting_state_default() {
        let state = MeetingState::default();
        assert_eq!(state.phase, MeetingPhase::Idle);
        assert!(state.meeting_id.is_none());
        assert!(state.started_at.is_none());
        assert!(state.title.is_none());
        assert!(state.audio_path.is_none());
        assert!(state.last_error.is_none());
    }

    #[tokio::test]
    async fn test_status_handle_start_recording() {
        let handle = MeetingStatusHandle::default();
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
        assert_eq!(state.phase, MeetingPhase::Recording);
        assert_eq!(state.meeting_id, Some(1));
        assert_eq!(state.title, Some("Standup".to_string()));
        assert!(state.started_at.is_some());
    }

    #[tokio::test]
    async fn test_start_recording_clears_prior_frozen_duration() {
        let handle = MeetingStatusHandle::default();

        // Meeting 1 stops and freezes its duration in Review, then errors —
        // neither `enter_review` nor `set_error` clears the frozen value.
        handle
            .start_recording(1, None, PathBuf::from("/tmp/one.wav"), true, true)
            .await;
        handle.enter_review(654).await;
        handle.set_error("boom".to_string()).await;

        // Meeting 2 starts without an intervening reset(). Its duration must
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
        let handle = MeetingStatusHandle::default();
        handle.set_phase(MeetingPhase::Compressing).await;
        assert_eq!(handle.get().await.phase, MeetingPhase::Compressing);
    }

    #[tokio::test]
    async fn test_status_handle_error() {
        let handle = MeetingStatusHandle::default();
        handle.set_error("test error".to_string()).await;

        let state = handle.get().await;
        assert_eq!(state.phase, MeetingPhase::Error);
        assert_eq!(state.last_error, Some("test error".to_string()));
    }

    #[tokio::test]
    async fn test_status_handle_reset() {
        let handle = MeetingStatusHandle::default();
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
        assert_eq!(state.phase, MeetingPhase::Idle);
        assert!(state.meeting_id.is_none());
    }

    #[tokio::test]
    async fn test_clear_if_current_resets_matching_meeting() {
        let handle = MeetingStatusHandle::default();
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
        assert_eq!(state.phase, MeetingPhase::Idle);
        assert!(state.meeting_id.is_none());
        assert!(state.title.is_none());
        assert!(state.audio_path.is_none());
    }

    #[tokio::test]
    async fn test_clear_if_current_ignores_other_meeting() {
        let handle = MeetingStatusHandle::default();
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
        assert_eq!(state.phase, MeetingPhase::Recording);
        assert_eq!(state.meeting_id, Some(8));
    }

    #[tokio::test]
    async fn test_clear_if_current_noop_when_idle() {
        let handle = MeetingStatusHandle::default();
        assert!(!handle.clear_if_current(7).await);
        assert_eq!(handle.get().await.phase, MeetingPhase::Idle);
    }

    #[tokio::test]
    async fn test_status_handle_lifecycle() {
        let handle = MeetingStatusHandle::default();

        // Start
        handle
            .start_recording(1, None, PathBuf::from("/tmp/meeting.wav"), true, true)
            .await;
        assert_eq!(handle.get().await.phase, MeetingPhase::Recording);

        // Compress
        handle.set_phase(MeetingPhase::Compressing).await;
        assert_eq!(handle.get().await.phase, MeetingPhase::Compressing);

        // Transcribe
        handle.set_phase(MeetingPhase::Transcribing).await;
        assert_eq!(handle.get().await.phase, MeetingPhase::Transcribing);

        // Complete
        handle.complete().await;
        assert_eq!(handle.get().await.phase, MeetingPhase::Completed);
    }

    #[tokio::test]
    async fn capture_health_tracks_each_expected_meeting_leg() {
        use crate::audio::capture_recovery::CaptureRecovery;

        let handle = MeetingStatusHandle::default();
        handle
            .start_recording(1, None, PathBuf::from("/tmp/meeting.wav"), false, true)
            .await;
        let degraded = handle.get().await;
        assert_eq!(degraded.phase, MeetingPhase::Recording);
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

        handle.enter_review(1).await;
        assert!(!handle.get().await.capture_degraded);
    }

    #[tokio::test]
    async fn capture_health_resets_on_every_session_end() {
        let handle = MeetingStatusHandle::default();
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
