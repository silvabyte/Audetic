//! Audio source abstraction for capturing audio from different inputs.

use anyhow::Result;

/// Trait for audio capture sources (microphone, system audio, etc.).
///
/// Each source captures audio independently and returns samples when stopped.
/// Sources may have different sample rates — the caller (mixer) handles resampling.
pub trait AudioSource {
    /// Start capturing audio.
    fn start(&mut self) -> Result<()>;

    /// Stop capturing and return all captured samples.
    fn stop(&mut self) -> Result<Vec<f32>>;

    /// Whether this source is currently capturing.
    fn is_active(&self) -> bool;

    /// The sample rate of captured audio.
    fn sample_rate(&self) -> u32;
}
