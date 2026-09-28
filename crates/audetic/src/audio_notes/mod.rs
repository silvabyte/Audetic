//! One capture, import, and transcription lifecycle for every Audio Note.
//! The persisted transcript is available immediately; independent intelligence
//! processing classifies it, produces artifacts, and dispatches downstream jobs.

pub mod audio_note_machine;
pub mod import;
pub mod media_inspector;
pub mod processing;
pub mod progress;
pub mod settings;
pub mod status;
pub mod title;

pub use audio_note_machine::{
    AudioNoteMachine, AudioNoteStartResult, AudioNoteStopResult, CaptureState, ToggleOutcome,
};
pub use import::{import_audio_note_file, ImportArgs, ImportResult};
pub use media_inspector::{FfprobeMediaInspector, MediaInspector};
pub use processing::{
    process_audio_note, AudioNoteDeliveryOptions, ProcessingArgs, ProcessingServices,
};
pub use progress::{AudioNoteProgressObserver, LiveProgressObserver, NoopProgressObserver};
pub use status::{
    AudioNoteCaptureSource, AudioNotePhase, AudioNoteStartOptions, AudioNoteState,
    AudioNoteStatusHandle,
};
