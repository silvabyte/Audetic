pub mod agents;
pub mod api;
pub mod app;
pub mod audio;
pub mod audio_note_artifacts;
pub mod audio_note_migration;
pub mod audio_notes;
pub mod db;
pub mod install;
pub mod note_intelligence;

// Lightweight, daemon-independent modules live in `audetic-core` and are
// re-exported here at their historical paths so the daemon's internal call
// sites (`crate::config`, `crate::global`) keep compiling unchanged. The
// standalone `audetic` CLI depends on `audetic-core` directly.
pub use audetic_core::{config, global};
pub mod keybind;
pub mod logs;
pub mod normalizer;
pub mod post_processing;
pub mod setup;
pub mod summary_templates;
pub mod system;
pub mod text_io;
pub mod transcription;
pub mod ui;
