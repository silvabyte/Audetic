//! Meeting status types and shared state handle.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

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

    /// Whether a meeting with this stored `status` is settled and therefore
    /// safe to soft-delete. Recording, review, and the processing phases are
    /// *in-flight* — the meeting machine and background pipeline still hold the
    /// id, so deleting would 404 the active/review UI (`/meetings/:id/audio`
    /// and detail) and break completion auto-nav. Allow-list terminal states so
    /// any future in-flight phase defaults to non-deletable.
    pub fn is_terminal(status: &str) -> bool {
        status == Self::Completed.as_str()
            || status == Self::Error.as_str()
            || status == Self::Cancelled.as_str()
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
    pub meeting_id: Option<i64>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub title: Option<String>,
    pub audio_path: Option<PathBuf>,
    pub last_error: Option<String>,
    /// Recorded length frozen at stop. Once set (Review onward), it is the
    /// duration reported to clients so the timer stops climbing and the trim
    /// UI has an accurate end bound.
    pub recorded_duration_seconds: Option<u64>,
}

impl Default for MeetingState {
    fn default() -> Self {
        Self {
            phase: MeetingPhase::Idle,
            meeting_id: None,
            started_at: None,
            title: None,
            audio_path: None,
            last_error: None,
            recorded_duration_seconds: None,
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
    ) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Recording;
        state.meeting_id = Some(meeting_id);
        state.started_at = Some(chrono::Utc::now());
        state.title = title;
        state.audio_path = Some(audio_path);
        state.last_error = None;
    }

    pub async fn set_phase(&self, phase: MeetingPhase) {
        let mut state = self.inner.lock().await;
        state.phase = phase;
    }

    /// Transition into the Review phase, freezing the recorded duration so the
    /// reported timer stops climbing and the trim UI knows the end bound.
    pub async fn enter_review(&self, duration_seconds: u64) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Review;
        state.recorded_duration_seconds = Some(duration_seconds);
        state.last_error = None;
    }

    pub async fn set_error(&self, error: String) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Error;
        state.last_error = Some(error);
    }

    pub async fn reset(&self) {
        let mut state = self.inner.lock().await;
        *state = MeetingState::default();
    }

    pub async fn complete(&self) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Completed;
    }

    pub async fn cancelled(&self) {
        let mut state = self.inner.lock().await;
        state.phase = MeetingPhase::Cancelled;
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
            )
            .await;

        let state = handle.get().await;
        assert_eq!(state.phase, MeetingPhase::Recording);
        assert_eq!(state.meeting_id, Some(1));
        assert_eq!(state.title, Some("Standup".to_string()));
        assert!(state.started_at.is_some());
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
            .start_recording(1, Some("Test".to_string()), PathBuf::from("/tmp/test.wav"))
            .await;
        handle.reset().await;

        let state = handle.get().await;
        assert_eq!(state.phase, MeetingPhase::Idle);
        assert!(state.meeting_id.is_none());
    }

    #[tokio::test]
    async fn test_status_handle_lifecycle() {
        let handle = MeetingStatusHandle::default();

        // Start
        handle
            .start_recording(1, None, PathBuf::from("/tmp/meeting.wav"))
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
}
