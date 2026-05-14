//! Observer trait for meeting pipeline progress events.
//!
//! The post-recording pipeline (`processing::process_meeting`) emits phase
//! transitions, errors, and completion through this trait so the pipeline
//! itself stays oblivious to live-recording specifics (the singleton status
//! handle, the Hyprland indicator). Live recordings inject
//! `LiveProgressObserver` to forward the events; imports and retries inject
//! `NoopProgressObserver` because they share the same DB-driven status
//! surface as every other meeting and shouldn't touch the live indicator.

use async_trait::async_trait;

use super::status::{MeetingPhase, MeetingStatusHandle};
use crate::ui::Indicator;

/// Receives progress events from the meeting processing pipeline.
#[async_trait]
pub trait MeetingProgressObserver: Send + Sync {
    /// Called when the pipeline transitions into a new phase.
    async fn on_phase(&self, phase: MeetingPhase);

    /// Called when the pipeline fails. After this the meeting is in a
    /// terminal `error` state in the database.
    async fn on_error(&self, message: &str);

    /// Called when the pipeline finishes successfully. The transcript preview
    /// is provided so notification surfaces can show it.
    async fn on_complete(&self, transcript_preview: &str);
}

/// Drops every event. Used for imports, retries, and tests — anything that
/// shouldn't drive the live-recording status handle or Hyprland indicator.
pub struct NoopProgressObserver;

#[async_trait]
impl MeetingProgressObserver for NoopProgressObserver {
    async fn on_phase(&self, _phase: MeetingPhase) {}
    async fn on_error(&self, _message: &str) {}
    async fn on_complete(&self, _transcript_preview: &str) {}
}

/// Observer for a live recording: fans events out to the in-memory status
/// handle (consumed by the API status endpoint and waybar) and the desktop
/// indicator (Hyprland notifications + audio feedback).
pub struct LiveProgressObserver {
    pub status: MeetingStatusHandle,
    pub indicator: Indicator,
}

impl LiveProgressObserver {
    pub fn new(status: MeetingStatusHandle, indicator: Indicator) -> Self {
        Self { status, indicator }
    }
}

#[async_trait]
impl MeetingProgressObserver for LiveProgressObserver {
    async fn on_phase(&self, phase: MeetingPhase) {
        self.status.set_phase(phase).await;
    }

    async fn on_error(&self, message: &str) {
        self.status.set_error(message.to_string()).await;
        if let Err(e) = self.indicator.show_error(message).await {
            tracing::warn!("Failed to show error indicator: {}", e);
        }
    }

    async fn on_complete(&self, transcript_preview: &str) {
        self.status.complete().await;
        if let Err(e) = self.indicator.show_complete(transcript_preview).await {
            tracing::warn!("Failed to show completion indicator: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Test observer that records every event for assertion. Stays in the
    /// test module to avoid coupling production code to test concerns.
    struct RecordingObserver {
        phases: Arc<Mutex<Vec<MeetingPhase>>>,
        errors: Arc<Mutex<Vec<String>>>,
        completions: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingObserver {
        fn new() -> Self {
            Self {
                phases: Arc::new(Mutex::new(Vec::new())),
                errors: Arc::new(Mutex::new(Vec::new())),
                completions: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl MeetingProgressObserver for RecordingObserver {
        async fn on_phase(&self, phase: MeetingPhase) {
            self.phases.lock().await.push(phase);
        }
        async fn on_error(&self, message: &str) {
            self.errors.lock().await.push(message.to_string());
        }
        async fn on_complete(&self, transcript_preview: &str) {
            self.completions
                .lock()
                .await
                .push(transcript_preview.to_string());
        }
    }

    #[tokio::test]
    async fn noop_observer_drops_everything() {
        let obs = NoopProgressObserver;
        obs.on_phase(MeetingPhase::Compressing).await;
        obs.on_error("boom").await;
        obs.on_complete("hello").await;
        // Just verifies the calls don't panic.
    }

    #[tokio::test]
    async fn live_observer_drives_status_handle() {
        let status = MeetingStatusHandle::default();
        let indicator = Indicator::new();
        let obs = LiveProgressObserver::new(status.clone(), indicator);

        obs.on_phase(MeetingPhase::Compressing).await;
        assert_eq!(status.get().await.phase, MeetingPhase::Compressing);

        obs.on_phase(MeetingPhase::Transcribing).await;
        assert_eq!(status.get().await.phase, MeetingPhase::Transcribing);
    }

    #[tokio::test]
    async fn live_observer_on_error_updates_handle() {
        let status = MeetingStatusHandle::default();
        let obs = LiveProgressObserver::new(status.clone(), Indicator::new());

        obs.on_error("boom").await;

        let state = status.get().await;
        assert_eq!(state.phase, MeetingPhase::Error);
        assert_eq!(state.last_error.as_deref(), Some("boom"));
    }

    #[tokio::test]
    async fn recording_observer_captures_pipeline_events() {
        let obs = RecordingObserver::new();
        obs.on_phase(MeetingPhase::Compressing).await;
        obs.on_phase(MeetingPhase::Transcribing).await;
        obs.on_complete("the transcript").await;

        assert_eq!(
            obs.phases.lock().await.as_slice(),
            &[MeetingPhase::Compressing, MeetingPhase::Transcribing]
        );
        assert_eq!(obs.errors.lock().await.len(), 0);
        assert_eq!(obs.completions.lock().await.len(), 1);
    }
}
