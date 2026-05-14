//! Meeting recording module.
//!
//! Captures both system audio and microphone during meetings, transcribes
//! the recording, then dispatches `meeting.completed` to
//! [`crate::post_processing`] for any user-defined follow-up commands.
//! Also handles importing existing media files as meetings — the import
//! path stages the file under the meetings dir, creates the row, and
//! drives the same post-recording pipeline a live recording uses.

pub mod import;
pub mod media_inspector;
pub mod meeting_machine;
pub mod processing;
pub mod progress;
pub mod status;

pub use import::{import_meeting_file, ImportArgs, ImportResult};
pub use media_inspector::{FfprobeMediaInspector, MediaInspector};
pub use meeting_machine::{
    retry_meeting_transcription, CaptureState, MeetingMachine, MeetingStartResult,
    MeetingStopResult, ToggleOutcome,
};
pub use processing::{process_meeting, ProcessingArgs, ProcessingServices};
pub use progress::{LiveProgressObserver, MeetingProgressObserver, NoopProgressObserver};
pub use status::{MeetingPhase, MeetingStartOptions, MeetingState, MeetingStatusHandle};
