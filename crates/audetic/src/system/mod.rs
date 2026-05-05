//! System-level dependency resolution.
//!
//! Today this only covers FFmpeg (used by the meeting + transcribe flows for
//! mp3 compression). The daemon prefers an app-local copy installed via
//! `ffmpeg-sidecar` so users don't need a system-wide `apt`/`pacman` install.

pub mod ffmpeg;
