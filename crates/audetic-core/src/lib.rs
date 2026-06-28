//! Shared, daemon-independent building blocks for Audetic.
//!
//! Both the daemon (`audeticd`) and the standalone CLI (`audetic`) depend on
//! this crate. It deliberately carries only lightweight, self-contained pieces
//! — no audio capture, no HTTP server, no embedded web UI — so the CLI binary
//! can link it without pulling in the daemon's heavy dependency tree.
//!
//! The daemon re-exports these modules at their historical paths (e.g.
//! `crate::config`, `crate::api::url`, `crate::transcription::jobs_client`) so
//! its internal call sites keep compiling unchanged.

pub mod clipboard;
pub mod compression;
pub mod config;
pub mod ffmpeg;
pub mod global;
pub mod jobs_client;
pub mod local_models;
pub mod url;
