pub mod audio_mixer;
pub mod audio_source;
pub mod audio_stream_manager;
pub mod mic_source;
pub mod recording_machine;
pub mod system_source;

pub use audio_stream_manager::AudioStreamManager;
pub use recording_machine::{
    BehaviorOptions, CompletedJob, JobOptions, RecordingMachine, RecordingPhase, RecordingStatus,
    RecordingStatusHandle, ToggleResult,
};
