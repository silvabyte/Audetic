//! Audio source abstraction for capturing audio from different inputs.

use anyhow::Result;

use super::capture_recovery::CaptureRecovery;
use super::stream_event::StreamDeath;

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

/// Meeting microphone capture, including live-stream replacement while the
/// logical meeting session remains active.
#[async_trait::async_trait(?Send)]
pub trait MeetingMicSource: AudioSource {
    async fn default_input_switched(&mut self) -> Result<CaptureRecovery> {
        Ok(CaptureRecovery::Ignored)
    }

    async fn stream_died(&mut self, _death: StreamDeath) -> Result<CaptureRecovery> {
        Ok(CaptureRecovery::Ignored)
    }

    fn has_live_stream(&self) -> bool {
        self.is_active()
    }
}
