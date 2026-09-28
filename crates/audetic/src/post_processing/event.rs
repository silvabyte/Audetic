//! Unified events consumed by explicitly configured post-processing jobs.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utoipa::ToSchema;

pub const PAYLOAD_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum EventKind {
    #[serde(rename = "audio_note.completed")]
    AudioNoteCompleted,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AudioNoteCompleted => "audio_note.completed",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "audio_note.completed" => Some(Self::AudioNoteCompleted),
            _ => None,
        }
    }
}

pub const ALL_EVENT_KINDS: &[EventKind] = &[EventKind::AudioNoteCompleted];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioNoteCompletedPayload {
    pub note_id: i64,
    pub title: Option<String>,
    pub audio_path: PathBuf,
    pub transcript_path: PathBuf,
    pub transcript_text: String,
    pub duration_seconds: u64,
    pub classification: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub enum Event {
    AudioNoteCompleted(AudioNoteCompletedPayload),
}

impl Event {
    pub fn kind(&self) -> EventKind {
        EventKind::AudioNoteCompleted
    }

    pub fn to_envelope(&self) -> serde_json::Value {
        let Self::AudioNoteCompleted(payload) = self;
        serde_json::json!({
            "event": self.kind().as_str(), "version": PAYLOAD_VERSION,
            "timestamp": chrono::Utc::now().to_rfc3339(), "data": payload,
        })
    }

    pub fn synthetic(kind: EventKind) -> Self {
        match kind {
            EventKind::AudioNoteCompleted => Self::AudioNoteCompleted(AudioNoteCompletedPayload {
                note_id: 0,
                title: Some("Synthetic audio note".into()),
                audio_path: PathBuf::from("/tmp/audetic/test-note.wav"),
                transcript_path: PathBuf::from("/tmp/audetic/test-note.txt"),
                transcript_text: "This is a synthetic test transcript.".into(),
                duration_seconds: 60,
                classification: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_envelope_and_kind_round_trip() {
        for kind in ALL_EVENT_KINDS {
            assert_eq!(EventKind::from_str(kind.as_str()), Some(*kind));
            let event = Event::synthetic(*kind);
            assert_eq!(event.kind(), *kind);
            let envelope = event.to_envelope();
            assert_eq!(envelope["event"], "audio_note.completed");
            assert_eq!(envelope["version"], PAYLOAD_VERSION);
            assert_eq!(envelope["data"]["note_id"], 0);
            assert!(envelope["data"]["classification"].is_null());
            assert!(envelope["data"]["transcript_text"].is_string());
        }
        assert_eq!(EventKind::from_str("meeting.completed"), None);
        assert_eq!(EventKind::from_str("dictation.completed"), None);
    }
}
