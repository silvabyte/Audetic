//! Meeting recording module.
//!
//! Captures both system audio and microphone during meetings, transcribes
//! the recording, then hands off to [`crate::post_processing`] for any
//! user-defined follow-up commands.

pub mod meeting_machine;
pub mod status;

pub use meeting_machine::{
    retry_meeting_transcription, CaptureState, MeetingMachine, MeetingStartResult,
    MeetingStopResult, ToggleOutcome,
};
pub use status::{MeetingPhase, MeetingStartOptions, MeetingState, MeetingStatusHandle};
