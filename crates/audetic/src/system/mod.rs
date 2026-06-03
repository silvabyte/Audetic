//! System-level dependency resolution.
//!
//! Today this only covers FFmpeg (used by the meeting + transcribe flows for
//! mp3 compression). The daemon prefers an app-local copy installed via
//! `ffmpeg-sidecar` so users don't need a system-wide `apt`/`pacman` install.

// FFmpeg resolution/install is shared with the standalone CLI; it lives in
// `audetic-core` and is re-exported here as `crate::system::ffmpeg`.
pub use audetic_core::ffmpeg;
