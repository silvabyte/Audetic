//! Events that post-processing jobs can subscribe to.
//!
//! Each variant of [`Event`] carries the data the action needs. The on-disk
//! `event` column stores the stable [`EventKind`] string — adding a new
//! event means adding a kind here, then dispatching from the appropriate
//! state machine.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utoipa::ToSchema;

/// JSON-payload format version. Bump on breaking shape changes.
pub const PAYLOAD_VERSION: u32 = 1;

/// Stable identifier for an event type, as stored in the
/// `post_processing_jobs.event` column and exposed over the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    #[serde(rename = "dictation.completed")]
    DictationCompleted,
    #[serde(rename = "meeting.completed")]
    MeetingCompleted,
}

impl EventKind {
    /// Canonical wire name — also what's persisted in the `event` column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DictationCompleted => "dictation.completed",
            Self::MeetingCompleted => "meeting.completed",
        }
    }

    /// Parse from the wire string. Returns `None` for unknown kinds so
    /// callers can decide whether to surface a 400 or skip.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "dictation.completed" => Some(Self::DictationCompleted),
            "meeting.completed" => Some(Self::MeetingCompleted),
            _ => None,
        }
    }
}

/// Every event kind, in display order. Used by the API endpoint that
/// powers the web UI's event dropdown.
pub const ALL_EVENT_KINDS: &[EventKind] =
    &[EventKind::DictationCompleted, EventKind::MeetingCompleted];

/// Payload for `dictation.completed`. Mirrors the fields the user is
/// likely to want — the `dictation_id` is enough to call
/// `GET /api/history/:id` for the full record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictationCompletedPayload {
    pub dictation_id: i64,
    pub workflow_type: String,
    pub audio_path: PathBuf,
    pub text: String,
}

/// Payload for `meeting.completed`. Carries paths to the durable audio
/// and transcript files plus the transcript text inline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeetingCompletedPayload {
    pub meeting_id: i64,
    pub title: Option<String>,
    pub audio_path: PathBuf,
    pub transcript_path: PathBuf,
    pub transcript_text: String,
    pub duration_seconds: u64,
}

/// An event fired by the daemon, dispatched to matching jobs.
#[derive(Debug, Clone)]
pub enum Event {
    DictationCompleted(DictationCompletedPayload),
    MeetingCompleted(MeetingCompletedPayload),
}

impl Event {
    pub fn kind(&self) -> EventKind {
        match self {
            Self::DictationCompleted(_) => EventKind::DictationCompleted,
            Self::MeetingCompleted(_) => EventKind::MeetingCompleted,
        }
    }

    /// Serialize as the JSON envelope written to a command's stdin:
    /// `{ event, version, timestamp, data }`.
    pub fn to_envelope(&self) -> serde_json::Value {
        let (event, data) = match self {
            Self::DictationCompleted(p) => (
                EventKind::DictationCompleted.as_str(),
                serde_json::to_value(p).unwrap_or(serde_json::Value::Null),
            ),
            Self::MeetingCompleted(p) => (
                EventKind::MeetingCompleted.as_str(),
                serde_json::to_value(p).unwrap_or(serde_json::Value::Null),
            ),
        };
        serde_json::json!({
            "event": event,
            "version": PAYLOAD_VERSION,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "data": data,
        })
    }

    /// Build a synthetic payload of the given kind for the `test` endpoint —
    /// lets users dry-run a job without waiting for a real event.
    pub fn synthetic(kind: EventKind) -> Self {
        match kind {
            EventKind::DictationCompleted => Self::DictationCompleted(DictationCompletedPayload {
                dictation_id: 0,
                workflow_type: "VoiceToText".to_string(),
                audio_path: PathBuf::from("/tmp/audetic/test-dictation.wav"),
                text: "This is a synthetic test transcript.".to_string(),
            }),
            EventKind::MeetingCompleted => Self::MeetingCompleted(MeetingCompletedPayload {
                meeting_id: 0,
                title: Some("Synthetic test meeting".to_string()),
                audio_path: PathBuf::from("/tmp/audetic/test-meeting.mp3"),
                transcript_path: PathBuf::from("/tmp/audetic/test-meeting.txt"),
                transcript_text: "This is a synthetic test transcript.".to_string(),
                duration_seconds: 60,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_round_trips_string() {
        for k in ALL_EVENT_KINDS {
            assert_eq!(EventKind::from_str(k.as_str()), Some(*k));
        }
    }

    #[test]
    fn event_kind_unknown_string_is_none() {
        assert_eq!(EventKind::from_str("not.an.event"), None);
    }

    #[test]
    fn envelope_carries_event_version_and_data() {
        let payload = DictationCompletedPayload {
            dictation_id: 42,
            workflow_type: "VoiceToText".to_string(),
            audio_path: PathBuf::from("/tmp/a.wav"),
            text: "hi".to_string(),
        };
        let event = Event::DictationCompleted(payload);
        let env = event.to_envelope();
        assert_eq!(env["event"], "dictation.completed");
        assert_eq!(env["version"], PAYLOAD_VERSION);
        assert_eq!(env["data"]["dictation_id"], 42);
        assert_eq!(env["data"]["text"], "hi");
    }

    #[test]
    fn synthetic_payloads_match_their_kinds() {
        for k in ALL_EVENT_KINDS {
            assert_eq!(Event::synthetic(*k).kind(), *k);
        }
    }
}
