//! Meeting recording module.
//!
//! Captures both system audio and microphone during meetings,
//! transcribes the recording, and optionally runs post-processing hooks.

pub mod meeting_machine;
pub mod post_meeting_hook;
pub mod status;

pub use meeting_machine::{MeetingMachine, MeetingStartResult, MeetingStopResult, ToggleOutcome};
pub use post_meeting_hook::{MeetingResult, PostMeetingHook, ShellCommandHook};
pub use status::{MeetingPhase, MeetingStartOptions, MeetingState, MeetingStatusHandle};
