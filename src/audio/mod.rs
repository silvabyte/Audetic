pub mod audio_stream_manager;
pub mod recording_machine;

pub use audio_stream_manager::AudioStreamManager;
pub use recording_machine::{
    BehaviorOptions, CompletedJob, JobOptions, RecordingMachine, RecordingPhase, RecordingStatus,
    RecordingStatusHandle, ToggleResult,
};
