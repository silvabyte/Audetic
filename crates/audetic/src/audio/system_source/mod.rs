//! Platform-specific system-audio capture (what others say on Zoom/Meet/etc.).
//!
//! Linux uses PipeWire's `pw-cat --record` to drain the default sink's monitor
//! source. macOS uses cpal's native loopback (an audio tap on the default
//! output device, introduced in cpal 0.17 / macOS 14.6+).
//!
//! Both impls expose the same `SystemAudioSource` struct implementing
//! `AudioSource`, so callers don't care which platform they're on.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::SystemAudioSource;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::SystemAudioSource;

#[cfg(target_os = "macos")]
pub mod permissions;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod stub;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use stub::SystemAudioSource;
