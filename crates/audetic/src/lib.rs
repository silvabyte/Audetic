pub mod agents;
pub mod api;
pub mod app;
pub mod audio;
pub mod db;
pub mod history;
pub mod install;

// Lightweight, daemon-independent modules live in `audetic-core` and are
// re-exported here at their historical paths so the daemon's internal call
// sites (`crate::config`, `crate::global`) keep compiling unchanged. The
// standalone `audetic` CLI depends on `audetic-core` directly.
pub use audetic_core::{config, global};
pub mod keybind;
pub mod logs;
pub mod meeting;
pub mod meeting_artifacts;
pub mod normalizer;
pub mod post_processing;
pub mod summary_templates;
pub mod system;
pub mod text_io;
pub mod transcription;
pub mod ui;
pub mod update;
